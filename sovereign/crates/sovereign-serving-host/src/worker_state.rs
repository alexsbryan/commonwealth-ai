// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pinned-worker state reader.
//!
//! The host's pinned path needs the bootstrap facts a pod's TLS handle is
//! built from. The pod set and the snapshot directory are the daemon's, and
//! `sovereign-pods` (Compute) is neither a package member nor a shared leaf —
//! a third `[[exception]]` on `sovereign-serving-host` is the kill clause
//! (`sovereign/SERVING_BOUNDARY.md` "Grandfathered") — so the state arrives
//! through this port, which the daemon implements.
//!
//! The wire vocabulary itself is the shared leaf's
//! (`sovereign_contracts::worker_pod`), so the port's payload is a type both
//! sides already speak; only the *reader* crosses here. `WorkerState` is the
//! reader; the daemon answers it from the pods it registered
//! (`PinnedWorkerEndpointSource`) and the snapshots it loaded.
//!
//! Absence is reported, never defaulted: an unregistered node id is `None`,
//! not an empty blob. The default implementation answers nothing.

use kernel_types::NodeId;
use sovereign_contracts::worker_pod::BootstrapBlob;

/// Read a registered pinned worker pod's bootstrap state.
pub trait WorkerState: Send + Sync + std::fmt::Debug {
    /// The bootstrap blob for a pinned pod, by synthetic node id. `None` when
    /// no pod is registered under that id.
    fn bootstrap_for(&self, node_id: &NodeId) -> Option<BootstrapBlob>;
}

/// The default reader: this host holds no pinned workers.
#[derive(Debug, Default)]
pub struct NoWorkers;

impl WorkerState for NoWorkers {
    fn bootstrap_for(&self, _node_id: &NodeId) -> Option<BootstrapBlob> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// The in-memory reader a daemon or CLI supplies.
    #[derive(Debug, Default)]
    struct StubWorkers(HashMap<NodeId, BootstrapBlob>);

    impl WorkerState for StubWorkers {
        fn bootstrap_for(&self, node_id: &NodeId) -> Option<BootstrapBlob> {
            self.0.get(node_id).cloned()
        }
    }

    fn blob(job: &str) -> BootstrapBlob {
        BootstrapBlob {
            version: sovereign_contracts::worker_pod::BOOTSTRAP_VERSION,
            job_id: job.into(),
            seed: [7u8; 32],
            owner_verifying_key: [9u8; 32],
            worker_token: "tok".into(),
            expected_uploads: Default::default(),
            expires_unix: u64::MAX,
        }
    }

    /// Positive control: a registered pod resolves its bootstrap state.
    #[test]
    fn a_registered_pod_resolves_its_state() {
        let id = NodeId::from_u128(42);
        let mut workers = StubWorkers::default();
        workers.0.insert(id, blob("sep-job"));
        assert_eq!(
            workers.bootstrap_for(&id).map(|b| b.job_id),
            Some("sep-job".to_string())
        );
    }

    /// Negative control: an unregistered id is absent, never defaulted — and
    /// the default reader answers nothing at all.
    #[test]
    fn an_unregistered_pod_is_absent() {
        let workers = StubWorkers::default();
        assert!(workers.bootstrap_for(&NodeId::from_u128(1)).is_none());
        assert!(NoWorkers.bootstrap_for(&NodeId::from_u128(1)).is_none());
    }
}
