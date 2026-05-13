//! Prebuilt-index snapshot format.
//!
//! A snapshot is a `.tar.zst` archive that captures one node's
//! fully-indexed corpus so another node can restore it without
//! re-running acquire/extract/chunk/embed. The archive layout is:
//!
//! ```text
//! _snapshot_manifest.json     # contract — embedding model, fingerprint, contents
//! indexes/<corpus_id>/        # mirrors ~/.sovereign/indexes/<corpus_id>/
//! enrichment/<corpus_id>/     # mirrors ~/.sovereign/enrichment/<corpus_id>/ (optional)
//! ```
//!
//! The manifest is the load-bearing piece. A restorer reads it
//! *before* extracting anything and refuses to proceed if the
//! `embedding_model` does not match the locally-loaded model — a
//! Qwen-built snapshot is useless to a node running Jina, and silent
//! restore would poison the vector index.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Schema version for `_snapshot_manifest.json`. Bump when the on-disk
/// snapshot layout changes in a backwards-incompatible way.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Filename of the manifest at the tarball root.
pub const SNAPSHOT_MANIFEST_FILENAME: &str = "_snapshot_manifest.json";

/// Path within the tarball where the index subtree lives, relative to
/// the archive root. The single `{}` placeholder is replaced with the
/// corpus id at write/read time.
pub const SNAPSHOT_INDEX_PREFIX: &str = "indexes";

/// Path within the tarball where the enrichment subtree lives.
pub const SNAPSHOT_ENRICHMENT_PREFIX: &str = "enrichment";

/// The contract a snapshot publisher writes and a restorer reads.
///
/// Fields fall into three groups:
///
/// - **Identity / compatibility** (`corpus_id`, `embedding_model`,
///   `embedding_dimensions`, `filter_signature`): the restorer must
///   compare these against the local recipe and loaded model before
///   accepting the archive.
/// - **Provenance** (`canonical_fingerprint`, `source_recipe_sha256`,
///   `producer_version`, `created_at`): lets a restorer audit which
///   recipe revision and embedding stack produced the bytes.
/// - **Contents** (`chunk_count`, `atlas_included`, `residual_gap_pct`,
///   `archive_size_bytes`, `archive_sha256`, `notes`): describe what
///   the archive contains and any known gaps the user is opting into.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    /// Schema version — must equal [`SNAPSHOT_SCHEMA_VERSION`] for the
    /// current restorer to accept the archive.
    pub schema_version: u32,

    // ── Identity & compatibility ─────────────────────────────────
    pub corpus_id: String,
    pub corpus_name: String,
    /// Short tag identifying this snapshot, e.g.
    /// `"wikipedia-qwen3-embedding-0.6b-2026-05-12"`. Used in filenames
    /// and registry entries.
    pub snapshot_id: String,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    /// Mirrors `IndexMeta.scope.filter_signature` — lets the restorer
    /// confirm the recipe filter (e.g. vital_articles_l5) that produced
    /// this archive matches the local recipe's filter.
    #[serde(default)]
    pub filter_signature: Option<String>,

    // ── Provenance ───────────────────────────────────────────────
    /// Mirrors `IndexMeta.canonical_fingerprint`. Two snapshots with the
    /// same fingerprint contain the same chunk-content set (modulo
    /// chunk ordering).
    #[serde(default)]
    pub canonical_fingerprint: Option<String>,
    /// SHA-256 of the `recipe.toml` text that drove the ingest. Lets a
    /// restorer detect that the snapshot was produced from a different
    /// recipe revision than the one it has locally.
    #[serde(default)]
    pub source_recipe_sha256: Option<String>,
    /// `sovereign-cli` version that produced the archive.
    pub producer_version: String,
    /// Unix seconds at which the publisher wrote the manifest.
    pub created_at: i64,

    // ── Contents ─────────────────────────────────────────────────
    pub chunk_count: u64,
    /// `true` if the archive includes `enrichment/<corpus_id>/`
    /// alongside the index subtree.
    pub atlas_included: bool,
    /// Known residual incompleteness as a percentage (e.g. `2.81` for
    /// the 2026-05-12 Wikipedia snapshot — junk titles in the L5 list
    /// that never resolve). `None` means "unknown / not measured".
    #[serde(default)]
    pub residual_gap_pct: Option<f32>,
    /// Total size of the `.tar.zst` archive in bytes, populated by the
    /// publisher after the archive is finalised. Informational — the
    /// authoritative integrity check is `archive_sha256`.
    #[serde(default)]
    pub archive_size_bytes: Option<u64>,
    /// Hex-encoded SHA-256 of the `.tar.zst` archive. The publisher
    /// computes this *after* the manifest is sealed by writing the
    /// manifest first, then computing the hash of the resulting
    /// archive. Restorers verify this against the recipe's
    /// `PrebuiltConfig.sha256`, not against this field — the
    /// in-archive copy is for human inspection only.
    #[serde(default)]
    pub archive_sha256: Option<String>,
    /// Free-form publisher notes shown to a restorer on a successful
    /// install — e.g. "Snapshot from 2026-05-12; 2.81% L5 residual gap
    /// is junk-titles in the upstream list, not missing articles."
    #[serde(default)]
    pub notes: Option<String>,
}

impl SnapshotManifest {
    /// Build a manifest with required fields set and optional fields
    /// left empty. Callers populate `archive_size_bytes`/`archive_sha256`
    /// after the archive bytes are finalised.
    pub fn new(
        corpus_id: impl Into<String>,
        corpus_name: impl Into<String>,
        snapshot_id: impl Into<String>,
        embedding_model: impl Into<String>,
        embedding_dimensions: usize,
        chunk_count: u64,
        atlas_included: bool,
        producer_version: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            corpus_id: corpus_id.into(),
            corpus_name: corpus_name.into(),
            snapshot_id: snapshot_id.into(),
            embedding_model: embedding_model.into(),
            embedding_dimensions,
            filter_signature: None,
            canonical_fingerprint: None,
            source_recipe_sha256: None,
            producer_version: producer_version.into(),
            created_at: Utc::now().timestamp(),
            chunk_count,
            atlas_included,
            residual_gap_pct: None,
            archive_size_bytes: None,
            archive_sha256: None,
            notes: None,
        }
    }

    /// Serialise to pretty JSON suitable for writing into a tar entry.
    pub fn to_json_pretty(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| Error::Serialization(format!("snapshot manifest: {e}")))
    }

    /// Parse a manifest from JSON bytes (as read out of a tar entry).
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|e| Error::Serialization(format!("snapshot manifest: {e}")))?;
        if manifest.schema_version != SNAPSHOT_SCHEMA_VERSION {
            return Err(Error::InvalidInput(format!(
                "snapshot manifest schema_version={} but restorer expects {}",
                manifest.schema_version, SNAPSHOT_SCHEMA_VERSION
            )));
        }
        Ok(manifest)
    }

    /// Verify this manifest is compatible with the locally-loaded
    /// embedding model. Returns `Ok(())` on match, `Err` with a
    /// human-readable message otherwise.
    pub fn check_embedding_compatibility(
        &self,
        local_model: &str,
        local_dimensions: usize,
    ) -> Result<()> {
        if self.embedding_model != local_model {
            return Err(Error::InvalidInput(format!(
                "snapshot was built with embedding model '{}' but local model is '{}'; \
                 restore would poison the vector index — falling through to full ingest",
                self.embedding_model, local_model
            )));
        }
        if self.embedding_dimensions != local_dimensions {
            return Err(Error::InvalidInput(format!(
                "snapshot built with {}-dim vectors but local model emits {}-dim",
                self.embedding_dimensions, local_dimensions
            )));
        }
        Ok(())
    }
}

/// Tarball-internal path for the index subtree of a given corpus.
pub fn snapshot_index_path(corpus_id: &str) -> String {
    format!("{SNAPSHOT_INDEX_PREFIX}/{corpus_id}")
}

/// Tarball-internal path for the enrichment subtree of a given corpus.
pub fn snapshot_enrichment_path(corpus_id: &str) -> String {
    format!("{SNAPSHOT_ENRICHMENT_PREFIX}/{corpus_id}")
}

/// Conventional snapshot filename used by the publisher and consumed
/// by the recipe's `PrebuiltConfig.hf_filename`. Format:
/// `<corpus_id>-<embedding_model>-<YYYY-MM-DD>.tar.zst`. The date is
/// derived from `created_at`.
pub fn default_snapshot_filename(manifest: &SnapshotManifest) -> String {
    let date = chrono::DateTime::<Utc>::from_timestamp(manifest.created_at, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown".to_string());
    format!(
        "{}-{}-{}.tar.zst",
        manifest.corpus_id, manifest.embedding_model, date
    )
}

/// Look up an existing index's `_corpus_meta.json` on disk and produce
/// the subset of fields the snapshot manifest needs. Helper used by
/// the publisher; not part of the manifest itself.
pub fn read_local_index_meta(index_dir: &Path) -> Result<LocalIndexMetaSummary> {
    let meta_path = index_dir.join("_corpus_meta.json");
    let bytes = std::fs::read(&meta_path)?;
    let raw: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
        Error::Serialization(format!("parse {}: {e}", meta_path.display()))
    })?;
    Ok(LocalIndexMetaSummary {
        corpus_id: raw
            .get("corpus_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        corpus_name: raw
            .get("corpus_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        embedding_model: raw
            .get("embedding_model")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        embedding_dimensions: raw
            .get("embedding_dimensions")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        canonical_fingerprint: raw
            .get("canonical_fingerprint")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        filter_signature: raw
            .pointer("/scope/filter_signature")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

/// Subset of `IndexMeta` the publisher reads back to populate the
/// snapshot manifest. Kept narrow so the snapshot module doesn't pin
/// itself to every field of the private `IndexMeta` struct.
#[derive(Debug, Clone)]
pub struct LocalIndexMetaSummary {
    pub corpus_id: String,
    pub corpus_name: String,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub canonical_fingerprint: Option<String>,
    pub filter_signature: Option<String>,
}

// ─── Publisher ───────────────────────────────────────────────────────────────

/// Caller-supplied inputs to [`publish_snapshot`].
///
/// `chunk_count` is supplied by the caller rather than read from the
/// LanceDB table here, because the CLI already runs `corpus diag`-style
/// counting and has a friendlier place to surface a count mismatch.
#[derive(Debug, Clone)]
pub struct PublishOptions {
    /// Source index directory, typically
    /// `~/.sovereign/indexes/<corpus_id>/`.
    pub index_dir: PathBuf,
    /// Optional enrichment directory (atlas + caches), typically
    /// `~/.sovereign/enrichment/<corpus_id>/`. When `Some`, its contents
    /// are tarred under `enrichment/<corpus_id>/` and `atlas_included`
    /// is recorded `true`.
    pub enrichment_dir: Option<PathBuf>,
    /// Destination archive path. Parent directory must exist.
    pub output_path: PathBuf,
    /// Snapshot identifier (e.g.
    /// `"wikipedia-qwen3-embedding-0.6b-2026-05-12"`).
    pub snapshot_id: String,
    /// Total chunks in the source index — surfaces in the manifest so
    /// restorers can sanity-check after extract.
    pub chunk_count: u64,
    /// Known incompleteness in percent, e.g. `2.81` for the Wikipedia
    /// snapshot's upstream junk-title residual.
    pub residual_gap_pct: Option<f32>,
    /// Free-form publisher notes.
    pub notes: Option<String>,
    /// SHA-256 of the source `recipe.toml`, hex-encoded.
    pub source_recipe_sha256: Option<String>,
    /// `sovereign-cli` version string, e.g. `"sovereign-cli/0.1.0"`.
    pub producer_version: String,
    /// Zstd compression level. 19 is the typical "high-ratio" choice
    /// for cold-distribution artifacts; 3 is the fast default. Caller
    /// picks based on whether they care about archive size or upload
    /// time more.
    pub zstd_level: i32,
}

/// Result of a successful publish.
#[derive(Debug, Clone)]
pub struct PublishOutcome {
    /// Manifest written into the archive (with `archive_*` fields
    /// populated after sealing).
    pub manifest: SnapshotManifest,
    /// Path the archive was written to.
    pub archive_path: PathBuf,
    /// Hex-encoded SHA-256 of the finalised archive bytes. This is the
    /// value that goes into the recipe's `PrebuiltConfig.sha256`.
    pub archive_sha256: String,
    /// Size of the finalised archive in bytes.
    pub archive_size_bytes: u64,
}

/// Build a `.tar.zst` snapshot of `opts.index_dir` (and optionally
/// `opts.enrichment_dir`) at `opts.output_path` and produce a manifest
/// describing it.
///
/// The archive layout is:
/// ```text
/// _snapshot_manifest.json
/// indexes/<corpus_id>/...
/// enrichment/<corpus_id>/...    (if enrichment_dir is Some)
/// ```
///
/// The manifest inside the archive carries `archive_size_bytes=None`
/// and `archive_sha256=None` — the archive cannot contain its own
/// hash. The post-seal hash returned in [`PublishOutcome::archive_sha256`]
/// is what callers paste into the recipe's `[prebuilt].sha256`.
pub fn publish_snapshot(opts: PublishOptions) -> Result<PublishOutcome> {
    let index_meta = read_local_index_meta(&opts.index_dir)?;
    let atlas_included = opts.enrichment_dir.is_some();

    let mut manifest = SnapshotManifest::new(
        index_meta.corpus_id.clone(),
        index_meta.corpus_name.clone(),
        opts.snapshot_id.clone(),
        index_meta.embedding_model.clone(),
        index_meta.embedding_dimensions,
        opts.chunk_count,
        atlas_included,
        opts.producer_version.clone(),
    );
    manifest.filter_signature = index_meta.filter_signature.clone();
    manifest.canonical_fingerprint = index_meta.canonical_fingerprint.clone();
    manifest.source_recipe_sha256 = opts.source_recipe_sha256.clone();
    manifest.residual_gap_pct = opts.residual_gap_pct;
    manifest.notes = opts.notes.clone();

    write_snapshot_archive(&manifest, &opts)?;

    let (archive_sha256, archive_size_bytes) = hash_and_size(&opts.output_path)?;
    manifest.archive_size_bytes = Some(archive_size_bytes);
    manifest.archive_sha256 = Some(archive_sha256.clone());

    Ok(PublishOutcome {
        manifest,
        archive_path: opts.output_path,
        archive_sha256,
        archive_size_bytes,
    })
}

fn write_snapshot_archive(manifest: &SnapshotManifest, opts: &PublishOptions) -> Result<()> {
    if let Some(parent) = opts.output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = File::create(&opts.output_path)?;
    let zstd_writer = zstd::stream::Encoder::new(file, opts.zstd_level)
        .map_err(|e| Error::Io(io::Error::new(io::ErrorKind::Other, format!("zstd init: {e}"))))?
        .auto_finish();
    let mut tar = tar::Builder::new(zstd_writer);
    tar.follow_symlinks(false);

    let manifest_json = manifest.to_json_pretty()?;
    let mut manifest_header = tar::Header::new_gnu();
    manifest_header.set_size(manifest_json.len() as u64);
    manifest_header.set_mode(0o644);
    manifest_header.set_mtime(manifest.created_at.max(0) as u64);
    manifest_header.set_cksum();
    tar.append_data(
        &mut manifest_header,
        SNAPSHOT_MANIFEST_FILENAME,
        manifest_json.as_bytes(),
    )?;

    let index_prefix = snapshot_index_path(&manifest.corpus_id);
    tar.append_dir_all(&index_prefix, &opts.index_dir)?;

    if let Some(enrichment_dir) = opts.enrichment_dir.as_ref() {
        let enrichment_prefix = snapshot_enrichment_path(&manifest.corpus_id);
        tar.append_dir_all(&enrichment_prefix, enrichment_dir)?;
    }

    tar.finish()?;
    Ok(())
}

fn hash_and_size(path: &Path) -> Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20]; // 1 MiB
    let mut total: u64 = 0;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok((hex, total))
}

/// Build the `[prebuilt]` TOML snippet a recipe author pastes into a
/// `recipe.toml` after a successful publish. Indented and commented for
/// readability.
pub fn prebuilt_toml_snippet(outcome: &PublishOutcome, hf_repo: &str) -> String {
    let filename = outcome
        .archive_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("snapshot.tar.zst");
    format!(
        "[prebuilt]\n\
         # Paste this into sovereign-recipes/{corpus}/recipe.toml.\n\
         # Snapshot {snap} ({size:.2} GB) — {model} {dims}-dim.\n\
         hf_repo = \"{repo}\"\n\
         hf_filename = \"{filename}\"\n\
         sha256 = \"{sha}\"\n\
         compatible_embedding_model = \"{model}\"\n",
        corpus = outcome.manifest.corpus_id,
        snap = outcome.manifest.snapshot_id,
        size = outcome.archive_size_bytes as f64 / 1.073e9_f64,
        model = outcome.manifest.embedding_model,
        dims = outcome.manifest.embedding_dimensions,
        repo = hf_repo,
        filename = filename,
        sha = outcome.archive_sha256,
    )
}

/// Read the manifest out of an existing `.tar.zst` snapshot without
/// extracting the rest of the archive. Used by `corpus snapshot inspect`
/// and by the restorer's pre-flight check.
pub fn read_manifest_from_archive(archive_path: &Path) -> Result<SnapshotManifest> {
    let file = File::open(archive_path)?;
    let zstd_reader = zstd::stream::Decoder::new(file)
        .map_err(|e| Error::Io(io::Error::new(io::ErrorKind::Other, format!("zstd open: {e}"))))?;
    let mut archive = tar::Archive::new(zstd_reader);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path.to_str() == Some(SNAPSHOT_MANIFEST_FILENAME) {
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes)?;
            return SnapshotManifest::from_json_bytes(&bytes);
        }
    }
    Err(Error::InvalidInput(format!(
        "{SNAPSHOT_MANIFEST_FILENAME} not found in {}",
        archive_path.display()
    )))
}

// ─── Restorer ────────────────────────────────────────────────────────────────

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

    std::fs::create_dir_all(sovereign_data_dir)?;
    let archive_size_bytes = extract_snapshot_entries(archive_path, sovereign_data_dir)?;

    let index_dir = sovereign_data_dir.join(snapshot_index_path(&manifest.corpus_id));
    let enrichment_dir = if manifest.atlas_included {
        let p = sovereign_data_dir.join(snapshot_enrichment_path(&manifest.corpus_id));
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

    Ok(RestoreOutcome {
        manifest,
        index_dir,
        enrichment_dir,
        archive_size_bytes,
    })
}

/// Stream entries out of the archive into `dest`, skipping the
/// manifest (which is an archive-internal artifact, not part of the
/// on-disk index layout). Returns the total uncompressed bytes
/// written. Path-traversal entries (`..`, absolute paths) are
/// rejected — the `tar` crate's `Entry::unpack_in` enforces this.
fn extract_snapshot_entries(archive_path: &Path, dest: &Path) -> Result<u64> {
    let file = File::open(archive_path)?;
    let zstd_reader = zstd::stream::Decoder::new(file)
        .map_err(|e| Error::Io(io::Error::new(io::ErrorKind::Other, format!("zstd open: {e}"))))?;
    let mut archive = tar::Archive::new(zstd_reader);
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    archive.set_overwrite(true);

    let mut total: u64 = 0;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path.to_str() == Some(SNAPSHOT_MANIFEST_FILENAME) {
            continue;
        }
        let size = entry.size();
        // `unpack_in` is path-traversal-safe: rejects absolute paths
        // and `..` components.
        let unpacked = entry.unpack_in(dest)?;
        if unpacked {
            total += size;
        } else {
            tracing::warn!(
                entry = %path.display(),
                "snapshot extract: tar crate refused unsafe entry (rejected)"
            );
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> SnapshotManifest {
        SnapshotManifest::new(
            "wikipedia",
            "Wikipedia (English)",
            "wikipedia-qwen3-embedding-0.6b-2026-05-12",
            "qwen3-embedding-0.6b",
            1024,
            1_847_442,
            true,
            "sovereign-cli/0.1.0",
        )
    }

    #[test]
    fn manifest_roundtrip_preserves_fields() {
        let mut m = sample_manifest();
        m.filter_signature = Some("abc123".into());
        m.canonical_fingerprint = Some("def456".into());
        m.residual_gap_pct = Some(2.81);
        m.notes = Some("residual gap is upstream junk titles".into());

        let json = m.to_json_pretty().unwrap();
        let parsed = SnapshotManifest::from_json_bytes(json.as_bytes()).unwrap();

        assert_eq!(parsed.corpus_id, "wikipedia");
        assert_eq!(parsed.embedding_model, "qwen3-embedding-0.6b");
        assert_eq!(parsed.chunk_count, 1_847_442);
        assert!(parsed.atlas_included);
        assert_eq!(parsed.residual_gap_pct, Some(2.81));
        assert_eq!(parsed.filter_signature.as_deref(), Some("abc123"));
    }

    #[test]
    fn manifest_rejects_future_schema_version() {
        let mut m = sample_manifest();
        m.schema_version = SNAPSHOT_SCHEMA_VERSION + 1;
        let json = m.to_json_pretty().unwrap();
        let err = SnapshotManifest::from_json_bytes(json.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("schema_version"));
    }

    #[test]
    fn embedding_compatibility_blocks_model_mismatch() {
        let m = sample_manifest();
        let err = m
            .check_embedding_compatibility("jina-v2-en", 1024)
            .unwrap_err();
        assert!(err.to_string().contains("qwen3-embedding-0.6b"));
        assert!(err.to_string().contains("jina-v2-en"));
    }

    #[test]
    fn embedding_compatibility_blocks_dimension_mismatch() {
        let m = sample_manifest();
        let err = m
            .check_embedding_compatibility("qwen3-embedding-0.6b", 768)
            .unwrap_err();
        assert!(err.to_string().contains("1024"));
        assert!(err.to_string().contains("768"));
    }

    #[test]
    fn embedding_compatibility_accepts_exact_match() {
        let m = sample_manifest();
        m.check_embedding_compatibility("qwen3-embedding-0.6b", 1024)
            .unwrap();
    }

    #[test]
    fn default_filename_includes_corpus_model_date() {
        let m = sample_manifest();
        let name = default_snapshot_filename(&m);
        assert!(name.starts_with("wikipedia-qwen3-embedding-0.6b-"));
        assert!(name.ends_with(".tar.zst"));
    }

    #[test]
    fn snapshot_paths_use_corpus_id_subdir() {
        assert_eq!(snapshot_index_path("wikipedia"), "indexes/wikipedia");
        assert_eq!(
            snapshot_enrichment_path("wikipedia"),
            "enrichment/wikipedia"
        );
    }

    fn write_fake_index_dir(root: &Path, corpus_id: &str, model: &str, dims: usize) {
        std::fs::create_dir_all(root).unwrap();
        let meta = serde_json::json!({
            "corpus_id": corpus_id,
            "corpus_name": "Test Corpus",
            "embedding_model": model,
            "embedding_dimensions": dims,
            "canonical_fingerprint": "fp-abc",
            "scope": { "filter_signature": "sig-xyz" }
        });
        std::fs::write(
            root.join("_corpus_meta.json"),
            serde_json::to_vec_pretty(&meta).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join("chunks.lance"), b"fake lance bytes").unwrap();
        std::fs::create_dir_all(root.join("atlas")).unwrap();
        std::fs::write(root.join("atlas/_summary.json"), b"{}").unwrap();
    }

    fn write_fake_enrichment_dir(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("config.json"), b"{}").unwrap();
        std::fs::create_dir_all(root.join("cache")).unwrap();
        std::fs::write(root.join("cache/_tokens.json"), b"{}").unwrap();
    }

    #[test]
    fn publish_roundtrip_includes_manifest_and_index_subtree() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("indexes/wikitest");
        let enrichment_dir = tmp.path().join("enrichment/wikitest");
        let output_path = tmp.path().join("out.tar.zst");
        write_fake_index_dir(&index_dir, "wikitest", "qwen3-embedding-0.6b", 1024);
        write_fake_enrichment_dir(&enrichment_dir);

        let outcome = publish_snapshot(PublishOptions {
            index_dir,
            enrichment_dir: Some(enrichment_dir),
            output_path: output_path.clone(),
            snapshot_id: "wikitest-2026-05-12".into(),
            chunk_count: 42,
            residual_gap_pct: Some(2.81),
            notes: Some("test".into()),
            source_recipe_sha256: None,
            producer_version: "sovereign-cli/test".into(),
            zstd_level: 3,
        })
        .unwrap();

        assert!(outcome.archive_size_bytes > 0);
        assert_eq!(outcome.archive_sha256.len(), 64);
        assert!(outcome.manifest.atlas_included);
        assert_eq!(outcome.manifest.canonical_fingerprint.as_deref(), Some("fp-abc"));

        let read_back = read_manifest_from_archive(&output_path).unwrap();
        assert_eq!(read_back.corpus_id, "wikitest");
        assert_eq!(read_back.snapshot_id, "wikitest-2026-05-12");
        assert_eq!(read_back.embedding_model, "qwen3-embedding-0.6b");
        assert_eq!(read_back.chunk_count, 42);
        assert_eq!(read_back.residual_gap_pct, Some(2.81));
        // The in-archive manifest carries no archive_sha256 — that is
        // produced from the sealed bytes, returned in PublishOutcome.
        assert!(read_back.archive_sha256.is_none());

        // Snippet includes the post-seal sha256 and the expected fields.
        let snippet = prebuilt_toml_snippet(&outcome, "svrnmesh/wikipedia-index");
        assert!(snippet.contains(&outcome.archive_sha256));
        assert!(snippet.contains("svrnmesh/wikipedia-index"));
        assert!(snippet.contains("compatible_embedding_model = \"qwen3-embedding-0.6b\""));
    }

    #[test]
    fn publish_without_enrichment_marks_atlas_not_included() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("indexes/wikitest");
        let output_path = tmp.path().join("out.tar.zst");
        write_fake_index_dir(&index_dir, "wikitest", "qwen3-embedding-0.6b", 1024);

        let outcome = publish_snapshot(PublishOptions {
            index_dir,
            enrichment_dir: None,
            output_path: output_path.clone(),
            snapshot_id: "wikitest-2026-05-12".into(),
            chunk_count: 7,
            residual_gap_pct: None,
            notes: None,
            source_recipe_sha256: None,
            producer_version: "sovereign-cli/test".into(),
            zstd_level: 3,
        })
        .unwrap();

        assert!(!outcome.manifest.atlas_included);
        let read_back = read_manifest_from_archive(&output_path).unwrap();
        assert!(!read_back.atlas_included);
    }

    fn publish_to(tmp: &Path) -> (PathBuf, PublishOutcome) {
        let index_dir = tmp.join("indexes/wikitest");
        let enrichment_dir = tmp.join("enrichment/wikitest");
        let output_path = tmp.join("out.tar.zst");
        write_fake_index_dir(&index_dir, "wikitest", "qwen3-embedding-0.6b", 1024);
        write_fake_enrichment_dir(&enrichment_dir);
        let outcome = publish_snapshot(PublishOptions {
            index_dir,
            enrichment_dir: Some(enrichment_dir),
            output_path: output_path.clone(),
            snapshot_id: "wikitest-2026-05-12".into(),
            chunk_count: 42,
            residual_gap_pct: Some(2.81),
            notes: Some("test".into()),
            source_recipe_sha256: None,
            producer_version: "sovereign-cli/test".into(),
            zstd_level: 3,
        })
        .unwrap();
        (output_path, outcome)
    }

    #[test]
    fn restore_roundtrip_recovers_index_and_enrichment() {
        let pub_tmp = tempfile::tempdir().unwrap();
        let (archive_path, outcome) = publish_to(pub_tmp.path());

        let restore_tmp = tempfile::tempdir().unwrap();
        let result = restore_snapshot_archive(
            &archive_path,
            restore_tmp.path(),
            Some(&outcome.archive_sha256),
            "qwen3-embedding-0.6b",
            1024,
        )
        .unwrap();

        assert_eq!(result.manifest.corpus_id, "wikitest");
        assert!(result.enrichment_dir.is_some());
        assert!(result.index_dir.join("_corpus_meta.json").exists());
        assert!(result.index_dir.join("atlas/_summary.json").exists());
        assert!(result.enrichment_dir.as_ref().unwrap().join("config.json").exists());
        // The archive-internal manifest must NOT land on disk.
        assert!(!restore_tmp.path().join(SNAPSHOT_MANIFEST_FILENAME).exists());
    }

    #[test]
    fn restore_refuses_sha256_mismatch() {
        let pub_tmp = tempfile::tempdir().unwrap();
        let (archive_path, _) = publish_to(pub_tmp.path());
        let restore_tmp = tempfile::tempdir().unwrap();
        let err = restore_snapshot_archive(
            &archive_path,
            restore_tmp.path(),
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
            "qwen3-embedding-0.6b",
            1024,
        )
        .unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"));
        // Nothing should have been extracted when the hash check fails.
        assert!(!restore_tmp.path().join("indexes/wikitest").exists());
    }

    #[test]
    fn restore_refuses_embedding_model_mismatch() {
        let pub_tmp = tempfile::tempdir().unwrap();
        let (archive_path, outcome) = publish_to(pub_tmp.path());
        let restore_tmp = tempfile::tempdir().unwrap();
        let err = restore_snapshot_archive(
            &archive_path,
            restore_tmp.path(),
            Some(&outcome.archive_sha256),
            "jina-v2-en",
            1024,
        )
        .unwrap_err();
        assert!(err.to_string().contains("qwen3-embedding-0.6b"));
        assert!(err.to_string().contains("jina-v2-en"));
        // Pre-extract gate — directory should not appear.
        assert!(!restore_tmp.path().join("indexes/wikitest").exists());
    }
}
