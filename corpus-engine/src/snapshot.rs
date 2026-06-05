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

// Restore lives in `snapshot_restore.rs`; re-export so the historical
// `corpus_engine::snapshot::restore_snapshot_archive` path keeps working
// for downstream callers that imported it before the split.
pub use crate::snapshot_restore::{restore_snapshot_archive, RestoreOutcome};

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

/// Verdict of comparing a snapshot manifest's embedding identity against
/// the locally-loaded model. Dimensions are the hard floor (mismatched
/// dims can't be compared at all); a name mismatch with matching dims is
/// only *plausibly* incompatible — the same model under a different
/// label/quant looks identical here — so the restorer VERIFIES it by
/// re-embedding sample chunks before trusting the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingCompat {
    /// Model name AND dimensions match the local model.
    Exact,
    /// Dimensions match, model name differs — verify the space by probe.
    NameMismatch,
    /// Dimensions differ — vectors are not comparable; never usable.
    DimsMismatch,
}

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

    /// Additional corpora bundled into this archive alongside the
    /// primary `corpus_id`. Used by per-article-corpus pipelines (e.g.
    /// SEP's 1770 `sep-<slug>` sibling corpora) so a single
    /// `corpus install sep` restores both the canonical parent and
    /// every per-article atlas. Each entry lands at
    /// `indexes/<bundled_id>/` in the archive and extracts to
    /// `~/.sovereign/indexes/<bundled_id>/` on restore.
    ///
    /// **Renaming caveat:** the `--as <new-id>` restore flag only
    /// rewrites the primary corpus_id; bundled siblings retain their
    /// archive names (which is the correct behavior — sibling ids are
    /// load-bearing for query-time multi-corpus retrieval).
    #[serde(default)]
    pub bundled_corpora: Vec<String>,
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
            bundled_corpora: Vec::new(),
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

    /// Classify this manifest's embedding identity against the
    /// locally-loaded model. Dimensions are the hard floor; a name-only
    /// mismatch returns [`EmbeddingCompat::NameMismatch`] for the caller
    /// to VERIFY by probe rather than trust or reject on the label alone
    /// — model names drift across dir/stem/repo/quant for the same model.
    pub fn check_embedding_compatibility(
        &self,
        local_model: &str,
        local_dimensions: usize,
    ) -> EmbeddingCompat {
        if self.embedding_dimensions != local_dimensions {
            EmbeddingCompat::DimsMismatch
        } else if self.embedding_model == local_model {
            EmbeddingCompat::Exact
        } else {
            EmbeddingCompat::NameMismatch
        }
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
    let raw: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Serialization(format!("parse {}: {e}", meta_path.display())))?;
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
    /// Additional sibling corpora to bundle into the archive alongside
    /// the primary index. Each entry is `(sibling_corpus_id,
    /// sibling_index_dir)` — typically `("sep-aristotle",
    /// PathBuf("~/.sovereign/indexes/sep-aristotle/"))`. Each sibling
    /// tars under `indexes/<sibling_id>/` and its id is recorded in
    /// `manifest.bundled_corpora`. Empty for the common single-corpus
    /// case.
    pub sibling_index_dirs: Vec<(String, PathBuf)>,
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
///
/// ## Consistency under concurrent LanceDB writes
///
/// Naively walking `index_dir/chunks.lance/` with tar's `readdir`-based
/// `append_dir_all` is racy: if LanceDB writes a new fragment + new
/// manifest pointer during the (slow) zstd pass, the walker can see
/// the new manifest but miss the new fragment file — empirically
/// dropped 9/3021 fragments on the first wikipedia publish.
///
/// To make the capture transactional, this function opens
/// `chunks.lance/` as a `lance::Dataset` before tar starts, snapshots
/// the fragment list + index UUIDs at that version, and tars exactly
/// those files (plus the manifest file for that version). Subsequent
/// writes by LanceDB land in *later* versions whose pointers we don't
/// include, so the restored archive is internally consistent.
pub async fn publish_snapshot(opts: PublishOptions) -> Result<PublishOutcome> {
    let index_meta = read_local_index_meta(&opts.index_dir)?;
    // `atlas_included` reflects whether atlas data ends up *anywhere*
    // in the archive — either as a separate `enrichment/<id>/` subtree
    // (the legacy location) or as `<index_dir>/atlas/` (the in-place
    // layout wikipedia uses). The flag is the contract a restorer
    // reads to know whether to expect atlas-driven retrieval; the
    // physical location of the bytes is a publisher detail.
    let atlas_in_index = opts.index_dir.join("atlas").is_dir();
    let atlas_in_enrichment = opts.enrichment_dir.is_some();
    let atlas_included = atlas_in_index || atlas_in_enrichment;

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
    manifest.bundled_corpora = opts
        .sibling_index_dirs
        .iter()
        .map(|(id, _)| id.clone())
        .collect();

    // Anchor a transactional view of any LanceDB datasets under
    // index_dir BEFORE the (slow) tar pass. The view is just a list of
    // file paths + manifest version — cheap to hold, cheap to clone
    // into the blocking task.
    let chunks_lance_path = opts.index_dir.join("chunks.lance");
    let lance_view = if chunks_lance_path.is_dir() {
        Some(LanceSnapshotView::open(&chunks_lance_path).await?)
    } else {
        None
    };

    // Tar + zstd are blocking I/O over multi-GB data; offload from the
    // tokio reactor so the runtime stays responsive.
    let opts_for_tar = opts.clone();
    let manifest_for_tar = manifest.clone();
    let lance_for_tar = lance_view.clone();
    tokio::task::spawn_blocking(move || {
        write_snapshot_archive(&manifest_for_tar, &opts_for_tar, lance_for_tar.as_ref())
    })
    .await
    .map_err(|e| Error::Database(format!("snapshot tar task panicked: {e}")))??;

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

/// Transactional anchor for one LanceDB dataset's on-disk state at a
/// specific version. Captured before tar starts; safe to clone into a
/// `spawn_blocking` task. The strings here are paths relative to the
/// lance dataset root (i.e. relative to `chunks.lance/`).
#[derive(Clone, Debug)]
struct LanceSnapshotView {
    /// Lance manifest version number, for logging.
    version: u64,
    /// Filename of the manifest file on disk (e.g.
    /// `00000000000000000001.manifest` for V2 scheme,
    /// `1.manifest` for V1).
    manifest_filename: String,
    /// Data file paths referenced by this version's manifest, relative
    /// to the lance dataset root.
    data_files: Vec<String>,
    /// Index UUIDs referenced by this version's manifest. Each maps
    /// to `_indices/<uuid>/` on disk.
    index_uuids: Vec<String>,
}

impl LanceSnapshotView {
    async fn open(lance_dir: &Path) -> Result<Self> {
        // `load_indices` is a trait method; bring DatasetIndexExt into
        // scope so the call below resolves on a `lance::Dataset`.
        use lance_index::DatasetIndexExt;

        let path_str = lance_dir.to_str().ok_or_else(|| {
            Error::InvalidInput(format!("non-utf8 lance path: {}", lance_dir.display()))
        })?;
        let ds = lance::Dataset::open(path_str)
            .await
            .map_err(|e| Error::Database(format!("lance::Dataset::open({path_str}): {e}")))?;
        let version = ds.manifest().version;
        let fragments = ds.fragments();
        // Lance stores `df.path` as just the filename (e.g.
        // `000...abc.lance`); the dataset reader resolves it via
        // `dataset.data_dir().child(path)`. We mirror that by prefixing
        // `data/` so the strings here are relative to the lance dataset
        // root and can be used directly with `Path::join`.
        let mut data_files: Vec<String> = fragments
            .iter()
            .flat_map(|f| f.files.iter().map(|df| format!("data/{}", df.path)))
            .collect();
        // Deterministic order, helps when comparing two tars built
        // from the same version.
        data_files.sort();
        let indices = ds
            .load_indices()
            .await
            .map_err(|e| Error::Database(format!("lance::Dataset::load_indices: {e}")))?;
        let mut index_uuids: Vec<String> = indices.iter().map(|i| i.uuid.to_string()).collect();
        index_uuids.sort();

        let manifest_filename = detect_lance_manifest_filename(lance_dir, version)?;

        tracing::info!(
            lance_dir = %lance_dir.display(),
            version,
            fragments = data_files.len(),
            indices = index_uuids.len(),
            manifest = %manifest_filename,
            "snapshot: anchored lance dataset for transactional capture"
        );
        Ok(LanceSnapshotView {
            version,
            manifest_filename,
            data_files,
            index_uuids,
        })
    }
}

/// Find which manifest filename Lance wrote for a given version.
/// Tries the V2 (zero-padded, `u64::MAX - version`) scheme first since
/// it's the modern default, falls back to V1 (`{version}.manifest`)
/// for older datasets.
fn detect_lance_manifest_filename(lance_dir: &Path, version: u64) -> Result<String> {
    let versions_dir = lance_dir.join("_versions");
    let inverted = u64::MAX - version;
    let v2 = format!("{inverted:020}.manifest");
    if versions_dir.join(&v2).is_file() {
        return Ok(v2);
    }
    let v1 = format!("{version}.manifest");
    if versions_dir.join(&v1).is_file() {
        return Ok(v1);
    }
    Err(Error::Database(format!(
        "no manifest file on disk for lance dataset at {} version {version}; \
         looked for `{v2}` (V2 scheme) and `{v1}` (V1 scheme) under _versions/",
        lance_dir.display()
    )))
}

fn write_snapshot_archive(
    manifest: &SnapshotManifest,
    opts: &PublishOptions,
    lance_view: Option<&LanceSnapshotView>,
) -> Result<()> {
    if let Some(parent) = opts.output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write to a sidecar `.part` file; atomic-rename to the final
    // path only after the tar finishes cleanly. Resumable-publish
    // relies on this: presence of `<output_path>` without a sibling
    // `.part` is the signal "archive is complete; skip rebuild."
    // An interrupted build (SIGTERM during tar/zstd) leaves a
    // `<output_path>.part` which a subsequent run discards before
    // re-building.
    let part_path = opts.output_path.with_extension(
        opts.output_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!("{e}.part"))
            .unwrap_or_else(|| "part".into()),
    );
    // If a stale `.part` is lying around from a prior crash, remove it
    // before opening — File::create would truncate anyway, but rm is
    // explicit about the intent.
    if part_path.exists() {
        if let Err(e) = std::fs::remove_file(&part_path) {
            tracing::warn!(
                path = %part_path.display(),
                error = %e,
                "snapshot: stale .part removal failed; will overwrite"
            );
        }
    }
    let file = File::create(&part_path)?;
    let zstd_writer = zstd::stream::Encoder::new(file, opts.zstd_level)
        .map_err(|e| Error::Io(io::Error::other(format!("zstd init: {e}"))))?
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

    if let Some(view) = lance_view {
        // Walk index_dir top-level entries, deferring chunks.lance to
        // the Lance-aware path below for transactional consistency.
        append_dir_skipping(&mut tar, &index_prefix, &opts.index_dir, &["chunks.lance"])?;
        let lance_archive_prefix = format!("{index_prefix}/chunks.lance");
        let chunks_lance_dir = opts.index_dir.join("chunks.lance");
        append_lance_snapshot(&mut tar, &lance_archive_prefix, &chunks_lance_dir, view)?;
    } else {
        // No lance dataset present (catalog corpora, etc.) — fall back
        // to the naive walk. Safe because there's no LanceDB writer
        // racing with us.
        tar.append_dir_all(&index_prefix, &opts.index_dir)?;
    }

    if let Some(enrichment_dir) = opts.enrichment_dir.as_ref() {
        let enrichment_prefix = snapshot_enrichment_path(&manifest.corpus_id);
        append_dir_recursive(&mut tar, &enrichment_prefix, enrichment_dir)?;
    }

    // Sibling-corpus bundling (e.g. SEP's 1770 per-article atlases).
    // Each sibling tars under `indexes/<sibling_id>/`. If the sibling
    // happens to carry a `chunks.lance/` we'd need Lance-aware capture
    // — but the common case (per-article-atlas pipelines) has no Lance
    // dataset, so naive walk is correct and cheap. Surface a hard error
    // if we see one, rather than silently risk fragment-drop.
    for (sibling_id, sibling_dir) in &opts.sibling_index_dirs {
        let sibling_prefix = snapshot_index_path(sibling_id);
        let sibling_lance = sibling_dir.join("chunks.lance");
        if sibling_lance.is_dir() {
            return Err(Error::InvalidInput(format!(
                "sibling corpus '{sibling_id}' has a chunks.lance dataset; \
                 sibling bundling currently only supports atlas-only siblings \
                 (no Lance-aware capture for siblings yet)"
            )));
        }
        append_dir_recursive(&mut tar, &sibling_prefix, sibling_dir)?;
    }

    tar.finish()?;
    // Tar succeeded — promote .part to the final path atomically.
    std::fs::rename(&part_path, &opts.output_path)?;
    Ok(())
}

/// Append every top-level entry of `src` to `tar` under `prefix`,
/// except those whose top-level name is in `skip`. Directories below
/// the skipped names are not walked. Used to defer `chunks.lance/`
/// (Lance-aware capture) while still grabbing siblings like
/// `_corpus_meta.json`, `atlas/`, `wikipedia_graph.db`.
fn append_dir_skipping<W: io::Write>(
    tar: &mut tar::Builder<W>,
    prefix: &str,
    src: &Path,
    skip: &[&str],
) -> Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name_owned = entry.file_name();
        let name = name_owned.to_string_lossy();
        if skip.iter().any(|s| *s == name.as_ref()) {
            continue;
        }
        let path = entry.path();
        let archive_path = format!("{prefix}/{name}");
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            append_dir_recursive(tar, &archive_path, &path)?;
        } else if file_type.is_file() {
            if is_ephemeral_artifact(&name) {
                continue;
            }
            tar.append_path_with_name(&path, &archive_path)?;
        } else {
            tracing::debug!(path = %path.display(), "snapshot: skipping non-regular entry");
        }
    }
    Ok(())
}

/// Local-only artifacts that must NEVER ship in a distributed snapshot:
/// recoalesce backups (`*.orig`, written by `enrich investigation recoalesce`)
/// and the Phase-1 resume checkpoint (`_phase1_checkpoint.jsonl`). They're
/// node-local working state, not part of the canonical corpus a downloader
/// restores.
fn is_ephemeral_artifact(name: &str) -> bool {
    name.ends_with(".orig") || name == "_phase1_checkpoint.jsonl"
}

/// Recursively append a directory subtree, skipping [`is_ephemeral_artifact`]
/// files at every level. Replaces `tar.append_dir_all`, which would ship the
/// local backups/checkpoints nested under `investigation/` etc.
fn append_dir_recursive<W: io::Write>(
    tar: &mut tar::Builder<W>,
    archive_path: &str,
    src: &Path,
) -> Result<()> {
    tar.append_dir(archive_path, src)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name_owned = entry.file_name();
        let name = name_owned.to_string_lossy();
        let path = entry.path();
        let child = format!("{archive_path}/{name}");
        let ft = entry.file_type()?;
        if ft.is_dir() {
            append_dir_recursive(tar, &child, &path)?;
        } else if ft.is_file() {
            if is_ephemeral_artifact(&name) {
                continue;
            }
            tar.append_path_with_name(&path, &child)?;
        }
    }
    Ok(())
}

/// Tar exactly the Lance files referenced by `view`'s manifest, plus
/// the manifest file itself, plus any index subtrees referenced by
/// UUID. This is the consistency-correct replacement for
/// `tar.append_dir_all(prefix, chunks_lance_dir)`.
///
/// The set is closed under the manifest: every file we write is
/// referenced by the manifest, and every file the manifest references
/// is present. Lance opening the restored dataset reads the same
/// manifest and finds all its dependencies.
fn append_lance_snapshot<W: io::Write>(
    tar: &mut tar::Builder<W>,
    archive_prefix: &str,
    lance_dir: &Path,
    view: &LanceSnapshotView,
) -> Result<()> {
    // 1. Manifest file. Lance reads `_versions/` alphabetically; since
    //    this is the only file there in the restored archive, it wins
    //    the "latest version" race trivially.
    let manifest_rel = format!("_versions/{}", view.manifest_filename);
    let manifest_abs = lance_dir.join("_versions").join(&view.manifest_filename);
    tar.append_path_with_name(&manifest_abs, format!("{archive_prefix}/{manifest_rel}"))?;

    // 2. Fragment data files. These are content-addressed and immutable
    //    on disk per Lance's storage model, so we can rely on their
    //    paths matching the manifest's expectations.
    for rel in &view.data_files {
        let abs = lance_dir.join(rel);
        if !abs.is_file() {
            return Err(Error::Database(format!(
                "lance manifest version {} references missing data file {}",
                view.version,
                abs.display()
            )));
        }
        tar.append_path_with_name(&abs, format!("{archive_prefix}/{rel}"))?;
    }

    // 3. Index subtrees. Lance writes each index under
    //    `_indices/<uuid>/<files>`; the manifest's index_section names
    //    the live indexes by UUID. New index builds create new UUIDs,
    //    so the directory we read here corresponds to this version.
    for uuid in &view.index_uuids {
        let idx_dir = lance_dir.join("_indices").join(uuid);
        if idx_dir.is_dir() {
            let idx_archive = format!("{archive_prefix}/_indices/{uuid}");
            tar.append_dir_all(&idx_archive, &idx_dir)?;
        } else {
            tracing::warn!(
                uuid,
                "snapshot: lance manifest references index uuid with no on-disk directory — skipping"
            );
        }
    }

    Ok(())
}

pub(crate) fn hash_and_size(path: &Path) -> Result<(String, u64)> {
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
        .map_err(|e| Error::Io(io::Error::other(format!("zstd open: {e}"))))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot_restore::restore_snapshot_archive;

    #[test]
    fn is_ephemeral_artifact_matches_backups_and_checkpoints() {
        assert!(is_ephemeral_artifact("entities.json.orig"));
        assert!(is_ephemeral_artifact("relationships.json.orig"));
        assert!(is_ephemeral_artifact("_phase1_checkpoint.jsonl"));
        assert!(!is_ephemeral_artifact("entities.json"));
        assert!(!is_ephemeral_artifact("pattern_findings.json"));
    }

    #[test]
    fn append_dir_recursive_excludes_ephemeral_artifacts() {
        // A snapshot must not ship recoalesce backups or resume checkpoints,
        // even when nested under investigation/.
        let dir = tempfile::tempdir().unwrap();
        let inv = dir.path().join("investigation");
        std::fs::create_dir_all(&inv).unwrap();
        std::fs::write(inv.join("entities.json"), b"{}").unwrap();
        std::fs::write(inv.join("entities.json.orig"), b"{}").unwrap();
        std::fs::write(inv.join("_phase1_checkpoint.jsonl"), b"{}").unwrap();
        std::fs::write(dir.path().join("_corpus_meta.json"), b"{}").unwrap();

        let mut buf = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut buf);
            append_dir_recursive(&mut tar, "indexes/x", dir.path()).unwrap();
            tar.finish().unwrap();
        }

        let mut archive = tar::Archive::new(&buf[..]);
        let names: Vec<String> = archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n.ends_with("investigation/entities.json")));
        assert!(names.iter().any(|n| n.ends_with("_corpus_meta.json")));
        assert!(
            !names.iter().any(|n| n.ends_with(".orig")),
            "recoalesce .orig backups must not ship: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.ends_with("_phase1_checkpoint.jsonl")),
            "resume checkpoint must not ship: {names:?}"
        );
    }

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
    fn embedding_compatibility_flags_model_name_mismatch() {
        let m = sample_manifest();
        // Same dims (1024), different name → NameMismatch: verify by probe,
        // do NOT reject on the label alone.
        assert_eq!(
            m.check_embedding_compatibility("jina-v2-en", 1024),
            EmbeddingCompat::NameMismatch
        );
    }

    #[test]
    fn embedding_compatibility_blocks_dimension_mismatch() {
        let m = sample_manifest();
        // Different dims → DimsMismatch: the hard floor, never usable.
        assert_eq!(
            m.check_embedding_compatibility("qwen3-embedding-0.6b", 768),
            EmbeddingCompat::DimsMismatch
        );
    }

    #[test]
    fn embedding_compatibility_accepts_exact_match() {
        let m = sample_manifest();
        assert_eq!(
            m.check_embedding_compatibility("qwen3-embedding-0.6b", 1024),
            EmbeddingCompat::Exact
        );
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

    #[tokio::test]
    async fn publish_roundtrip_includes_manifest_and_index_subtree() {
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
            sibling_index_dirs: Vec::new(),
        })
        .await
        .unwrap();

        assert!(outcome.archive_size_bytes > 0);
        assert_eq!(outcome.archive_sha256.len(), 64);
        assert!(outcome.manifest.atlas_included);
        assert_eq!(
            outcome.manifest.canonical_fingerprint.as_deref(),
            Some("fp-abc")
        );

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

    #[tokio::test]
    async fn publish_without_enrichment_but_with_atlas_in_index_marks_atlas_included() {
        // `write_fake_index_dir` always writes a tiny `atlas/` subdir
        // inside the index dir (the wikipedia-style in-place layout).
        // Even with `enrichment_dir: None`, atlas_included must be
        // `true` because the data IS in the archive — it just comes
        // from indexes/<id>/atlas/ rather than enrichment/<id>/.
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
            sibling_index_dirs: Vec::new(),
        })
        .await
        .unwrap();

        assert!(outcome.manifest.atlas_included);
        let read_back = read_manifest_from_archive(&output_path).unwrap();
        assert!(read_back.atlas_included);
    }

    #[tokio::test]
    async fn publish_with_neither_atlas_location_marks_not_included() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("indexes/wikitest");
        let output_path = tmp.path().join("out.tar.zst");
        // Write the index dir WITHOUT an atlas subdir.
        std::fs::create_dir_all(&index_dir).unwrap();
        let meta = serde_json::json!({
            "corpus_id": "wikitest",
            "corpus_name": "Test",
            "embedding_model": "qwen3-embedding-0.6b",
            "embedding_dimensions": 1024,
            "scope": { "filter_signature": "sig" },
        });
        std::fs::write(
            index_dir.join("_corpus_meta.json"),
            serde_json::to_vec_pretty(&meta).unwrap(),
        )
        .unwrap();
        std::fs::write(index_dir.join("chunks.lance"), b"fake lance bytes").unwrap();

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
            sibling_index_dirs: Vec::new(),
        })
        .await
        .unwrap();

        assert!(!outcome.manifest.atlas_included);
        let read_back = read_manifest_from_archive(&output_path).unwrap();
        assert!(!read_back.atlas_included);
    }

    async fn publish_to(tmp: &Path) -> (PathBuf, PublishOutcome) {
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
            sibling_index_dirs: Vec::new(),
        })
        .await
        .unwrap();
        (output_path, outcome)
    }

    /// Build a sibling index dir that mimics SEP's per-article layout:
    /// pure atlas (atoms.json + edges.json), no chunks.lance. Used by
    /// the sibling-bundling tests below.
    fn write_fake_sibling_index_dir(dir: &Path, corpus_id: &str) {
        std::fs::create_dir_all(dir.join("atlas")).unwrap();
        let meta = serde_json::json!({
            "corpus_id": corpus_id,
            "corpus_name": "Sibling Test",
            "embedding_model": "qwen3-embedding-0.6b",
            "embedding_dimensions": 1024,
        });
        std::fs::write(
            dir.join("_corpus_meta.json"),
            serde_json::to_vec_pretty(&meta).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("atlas/atoms.json"),
            br#"{"atoms": [], "schema_version": "2.0"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("atlas/edges.json"),
            br#"{"edges": [], "schema_version": "2.0"}"#,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn publish_with_siblings_bundles_them_and_records_in_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("indexes/sep");
        let sibling_a = tmp.path().join("indexes/sep-aristotle");
        let sibling_b = tmp.path().join("indexes/sep-descartes");
        let output_path = tmp.path().join("out.tar.zst");
        write_fake_index_dir(&index_dir, "sep", "qwen3-embedding-0.6b", 1024);
        write_fake_sibling_index_dir(&sibling_a, "sep-aristotle");
        write_fake_sibling_index_dir(&sibling_b, "sep-descartes");

        let outcome = publish_snapshot(PublishOptions {
            index_dir,
            enrichment_dir: None,
            output_path: output_path.clone(),
            snapshot_id: "sep-2026-05-22".into(),
            chunk_count: 100,
            residual_gap_pct: Some(0.0),
            notes: None,
            source_recipe_sha256: None,
            producer_version: "sovereign-cli/test".into(),
            zstd_level: 3,
            sibling_index_dirs: vec![
                ("sep-aristotle".to_string(), sibling_a),
                ("sep-descartes".to_string(), sibling_b),
            ],
        })
        .await
        .unwrap();

        // Manifest records the siblings, sorted by caller.
        assert_eq!(
            outcome.manifest.bundled_corpora,
            vec!["sep-aristotle".to_string(), "sep-descartes".to_string()]
        );

        // Round-trip restore lands every sibling under the right path.
        let restore_tmp = tempfile::tempdir().unwrap();
        let result = restore_snapshot_archive(
            &output_path,
            restore_tmp.path(),
            "sep",
            Some(&outcome.archive_sha256),
            "qwen3-embedding-0.6b",
            1024,
        )
        .unwrap();
        assert_eq!(result.manifest.bundled_corpora.len(), 2);
        assert!(restore_tmp
            .path()
            .join("indexes/sep-aristotle/atlas/atoms.json")
            .exists());
        assert!(restore_tmp
            .path()
            .join("indexes/sep-descartes/atlas/atoms.json")
            .exists());
        // Primary still landed correctly.
        assert!(restore_tmp
            .path()
            .join("indexes/sep/_corpus_meta.json")
            .exists());
    }

    #[tokio::test]
    async fn publish_with_sibling_carrying_lance_dataset_errors() {
        // SEP-style siblings are atlas-only. A sibling with a
        // chunks.lance dir would need Lance-aware capture which isn't
        // wired for siblings yet — fail loudly rather than risk
        // silent fragment-drop.
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("indexes/parent");
        let sibling = tmp.path().join("indexes/parent-child");
        let output_path = tmp.path().join("out.tar.zst");
        write_fake_index_dir(&index_dir, "parent", "qwen3-embedding-0.6b", 1024);
        // Sibling with a (fake) chunks.lance dir present.
        std::fs::create_dir_all(sibling.join("chunks.lance")).unwrap();
        std::fs::write(sibling.join("_corpus_meta.json"), b"{}").unwrap();

        let err = publish_snapshot(PublishOptions {
            index_dir,
            enrichment_dir: None,
            output_path,
            snapshot_id: "parent-2026-05-22".into(),
            chunk_count: 1,
            residual_gap_pct: None,
            notes: None,
            source_recipe_sha256: None,
            producer_version: "sovereign-cli/test".into(),
            zstd_level: 3,
            sibling_index_dirs: vec![("parent-child".into(), sibling)],
        })
        .await
        .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("parent-child") && msg.contains("chunks.lance"),
            "error must name the sibling and the offending dir: {msg}"
        );
    }

    #[tokio::test]
    async fn restore_roundtrip_recovers_index_and_enrichment() {
        let pub_tmp = tempfile::tempdir().unwrap();
        let (archive_path, outcome) = publish_to(pub_tmp.path()).await;

        let restore_tmp = tempfile::tempdir().unwrap();
        let result = restore_snapshot_archive(
            &archive_path,
            restore_tmp.path(),
            "wikitest",
            Some(&outcome.archive_sha256),
            "qwen3-embedding-0.6b",
            1024,
        )
        .unwrap();

        assert_eq!(result.manifest.corpus_id, "wikitest");
        assert!(result.enrichment_dir.is_some());
        assert!(result.index_dir.join("_corpus_meta.json").exists());
        assert!(result.index_dir.join("atlas/_summary.json").exists());
        assert!(result
            .enrichment_dir
            .as_ref()
            .unwrap()
            .join("config.json")
            .exists());
        // The archive-internal manifest must NOT land on disk.
        assert!(!restore_tmp.path().join(SNAPSHOT_MANIFEST_FILENAME).exists());
    }

    #[tokio::test]
    async fn restore_with_rename_lands_under_new_corpus_id_and_patches_meta() {
        let pub_tmp = tempfile::tempdir().unwrap();
        let (archive_path, outcome) = publish_to(pub_tmp.path()).await;

        let restore_tmp = tempfile::tempdir().unwrap();
        let result = restore_snapshot_archive(
            &archive_path,
            restore_tmp.path(),
            "wikitest-sibling",
            Some(&outcome.archive_sha256),
            "qwen3-embedding-0.6b",
            1024,
        )
        .unwrap();

        // Rename lands under the target id, not the archive's.
        assert_eq!(
            result.index_dir,
            restore_tmp.path().join("indexes/wikitest-sibling")
        );
        assert!(restore_tmp
            .path()
            .join("indexes/wikitest-sibling/_corpus_meta.json")
            .exists());
        assert!(restore_tmp
            .path()
            .join("indexes/wikitest-sibling/atlas/_summary.json")
            .exists());
        // Original archive_corpus_id path must NOT be created.
        assert!(!restore_tmp.path().join("indexes/wikitest").exists());
        // Enrichment subtree renamed too.
        assert!(restore_tmp
            .path()
            .join("enrichment/wikitest-sibling/config.json")
            .exists());
        assert!(!restore_tmp.path().join("enrichment/wikitest").exists());
        // Patched _corpus_meta.json points at the new id.
        let meta = std::fs::read_to_string(result.index_dir.join("_corpus_meta.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(
            v.get("corpus_id").and_then(|x| x.as_str()),
            Some("wikitest-sibling")
        );
        // The returned manifest still reflects the archive's original id —
        // it's a description of the archive, not of the on-disk state.
        assert_eq!(result.manifest.corpus_id, "wikitest");
    }

    #[tokio::test]
    async fn restore_refuses_sha256_mismatch() {
        let pub_tmp = tempfile::tempdir().unwrap();
        let (archive_path, _) = publish_to(pub_tmp.path()).await;
        let restore_tmp = tempfile::tempdir().unwrap();
        let err = restore_snapshot_archive(
            &archive_path,
            restore_tmp.path(),
            "wikitest",
            Some("0000000000000000000000000000000000000000000000000000000000000000"),
            "qwen3-embedding-0.6b",
            1024,
        )
        .unwrap_err();
        assert!(err.to_string().contains("sha256 mismatch"));
        // Nothing should have been extracted when the hash check fails.
        assert!(!restore_tmp.path().join("indexes/wikitest").exists());
    }

    #[tokio::test]
    async fn restore_refuses_embedding_model_mismatch() {
        let pub_tmp = tempfile::tempdir().unwrap();
        let (archive_path, outcome) = publish_to(pub_tmp.path()).await;
        let restore_tmp = tempfile::tempdir().unwrap();
        let err = restore_snapshot_archive(
            &archive_path,
            restore_tmp.path(),
            "wikitest",
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
