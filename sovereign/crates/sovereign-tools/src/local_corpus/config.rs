//! Configuration types for a locally-sourced corpus.
//!
//! One `LocalCorpusConfig` describes everything about one corpus
//! (folder or vault) and is persisted verbatim in the StateStore so the
//! corpus can be reconstructed on relaunch.
//!
//! Two factory constructors build the two shipping flavours:
//!   - `LocalCorpusConfig::document_folder(path, display_name)`
//!   - `LocalCorpusConfig::obsidian_vault(path)`
//!
//! `recipe_toml(&config, snapshot_path)` renders a `corpus_engine::Recipe`
//! as a TOML string suitable for `CorpusEngine::ingest` via
//! `CorpusSpec::RecipePath`. We use RecipePath (not a direct in-memory
//! Recipe) because `CorpusEngine::resolve_recipe` only reads from disk
//! or from the built-in registry — this keeps us off its hot path and
//! lets the engine own the Recipe parsing invariants.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ─── Distribution scope ───────────────────────────────────────────────

/// Declared distribution boundary for a corpus. v1 always uses `Local`
/// for both folder and vault corpora; `Mesh` and `Public` are reserved
/// for the Commonwealth layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CorpusScope {
    /// Never leaves this machine.
    Local,
    /// Shareable within a Commonwealth mesh (reserved, v2+).
    Mesh,
    /// Openly distributed (public recipes only, v2+).
    Public,
}

impl CorpusScope {
    /// Render to the string that `corpus_engine::CorpusMeta::scope`
    /// accepts. Keep in sync with `corpus-engine/src/recipe.rs`.
    pub fn as_recipe_str(&self) -> &'static str {
        match self {
            CorpusScope::Local => "local",
            CorpusScope::Mesh => "mesh",
            CorpusScope::Public => "public",
        }
    }
}

// ─── Source-type discriminator ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LocalCorpusSourceType {
    ObsidianVault {
        /// Parse YAML frontmatter (tags, aliases, created, modified,
        /// type) as chunk metadata. Always `true` in v1.
        parse_frontmatter: bool,
        /// Parse `[[wiki-links]]` into a sidecar link graph. Always
        /// `true` in v1 when markdown is the source (M3).
        follow_wiki_links: bool,
    },
    DocumentFolder,
    /// A directory the user wants kept in sync — adds, edits, and
    /// deletes are reflected in the index by a polling reconciliation
    /// worker. Read-only on source: nothing under the folder is ever
    /// written, moved, or renamed by Sovereign. See
    /// `local_corpus/watched/` for the worker implementation and the
    /// plan at `~/.claude/plans/let-s-build-out-this-noble-ladybug.md`.
    WatchedFolder(WatchedFolderConfig),
}

// ─── Watched-folder configuration ─────────────────────────────────────

/// Per-corpus tunables for a `WatchedFolder` source. Stored verbatim on
/// the `LocalCorpusSourceType::WatchedFolder` variant so the worker can
/// reconstruct its behaviour after a daemon restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchedFolderConfig {
    /// When `false`, symlinked files and directories are skipped
    /// (default). When `true`, the walker follows symlinks and tracks
    /// visited inodes via `(dev, ino)` to break loops.
    ///
    /// `#[serde(default)]` lets thin clients (CLI, HTTP callers) omit
    /// the field and fall through to the `false` default — without
    /// this, every partial-config POST to
    /// `/internal/corpus/watch/register` would have to enumerate
    /// every field even when they want the defaults.
    #[serde(default)]
    pub follow_symlinks: bool,
    /// Threshold guard against catastrophic deletion (drive unmount,
    /// `rm -rf`, etc.). Evaluated before any deletion is applied.
    ///
    /// `#[serde(default)]` — same rationale as `follow_symlinks`.
    /// Falls back to `DeletionGuardConfig::default()` (absolute=100,
    /// fractional=0.25, enabled).
    #[serde(default)]
    pub deletion_guard: DeletionGuardConfig,
    /// Polling cadence between sweeps. Floored at 60 s by the scheduler
    /// regardless of the configured value — tighter intervals hammer
    /// the disk and shrink the deletion-guard window below human
    /// reaction time. Ignored when `sync_mode == Manual`.
    ///
    /// `#[serde(default = ...)]` — default 120s (matches the worker
    /// constant). Without this, omitting the field from the wire
    /// payload (CLI's partial-config path, peer mesh setup) would
    /// 422 on every register.
    #[serde(default = "default_sweep_interval_secs")]
    pub sweep_interval_secs: u64,
    /// Soft-delete grace window. Removed files keep a tombstone in the
    /// per-corpus state file; restoring the file with the same content
    /// hash within this window short-circuits re-extraction. Default 7
    /// days.
    #[serde(default = "default_soft_delete_grace_secs")]
    pub soft_delete_grace_secs: u64,
    /// Glob patterns excluded from the walk, in addition to the
    /// built-in defaults (`.git/`, `node_modules/`, `.DS_Store`, …).
    /// Matched against the path relative to the watched root.
    #[serde(default)]
    pub exclude_globs: Vec<String>,
    /// When `true`, scanned PDFs (no text layer) get OCR'd through
    /// the existing `local_corpus::ocr` pipeline (rasterize →
    /// tesseract → daemon cleanup) during a sweep. Requires the
    /// daemon to have an `OcrCtx` installed (`set_ocr_ctx`); the
    /// desktop runs `lcOcrAvailable()` to decide whether to surface
    /// the toggle. When `false` (the default), scanned PDFs land in
    /// `WatchedFolderState.failed_files` with reason
    /// `"scanned_no_text"` and don't enter the index.
    ///
    /// `#[serde(default)]` keeps existing on-disk corpora
    /// backwards-compatible — a JSON sidecar written before this
    /// field existed deserialises as `with_ocr: false`.
    #[serde(default)]
    pub with_ocr: bool,
    /// Sync cadence policy. `Continuous` (default) sweeps on the
    /// scheduler tick using `sweep_interval_secs`. `Manual` opts out
    /// of periodic sweeps; the corpus only sweeps when an explicit
    /// `sync-now` request flips the per-state pending flag.
    /// Folder-ingest v1 §3.5 — useful for `~/Downloads/` and inbox-
    /// style folders the user curates in batches.
    ///
    /// `#[serde(default)]` keeps pre-v1 sidecars round-tripping as
    /// `Continuous`, preserving today's behaviour.
    #[serde(default)]
    pub sync_mode: SyncMode,
    /// When `true`, the corpus is excluded from the agent's ambient
    /// situated-context assembly. The folder remains searchable on
    /// explicit query and via Inner Work mode (§4.15), but
    /// background "what does the user know about X?" assembly skips
    /// it. Folder-ingest v1 §3.4. Default `false` because most
    /// folders aren't sensitive and surfacing the flag prominently
    /// would suggest concerns the user doesn't have.
    ///
    /// Per ARCH §7.4, the flag is enforced at the assembly seam
    /// (defence-in-depth) — not just at the recipe level. The
    /// recipe-level invariants (`scope=Local`, `mesh_sharing=false`)
    /// already prevent off-machine egress; the sensitive flag is the
    /// additional layer that keeps sensitive corpora out of routine
    /// in-machine ambient context.
    #[serde(default)]
    pub sensitive: bool,
    /// Folder-ingest v1 §3.1: additional roots layered on top of
    /// the primary `LocalCorpusConfig.root_path`. Empty by default —
    /// most corpora are single-rooted. The walker iterates the
    /// primary first, then every additional root in order.
    ///
    /// Plan refinement vs. the original "Vec<RootSpec> everywhere"
    /// shape: keeping the primary `root_path` as a stable anchor
    /// lets `corpus_id` (which derives from a sha256 of the
    /// canonicalised primary) survive add/remove of additional
    /// roots without breaking the index. Existing pre-v1 watched
    /// corpora deserialise as zero-additional-roots and behave
    /// identically to today.
    ///
    /// doc_id shape: primary entries keep their original
    /// `relative/path.md` shape (so a single-root corpus is
    /// byte-identical to the pre-v1 layout). Entries from
    /// additional root `n` are namespaced under `_r{n}/` so
    /// same-relative-path-different-content across roots can
    /// coexist without clobber. Cross-root content-hash dedup
    /// (Phase D.2) lifts identical-content cross-root entries
    /// onto the canonical's `aux_paths`.
    #[serde(default)]
    pub additional_roots: Vec<RootSpec>,
    /// Folder-ingest v1 §3.3: per-folder atlas enrichment
    /// configuration. `Off` (default) keeps the folder
    /// retrieval-only — no positions, fault lines, or atom
    /// graph; the agent draws from the folder via standard
    /// retrieval. `On` opts the folder into the philosophy_atlas
    /// (or referential / literary) pipeline, producing the typed
    /// atom graph the situated-context layer can use for richer
    /// evidence assembly.
    ///
    /// v1 posture (committed in the plan): Initial enrichment is
    /// a full pipeline run. Subsequent ingest does NOT auto-re-
    /// enrich — the detail UI surfaces "M new docs since last
    /// build" and the user opts in to a rebuild. Disabling
    /// removes the atlas dir cleanly via
    /// `corpus_engine::atlas_teardown`; the chunk index is
    /// untouched.
    #[serde(default)]
    pub enrichment: WatchedEnrichmentConfig,
}

/// Folder-ingest v1 §3.3 — per-folder atlas enrichment opt-in.
/// `Off` is the default for every newly-registered watched
/// corpus; the user enables enrichment deliberately after
/// reading the cost framing in the detail UI.
///
/// Distinct from `LocalCorpusConfig.enrichment: EnrichmentConfig`
/// (the recipe-driven, single-flag opt-in for SEP / Wikipedia
/// builds). The watched-folder surface needs to track which
/// pipeline the user picked + when it last built; bundling that
/// onto the recipe-style flag would conflate two different
/// enrichment lifecycles, so this enum lives next to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[derive(Default)]
pub enum WatchedEnrichmentConfig {
    /// No atlas. Folder is searchable via standard retrieval
    /// only.
    #[default]
    Off,
    /// Atlas enrichment is enabled with `pipeline_id` chosen at
    /// enable time (one of `"philosophy_atlas"`,
    /// `"referential_atlas"`, `"literary_atlas"`). The
    /// `last_built_*` fields track the most recent successful
    /// build so the UI can render "M new docs since last build"
    /// and the rebuild-cost estimate.
    On {
        pipeline_id: String,
        #[serde(default)]
        last_built_at_unix: u64,
        #[serde(default)]
        last_built_doc_count: usize,
    },
}

/// One additional root attached to a watched-folder corpus.
/// Folder-ingest v1 §3.1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RootSpec {
    /// Canonicalised absolute path. Stored canonical so a
    /// re-register / list operation reads back the same value the
    /// scheduler walked.
    pub path: PathBuf,
    /// Unix seconds when the root was added. Surfaced in the UI
    /// so the user can spot a recently-added folder that hasn't
    /// finished its first sweep.
    pub added_at_unix: u64,
}

/// Per-folder sync cadence policy. See `WatchedFolderConfig.sync_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SyncMode {
    /// Sweep periodically per `sweep_interval_secs`. Default for
    /// folders the user actively maintains.
    #[default]
    Continuous,
    /// Sweep only on explicit `sync-now`. The scheduler skips this
    /// corpus on its periodic tick. For inbox-style folders the user
    /// curates in batches.
    Manual,
}

impl Default for WatchedFolderConfig {
    fn default() -> Self {
        Self {
            follow_symlinks: false,
            deletion_guard: DeletionGuardConfig::default(),
            sweep_interval_secs: default_sweep_interval_secs(),
            soft_delete_grace_secs: default_soft_delete_grace_secs(),
            exclude_globs: Vec::new(),
            with_ocr: false,
            sync_mode: SyncMode::Continuous,
            sensitive: false,
            additional_roots: Vec::new(),
            enrichment: WatchedEnrichmentConfig::Off,
        }
    }
}

// ─── Serde defaults (used by `#[serde(default = …)]`) ─────────────────
//
// Keep these in sync with the `Default` impl above. Free functions are
// the only shape serde's `default = "…"` attribute accepts, and
// inlining the value at the attribute site would silently let the two
// defaults diverge.

fn default_sweep_interval_secs() -> u64 {
    120
}

fn default_soft_delete_grace_secs() -> u64 {
    7 * 86_400
}

/// Catastrophe gate: a sweep that would remove `>= absolute_threshold`
/// files OR `>= fractional_threshold * live_count` files pauses the
/// corpus into `WatchedFolderStatus::PausedAwaitingConfirmation`. The
/// adds + updates from the same sweep still apply.
///
/// Defaults are deliberately generous: a folder of 50 files losing 30
/// trips on percentage; a folder of 200,000 files losing 5,000 trips
/// on absolute. Both failure modes are real, so the thresholds compose
/// as OR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletionGuardConfig {
    pub absolute_threshold: usize,
    pub fractional_threshold: f32,
    /// `false` disables the guard entirely — eager deletion. Default
    /// `true`. Bypass is exposed because the spec's discipline
    /// ("visible placeholder beats silent loss every time") is the
    /// right default but not the only credible setting.
    pub enabled: bool,
}

impl Default for DeletionGuardConfig {
    fn default() -> Self {
        Self {
            absolute_threshold: 100,
            fractional_threshold: 0.25,
            enabled: true,
        }
    }
}

// ─── Write-back (Obsidian only) ───────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteBackConfig {
    /// Namespace prefix for all sovereign-owned tags. `sovereign`
    /// hardcoded in v1 but kept configurable so the spec's invariant
    /// ("only `<namespace>/*` tags are ever written or removed") has a
    /// single source of truth.
    pub namespace: String,
    /// Directory name for Map-of-Content index notes written inside
    /// the vault, e.g. `_sovereign-index`.
    pub index_dir: String,
    /// Where snapshots are persisted. Must live OUTSIDE the vault —
    /// see spec §6.5 and invariant note: a snapshot written inside the
    /// vault would be ingested as a note on the next run.
    pub snapshot_dir: PathBuf,
    /// Number of snapshots to retain per corpus.
    pub snapshot_retention: usize,
}

// ─── Filesystem watcher ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    pub enabled: bool,
    pub debounce_ms: u64,
}

// ─── Pre-scan classifier knobs ────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreScanConfig {
    /// Run the "scanned PDF (no text layer)" heuristic. Meaningful for
    /// folder drops, pointless for `.md`-only vaults.
    pub scanned_pdf_detection: bool,
    /// Detect encrypted PDFs. Same rationale as above.
    pub password_detection: bool,
    /// Files above this size are indexed but flagged as slow in the
    /// UI. 0 disables the flag entirely.
    pub large_file_threshold_mb: u64,
}

// ─── LocalCorpusConfig (top-level) ────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalCorpusConfig {
    pub id: String,
    pub display_name: String,
    pub root_path: PathBuf,
    pub source_type: LocalCorpusSourceType,
    /// Lowercase file extensions without the leading dot, e.g. `["pdf",
    /// "txt"]` or `["md"]`.
    pub extensions: Vec<String>,
    pub chunker: ChunkerKind,
    pub write_back: Option<WriteBackConfig>,
    pub enrichment: Option<EnrichmentConfig>,
    pub watcher: WatcherConfig,
    pub pre_scan: PreScanConfig,
    pub scope: CorpusScope,
    /// Run OCR on PDFs that the pre-scanner classifies as
    /// `ScannedNoText`. Default `false` — set by the desktop layer
    /// when the user clicks "Read them with OCR" on the pre-scan
    /// panel. The flag persists per-corpus so re-ingest after
    /// adding more files behaves the same way without re-prompting.
    #[serde(default)]
    pub ocr_pdfs: bool,
}

/// Chunker choice. Mirrors `corpus_engine::recipe::ChunkerConfig` one
/// variant at a time so we don't leak the engine's schema into the
/// config surface that users see.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChunkerKind {
    /// Paragraph-based, good default for PDFs and plain text.
    Paragraph {
        max_chars: usize,
        overlap_chars: usize,
    },
    /// Heading-aware, used for markdown. `split_on_headings` is a
    /// list of heading levels to split on (`[2, 3]` = H2 and H3).
    /// v1: treated as H2/H3 hint; full parameterisation lands in M3.
    Semantic {
        max_chars: usize,
        overlap_chars: usize,
        split_on_headings: Vec<u8>,
    },
}

/// Enrichment flag. v1 only supports `enabled: bool`; the clustering
/// parameters for Obsidian live in a separate `ClusterConfig` passed
/// to `LocalCorpusManager::cluster()` directly (spec §6.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichmentConfig {
    pub enabled: bool,
}

// ─── Defaults ────────────────────────────────────────────────────────

impl LocalCorpusConfig {
    /// Default config for a drag-dropped documents folder (PDFs + TXT).
    pub fn document_folder(path: PathBuf, display_name: String) -> Self {
        let canon = canonical_or_as_is(&path);
        let id = format!("folder-{}", sha256_short(&canon));
        Self {
            id,
            display_name,
            root_path: canon,
            source_type: LocalCorpusSourceType::DocumentFolder,
            extensions: vec!["pdf".into(), "txt".into()],
            chunker: ChunkerKind::Paragraph {
                max_chars: 2048,
                overlap_chars: 256,
            },
            write_back: None,
            enrichment: None,
            watcher: WatcherConfig {
                enabled: false,
                debounce_ms: 0,
            },
            pre_scan: PreScanConfig {
                scanned_pdf_detection: true,
                password_detection: true,
                large_file_threshold_mb: 200,
            },
            scope: CorpusScope::Local,
            ocr_pdfs: false,
        }
    }

    /// Default config for a `WatchedFolder` corpus. Mirrors the
    /// `document_folder` defaults (PDF + TXT + MD readers, paragraph
    /// chunker, scanned-PDF detection on) but defaults `watcher.enabled`
    /// to `true` so the per-corpus mtime/size cache survives restarts
    /// — the watched-folder scheduler relies on the persisted
    /// `WatchedFolderConfig` carried inside `source_type`.
    ///
    /// `scope` is hardcoded to `Local` and `mesh_sharing` is wired off
    /// downstream in `recipe_toml`. These are not parameterised: per
    /// `ARCH_PRINCIPLES.md` §7, watched folders are a personal
    /// knowledge surface and the privacy invariant is structural.
    pub fn watched_folder(
        path: PathBuf,
        display_name: String,
        watched: WatchedFolderConfig,
    ) -> Self {
        let canon = canonical_or_as_is(&path);
        let id = format!("watched-{}", sha256_short(&canon));
        // Pull `with_ocr` off the watched config and project it onto
        // the LocalCorpusConfig.ocr_pdfs flag — single source of truth
        // for "should the OCR path run?", reused by both the initial
        // ingest path (which already honours `ocr_pdfs`) and the
        // sweep path (`apply.rs::apply_watched_diff` reads it).
        let with_ocr = watched.with_ocr;
        Self {
            id,
            display_name,
            root_path: canon,
            source_type: LocalCorpusSourceType::WatchedFolder(watched),
            // v1 format breadth: PDF + plain text + markdown +
            // Word docs + EPUBs + saved web pages. Each extension
            // dispatches in `extract_stage::extract_one`; everything
            // else falls into `skipped_by_extension` and surfaces in
            // watched-folder status.
            extensions: vec![
                "pdf".into(),
                "txt".into(),
                "md".into(),
                "docx".into(),
                "epub".into(),
                "html".into(),
                "htm".into(),
                "mhtml".into(),
            ],
            chunker: ChunkerKind::Paragraph {
                max_chars: 2048,
                overlap_chars: 256,
            },
            write_back: None,
            enrichment: None,
            watcher: WatcherConfig {
                enabled: true,
                debounce_ms: 0,
            },
            pre_scan: PreScanConfig {
                scanned_pdf_detection: true,
                password_detection: true,
                large_file_threshold_mb: 200,
            },
            scope: CorpusScope::Local,
            ocr_pdfs: with_ocr,
        }
    }

    /// Default config for an Obsidian vault (markdown + frontmatter).
    /// `snapshot_root` is the directory where snapshots will be stored
    /// — typically `~/.sovereign/vault-snapshots/`. The per-corpus
    /// subdirectory is appended automatically.
    pub fn obsidian_vault(path: PathBuf, snapshot_root: PathBuf) -> Self {
        let canon = canonical_or_as_is(&path);
        let id = format!("obsidian-{}", sha256_short(&canon));
        let display_name = canon
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "My vault".to_string());

        Self {
            id: id.clone(),
            display_name,
            root_path: canon,
            source_type: LocalCorpusSourceType::ObsidianVault {
                parse_frontmatter: true,
                follow_wiki_links: true,
            },
            extensions: vec!["md".into()],
            chunker: ChunkerKind::Semantic {
                max_chars: 2048,
                overlap_chars: 128,
                split_on_headings: vec![2, 3],
            },
            write_back: Some(WriteBackConfig {
                namespace: "sovereign".into(),
                index_dir: "_sovereign-index".into(),
                snapshot_dir: snapshot_root.join(&id),
                // Live-sync (Phase A): the reconciliation worker
                // can fire writeback after every sweep with new
                // atoms. A count-of-3 retention burns through the
                // baseline within ~3 minutes of active editing, so
                // we hold 24 instead — at the daemon's ~120s sweep
                // cadence with a 5-minute writeback debounce, 24
                // snapshots cover roughly a 2-hour edit session.
                // Snapshots live outside the vault and are small
                // (one JSON per write); the disk cost is bounded.
                snapshot_retention: 24,
            }),
            enrichment: Some(EnrichmentConfig { enabled: false }),
            watcher: WatcherConfig {
                enabled: true,
                debounce_ms: 800,
            },
            pre_scan: PreScanConfig {
                scanned_pdf_detection: false,
                password_detection: false,
                large_file_threshold_mb: 200,
            },
            scope: CorpusScope::Local,
            ocr_pdfs: false,
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────

fn canonical_or_as_is(p: &Path) -> PathBuf {
    // `canonicalize` fails when the path doesn't exist yet, which is
    // intentional: callers should only pass validated paths. When it
    // does fail, fall through to the raw path so tests can pass
    // synthetic paths through construction without hitting disk.
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

fn sha256_short(path: &Path) -> String {
    let mut h = Sha256::new();
    h.update(path.to_string_lossy().as_bytes());
    let digest = h.finalize();
    digest
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
}

// ─── Recipe TOML rendering ────────────────────────────────────────────

/// Render the Recipe that `CorpusEngine::ingest` will consume for this
/// local corpus. The returned TOML string is written to a temp file by
/// the manager and handed to the engine via `CorpusSpec::RecipePath`.
///
/// `jsonl_source` is the path to the pre-extracted JSONL file that
/// `ExtractStage` produces. Corpus-engine's `jsonl` extractor reads the
/// `content` / `title` / `source_id` fields we wrote.
pub fn recipe_toml(config: &LocalCorpusConfig, jsonl_source: &Path) -> String {
    let chunk = match &config.chunker {
        ChunkerKind::Paragraph {
            max_chars,
            overlap_chars,
        } => format!(
            r#"[chunk]
type = "paragraph"
max_chars = {max_chars}
overlap_chars = {overlap_chars}
"#
        ),
        // v1: the engine's `Semantic` variant only accepts `max_chars`.
        // M3 extends it to carry the heading list; until then we emit a
        // paragraph chunker with the same max_chars so ingestion works.
        // When the engine variant is extended, update this branch.
        ChunkerKind::Semantic {
            max_chars,
            overlap_chars,
            ..
        } => format!(
            r#"[chunk]
type = "paragraph"
max_chars = {max_chars}
overlap_chars = {overlap_chars}
"#
        ),
    };

    let description = format!(
        "Local corpus ({}): {}",
        source_type_tag(&config.source_type),
        config.display_name
    );

    // The `[display]` section routes the recipe into the tiered
    // retrieval surface (GLiNER + per-source-doc RAPTOR + entity-walk
    // rerank). `is_tiered_category` in `sovereign-core/src/conv_briefing.rs`
    // gates on these category strings — without one, the recipe lands
    // in the index but the daemon's FolderTieredProvider never picks
    // it up.
    //
    // The `[enrichment] type = "tiered"` section routes the ingest
    // through `corpus-engine::enrichment::tiered::run_tiered_enrichment`
    // → `run_folder_tiered_enrichment`, which dispatches per-note
    // GLiNER NER + per-note RAPTOR via `FolderTieredProvider`. Without
    // it, ingest writes chunks but never invokes the tiered surface
    // (legacy field-model engine wouldn't fit either — vault has no
    // matching pipeline since `obsidian_atlas` was retired).
    let (display, enrichment) = match &config.source_type {
        LocalCorpusSourceType::ObsidianVault { .. } => (
            "[display]\ncategory = \"vault\"\nicon = \"book-open\"\n\n",
            "[enrichment]\nenabled = true\ntype = \"tiered\"\n\n",
        ),
        LocalCorpusSourceType::WatchedFolder(_) => (
            "[display]\ncategory = \"watched_folder\"\nicon = \"folder\"\n\n",
            "[enrichment]\nenabled = true\ntype = \"tiered\"\n\n",
        ),
        // DocumentFolder is one-shot drag-drop ingest — no daemon-side
        // sweeper, no tiered enrichment. Stays uncategorised.
        LocalCorpusSourceType::DocumentFolder => ("", ""),
    };

    format!(
        r#"[corpus]
id = "{id}"
name = "{display_name}"
description = "{description}"
license = "local"
mesh_sharing = false
scope = "{scope}"
schema_version = 1

[acquire]
type = "local_file"
path = "{source_path}"

[extract]
type = "jsonl"
content_field = "content"
title_field = "title"

{chunk}
[index]
fts = true
vector = true

{display}{enrichment}"#,
        id = escape_toml(&config.id),
        display_name = escape_toml(&config.display_name),
        description = escape_toml(&description),
        scope = config.scope.as_recipe_str(),
        source_path = escape_toml(&jsonl_source.to_string_lossy()),
        chunk = chunk,
        display = display,
        enrichment = enrichment,
    )
}

fn source_type_tag(t: &LocalCorpusSourceType) -> &'static str {
    match t {
        LocalCorpusSourceType::ObsidianVault { .. } => "obsidian",
        LocalCorpusSourceType::DocumentFolder => "folder",
        LocalCorpusSourceType::WatchedFolder(_) => "watched",
    }
}

impl LocalCorpusSourceType {
    /// True for `WatchedFolder` variants; useful for filtering the
    /// manager's list when the daemon spawns reconciliation workers.
    pub fn is_watched(&self) -> bool {
        matches!(self, LocalCorpusSourceType::WatchedFolder(_))
    }

    /// True for any source type the daemon's reconciliation worker
    /// should periodically sweep. Covers `WatchedFolder` *and*
    /// `ObsidianVault` — both surface user edits the worker must
    /// reflect into the index. `DocumentFolder` is one-shot (drag-drop
    /// ingest) and never enters the dispatch loop.
    ///
    /// Distinct from `is_watched()` so any caller that means "is this
    /// specifically a WatchedFolder source" (UI affordances, recipe
    /// distinctions) keeps its narrower semantics unchanged.
    pub fn should_reconcile(&self) -> bool {
        matches!(
            self,
            LocalCorpusSourceType::WatchedFolder(_) | LocalCorpusSourceType::ObsidianVault { .. }
        )
    }

    /// Tag for the reconciliation worker telling it how to interpret a
    /// dispatched corpus. `None` for source types the worker should
    /// never see (i.e. `DocumentFolder`). Branching on this in the
    /// worker keeps the dispatch path single while letting the
    /// per-variant differences (vault writeback, watched additional
    /// roots) stay explicit.
    pub fn reconcile_kind(&self) -> Option<ReconcileKind> {
        match self {
            LocalCorpusSourceType::WatchedFolder(_) => Some(ReconcileKind::WatchedFolder),
            LocalCorpusSourceType::ObsidianVault { .. } => Some(ReconcileKind::ObsidianVault),
            LocalCorpusSourceType::DocumentFolder => None,
        }
    }

    /// Borrow the `WatchedFolderConfig` if this is a watched-folder
    /// source. Returns `None` for the other source types.
    pub fn watched_config(&self) -> Option<&WatchedFolderConfig> {
        match self {
            LocalCorpusSourceType::WatchedFolder(cfg) => Some(cfg),
            _ => None,
        }
    }
}

/// Discriminator the reconciliation worker uses to branch between the
/// two reconcilable source types. Mirrors `LocalCorpusSourceType` but
/// strips the per-variant payload so callers that only need to dispatch
/// on shape don't carry the full config around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileKind {
    WatchedFolder,
    ObsidianVault,
}

fn escape_toml(s: &str) -> String {
    // Quote for a basic-string TOML value. Escape backslashes and quotes.
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str(r#"\""#),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_default_has_no_writeback() {
        let cfg = LocalCorpusConfig::document_folder(
            PathBuf::from("/tmp/some-folder"),
            "City council 2024".into(),
        );
        assert!(cfg.write_back.is_none());
        assert!(cfg.enrichment.is_none());
        assert_eq!(cfg.extensions, vec!["pdf".to_string(), "txt".to_string()]);
        assert_eq!(cfg.scope, CorpusScope::Local);
        assert!(cfg.id.starts_with("folder-"));
        assert!(!cfg.watcher.enabled);
        assert!(cfg.pre_scan.scanned_pdf_detection);
    }

    #[test]
    fn vault_recipe_emits_vault_display_and_tiered_enrichment() {
        // Routes the recipe into FolderTieredProvider's per-source-doc
        // RAPTOR + GLiNER dispatch. Both [display] and [enrichment]
        // are load-bearing: [display] flips the briefing-side gate,
        // [enrichment] flips the ingest-side dispatch into the tiered
        // runner. Without either, the daemon ingests the vault but no
        // tiered enrichment fires (or it fires but never surfaces in
        // chat).
        let snap = PathBuf::from("/tmp/snapshots");
        let cfg = LocalCorpusConfig::obsidian_vault(PathBuf::from("/tmp/my-vault"), snap);
        let toml = recipe_toml(&cfg, &PathBuf::from("/tmp/staged.jsonl"));
        assert!(
            toml.contains("[display]\ncategory = \"vault\""),
            "vault recipe did not emit [display] vault category. \
             Full recipe:\n{toml}"
        );
        assert!(
            toml.contains("[enrichment]\nenabled = true\ntype = \"tiered\""),
            "vault recipe did not emit [enrichment] tiered block. \
             Full recipe:\n{toml}"
        );
    }

    #[test]
    fn watched_folder_recipe_emits_watched_folder_display_and_tiered() {
        let mut cfg = LocalCorpusConfig::document_folder(
            PathBuf::from("/tmp/some-folder"),
            "City council".into(),
        );
        cfg.source_type = LocalCorpusSourceType::WatchedFolder(WatchedFolderConfig::default());
        let toml = recipe_toml(&cfg, &PathBuf::from("/tmp/staged.jsonl"));
        assert!(
            toml.contains("[display]\ncategory = \"watched_folder\""),
            "watched-folder recipe did not emit [display] category. \
             Full recipe:\n{toml}"
        );
        assert!(
            toml.contains("[enrichment]\nenabled = true\ntype = \"tiered\""),
            "watched-folder recipe did not emit [enrichment] tiered block. \
             Full recipe:\n{toml}"
        );
    }

    #[test]
    fn document_folder_recipe_skips_display_and_enrichment() {
        // Document folders are one-shot drag-drop ingest — no daemon
        // sweeper, no tiered enrichment. They should NOT emit a tiered
        // display category OR an enrichment block.
        let cfg =
            LocalCorpusConfig::document_folder(PathBuf::from("/tmp/folder"), "Manuals".into());
        let toml = recipe_toml(&cfg, &PathBuf::from("/tmp/staged.jsonl"));
        assert!(
            !toml.contains("[display]"),
            "document folder recipe leaked a [display] section. \
             Full recipe:\n{toml}"
        );
        assert!(
            !toml.contains("[enrichment]"),
            "document folder recipe leaked an [enrichment] section. \
             Full recipe:\n{toml}"
        );
    }

    #[test]
    fn vault_default_has_writeback_and_watcher() {
        let snap = PathBuf::from("/tmp/snapshots");
        let cfg = LocalCorpusConfig::obsidian_vault(PathBuf::from("/tmp/my-vault"), snap);
        let wb = cfg
            .write_back
            .as_ref()
            .expect("vault should have writeback");
        assert_eq!(wb.namespace, "sovereign");
        assert_eq!(wb.index_dir, "_sovereign-index");
        assert_eq!(wb.snapshot_retention, 24);
        assert!(wb.snapshot_dir.starts_with("/tmp/snapshots"));
        assert!(wb.snapshot_dir.to_string_lossy().contains(&cfg.id));
        assert!(cfg.watcher.enabled);
        assert_eq!(cfg.watcher.debounce_ms, 800);
        assert_eq!(cfg.extensions, vec!["md".to_string()]);
        assert!(matches!(cfg.chunker, ChunkerKind::Semantic { .. }));
        assert!(cfg.id.starts_with("obsidian-"));
        // Scanned-PDF detection is pointless for markdown vaults.
        assert!(!cfg.pre_scan.scanned_pdf_detection);
    }

    #[test]
    fn watched_folder_config_partial_payload_deserialises_via_defaults() {
        // Regression: when /internal/corpus/watch/register was first
        // wired, the CLI sent a partial config (only the knobs the
        // user explicitly set) and the daemon's WatchedFolderConfig
        // had no #[serde(default)] on its non-optional fields,
        // returning 422 on every register that omitted any of
        // follow_symlinks / deletion_guard / sweep_interval_secs /
        // soft_delete_grace_secs / exclude_globs. The fix is at the
        // *deserializer* (server) — clients should be allowed to send
        // only the fields they care about. This test pins the
        // partial-payload contract.

        // 1. Empty object → all defaults.
        let cfg: WatchedFolderConfig = serde_json::from_str("{}").expect("empty {} parses");
        assert_eq!(cfg.sweep_interval_secs, 120);
        assert_eq!(cfg.soft_delete_grace_secs, 7 * 86_400);
        assert!(!cfg.follow_symlinks);
        assert_eq!(cfg.deletion_guard.absolute_threshold, 100);
        assert!((cfg.deletion_guard.fractional_threshold - 0.25).abs() < 1e-6);
        assert!(cfg.deletion_guard.enabled);
        assert!(cfg.exclude_globs.is_empty());
        assert_eq!(cfg.sync_mode, SyncMode::Continuous);
        assert!(!cfg.sensitive);
        assert!(cfg.additional_roots.is_empty());

        // 2. CLI's actual shape: only the fields the user set.
        let partial = r#"{
            "follow_symlinks": true,
            "exclude_globs": ["COMMONWEALTH/**", ".obsidian/**"]
        }"#;
        let cfg: WatchedFolderConfig =
            serde_json::from_str(partial).expect("CLI partial payload parses");
        assert!(cfg.follow_symlinks);
        assert_eq!(cfg.exclude_globs.len(), 2);
        // Defaults still kick in for omitted fields:
        assert_eq!(cfg.sweep_interval_secs, 120);
        assert_eq!(cfg.deletion_guard.absolute_threshold, 100);
    }

    #[test]
    fn serde_defaults_match_default_impl() {
        // The free functions referenced by `#[serde(default = "…")]`
        // and the `Default` impl must agree. A drift here means a
        // server that builds via Default sees one value while a
        // server that deserialises an empty payload sees another —
        // the bug-fix only works as long as the two stay locked.
        let from_default = WatchedFolderConfig::default();
        let from_empty: WatchedFolderConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(
            from_default.sweep_interval_secs,
            from_empty.sweep_interval_secs
        );
        assert_eq!(
            from_default.soft_delete_grace_secs,
            from_empty.soft_delete_grace_secs
        );
        assert_eq!(from_default.follow_symlinks, from_empty.follow_symlinks);
    }

    #[test]
    fn should_reconcile_covers_watched_and_vault_but_not_folder() {
        let folder = LocalCorpusConfig::document_folder(PathBuf::from("/tmp/f"), "f".into());
        assert!(!folder.source_type.should_reconcile());
        assert!(folder.source_type.reconcile_kind().is_none());

        let watched = LocalCorpusConfig::watched_folder(
            PathBuf::from("/tmp/w"),
            "w".into(),
            WatchedFolderConfig::default(),
        );
        assert!(watched.source_type.should_reconcile());
        assert_eq!(
            watched.source_type.reconcile_kind(),
            Some(ReconcileKind::WatchedFolder)
        );

        let vault =
            LocalCorpusConfig::obsidian_vault(PathBuf::from("/tmp/v"), PathBuf::from("/tmp/snap"));
        assert!(vault.source_type.should_reconcile());
        assert_eq!(
            vault.source_type.reconcile_kind(),
            Some(ReconcileKind::ObsidianVault)
        );
    }

    #[test]
    fn corpus_id_is_stable_for_same_path() {
        let a = LocalCorpusConfig::document_folder(PathBuf::from("/tmp/x"), "x".into());
        let b = LocalCorpusConfig::document_folder(PathBuf::from("/tmp/x"), "x".into());
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn corpus_id_differs_by_path() {
        let a = LocalCorpusConfig::document_folder(PathBuf::from("/tmp/a"), "a".into());
        let b = LocalCorpusConfig::document_folder(PathBuf::from("/tmp/b"), "b".into());
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn recipe_toml_parses_as_valid_recipe() {
        // The round-trip test: our generated TOML must parse cleanly
        // via corpus_engine::Recipe::from_toml. Catches any drift
        // between field names / defaults across crate versions.
        let cfg = LocalCorpusConfig::document_folder(
            PathBuf::from("/tmp/docs"),
            "City council 2024".into(),
        );
        let jsonl = PathBuf::from("/tmp/staged.jsonl");
        let toml = recipe_toml(&cfg, &jsonl);
        let recipe =
            corpus_engine::Recipe::from_toml(&toml).expect("generated recipe TOML must parse");
        assert_eq!(recipe.corpus.id, cfg.id);
        assert_eq!(recipe.corpus.scope.as_deref(), Some("local"));
        assert!(!recipe.corpus.mesh_sharing);
    }

    #[test]
    fn watched_folder_default_extensions_and_scope() {
        let cfg = LocalCorpusConfig::watched_folder(
            PathBuf::from("/tmp/notes"),
            "Research notes".into(),
            WatchedFolderConfig::default(),
        );
        assert_eq!(cfg.scope, CorpusScope::Local);
        assert!(cfg.id.starts_with("watched-"));
        // v1 format breadth: PDF + plain text + markdown + Word
        // docs + EPUBs + saved web pages.
        assert_eq!(
            cfg.extensions,
            vec![
                "pdf".to_string(),
                "txt".into(),
                "md".into(),
                "docx".into(),
                "epub".into(),
                "html".into(),
                "htm".into(),
                "mhtml".into(),
            ]
        );
        assert!(matches!(
            cfg.source_type,
            LocalCorpusSourceType::WatchedFolder(_)
        ));
        assert!(cfg.write_back.is_none()); // Read-only on source — no writeback path.
    }

    #[test]
    fn watched_folder_recipe_toml_pins_privacy_invariants() {
        // ARCH §7: privacy invariants must be structural. A watched
        // folder must serialise with `scope = "local"` and
        // `mesh_sharing = false` regardless of caller config. If a
        // future refactor parameterises either, this test fails first.
        let cfg = LocalCorpusConfig::watched_folder(
            PathBuf::from("/tmp/notes"),
            "Research notes".into(),
            WatchedFolderConfig::default(),
        );
        let jsonl = PathBuf::from("/tmp/staged.jsonl");
        let toml = recipe_toml(&cfg, &jsonl);
        let recipe =
            corpus_engine::Recipe::from_toml(&toml).expect("watched-folder recipe TOML must parse");
        assert_eq!(recipe.corpus.scope.as_deref(), Some("local"));
        assert!(!recipe.corpus.mesh_sharing);
    }

    #[test]
    fn deletion_guard_defaults_are_on_with_credible_thresholds() {
        let g = DeletionGuardConfig::default();
        assert!(g.enabled);
        assert_eq!(g.absolute_threshold, 100);
        assert!((g.fractional_threshold - 0.25).abs() < f32::EPSILON);
    }

    #[test]
    fn watched_folder_config_defaults_match_spec() {
        let c = WatchedFolderConfig::default();
        assert_eq!(c.sweep_interval_secs, 120);
        assert_eq!(c.soft_delete_grace_secs, 7 * 86_400);
        assert!(!c.follow_symlinks);
        assert!(c.exclude_globs.is_empty());
        // v1 defaults — both opt-in features stay off until the user
        // deliberately enables them.
        assert_eq!(c.sync_mode, SyncMode::Continuous);
        assert!(!c.sensitive);
    }

    #[test]
    fn watched_folder_config_round_trips_pre_v1_sidecars() {
        // A JSON sidecar written before the v1 fields existed must
        // deserialise as the documented defaults — no manual
        // migration step. The serde(default) attributes are
        // load-bearing for any user with an existing watched corpus.
        let pre_v1 = r#"{
            "follow_symlinks": false,
            "deletion_guard": { "absolute_threshold": 100, "fractional_threshold": 0.25, "enabled": true },
            "sweep_interval_secs": 120,
            "soft_delete_grace_secs": 604800,
            "exclude_globs": []
        }"#;
        let c: WatchedFolderConfig =
            serde_json::from_str(pre_v1).expect("pre-v1 sidecar must deserialise");
        assert_eq!(c.sync_mode, SyncMode::Continuous);
        assert!(!c.sensitive);
        assert!(!c.with_ocr);
    }

    #[test]
    fn watched_folder_config_serializes_manual_sync_mode_lowercase() {
        // The on-disk representation is the snake_case string —
        // pinned so a refactor that drops `#[serde(rename_all)]`
        // can't silently break round-trip with existing sidecars.
        let mut c = WatchedFolderConfig::default();
        c.sync_mode = SyncMode::Manual;
        c.sensitive = true;
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains(r#""sync_mode":"manual""#), "got: {json}");
        assert!(json.contains(r#""sensitive":true"#), "got: {json}");
    }

    #[test]
    fn watched_enrichment_config_default_is_off() {
        // Folder-ingest v1 §3.3: enrichment defaults to Off.
        // Pinned because the spec is explicit: "off is always
        // safe and useful. Enabling is an investment the user
        // makes deliberately." A refactor that flips the default
        // to a per-pipeline enabled state breaks this contract.
        let c = WatchedFolderConfig::default();
        assert_eq!(c.enrichment, WatchedEnrichmentConfig::Off);
    }

    #[test]
    fn watched_enrichment_config_round_trips_on_variant() {
        let on = WatchedEnrichmentConfig::On {
            pipeline_id: "philosophy_atlas".into(),
            last_built_at_unix: 1_700_000_000,
            last_built_doc_count: 42,
        };
        let json = serde_json::to_string(&on).unwrap();
        assert!(json.contains(r#""kind":"on""#), "got: {json}");
        assert!(
            json.contains(r#""pipeline_id":"philosophy_atlas""#),
            "got: {json}"
        );
        let parsed: WatchedEnrichmentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, on);
    }

    #[test]
    fn watched_enrichment_config_pre_v1_sidecars_default_off() {
        // A pre-v1 watched-folder sidecar (no `enrichment` field)
        // must deserialize as `Off` so existing corpora upgrade
        // cleanly. Same `#[serde(default)]` pattern as
        // `additional_roots` and `sensitive`.
        let pre_v1 = r#"{
            "follow_symlinks": false,
            "deletion_guard": { "absolute_threshold": 100, "fractional_threshold": 0.25, "enabled": true },
            "sweep_interval_secs": 120,
            "soft_delete_grace_secs": 604800,
            "exclude_globs": []
        }"#;
        let c: WatchedFolderConfig = serde_json::from_str(pre_v1).unwrap();
        assert_eq!(c.enrichment, WatchedEnrichmentConfig::Off);
    }

    #[test]
    fn recipe_toml_escapes_quotes_and_backslashes() {
        let cfg = LocalCorpusConfig::document_folder(
            PathBuf::from(r#"/tmp/has "quotes" and \backslashes"#),
            r#"Alex's "Notes""#.into(),
        );
        let jsonl = PathBuf::from("/tmp/staged.jsonl");
        let toml = recipe_toml(&cfg, &jsonl);
        let recipe =
            corpus_engine::Recipe::from_toml(&toml).expect("escaped TOML must still parse");
        // Display name round-trips byte-for-byte.
        assert_eq!(recipe.corpus.name, r#"Alex's "Notes""#);
    }
}
