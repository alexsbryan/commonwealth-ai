//! Snapshot restorer — extracted out of `crate::snapshot`.
//!
//! `restore_snapshot_archive` is the public entry; the rest are the
//! private helpers that drive the tar/zstd extraction and the corpus-id
//! rewrite invariant. Behaviour-preserving — same arms, same errors,
//! same tracing as before the move.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::snapshot::{
    hash_and_size, read_manifest_from_archive, snapshot_enrichment_path, snapshot_index_path,
    SnapshotManifest, SNAPSHOT_ENRICHMENT_PREFIX, SNAPSHOT_INDEX_PREFIX,
    SNAPSHOT_MANIFEST_FILENAME,
};

/// Result of a successful restore.
#[derive(Debug, Clone)]
pub struct RestoreOutcome {
    /// The manifest as read from the archive.
    pub manifest: SnapshotManifest,
    /// Path the index subtree was extracted into, typically
    /// `<sovereign_data_dir>/indexes/<corpus_id>/`.
    pub index_dir: PathBuf,
    /// Path the enrichment subtree was extracted into, when the
    /// archive carried one.
    pub enrichment_dir: Option<PathBuf>,
    /// Bytes consumed verifying the archive sha256 (== archive size).
    pub archive_size_bytes: u64,
}

/// Extract a `.tar.zst` snapshot under `sovereign_data_dir` and return
/// a description of what was restored.
///
/// `sovereign_data_dir` is the parent of `indexes/` and `enrichment/` —
/// typically `~/.sovereign/`. The archive's entries already include
/// these subdirectory prefixes, so extracting under that root places
/// each piece in the right place.
///
/// `target_corpus_id` lets the caller rename the corpus on restore:
/// tar entries with prefix `indexes/<archive_corpus_id>/` are rewritten
/// to `indexes/<target_corpus_id>/` during extraction, and the
/// on-disk `_corpus_meta.json::corpus_id` is patched to match. Pass
/// `manifest.corpus_id` (or any equal value) to preserve the original
/// id. Renaming enables side-by-side installs (testing, branches),
/// and also prevents a "sibling" recipe pointing at the same
/// published snapshot from silently clobbering the original install.
///
/// `expected_sha256`, when set, gates restore on a streaming hash of
/// the archive before any tar entry is extracted. This is the
/// authoritative integrity check; the in-archive manifest carries a
/// purely informational `archive_sha256: None`.
///
/// `local_embedding_model` / `local_embedding_dimensions` describe the
/// embedding model that will be used at query time. If the manifest's
/// model name or dimensions don't match, restore is refused with an
/// error — silently extracting an incompatible index would poison the
/// vector store. The caller is expected to catch this error and fall
/// through to the normal acquire/extract/chunk/embed path.
pub fn restore_snapshot_archive(
    archive_path: &Path,
    sovereign_data_dir: &Path,
    target_corpus_id: &str,
    expected_sha256: Option<&str>,
    local_embedding_model: &str,
    local_embedding_dimensions: usize,
) -> Result<RestoreOutcome> {
    if let Some(want) = expected_sha256 {
        let (got, size) = hash_and_size(archive_path)?;
        if got != want {
            return Err(Error::InvalidInput(format!(
                "snapshot sha256 mismatch at {}: archive={} expected={}",
                archive_path.display(),
                got,
                want
            )));
        }
        tracing::info!(
            archive = %archive_path.display(),
            sha256 = %got,
            size_bytes = size,
            "snapshot: sha256 verified"
        );
    }

    let manifest = read_manifest_from_archive(archive_path)?;
    manifest.check_embedding_compatibility(local_embedding_model, local_embedding_dimensions)?;
    let archive_corpus_id = manifest.corpus_id.clone();
    let renaming = archive_corpus_id != target_corpus_id;
    if renaming {
        tracing::info!(
            archive_corpus_id = %archive_corpus_id,
            target_corpus_id = %target_corpus_id,
            "snapshot: extracting with rename"
        );
    }

    std::fs::create_dir_all(sovereign_data_dir)?;
    let archive_size_bytes = extract_snapshot_entries(
        archive_path,
        sovereign_data_dir,
        &archive_corpus_id,
        target_corpus_id,
    )?;

    let index_dir = sovereign_data_dir.join(snapshot_index_path(target_corpus_id));
    let enrichment_dir = if manifest.atlas_included {
        let p = sovereign_data_dir.join(snapshot_enrichment_path(target_corpus_id));
        if p.exists() {
            Some(p)
        } else {
            None
        }
    } else {
        None
    };
    if !index_dir.exists() {
        return Err(Error::InvalidInput(format!(
            "snapshot archive did not produce expected index directory at {}",
            index_dir.display()
        )));
    }

    if renaming {
        patch_meta_corpus_id(&index_dir, target_corpus_id)?;
    }

    Ok(RestoreOutcome {
        manifest,
        index_dir,
        enrichment_dir,
        archive_size_bytes,
    })
}

/// Read the restored `_corpus_meta.json`, overwrite its `corpus_id`
/// field (and `corpus_name` to a sensible default if it still matches
/// the archive's) with `target_corpus_id`, and write the file back.
/// Preserves every other field byte-for-byte by going through
/// `serde_json::Value` instead of a typed struct.
fn patch_meta_corpus_id(index_dir: &Path, target_corpus_id: &str) -> Result<()> {
    let meta_path = index_dir.join("_corpus_meta.json");
    let bytes = std::fs::read(&meta_path)?;
    let mut value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Serialization(format!("parse {}: {e}", meta_path.display())))?;
    let obj = value.as_object_mut().ok_or_else(|| {
        Error::Serialization(format!("{} is not a JSON object", meta_path.display()))
    })?;
    obj.insert(
        "corpus_id".to_string(),
        serde_json::Value::String(target_corpus_id.to_string()),
    );
    let serialised = serde_json::to_vec_pretty(&value)
        .map_err(|e| Error::Serialization(format!("re-serialise {}: {e}", meta_path.display())))?;
    std::fs::write(&meta_path, serialised)?;
    tracing::info!(
        meta = %meta_path.display(),
        corpus_id = %target_corpus_id,
        "snapshot: patched _corpus_meta.json::corpus_id"
    );
    Ok(())
}

/// Stream entries out of the archive into `dest`, skipping the
/// manifest (which is an archive-internal artifact, not part of the
/// on-disk index layout). Returns the total uncompressed bytes
/// written.
///
/// When `archive_corpus_id != target_corpus_id`, tar entry paths of
/// the form `indexes/<archive_id>/...` and `enrichment/<archive_id>/...`
/// are rewritten to use `<target_id>/...` before extraction. This lets
/// a caller restore the archive under a different corpus id without
/// post-hoc directory renaming.
///
/// Path-traversal entries (`..`, absolute paths) are rejected — both
/// because we constrain the rewritten path ourselves and because the
/// `tar` crate's unpack path enforces it as a defence in depth.
fn extract_snapshot_entries(
    archive_path: &Path,
    dest: &Path,
    archive_corpus_id: &str,
    target_corpus_id: &str,
) -> Result<u64> {
    let file = File::open(archive_path)?;
    let zstd_reader = zstd::stream::Decoder::new(file)
        .map_err(|e| Error::Io(io::Error::other(format!("zstd open: {e}"))))?;
    let mut archive = tar::Archive::new(zstd_reader);
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    archive.set_overwrite(true);

    let needs_rewrite = archive_corpus_id != target_corpus_id;
    let mut total: u64 = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let original_path = entry.path()?.into_owned();
        if original_path.to_str() == Some(SNAPSHOT_MANIFEST_FILENAME) {
            continue;
        }
        let rewritten = if needs_rewrite {
            rewrite_corpus_id_in_path(&original_path, archive_corpus_id, target_corpus_id)
        } else {
            original_path.clone()
        };
        if rewritten.is_absolute()
            || rewritten
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            tracing::warn!(
                entry = %original_path.display(),
                rewritten = %rewritten.display(),
                "snapshot extract: refusing unsafe path"
            );
            continue;
        }
        let target = dest.join(&rewritten);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let size = entry.size();
        entry.unpack(&target)?;
        total += size;
    }
    Ok(total)
}

/// Rewrite the leading `<prefix>/<archive_id>/...` of a tar entry path
/// to `<prefix>/<target_id>/...` for the two known prefix dirs
/// (`indexes/`, `enrichment/`). Returns the input unchanged if it
/// doesn't match the expected layout — defensive: an archive that
/// added new top-level dirs in a future schema would pass through
/// without surprise rewrites.
fn rewrite_corpus_id_in_path(
    path: &Path,
    archive_corpus_id: &str,
    target_corpus_id: &str,
) -> PathBuf {
    let mut components = path.components();
    let Some(first) = components.next() else {
        return path.to_path_buf();
    };
    let prefix = match first {
        std::path::Component::Normal(s) => s.to_string_lossy().to_string(),
        _ => return path.to_path_buf(),
    };
    if prefix != SNAPSHOT_INDEX_PREFIX && prefix != SNAPSHOT_ENRICHMENT_PREFIX {
        return path.to_path_buf();
    }
    let Some(second) = components.next() else {
        return path.to_path_buf();
    };
    let archive_id_component = match second {
        std::path::Component::Normal(s) => s.to_string_lossy().to_string(),
        _ => return path.to_path_buf(),
    };
    if archive_id_component != archive_corpus_id {
        return path.to_path_buf();
    }
    let mut out = PathBuf::from(&prefix);
    out.push(target_corpus_id);
    for c in components {
        out.push(c.as_os_str());
    }
    out
}
