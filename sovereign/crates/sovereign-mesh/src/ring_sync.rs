// SPDX-License-Identifier: AGPL-3.0-or-later
//! Moved to `commonwealth_rails::ring_sync` (phase-b pb-rails-parity); its pub
//! items are re-exported here at their historical paths.
//!
//! What stays is the daemon's half of the seam: [`FabricPart`] is a
//! [`RingSyncHost`] (its journals are the rail port, which dials cw-rails'
//! loopback doors), so the daemon runs the ONE round implementation until the
//! flip turns its copy off.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use commonwealth_rail::{Compaction, Digest, Op, Roster, SignedOp};
use commonwealth_rails::ring_sync::{JournalFut, RingSyncHost, RingSyncJournal, RoundMembers};
use commonwealth_transport::PeerTransport;
use tokio::sync::Notify;

pub use commonwealth_rails::ring_sync::{
    exchange, ExchangeOutcome, ExchangeStop, RingSyncHandle, RoundOutcome,
    DEFAULT_RING_SYNC_INTERVAL, MAX_CHUNKS_PER_EXCHANGE,
};

use crate::fabric::FabricPart;
use crate::rail_port::{LocalRingRail, RingRailPort};

/// Spawn the daemon's round over `fabric`. See
/// [`commonwealth_rails::ring_sync::spawn_ring_sync_loop`].
pub fn spawn_ring_sync_loop(
    fabric: Arc<FabricPart>,
    interval: Duration,
    nudge: Arc<Notify>,
) -> RingSyncHandle {
    commonwealth_rails::ring_sync::spawn_ring_sync_loop(fabric, interval, nudge)
}

/// One round over `fabric`. See
/// [`commonwealth_rails::ring_sync::run_one_round`].
pub async fn run_one_round(fabric: &FabricPart) -> RoundOutcome {
    commonwealth_rails::ring_sync::run_one_round(fabric).await
}

impl RingSyncHost for FabricPart {
    fn journal(&self) -> Option<Arc<dyn RingSyncJournal>> {
        self.ring_rail()
            .map(|port| Arc::new(PortJournal(port)) as Arc<dyn RingSyncJournal>)
    }

    fn http(&self) -> Result<&reqwest::Client, &str> {
        crate::gossip::gossip_client()
    }

    fn members(&self) -> Pin<Box<dyn Future<Output = Option<RoundMembers>> + Send + '_>> {
        Box::pin(async move {
            let self_id = self.identity.current();
            let now_secs = self.clock().now_unix_secs();
            let mesh = self.mesh.read().await;
            Some(RoundMembers::of(&mesh, self_id, now_secs))
        })
    }

    fn transport(&self) -> Arc<dyn PeerTransport> {
        self.peer_transport()
    }
}

/// The daemon's rail port as the round's journal: every call forwards.
struct PortJournal(Arc<dyn RingRailPort>);

impl RingSyncJournal for PortJournal {
    fn namespaces(&self) -> JournalFut<'_, Vec<String>> {
        self.0.namespaces()
    }
    fn roster(&self, namespace: &str) -> JournalFut<'_, Roster> {
        self.0.roster(namespace)
    }
    fn journal_digest(&self, namespace: &str) -> JournalFut<'_, Digest> {
        self.0.journal_digest(namespace)
    }
    fn journal_ingest_all(&self, namespace: &str, ops: &[Op<SignedOp>]) -> JournalFut<'_, usize> {
        self.0.journal_ingest_all(namespace, ops)
    }
    fn journal_ops_missing_from_within(
        &self,
        namespace: &str,
        theirs: &Digest,
        budget_bytes: usize,
    ) -> JournalFut<'_, (Vec<Op<SignedOp>>, bool)> {
        self.0
            .journal_ops_missing_from_within(namespace, theirs, budget_bytes)
    }
    fn journal_compact(&self, namespace: &str, roster: &Roster) -> JournalFut<'_, Compaction> {
        self.0.journal_compact(namespace, roster)
    }
}

/// A local rail is its inner `RingRail`'s journal — the one cw-rails rounds
/// over — so an [`exchange`] against a `LocalRingRail` reads that impl.
impl RingSyncJournal for LocalRingRail {
    fn namespaces(&self) -> JournalFut<'_, Vec<String>> {
        RingSyncJournal::namespaces(self.inner().as_ref())
    }
    fn roster(&self, namespace: &str) -> JournalFut<'_, Roster> {
        RingSyncJournal::roster(self.inner().as_ref(), namespace)
    }
    fn journal_digest(&self, namespace: &str) -> JournalFut<'_, Digest> {
        self.inner().journal_digest(namespace)
    }
    fn journal_ingest_all(&self, namespace: &str, ops: &[Op<SignedOp>]) -> JournalFut<'_, usize> {
        self.inner().journal_ingest_all(namespace, ops)
    }
    fn journal_ops_missing_from_within(
        &self,
        namespace: &str,
        theirs: &Digest,
        budget_bytes: usize,
    ) -> JournalFut<'_, (Vec<Op<SignedOp>>, bool)> {
        self.inner()
            .journal_ops_missing_from_within(namespace, theirs, budget_bytes)
    }
    fn journal_compact(&self, namespace: &str, roster: &Roster) -> JournalFut<'_, Compaction> {
        self.inner().journal_compact(namespace, roster)
    }
}
