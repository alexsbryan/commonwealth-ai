// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's notes-rail convergence recorder, as the `Convergence` port.
//! Moved from `sovereign_mesh::peer_adapter` (pb-mesh-exit-mesh): it names
//! only contracts types, and the daemon is its one constructor.

use std::sync::Arc;

use sovereign_contracts::peer::{Convergence, ConvergenceRecord};

/// [`Convergence`] backed by the [`ConvergenceRecord`] that `/status` reads.
///
/// The record is carried onto `AppState` at construction
/// (`FabricSeed::convergence`), so the writers reached through this port
/// and the `/status` reader are the same instance by construction.
#[derive(Debug, Clone)]
pub struct MeshConvergence {
    inner: Arc<ConvergenceRecord>,
}

impl Default for MeshConvergence {
    fn default() -> Self {
        Self::new()
    }
}

impl MeshConvergence {
    /// A fresh recorder: both paths never-succeeded since boot.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ConvergenceRecord::new()),
        }
    }

    /// The underlying record, for installation onto `AppState`. Public for the
    /// same reason as [`MeshReplicatedKv::inner`]: the composition root in
    /// `sovereign-daemon` is the one caller.
    pub fn inner(&self) -> Arc<ConvergenceRecord> {
        Arc::clone(&self.inner)
    }
}

impl Convergence for MeshConvergence {
    fn record_outbound_publish_success(&self, at_unix: i64) {
        self.inner.record_outbound_publish_success(at_unix);
    }

    fn record_inbound_ingest_success(&self, at_unix: i64) {
        self.inner.record_inbound_ingest_success(at_unix);
    }

    fn snapshot(&self) -> (Option<i64>, Option<i64>) {
        self.inner.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_convergence_reports_absence_until_a_path_runs() {
        let c = MeshConvergence::new();
        assert_eq!(c.snapshot(), (None, None));
        c.record_outbound_publish_success(1_700_000_000);
        assert_eq!(c.snapshot(), (Some(1_700_000_000), None));
        c.record_inbound_ingest_success(1_700_000_042);
        assert_eq!(c.snapshot(), (Some(1_700_000_000), Some(1_700_000_042)));
    }

    /// The constructed record and the port write to the same place — the
    /// property `FabricSeed::convergence`'s "ONE instance" comment
    /// claims and nothing asserted.
    #[test]
    fn mesh_convergence_port_and_installed_record_are_one_instance() {
        let c = MeshConvergence::new();
        let installed = c.inner();
        c.record_outbound_publish_success(99);
        assert_eq!(installed.snapshot(), (Some(99), None));
        installed.record_inbound_ingest_success(100);
        assert_eq!(c.snapshot(), (Some(99), Some(100)));
    }
}
