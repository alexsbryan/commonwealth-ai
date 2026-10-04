// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-project state machine for the daemon's reindexer + watchers.
//!
//! `ProjectState` holds the live, read-mostly state that the daemon's
//! `supervised_task::supervise` writes and MCP tools / HTTP endpoints read.
//! One instance per registered project.
//!
//! This module intentionally carries no IO (registry load/save,
//! HTTP) — those live alongside in `project_http.rs` and a follow-up
//! `registry.rs`. Keeping the state machine pure makes the panic
//! boundary around supervised tasks easier to reason about: a bad
//! `ProjectState.set()` cannot reach the filesystem or the network.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::RwLock;

// The on-disk schema and the two status enums live in the contracts leaf
// (fp-2, §12 decision 3) and are re-exported here at their historical
// `projects::` paths. [`ProjectState`] stays — it is live machinery.
pub use sovereign_contracts::watcher_projects::{
    ProjectEntry, Registry, WatcherKind, WatcherStatus, WatcherToggles,
};

/// Shared state for one registered project. All supervised tasks
/// hold an `Arc<ProjectState>`; the daemon's HTTP layer and MCP
/// tool layer hold read handles.
pub struct ProjectState {
    pub corpus_id: String,
    watchers: RwLock<HashMap<WatcherKind, WatcherStatus>>,
    /// Unix seconds of the last successful SCIP rebuild. Read by
    /// the lazy-rebuild signal: when the age exceeds the threshold
    /// and a tool call comes in, the tool nudges a rebuild.
    graph_updated_at: AtomicU64,
    /// Single-writer guard for the rebuild queue. Callers do a
    /// CAS; only the winning thread runs the rebuild body, others
    /// set `rebuild_dirty` so the current run knows to loop.
    rebuild_in_flight: AtomicBool,
    /// Set while a rebuild is running to indicate "another request
    /// came in; please do one more pass after this one". The
    /// worker loop reads + clears this on each cycle.
    rebuild_dirty: AtomicBool,
    /// Consecutive FAILED rebuilds (coalescing lock contention excluded).
    /// Reset to 0 on the first success. Exists because a rebuild failing
    /// identically every poll cycle was invisible outside the daemon log
    /// (live incident 2026-08-06) — this feeds `/v1/projects` and doctor.
    rebuild_failures: AtomicU64,
    /// The most recent failure `(error, unix_secs)`, cleared on success.
    last_rebuild_error: RwLock<Option<(String, u64)>>,
}

impl ProjectState {
    pub fn new(corpus_id: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            corpus_id: corpus_id.into(),
            watchers: RwLock::new(HashMap::new()),
            graph_updated_at: AtomicU64::new(0),
            rebuild_in_flight: AtomicBool::new(false),
            rebuild_dirty: AtomicBool::new(false),
            rebuild_failures: AtomicU64::new(0),
            last_rebuild_error: RwLock::new(None),
        })
    }

    pub async fn set(&self, kind: WatcherKind, status: WatcherStatus) {
        self.watchers.write().await.insert(kind, status);
    }

    pub async fn status(&self, kind: WatcherKind) -> WatcherStatus {
        self.watchers
            .read()
            .await
            .get(&kind)
            .cloned()
            .unwrap_or(WatcherStatus::Pending)
    }

    /// Read-only snapshot suitable for serializing into the
    /// `/v1/projects` response. Cheap — one `RwLock` read and a
    /// `HashMap` clone.
    pub async fn snapshot(&self) -> HashMap<WatcherKind, WatcherStatus> {
        self.watchers.read().await.clone()
    }

    /// Record a FAILED rebuild. Returns the new consecutive-failure count
    /// so the caller can throttle its logging on it.
    pub async fn record_rebuild_failure(&self, error: &str) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        *self.last_rebuild_error.write().await = Some((error.to_string(), now));
        self.rebuild_failures.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Clear failure state on a successful rebuild.
    pub async fn record_rebuild_success(&self) {
        self.rebuild_failures.store(0, Ordering::SeqCst);
        *self.last_rebuild_error.write().await = None;
    }

    /// Consecutive failed rebuilds since the last success (0 = healthy).
    pub fn rebuild_failure_count(&self) -> u64 {
        self.rebuild_failures.load(Ordering::SeqCst)
    }

    /// The most recent rebuild failure `(error, unix_secs)`, if the latest
    /// outcome was a failure.
    pub async fn last_rebuild_error(&self) -> Option<(String, u64)> {
        self.last_rebuild_error.read().await.clone()
    }

    /// Record a successful graph rebuild timestamp. The reindexer
    /// calls this after atomically swapping in a new graph.
    pub fn mark_graph_updated(&self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.graph_updated_at.store(now, Ordering::SeqCst);
    }

    /// Age of the current graph in seconds, or `None` if the graph
    /// has never been rebuilt in this daemon session.
    pub fn graph_age_secs(&self) -> Option<u64> {
        let then = self.graph_updated_at.load(Ordering::SeqCst);
        if then == 0 {
            return None;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Some(now.saturating_sub(then))
    }

    /// Claim the rebuild-in-flight slot. Returns `true` for the
    /// winning caller; losers should set `dirty` instead. Pairs
    /// with [`end_rebuild`].
    pub fn begin_rebuild(&self) -> bool {
        self.rebuild_in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Release the rebuild slot and return whether another request
    /// arrived during the rebuild. When `true`, the worker should
    /// loop back immediately for a second pass.
    pub fn end_rebuild(&self) -> bool {
        let dirty = self.rebuild_dirty.swap(false, Ordering::SeqCst);
        self.rebuild_in_flight.store(false, Ordering::SeqCst);
        dirty
    }

    /// Panic-safety release: clear the rebuild-in-flight claim
    /// WITHOUT touching the dirty bit. Called from the Reindexer's
    /// RAII guard when a rebuild task dies without running
    /// `end_rebuild` (panic, watchdog abort). Without this the claim
    /// sticks and every later signal coalesces into a silent no-op —
    /// the live wedge of 2026-08-14, where the project read "active"
    /// for hours with zero failures recorded.
    pub fn force_clear_rebuild(&self) {
        self.rebuild_in_flight.store(false, Ordering::SeqCst);
    }

    /// Mark the queue dirty. Always safe to call; no-op if no
    /// rebuild is in flight (the worker will read it after the
    /// current cycle, or a subsequent request will win the slot).
    pub fn mark_dirty(&self) {
        self.rebuild_dirty.store(true, Ordering::SeqCst);
    }

    pub fn is_rebuild_in_flight(&self) -> bool {
        self.rebuild_in_flight.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn entry(corpus_id: &str, root: &str) -> ProjectEntry {
        ProjectEntry {
            corpus_id: corpus_id.into(),
            root: PathBuf::from(root),
            registered_at: "2026-01-01T00:00:00Z".into(),
            watchers: WatcherToggles::default(),
        }
    }

    #[test]
    fn nested_conflict_flags_descendant_and_ancestor() {
        let mut r = Registry::default();
        r.upsert(entry("parent", "/repo/monorepo"));

        // Descendant of an existing root collides.
        let c = r
            .nested_conflict("child", Path::new("/repo/monorepo/sub"))
            .expect("descendant should conflict");
        assert_eq!(c.corpus_id, "parent");

        // Ancestor of an existing root collides too.
        let c = r
            .nested_conflict("grandparent", Path::new("/repo"))
            .expect("ancestor should conflict");
        assert_eq!(c.corpus_id, "parent");

        // Identical root under a different name is nested (equal paths
        // start_with each other).
        assert!(r
            .nested_conflict("alias", Path::new("/repo/monorepo"))
            .is_some());
    }

    #[test]
    fn nested_conflict_allows_siblings_and_self_update() {
        let mut r = Registry::default();
        r.upsert(entry("a", "/repo/a"));

        // Disjoint sibling is fine.
        assert!(r.nested_conflict("b", Path::new("/repo/b")).is_none());
        // Sibling whose name shares a prefix is NOT nested — the
        // comparison is component-wise, not string-prefix.
        assert!(r
            .nested_conflict("a2", Path::new("/repo/a-extras"))
            .is_none());
        // Re-registering the same corpus_id at the same root is an
        // update, never a conflict.
        assert!(r.nested_conflict("a", Path::new("/repo/a")).is_none());
    }

    #[tokio::test]
    async fn status_defaults_to_pending_for_unknown_kinds() {
        let s = ProjectState::new("test");
        assert_eq!(s.status(WatcherKind::Scip).await, WatcherStatus::Pending);
    }

    #[tokio::test]
    async fn set_overwrites_status() {
        let s = ProjectState::new("test");
        s.set(WatcherKind::Test, WatcherStatus::Idle).await;
        s.set(
            WatcherKind::Test,
            WatcherStatus::Crashed {
                reason: "boom".into(),
                count: 1,
            },
        )
        .await;
        match s.status(WatcherKind::Test).await {
            WatcherStatus::Crashed { count, .. } => assert_eq!(count, 1),
            other => panic!("expected Crashed, got {other:?}"),
        }
    }

    #[test]
    fn begin_rebuild_is_exclusive_and_releases_cleanly() {
        let s = ProjectState::new("test");
        assert!(s.begin_rebuild());
        // Second attempt fails — slot is held.
        assert!(!s.begin_rebuild());

        // Request arrives during rebuild.
        s.mark_dirty();
        assert!(s.end_rebuild(), "dirty bit must be visible to the worker");

        // After release, a new claim succeeds and dirty is clear.
        assert!(s.begin_rebuild());
        assert!(!s.end_rebuild(), "dirty should have been consumed");
    }

    #[test]
    fn graph_age_none_before_first_update_then_seconds_after() {
        let s = ProjectState::new("test");
        assert_eq!(s.graph_age_secs(), None);
        s.mark_graph_updated();
        let age = s.graph_age_secs().expect("age must be Some after update");
        assert!(age <= 1, "age within 1 second of now");
    }

    fn sample_entry(id: &str) -> ProjectEntry {
        ProjectEntry {
            corpus_id: id.into(),
            root: PathBuf::from(format!("/tmp/{id}")),
            registered_at: "2026-04-17T00:00:00Z".into(),
            watchers: WatcherToggles::default(),
        }
    }

    #[test]
    fn registry_load_from_missing_file_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("projects.json");
        let reg = Registry::load_from(&path).unwrap();
        assert_eq!(reg.entries().len(), 0);
    }

    #[test]
    fn registry_upsert_add_then_replace_preserves_registered_at() {
        let mut reg = Registry::default();
        let mut entry = sample_entry("alpha");
        entry.registered_at = "2026-01-01T00:00:00Z".into();
        assert!(reg.upsert(entry));

        let mut replacement = sample_entry("alpha");
        replacement.registered_at = "9999-12-31T00:00:00Z".into();
        replacement.root = PathBuf::from("/new/path");
        assert!(!reg.upsert(replacement), "second upsert is not a new entry");

        let found = reg.find("alpha").unwrap();
        assert_eq!(found.root, PathBuf::from("/new/path"));
        // registered_at must stick with the original — re-registering
        // a project shouldn't reset its creation timestamp.
        assert_eq!(found.registered_at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn registry_remove_returns_entry_and_leaves_others_alone() {
        let mut reg = Registry::default();
        reg.upsert(sample_entry("alpha"));
        reg.upsert(sample_entry("beta"));

        let removed = reg.remove("alpha").unwrap();
        assert_eq!(removed.corpus_id, "alpha");
        assert_eq!(reg.entries().len(), 1);
        assert_eq!(reg.entries()[0].corpus_id, "beta");

        assert!(reg.remove("alpha").is_none(), "second remove is a no-op");
    }

    #[test]
    fn registry_save_and_load_roundtrip_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("projects.json");

        let mut reg = Registry::default();
        reg.upsert(sample_entry("alpha"));
        reg.upsert(sample_entry("beta"));
        reg.save_to(&path).unwrap();
        // Atomic save leaves no `.new` sidecar.
        assert!(!tmp.path().join("projects.json.new").exists());

        let loaded = Registry::load_from(&path).unwrap();
        assert_eq!(loaded.entries().len(), 2);
        assert!(loaded.find("alpha").is_some());
        assert!(loaded.find("beta").is_some());
    }

    #[test]
    fn watcher_toggles_default_enables_everything() {
        let t = WatcherToggles::default();
        assert!(t.scip && t.test && t.lint);
        assert_eq!(t.scip_debounce_ms, 2000);
        assert_eq!(t.git_poll_secs, 30);
    }

    #[test]
    fn registry_deserializes_missing_watchers_with_defaults() {
        // Hand-written JSON without the `watchers` field — simulates
        // a registry written by an older sovereign build.
        let json =
            r#"[{"corpus_id":"old","root":"/tmp/old","registered_at":"2020-01-01T00:00:00Z"}]"#;
        let reg: Registry = serde_json::from_str(json).unwrap();
        let entry = reg.find("old").unwrap();
        assert!(entry.watchers.scip);
        assert_eq!(entry.watchers.git_poll_secs, 30);
    }

    #[test]
    fn watcher_status_serializes_with_state_tag() {
        let s = WatcherStatus::Crashed {
            reason: "panic".into(),
            count: 3,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"state\":\"crashed\""));
        assert!(json.contains("\"reason\":\"panic\""));
        assert!(json.contains("\"count\":3"));
    }
}
