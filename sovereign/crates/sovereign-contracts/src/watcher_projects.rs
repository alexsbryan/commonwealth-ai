// SPDX-License-Identifier: AGPL-3.0-or-later
//! The watcher `projects` on-disk schema — `ProjectEntry`, `Registry`,
//! `WatcherToggles` and the two status enums.
//!
//! Moved from `corpus-engine-watchers` (fp-2, §12 decision 3: wire vocabulary
//! into the contracts leaf). The live `ProjectState` and the watcher machinery
//! stay in the owning crate, which re-exports everything here at its
//! historical `projects::` path.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Which subsystem a [`WatcherStatus`] refers to. The daemon runs
/// one supervised task per (project, kind) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatcherKind {
    /// SCIP graph rebuilder (FS watcher + git poll + rebuild worker).
    Scip,
    /// User-configured test runner (e.g. `cargo test`).
    Test,
    /// User-configured lint runner (e.g. `cargo clippy`).
    Lint,
    /// Sovereign.toml config watcher that live-reloads on TOML changes.
    Config,
    /// The mesh work-plane donor loop (`work_donor`). Supervised for
    /// the same reason the three above are: its body runs a THIRD PARTY's
    /// argv, so a panic in it must take out its own task and nothing else.
    WorkDonor,
}

impl WatcherKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Scip => "scip",
            Self::Test => "test",
            Self::Lint => "lint",
            Self::Config => "config",
            Self::WorkDonor => "work_donor",
        }
    }
}

/// Live status of one supervised watcher. The supervisor transitions
/// between these; tools / HTTP reads observe whichever is current.
///
/// Serialized variants use kebab-case so the `/v1/projects` response
/// is ergonomic for scripts and dashboards without needing a
/// client-side mapping table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum WatcherStatus {
    /// Registered but not yet spawned.
    Pending,
    /// Spawned and waiting for work (between rebuild cycles,
    /// between debounces). Healthy.
    Idle,
    /// Actively running a cycle. Healthy.
    Active,
    /// Task body panicked; supervisor is awaiting the backoff
    /// before restarting. `count` is the running crash count for
    /// this supervision session.
    Crashed { reason: String, count: usize },
    /// Hit `MAX_AUTO_RESTARTS` crashes. Supervisor has given up;
    /// an operator must run `sovereign project watch restart`.
    Disabled { reason: String },
    /// Cancelled externally (daemon shutdown, project unregister,
    /// `sovereign project serve` lease acquired). Not an error.
    Aborted,
}

impl WatcherStatus {
    /// True when the tool layer should serve results normally.
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Idle | Self::Active)
    }
}

// ─── On-disk registry ─────────────────────────────────────────

/// One entry in `~/.svrnmesh/projects.json`. Persistence-layer
/// twin of the in-memory [`ProjectState`]: what the CLI's
/// `project register` command writes, and what the daemon reads at
/// startup to rebuild its supervisor topology.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectEntry {
    pub corpus_id: String,
    pub root: PathBuf,
    #[serde(default = "default_registered_at")]
    pub registered_at: String,
    #[serde(default)]
    pub watchers: WatcherToggles,
}

impl ProjectEntry {
    /// A freshly-registered project with default watchers, stamped now.
    ///
    /// The one place a new entry is minted, so the `registered_at`
    /// format cannot drift between the HTTP register route and `svrn
    /// setup`'s direct registry write (ARCH §10.6). Override
    /// `watchers` with struct-update syntax when the caller has an
    /// explicit toggle set.
    pub fn new(corpus_id: impl Into<String>, root: impl Into<PathBuf>) -> Self {
        Self {
            corpus_id: corpus_id.into(),
            root: root.into(),
            registered_at: default_registered_at(),
            watchers: WatcherToggles::default(),
        }
    }
}

/// Per-watcher enable + tuning knobs. Separate from `WatcherStatus`
/// (which is liveness state) — this is the user's configured intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherToggles {
    #[serde(default = "default_true")]
    pub scip: bool,
    #[serde(default = "default_true")]
    pub test: bool,
    #[serde(default = "default_true")]
    pub lint: bool,
    /// SCIP rebuild FS-debounce window in milliseconds. Higher
    /// values collapse larger event storms at the cost of latency
    /// on isolated saves.
    #[serde(default = "default_scip_debounce_ms")]
    pub scip_debounce_ms: u64,
    /// Git HEAD poll interval in seconds. Set to 0 to disable the
    /// git-poll signal entirely (fall back to FS + lazy).
    #[serde(default = "default_git_poll_secs")]
    pub git_poll_secs: u64,
    /// Per-project extra path components to drop at the FS watcher
    /// seam, in addition to the universal `.git` / `target` /
    /// `node_modules` / etc. hard-exclude list.
    ///
    /// Matched against any path component, so an entry of
    /// `.sovereign` excludes `<root>/.sovereign/**` and
    /// `<root>/sub/.sovereign/**` alike. Use this for tool-local
    /// state directories (sovereign's own `.sovereign/`, generated
    /// asset trees, IDE caches that aren't covered by `.gitignore`).
    ///
    /// The init CLI (`sovereign project init --watcher-ignore PATH`,
    /// repeatable) is the canonical way to seed this; editing
    /// `~/.svrnmesh/projects.json` directly also works.
    #[serde(default = "default_ignore_paths")]
    pub ignore_paths: Vec<String>,
}

impl Default for WatcherToggles {
    fn default() -> Self {
        Self {
            scip: true,
            test: true,
            lint: true,
            scip_debounce_ms: default_scip_debounce_ms(),
            git_poll_secs: default_git_poll_secs(),
            ignore_paths: default_ignore_paths(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_scip_debounce_ms() -> u64 {
    2000
}
fn default_git_poll_secs() -> u64 {
    30
}
/// Default ignore_paths seeded at project registration. `.sovereign/`
/// is the daemon's project-local state directory (notes.db, mesh.db,
/// features.db + their SQLite WAL/SHM sidecars) — including it by
/// default keeps the freshly-registered project quiet immediately.
/// Users in other deployment shapes can replace this via
/// `--watcher-ignore` at init time.
fn default_ignore_paths() -> Vec<String> {
    vec![".sovereign".to_string()]
}
fn default_registered_at() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Persisted registry at `~/.svrnmesh/projects.json`. Thin
/// wrapper around `Vec<ProjectEntry>` that handles load/save and
/// `corpus_id`-keyed add/remove. Safe to call `load()` when the
/// file doesn't exist — returns an empty registry.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Registry {
    entries: Vec<ProjectEntry>,
}

impl Registry {
    /// Canonical path: `<branded root>/projects.json` (rebrand-aware —
    /// prefers a populated `~/.svrnmesh`). Used by `sovereign project
    /// register|unregister|list` and by the daemon at startup; readers
    /// (symbol_lookup, doctor) resolve through the SAME accessor so
    /// writer and reader cannot split.
    pub fn default_path() -> PathBuf {
        crate::rebrand::projects_json()
    }

    /// Load from the canonical path. Missing file → empty registry.
    pub fn load() -> Result<Self, String> {
        Self::load_from(&Self::default_path())
    }

    pub fn load_from(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        if content.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&content).map_err(|e| format!("parse {}: {e}", path.display()))
    }

    /// Atomic save: write to `projects.json.new`, then rename over.
    /// Keeps the running daemon from ever reading a half-written
    /// file — parallel `project register` calls are safe.
    pub fn save(&self) -> Result<(), String> {
        self.save_to(&Self::default_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        let tmp_name = format!(
            "{}.new",
            path.file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("projects.json")
        );
        let tmp = path.with_file_name(tmp_name);
        let json = serde_json::to_string_pretty(self).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| format!("rename {} → {}: {e}", tmp.display(), path.display()))?;
        Ok(())
    }

    pub fn entries(&self) -> &[ProjectEntry] {
        &self.entries
    }

    pub fn find(&self, corpus_id: &str) -> Option<&ProjectEntry> {
        self.entries.iter().find(|e| e.corpus_id == corpus_id)
    }

    /// Upsert. If `corpus_id` already exists, the entry is replaced
    /// (preserving `registered_at` so we don't reset the timestamp
    /// on a re-register). Returns `true` iff this was a new entry.
    pub fn upsert(&mut self, mut entry: ProjectEntry) -> bool {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|e| e.corpus_id == entry.corpus_id)
        {
            entry.registered_at = existing.registered_at.clone();
            *existing = entry;
            false
        } else {
            self.entries.push(entry);
            true
        }
    }

    /// Remove by corpus_id. Returns the removed entry, or `None`
    /// if it wasn't registered.
    pub fn remove(&mut self, corpus_id: &str) -> Option<ProjectEntry> {
        let idx = self.entries.iter().position(|e| e.corpus_id == corpus_id)?;
        Some(self.entries.remove(idx))
    }

    /// Find an existing entry (under a different `corpus_id`) whose root
    /// nests with `root` — either an ancestor or a descendant of it.
    ///
    /// Nested registrations are how the freshness pipeline collapses:
    /// every save inside the shared subtree dirties all overlapping
    /// projects, each queues its own full-workspace SCIP export on the
    /// global rebuild permit, and on a busy day the queue never drains
    /// (observed 2026-07-23: 4 nested projects, all permanently
    /// `[rebuilding]`, one never built at all). Registration refuses
    /// this shape unless explicitly forced.
    ///
    /// Paths are canonicalized when possible so symlinked spellings of
    /// the same tree still collide; a path that can't be canonicalized
    /// (not yet on disk) is compared as spelled.
    pub fn nested_conflict(&self, corpus_id: &str, root: &Path) -> Option<&ProjectEntry> {
        fn canon(p: &Path) -> PathBuf {
            p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
        }
        let new_root = canon(root);
        self.entries.iter().find(|e| {
            if e.corpus_id == corpus_id {
                return false; // re-registering yourself is an update, not a conflict
            }
            let existing = canon(&e.root);
            new_root.starts_with(&existing) || existing.starts_with(&new_root)
        })
    }
}
