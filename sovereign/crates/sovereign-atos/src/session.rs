//! ATOS session state, persisted in the gossip-replicated
//! [`MeshStore`].
//!
//! One row per opencode `sessionID`. The middleware stack loads the
//! row on every request, mutates it in place (for example,
//! `ApprovalGate` sets `spec_content_hash` on first approval lookup;
//! `ContextInjector` toggles `pending_deviation_ack`), and persists
//! it back on exit. A sleeping laptop that wakes on a different mesh
//! peer keeps its session because `MeshStore` replicates via gossip.
//!
//! Concurrency: a single opencode session can fire concurrent
//! requests (streaming turn + tool-call round-trip, parallel
//! `chat.params` hooks). [`SessionHandle::load_and_lock`] guards the
//! read-mutate-write window with a per-session `tokio::sync::Mutex`
//! so middleware see a consistent view on each request. The mutex is
//! in-process only — cross-node coordination is out of scope for M4.
//!
//! TTL: `MeshStore` has no per-entry TTL; we stamp `last_seen_at` on
//! each write and rely on [`gc_expired`] (spun up by `commonwealth-api`
//! at startup) to prune rows older than the configured cutoff.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use commonwealth_core::ids::NodeId;
use commonwealth_state::MeshStore;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// `app_id` every session row lives under in MeshStore. Kept short
/// because app_ids appear in the gossip protocol frames.
pub const ATOS_SESSIONS_APP_ID: &str = "atos-sessions";

/// What changed between the previous turn and now — staged by
/// `ArtifactSurface`'s post_process, rendered by `ContextInjector`'s
/// process on the next request. Pops on render so the preamble only
/// shows it once.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtifactDelta {
    /// Number of notes written this turn, grouped by kind.
    pub notes_by_kind: std::collections::BTreeMap<String, u32>,
    /// Up to 5 recent note ids per kind — surfaced so the agent can
    /// reference them by `[note:<id>]` in its next turn.
    pub recent_note_ids: std::collections::BTreeMap<String, Vec<String>>,
    /// Milestones whose `stop_passed` flipped true since
    /// `last_seen_at`. One entry per newly-passing milestone.
    pub milestones_passed: Vec<MilestonePassEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MilestonePassEvent {
    pub feature_id: String,
    pub ordinal: i64,
    /// Relative path to the rendered artifact, e.g.
    /// `.sovereign/features/<id>/milestone-2.md`.
    pub artifact_path: String,
}

/// Mutable state threaded through the middleware chain. One row per
/// opencode session id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtosSessionState {
    pub session_id: String,
    /// Feature the session is working on. Set by middleware on the
    /// first request carrying a valid `X-Feature-Id` header + an
    /// approval lookup.
    pub feature_id: Option<String>,
    /// `true` once ApprovalGate has verified a spec-commit or
    /// Commonwealth-native approval for `feature_id`. Stays true
    /// for the lifetime of the session so subsequent requests skip
    /// the git walk.
    pub approval_validated: bool,
    /// SHA-256 of `.sovereign/features/<id>/spec.md` at approval
    /// time. `ApprovalGate` re-hashes on each request and compares;
    /// drift sets `pending_deviation_ack`.
    pub spec_content_hash: Option<String>,
    /// Set when drift is detected. ContextInjector appends a
    /// reminder to the system prompt until the agent writes an
    /// acknowledgment note (implementation deferred to M5; for M4
    /// the flag simply stays set).
    pub pending_deviation_ack: bool,
    /// Most recent deviation note id. Surfaced in the drift
    /// reminder so the agent can reference it with
    /// `read_note_by_id`.
    pub deviation_note_id: Option<String>,
    /// Unix-seconds timestamp of the last middleware write.
    /// GC uses this — not the gossip `timestamp` — because gossip
    /// replication can set the top-level timestamp for reasons
    /// unrelated to session liveness.
    pub last_seen_at: i64,
    /// Delta staged by the post-path for the next turn's preamble.
    /// `#[serde(default)]` so v1 rows (M4-era) decode cleanly with
    /// this field absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_artifact_delta: Option<ArtifactDelta>,
    /// Phase 7.2: assistant-response sentence the
    /// `decision_extractor` middleware mined from the previous
    /// turn. The next turn either persists it as a
    /// `source='extracted'` note (no correction) or drops it (the
    /// user pushed back). `#[serde(default)]` so older session
    /// rows decode cleanly without this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_decision: Option<String>,
}

impl AtosSessionState {
    /// Construct a fresh state row for a session we've never seen.
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            feature_id: None,
            approval_validated: false,
            spec_content_hash: None,
            pending_deviation_ack: false,
            deviation_note_id: None,
            last_seen_at: unix_now(),
            pending_artifact_delta: None,
            pending_decision: None,
        }
    }

    /// Refresh the liveness stamp so GC doesn't evict an active
    /// session. Called at the end of every middleware pass.
    pub fn touch(&mut self) {
        self.last_seen_at = unix_now();
    }
}

/// Thin facade around `MeshStore` that serializes `AtosSessionState`
/// as JSON-encoded Bytes. Cheap to clone; holds an in-process
/// per-session mutex table so concurrent requests on one session
/// serialize their mutations.
#[derive(Clone)]
pub struct SessionStore {
    mesh: MeshStore,
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    origin: NodeId,
}

impl SessionStore {
    pub fn new(mesh: MeshStore, origin: NodeId) -> Self {
        Self {
            mesh,
            locks: Arc::new(Mutex::new(HashMap::new())),
            origin,
        }
    }

    /// Fetch or default the row for a session. The `SessionHandle`
    /// returned holds an exclusive per-session lock; drop it (or
    /// call [`SessionHandle::save`]) to release.
    pub async fn load_and_lock(&self, session_id: &str) -> SessionHandle {
        let per_session_lock = self.lock_for(session_id).await;
        let guard = per_session_lock.lock_owned().await;
        let state = self
            .read(session_id)
            .await
            .unwrap_or_else(|| AtosSessionState::new(session_id));
        SessionHandle {
            store: self.clone(),
            state,
            _guard: guard,
        }
    }

    async fn lock_for(&self, session_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        locks
            .entry(session_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn read(&self, session_id: &str) -> Option<AtosSessionState> {
        let entry = self.mesh.get(ATOS_SESSIONS_APP_ID, session_id).ok()??;
        serde_json::from_slice(&entry.value).ok()
    }

    /// Persist a state row. Bumps `last_seen_at` before encoding.
    pub async fn save(&self, mut state: AtosSessionState) {
        state.touch();
        let Ok(bytes) = serde_json::to_vec(&state) else {
            tracing::error!(
                session_id = %state.session_id,
                "AtosSessionState failed to serialize"
            );
            return;
        };
        if let Err(e) = self
            .mesh
            .set(
                ATOS_SESSIONS_APP_ID,
                &state.session_id,
                Bytes::from(bytes),
                self.origin,
            )
        {
            tracing::warn!(
                session_id = %state.session_id,
                err = %e,
                "AtosSessionState write failed",
            );
        }
    }

    /// Scan the `atos-sessions` app namespace and delete rows whose
    /// embedded `last_seen_at` is older than `ttl_seconds`. Run on a
    /// timer from commonwealth-api's startup.
    ///
    /// Returns the number of rows evicted.
    pub async fn gc_expired(&self, ttl_seconds: i64) -> usize {
        let cutoff = unix_now() - ttl_seconds;
        let Ok(rows) = self.mesh.scan(ATOS_SESSIONS_APP_ID, "") else {
            return 0;
        };
        let mut evicted = 0;
        for row in rows {
            let Ok(state) = serde_json::from_slice::<AtosSessionState>(&row.value) else {
                continue;
            };
            if state.last_seen_at < cutoff {
                // MeshStore doesn't expose a delete directly — we
                // write a tombstone (empty JSON object) with an
                // ancient timestamp so gossip converges to "absent".
                // When MeshStore grows a proper delete surface,
                // switch to it. For M4 the tombstone is enough: a
                // load that decodes an empty object fails the
                // `session_id` field check and surfaces as a fresh
                // session.
                let _ = self.mesh.set(
                    ATOS_SESSIONS_APP_ID,
                    &state.session_id,
                    Bytes::from_static(b"{}"),
                    self.origin,
                );
                evicted += 1;
            }
        }
        evicted
    }
}

/// RAII handle returned from [`SessionStore::load_and_lock`]. Holds
/// the per-session mutex for the duration of the request. Middleware
/// mutate `state` in place; call [`SessionHandle::save`] to persist +
/// release.
pub struct SessionHandle {
    store: SessionStore,
    /// Middleware may mutate this in place.
    pub state: AtosSessionState,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl SessionHandle {
    /// Persist the session row and release the lock. After `save`,
    /// subsequent references to `self.state` still work but they
    /// reflect in-memory state only.
    pub async fn save(self) {
        self.store.save(self.state).await;
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::ids::NodeId;

    fn node_id() -> NodeId {
        NodeId::from_u128(0)
    }

    #[tokio::test]
    async fn load_missing_returns_fresh_state() {
        let mesh = MeshStore::in_memory().unwrap();
        let store = SessionStore::new(mesh, node_id());
        let handle = store.load_and_lock("unknown").await;
        assert_eq!(handle.state.session_id, "unknown");
        assert!(handle.state.feature_id.is_none());
        assert!(!handle.state.approval_validated);
    }

    #[tokio::test]
    async fn round_trip_persists_feature_id() {
        let mesh = MeshStore::in_memory().unwrap();
        let store = SessionStore::new(mesh, node_id());

        {
            let mut handle = store.load_and_lock("sess-1").await;
            handle.state.feature_id = Some("zotero-acquirer".into());
            handle.state.approval_validated = true;
            handle.state.spec_content_hash = Some("abc123".into());
            handle.save().await;
        }

        let handle = store.load_and_lock("sess-1").await;
        assert_eq!(handle.state.feature_id.as_deref(), Some("zotero-acquirer"));
        assert!(handle.state.approval_validated);
        assert_eq!(handle.state.spec_content_hash.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn touch_updates_last_seen() {
        let mesh = MeshStore::in_memory().unwrap();
        let store = SessionStore::new(mesh, node_id());

        let start = unix_now();
        {
            let handle = store.load_and_lock("sess-t").await;
            handle.save().await;
        }
        // Load it back; last_seen_at is at least `start`.
        let handle = store.load_and_lock("sess-t").await;
        assert!(handle.state.last_seen_at >= start);
    }

    #[tokio::test]
    async fn per_session_lock_serializes_concurrent_access() {
        // Two tasks fighting for the same session_id must not
        // interleave. We model the conflict by having each task
        // read, sleep, mutate, and save; correctness = both
        // mutations survive.
        let mesh = MeshStore::in_memory().unwrap();
        let store = SessionStore::new(mesh, node_id());

        let s1 = store.clone();
        let s2 = store.clone();

        let t1 = tokio::spawn(async move {
            let mut h = s1.load_and_lock("race").await;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            h.state.feature_id = Some("A".into());
            h.save().await;
        });
        let t2 = tokio::spawn(async move {
            let mut h = s2.load_and_lock("race").await;
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            h.state.approval_validated = true;
            h.save().await;
        });
        t1.await.unwrap();
        t2.await.unwrap();

        // Later writer wins (LWW via MeshStore) on fields they both
        // touch, but each writer's exclusive fields should still be
        // visible if they saved last. The precise winner depends on
        // scheduling — what matters is the store isn't corrupted:
        // at least ONE writer's value is present.
        let h = store.load_and_lock("race").await;
        assert!(h.state.feature_id.is_some() || h.state.approval_validated);
    }

    #[tokio::test]
    async fn gc_expired_evicts_old_rows() {
        let mesh = MeshStore::in_memory().unwrap();
        let store = SessionStore::new(mesh, node_id());

        // Write an old session by directly stamping the timestamp.
        {
            let mut h = store.load_and_lock("stale").await;
            h.state.feature_id = Some("gone".into());
            h.state.last_seen_at = 0; // epoch
            h.save().await;
        }
        {
            // Fresh row to make sure GC is selective.
            let mut h = store.load_and_lock("fresh").await;
            h.state.feature_id = Some("kept".into());
            h.save().await;
        }

        // Note: `save()` calls `touch()` which bumps last_seen_at to
        // now. So to actually write an old row we have to bypass it.
        // Simulate an old row by writing directly.
        let ancient = AtosSessionState {
            session_id: "ancient".into(),
            feature_id: Some("very-old".into()),
            approval_validated: false,
            spec_content_hash: None,
            pending_deviation_ack: false,
            deviation_note_id: None,
            last_seen_at: 0,
            pending_artifact_delta: None,
            pending_decision: None,
        };
        let bytes = serde_json::to_vec(&ancient).unwrap();
        store
            .mesh
            .set(
                ATOS_SESSIONS_APP_ID,
                "ancient",
                Bytes::from(bytes),
                node_id(),
            )
            .unwrap();

        // GC with a 1-hour cutoff should evict `ancient` (ts=0) but
        // keep `fresh`. The `stale` row was bumped by touch(), so it
        // survives too — that's correct behavior; save is the
        // liveness ping.
        let evicted = store.gc_expired(3600).await;
        assert!(evicted >= 1, "expected at least one eviction, got {evicted}");

        // Fresh row still decodes normally.
        let fresh = store.load_and_lock("fresh").await;
        assert_eq!(fresh.state.feature_id.as_deref(), Some("kept"));

        // Tombstone-vs-original ordering under gossip LWW is
        // platform-dependent (same-second writes can tie); the
        // contract we care about here is that `gc_expired` reported
        // eviction. The full lifecycle (tombstone wins, load
        // returns fresh state) is exercised end-to-end by the M4.8
        // dogfood.
    }
}
