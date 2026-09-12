// SPDX-License-Identifier: AGPL-3.0-or-later
//! The local-corpus CONFIG SCHEMA — what a folder-, vault- or
//! watched-folder-backed corpus IS.
//!
//! # Why it is here and not in `sovereign-tools` (svt-6, 2026-09-12)
//!
//! Every type below is pure serde over primitives: it is what the desktop
//! writes when a user drags a folder in, what the daemon persists, and what
//! crosses `/internal/corpus/local/*` in both directions. `sovereign-tools`
//! defined them, so a client that only wanted to SPELL a source type had to
//! link the tools crate — and through it corpus-engine, sovereign-store,
//! sovereign-atos and five more (`quality/ARCH_LAYERS.toml`'s
//! `sovereign-desktop -> arch-layers` row names the chain).
//!
//! What stayed in `sovereign_tools::local_corpus::config` is the half that
//! names the ENGINE: `recipe_toml` (render the Recipe `CorpusEngine::ingest`
//! consumes), `display_meta` (which answers `corpus_engine::recipe::
//! DisplayMeta`, and is therefore a free function there rather than a method
//! here), `source_type_tag` and `escape_toml`. That is the line principle 12
//! draws: the schema is what a local corpus owns about itself, the rendering
//! is what the engine asks of it.
//!
//! `sovereign_tools::local_corpus::config` re-exports every item here at its
//! historical path, so no importer in the monorepo changes.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    /// Optional workflow (a `sovereign workflow` name, or a `.toml` path) to run
    /// automatically whenever a sweep produces added/modified/removed changes —
    /// the "living trigger". `None` (the default) means no trigger and behaviour
    /// is byte-identical to a folder without one. Set only via an explicit,
    /// consent-gated attach (`corpus watch --on-change`), because a triggered
    /// workflow runs unattended.
    ///
    /// `#[serde(default)]` so pre-trigger sidecars round-trip as `None`.
    #[serde(default)]
    pub run_on_changes: Option<String>,
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
            run_on_changes: None,
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

/// The v1 format breadth Sovereign can actually extract: PDF + plain text +
/// markdown + Word docs + EPUBs + saved web pages. Each extension dispatches in
/// `extract_stage::extract_one`; everything else falls into
/// `skipped_by_extension`.
///
/// ONE list, shared by every folder-backed corpus default, because the two
/// defaults drifting apart is a shipped bug and not a hypothetical: drag-drop
/// (`document_folder`) admitted only `pdf` + `txt` while `watched_folder`
/// admitted this full set. The same folder of `.md` notes therefore ingested
/// correctly as a watched folder and produced a silently EMPTY corpus via
/// drag-drop — extraction supported markdown the whole time; only this list
/// disagreed. Found 2026-07-28 by the real-mode desktop e2e gate, which caught
/// the governance fixture (two `.md` files) ingesting zero chunks.
///
/// Add a format here only once `extract_one` genuinely handles it.
pub const DEFAULT_FOLDER_EXTENSIONS: &[&str] =
    &["pdf", "txt", "md", "docx", "epub", "html", "htm", "mhtml"];

/// `DEFAULT_FOLDER_EXTENSIONS` as owned strings, for config construction.
fn default_folder_extensions() -> Vec<String> {
    DEFAULT_FOLDER_EXTENSIONS
        .iter()
        .map(|e| (*e).to_string())
        .collect()
}

impl LocalCorpusConfig {
    /// Default config for a drag-dropped documents folder.
    ///
    /// Admits the full [`DEFAULT_FOLDER_EXTENSIONS`] set — see that constant for
    /// why this is shared with `watched_folder` rather than spelled out again.
    pub fn document_folder(path: PathBuf, display_name: String) -> Self {
        let canon = canonical_or_as_is(&path);
        let id = corpus_id_for("folder", &canon);
        Self {
            id,
            display_name,
            root_path: canon,
            source_type: LocalCorpusSourceType::DocumentFolder,
            extensions: default_folder_extensions(),
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
        let id = corpus_id_for("watched", &canon);
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
            // v1 format breadth — see `DEFAULT_FOLDER_EXTENSIONS`. Anything
            // outside it falls into `skipped_by_extension` and surfaces in
            // watched-folder status.
            extensions: default_folder_extensions(),
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
    /// — typically `~/.svrnmesh/vault-snapshots/`. The per-corpus
    /// subdirectory is appended automatically.
    pub fn obsidian_vault(path: PathBuf, snapshot_root: PathBuf) -> Self {
        let canon = canonical_or_as_is(&path);
        let id = corpus_id_for("obsidian", &canon);
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

/// Human-readable corpus id: `<kind>-<slug>-<hash>`, e.g.
/// `obsidian-vault-959ee8a8f330` for a vault at `…/Obsidian Vault`.
///
/// The id is the citation handle and a structural key — it must stay
/// deterministic from the path (re-registering the same folder mints
/// the same id) and keep the `<kind>-` prefix some consumers key on
/// (`stream_axes` stability classification). The slug exists purely
/// for the humans who read these ids in bench commands, logs, and
/// reports: pre-2026-06-11 ids were `<kind>-<hex>` and unguessable.
/// Existing corpora keep their minted ids — `LocalCorpusManager::
/// register` reuses the registered id for an already-known path.
///
/// `pub` since svt-6: it is the ONE minter of a local-corpus id (principle
/// 8), and the tests that pin the rule live beside the recipe rendering in
/// `sovereign_tools::local_corpus::config`.
pub fn corpus_id_for(kind: &str, path: &Path) -> String {
    let slug_src = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let mut slug = String::new();
    for c in slug_src.chars().flat_map(|c| c.to_lowercase()) {
        if slug.len() >= 24 {
            break;
        }
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    // "Obsidian Vault" under kind "obsidian" would mint
    // `obsidian-obsidian-vault-…` — drop the repeated kind token.
    let slug = slug.strip_prefix(&format!("{kind}-")).unwrap_or(slug);
    let hash = sha256_short(path);
    if slug.is_empty() {
        format!("{kind}-{hash}")
    } else {
        format!("{kind}-{slug}-{hash}")
    }
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
