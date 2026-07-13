// SPDX-License-Identifier: AGPL-3.0-or-later
//! `AtlasObserver` — passive sensor that turns CodeWatcher edit
//! events into work-atlas Observations.
//!
//! Registers as a `BackgroundWatcher` alongside the existing test /
//! lint watchers; the `WatcherCoordinator` fans every debounced batch
//! of changed files at this observer, in parallel with the rest. The
//! observer owns its own 30s per-path debounce — spec §4 forbids
//! inheriting `CodeWatcher`'s 800ms re-index debounce, because the
//! atlas signal is "is someone actively here" not "did this file
//! just change."
//!
//! Privacy: every write goes through `WorkAtlasStore::put_observation`,
//! which routes to `work-atlas` or `work-atlas-private` based on the
//! parent session's privacy. The Private namespace is structurally
//! excluded from gossip (`commonwealth-state::GOSSIP_EXCLUDED_APP_IDS`).
//!
//! Cross-mesh demo this enables:
//!   Workstation A edits `corpus-engine/src/engine/ingest.rs`.
//!   Workstation B's `work_in_flight --scope=… --match_mode=file`
//!   returns a `confidence=active` row stamped with A's node_id
//!   within one broadcast round (no 10s gossip wait).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use tokio::sync::Mutex;
use uuid::Uuid;

use corpus_engine_watchers::{BackgroundWatcher, WatcherStatus};

use crate::config::WorkAtlasConfig;
use crate::model::{AgentKind, ObservationRecord, ObservationSource, Privacy, SessionRecord};
use crate::store::{SessionIdentity, WorkAtlasStore};
use crate::tools::broadcast::ClaimBroadcaster;

/// Minimum interval between Observation upserts for the same file
/// path. The 800ms `CodeWatcher` debounce already coalesces editor
/// "save storms"; 30s on top gives a stable signal to peers and
/// bounds gossip volume on a developer in the middle of a save burst.
const PER_PATH_DEBOUNCE_SECS: u64 = 30;

/// Stable agent_session_token used for the ambient Human session
/// driven by CodeWatcher edits. Sharing this token between the
/// observer and the CLI's `sovereign claim` invocations would
/// collapse them into one session — Phase 2 keeps them distinct
/// (the CLI uses `cli:<node>`) so the explicit-vs-passive distinction
/// stays legible to the operator.
fn ambient_session_token(node_id_str: &str, repo_id: &str) -> String {
    let repo_short: String = repo_id.chars().take(12).collect();
    format!("edits:{node_id_str}:{repo_short}")
}

pub struct AtlasObserver {
    store: Arc<WorkAtlasStore>,
    config: WorkAtlasConfig,
    broadcaster: Arc<dyn ClaimBroadcaster>,
    repo_root: PathBuf,
    repo_id: String,
    current_branch: Option<String>,
    /// `(session_id, file_path)` → last_observed_at unix seconds.
    /// Phase 2 keeps one ambient session per workstation+repo so
    /// the session_id half of the key is constant in practice;
    /// keying on both anyway keeps Phase 2b (session segmentation)
    /// from needing schema changes here.
    debounce: Mutex<HashMap<(Uuid, PathBuf), u64>>,
}

impl AtlasObserver {
    /// Build an observer. `repo_id` may be empty when the repo has no
    /// `origin` remote — the observer becomes a no-op in that case
    /// rather than crashing the daemon, mirroring how `declare_scope`
    /// rejects with an actionable error.
    pub fn new(
        store: Arc<WorkAtlasStore>,
        config: WorkAtlasConfig,
        broadcaster: Arc<dyn ClaimBroadcaster>,
        repo_root: PathBuf,
        repo_id: String,
        current_branch: Option<String>,
    ) -> Self {
        Self {
            store,
            config,
            broadcaster,
            repo_root,
            repo_id,
            current_branch,
            debounce: Mutex::new(HashMap::new()),
        }
    }

    fn enabled(&self) -> bool {
        !self.repo_id.is_empty()
    }

    /// Ensure the ambient Human session exists and bump its
    /// `last_activity_at`. Returns the session for the caller to
    /// stamp Observation parentage. `None` when atlas is disabled
    /// (e.g. repo with no origin remote).
    async fn touch_ambient_session(&self) -> Option<SessionRecord> {
        if !self.enabled() {
            return None;
        }
        let node_id = self.store.node_id();
        let node_str = node_id.to_string();
        let token = ambient_session_token(&node_str, &self.repo_id);
        let identity = SessionIdentity {
            node_id,
            agent_session_token: Some(token),
            repo_id: self.repo_id.clone(),
        };
        match self.store.ensure_session(
            identity,
            self.config.node.default_privacy_enum(),
            AgentKind::Human,
            self.repo_root.clone(),
            self.current_branch.clone(),
        ) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(error = %e, "work_atlas:observer ambient session failed");
                None
            }
        }
    }

    /// Run one batch of debounce-filtered upserts. Public for the
    /// unit tests; production callers go through `on_files_changed`.
    pub async fn process(&self, paths: Vec<PathBuf>) {
        let Some(session) = self.touch_ambient_session().await else {
            return;
        };
        let now = now_secs();
        let mut to_broadcast: Vec<PathBuf> = Vec::new();
        {
            let mut deb = self.debounce.lock().await;
            for path in &paths {
                let key = (session.session_id, path.clone());
                let last = deb.get(&key).copied();
                let should_write = match last {
                    None => true,
                    Some(t) => now.saturating_sub(t) >= PER_PATH_DEBOUNCE_SECS,
                };
                if should_write {
                    deb.insert(key, now);
                    to_broadcast.push(path.clone());
                }
            }
        }

        if to_broadcast.is_empty() {
            return;
        }

        for path in to_broadcast {
            let (first_observed_at, event_count) =
                match self.store.get_observation(session.session_id, &path) {
                    Ok(Some((_, prior))) => (prior.first_observed_at, prior.event_count + 1),
                    _ => (now, 1),
                };
            let rec = ObservationRecord {
                session_id: session.session_id,
                file_path: path.clone(),
                source: ObservationSource::CodeWatcherEdit,
                first_observed_at,
                last_observed_at: now,
                event_count,
                symbol_refs: vec![],
            };
            if let Err(e) = self.store.put_observation(session.privacy, &rec) {
                tracing::warn!(
                    error = %e,
                    path = %path.display(),
                    "work_atlas:observer put_observation failed"
                );
                continue;
            }
            tracing::debug!(
                session_id = %session.session_id,
                path = %path.display(),
                event_count,
                "work_atlas:observation_recorded"
            );

            // Immediate fan-out so peers see the signal within the
            // round-trip rather than the next 10s gossip round.
            // Private observations skip this — `broadcast_now` would
            // refuse anyway, and the namespace is gossip-excluded.
            if session.privacy == Privacy::Public {
                let key = WorkAtlasStore::observation_key(session.session_id, &path);
                self.broadcaster
                    .broadcast(Privacy::Public.app_id(), &key)
                    .await;
            }
        }
    }
}

#[async_trait]
impl BackgroundWatcher for AtlasObserver {
    fn id(&self) -> &'static str {
        "work-atlas-observer"
    }

    fn description(&self) -> &'static str {
        "Synthesize work-atlas Observations from CodeWatcher edit events"
    }

    async fn on_files_changed(&self, paths: Vec<PathBuf>) {
        // The trait contract: return quickly. `process` itself is
        // O(paths) cheap MeshStore writes — well under the
        // coordinator's per-watcher budget — so we run it inline
        // instead of spawning. If put_observation or broadcast
        // latency ever grows, switch to `tokio::spawn` with an
        // `Arc<Self>` clone obtained from the coordinator.
        self.process(paths).await;
    }

    async fn current_status(&self) -> WatcherStatus {
        // The observer doesn't have a "run" model — it's a sink, not
        // a periodic runner. Report Fresh-passing whenever it's
        // enabled, NeverRun otherwise, so `WatcherCoordinator::status`
        // surfaces a sensible line.
        if self.enabled() {
            WatcherStatus::Fresh {
                pass: true,
                last_run_at: SystemTime::now(),
            }
        } else {
            WatcherStatus::Unconfigured
        }
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use commonwealth_core::ids::NodeId;
    use commonwealth_state::MeshStore;

    use crate::tools::broadcast::NullBroadcaster;

    use super::*;

    fn mk_observer(repo_id: &str) -> AtlasObserver {
        let mesh = Arc::new(MeshStore::in_memory().unwrap());
        let store = Arc::new(WorkAtlasStore::new(mesh, NodeId::from_u128(7)));
        AtlasObserver::new(
            store,
            WorkAtlasConfig::defaults(),
            Arc::new(NullBroadcaster),
            PathBuf::from("/tmp/repo"),
            repo_id.into(),
            Some("main".into()),
        )
    }

    #[tokio::test]
    async fn first_edit_creates_observation() {
        let obs = mk_observer(&"r".repeat(64));
        obs.process(vec![PathBuf::from("src/x.rs")]).await;
        let sessions = obs.store.scan_sessions().unwrap();
        assert_eq!(sessions.len(), 1, "ambient session was not created");
        let sid = sessions[0].session_id;
        let rec = obs
            .store
            .get_observation(sid, std::path::Path::new("src/x.rs"))
            .unwrap();
        let (_, rec) = rec.expect("observation missing");
        assert_eq!(rec.event_count, 1);
        assert!(matches!(rec.source, ObservationSource::CodeWatcherEdit));
    }

    #[tokio::test]
    async fn rapid_re_edit_is_debounced() {
        let obs = mk_observer(&"r".repeat(64));
        obs.process(vec![PathBuf::from("src/x.rs")]).await;
        // A second batch within 30s must not bump event_count — the
        // observer's debounce window swallows it.
        obs.process(vec![PathBuf::from("src/x.rs")]).await;
        let sid = obs.store.scan_sessions().unwrap()[0].session_id;
        let (_, rec) = obs
            .store
            .get_observation(sid, std::path::Path::new("src/x.rs"))
            .unwrap()
            .expect("observation missing");
        assert_eq!(rec.event_count, 1, "rapid re-edit broke through debounce");
    }

    #[tokio::test]
    async fn missing_origin_remote_disables_observer() {
        let obs = mk_observer(""); // repo_id empty → atlas disabled
        obs.process(vec![PathBuf::from("src/x.rs")]).await;
        assert!(
            obs.store.scan_sessions().unwrap().is_empty(),
            "observer wrote without an origin remote"
        );
    }

    #[tokio::test]
    async fn private_observation_lands_only_in_private_namespace() {
        let mesh = Arc::new(MeshStore::in_memory().unwrap());
        let store = Arc::new(WorkAtlasStore::new(Arc::clone(&mesh), NodeId::from_u128(7)));
        let mut cfg = WorkAtlasConfig::defaults();
        cfg.node.default_privacy = "private".into();
        let obs = AtlasObserver::new(
            store,
            cfg,
            Arc::new(NullBroadcaster),
            PathBuf::from("/tmp/repo"),
            "r".repeat(64),
            Some("main".into()),
        );
        obs.process(vec![PathBuf::from("src/x.rs")]).await;

        // Public namespace must remain empty.
        let public_hits = mesh.scan("work-atlas", "observation:").unwrap();
        assert!(
            public_hits.is_empty(),
            "private observation leaked to public namespace"
        );
        let private_hits = mesh.scan("work-atlas-private", "observation:").unwrap();
        assert_eq!(private_hits.len(), 1);
    }
}
