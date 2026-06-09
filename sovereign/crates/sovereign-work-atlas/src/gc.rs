// SPDX-License-Identifier: AGPL-3.0-or-later
//! TTL eviction for the work atlas.
//!
//! MeshStore's built-in `gc(ttl_seconds)` is app-wide and keyed on
//! entry timestamp — not on the per-record `ttl_expires_at` /
//! `last_activity_at` that the work atlas uses. This module scans
//! every 60s and drops anything past its deadline. Cheap: a handful
//! of records per node.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use uuid::Uuid;

use crate::config::WorkAtlasConfig;
use crate::model::Privacy;
use crate::store::WorkAtlasStore;

/// Sweep interval. Short enough to feel snappy (claims drop on time);
/// long enough that the scan cost is invisible.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct WorkAtlasGc {
    store: Arc<WorkAtlasStore>,
    config: WorkAtlasConfig,
}

impl WorkAtlasGc {
    pub fn new(store: Arc<WorkAtlasStore>, config: WorkAtlasConfig) -> Self {
        Self { store, config }
    }

    /// Spawn the sweep task. Returns a `JoinHandle`; aborting it
    /// stops the task on the next tick. The daemon's `serve` boot
    /// holds this handle for the lifetime of the process.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            tracing::info!(
                interval_secs = SWEEP_INTERVAL.as_secs(),
                idle_timeout_secs = self.config.sessions.idle_timeout_seconds,
                "work_atlas:gc loop started"
            );
            loop {
                tokio::time::sleep(SWEEP_INTERVAL).await;
                if let Err(e) = self.sweep_once().await {
                    tracing::warn!(error = %e, "work_atlas:gc sweep failed");
                }
            }
        })
    }

    /// One sweep pass — exposed for tests so they don't have to wait
    /// on the timer.
    pub async fn sweep_once(&self) -> Result<SweepReport, crate::store::WorkAtlasError> {
        let now = now_secs();
        let mut report = SweepReport::default();

        // 1. Drop expired claims, both namespaces.
        for privacy in [Privacy::Public, Privacy::Private] {
            for claim in self.store.scan_claims(privacy)? {
                if claim.ttl_expires_at < now && self.store.release_claim(claim.claim_id)? {
                    tracing::info!(
                        claim_id = %claim.claim_id,
                        session_id = %claim.session_id,
                        "work_atlas:claim_evicted_ttl"
                    );
                    report.claims_evicted += 1;
                }
            }
        }

        // 2. Drop idle sessions. Cascade-delete their claims AND
        //    their observations — the spec's point-in-time invariant
        //    says all of a dropped Session's records disappear with it.
        let idle_cutoff = now.saturating_sub(self.config.sessions.idle_timeout_seconds);
        for session in self.store.scan_sessions()? {
            if session.last_activity_at >= idle_cutoff {
                continue;
            }
            // Cascade: remove any claim still attributed to this session.
            for privacy in [Privacy::Public, Privacy::Private] {
                let cascade_ids: Vec<Uuid> = self
                    .store
                    .scan_claims(privacy)?
                    .into_iter()
                    .filter(|c| c.session_id == session.session_id)
                    .map(|c| c.claim_id)
                    .collect();
                for id in cascade_ids {
                    if self.store.release_claim(id)? {
                        report.claims_cascade_evicted += 1;
                    }
                }
            }
            // Cascade: same for observations. Iterate paths because
            // observation keys carry file_path; delete each.
            for privacy in [Privacy::Public, Privacy::Private] {
                let cascade_paths: Vec<std::path::PathBuf> = self
                    .store
                    .scan_observations(privacy)?
                    .into_iter()
                    .filter(|o| o.session_id == session.session_id)
                    .map(|o| o.file_path)
                    .collect();
                for path in cascade_paths {
                    if self
                        .store
                        .delete_observation(privacy, session.session_id, &path)?
                    {
                        report.observations_cascade_evicted += 1;
                    }
                }
            }
            if self
                .store
                .delete_session(session.session_id, session.privacy)?
            {
                tracing::info!(
                    session_id = %session.session_id,
                    idle_secs = now.saturating_sub(session.last_activity_at),
                    "work_atlas:session_evicted_idle"
                );
                report.sessions_evicted += 1;
            }
        }

        Ok(report)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SweepReport {
    pub claims_evicted: usize,
    pub claims_cascade_evicted: usize,
    pub observations_cascade_evicted: usize,
    pub sessions_evicted: usize,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use commonwealth_core::ids::NodeId;
    use commonwealth_state::MeshStore;
    use uuid::Uuid;

    use crate::model::{
        AgentKind, ClaimRecord, ObservationRecord, ObservationSource, SessionRecord,
    };

    use super::*;

    fn mk_store() -> Arc<WorkAtlasStore> {
        let mesh = Arc::new(MeshStore::in_memory().unwrap());
        Arc::new(WorkAtlasStore::new(mesh, NodeId::from_u128(1)))
    }

    fn mk_session(privacy: Privacy, last_activity_at: u64) -> SessionRecord {
        SessionRecord {
            session_id: Uuid::new_v4(),
            node_id: NodeId::from_u128(1),
            agent_kind: AgentKind::Agent,
            agent_session_token: Some("t".into()),
            repo_id: "r".repeat(64),
            repo_root: PathBuf::from("/tmp/x"),
            current_branch: None,
            privacy,
            created_at: 0,
            last_activity_at,
        }
    }

    #[tokio::test]
    async fn evicts_past_ttl_claims() {
        let store = mk_store();
        let session = mk_session(Privacy::Public, now_secs());
        store.put_session(&session).unwrap();
        let claim = ClaimRecord {
            claim_id: Uuid::new_v4(),
            session_id: session.session_id,
            intent: "x".into(),
            symbol_refs: vec![],
            declared_at: 0,
            // Expired one second ago.
            ttl_expires_at: now_secs().saturating_sub(1),
        };
        store.put_claim(Privacy::Public, &claim).unwrap();

        let gc = WorkAtlasGc::new(store.clone(), WorkAtlasConfig::defaults());
        let report = gc.sweep_once().await.unwrap();
        assert_eq!(report.claims_evicted, 1);
        assert!(store.get_claim(claim.claim_id).unwrap().is_none());
    }

    #[tokio::test]
    async fn cascade_evicts_observations_of_idle_session() {
        let store = mk_store();
        let cfg = WorkAtlasConfig::defaults();
        let stale = now_secs().saturating_sub(cfg.sessions.idle_timeout_seconds + 1);
        let session = mk_session(Privacy::Public, stale);
        store.put_session(&session).unwrap();
        let obs = ObservationRecord {
            session_id: session.session_id,
            file_path: PathBuf::from("src/x.rs"),
            source: ObservationSource::CodeWatcherEdit,
            first_observed_at: stale,
            last_observed_at: stale,
            event_count: 3,
            symbol_refs: vec![],
        };
        store.put_observation(Privacy::Public, &obs).unwrap();

        let gc = WorkAtlasGc::new(store.clone(), cfg);
        let report = gc.sweep_once().await.unwrap();
        assert_eq!(report.sessions_evicted, 1);
        assert_eq!(report.observations_cascade_evicted, 1);
        assert!(store
            .get_observation(session.session_id, std::path::Path::new("src/x.rs"))
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn cascade_evicts_claims_of_idle_session() {
        let store = mk_store();
        let cfg = WorkAtlasConfig::defaults();
        // Session is 1s older than the configured idle timeout.
        let stale = now_secs().saturating_sub(cfg.sessions.idle_timeout_seconds + 1);
        let session = mk_session(Privacy::Public, stale);
        store.put_session(&session).unwrap();
        let claim = ClaimRecord {
            claim_id: Uuid::new_v4(),
            session_id: session.session_id,
            intent: "x".into(),
            symbol_refs: vec![],
            declared_at: 0,
            // Not yet TTL-expired, but the parent session is idle.
            ttl_expires_at: u64::MAX,
        };
        store.put_claim(Privacy::Public, &claim).unwrap();

        let gc = WorkAtlasGc::new(store.clone(), cfg);
        let report = gc.sweep_once().await.unwrap();
        assert_eq!(report.sessions_evicted, 1);
        assert_eq!(report.claims_cascade_evicted, 1);
        assert!(store.get_claim(claim.claim_id).unwrap().is_none());
        assert!(store.get_session(session.session_id).unwrap().is_none());
    }
}
