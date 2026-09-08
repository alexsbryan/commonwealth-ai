// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MeshBroadcaster` — the production implementation of
//! [`sovereign_work_atlas::tools::ClaimBroadcaster`].
//!
//! Lives in `sovereign-mesh` (not in `sovereign-work-atlas`) so the
//! trait surface stays free of `AppState`. The dep direction is
//! work-atlas → (nothing extra); mesh → work-atlas. That keeps
//! work-atlas re-usable from contexts where AppState isn't in scope
//! (the CLI tools registry uses `NullBroadcaster` for the same
//! reason).
//!
//! # What "broadcast" means after cw-lift rung 2e
//!
//! It used to mean a POST: `gossip::broadcast_now` read the entry back out of
//! the store and shipped it to every online peer on `/internal/app/state`,
//! fire-and-forget, with the ten-second gossip round as its recovery. Both
//! that push and that round are deleted, and this is no longer a sender at
//! all — the census of senders of replicated state is ONE, and it is
//! `/internal/ring/sync`.
//!
//! The write itself already left: `WorkAtlasStore` wrote it through
//! `MeshStore::set`, which queued it in `rail_outbox` **in the same
//! transaction as the row**. So there is nothing here to send and nothing to
//! lose. What a latency-sensitive writer still needs is for the two hops that
//! carry it — the pump's drain, then the ring round — to happen NOW rather
//! than on their own clocks (2 s and 60 s). That is what this does:
//! [`rail_kv_pump::pump_once`] is the pump's own body, called rather than
//! re-spelled (§10.6), and the nudge afterwards is the one the pump's loop
//! raises in the same place.
//!
//! Concurrent with the pump's own tick this is safe by construction:
//! `RingJournal` holds the writer lock across read-decide-append, so the worst
//! case is one duplicate `Record` at a later `seq` carrying an identical
//! `(key, value, t)`. The fold orders on `(t, actor, id)` and both copies
//! carry the same `t` and actor, so the projected value is the same either
//! way.
//!
//! # Privacy
//!
//! The old guard here refused an excluded `app_id` before reading the entry to
//! POST it. Nothing is read and nothing is POSTed now, and the guard has
//! moved to where it cannot be skipped: `backend::enqueue_on` refuses to queue
//! an excluded namespace inside the store's own write transaction, so a
//! private claim is not in the outbox for this call to drain, whatever it is
//! passed (`commonwealth-state`'s
//! `an_excluded_namespace_never_enters_the_outbox`). The receiving half is
//! `MeshStore::apply_projection`, which returns an `Err` naming the namespace
//! (`apply_projection_refuses_an_excluded_namespace`). The work-atlas tools
//! still gate on `Privacy::Public` before calling here at all.

use async_trait::async_trait;
use commonwealth_api::state::AppState;
use sovereign_work_atlas::tools::ClaimBroadcaster;

use crate::rail_kv_pump;

/// Wraps `AppState` so the work-atlas tools can hurry a claim onto the ring
/// without taking a direct dep on `AppState`. `AppState` is `Clone` over an
/// internal `Arc`, so the stored value is cheap to copy and survives daemon
/// state transitions.
pub struct MeshBroadcaster {
    app_state: AppState,
}

impl MeshBroadcaster {
    pub fn new(app_state: AppState) -> Self {
        Self { app_state }
    }
}

impl std::fmt::Debug for MeshBroadcaster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshBroadcaster").finish_non_exhaustive()
    }
}

#[async_trait]
impl ClaimBroadcaster for MeshBroadcaster {
    /// `app_id` and `key` name the write that is in a hurry; they do not
    /// select what travels. The outbox is drained whole — it is a queue, not
    /// an index — and that is also why a private `app_id` arriving here cannot
    /// leak: its row was never queued.
    async fn broadcast(&self, app_id: &str, key: &str) {
        let out = rail_kv_pump::pump_once(&self.app_state).await;
        if out.appended > 0 {
            self.app_state.ring_write_nudge().notify_one();
        }
        tracing::debug!(
            app_id,
            key,
            appended = out.appended,
            deferred = out.deferred,
            refused = out.refused,
            "work_atlas: claim hurried onto the ring"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::ids::NodeId;
    use commonwealth_rail::{RingRail, SigningKey};
    use std::sync::Arc;
    use std::time::Duration;

    const PUBLIC: &str = sovereign_work_atlas::model::APP_ID_PUBLIC;
    const PRIVATE: &str = sovereign_work_atlas::model::APP_ID_PRIVATE;

    /// A node whose rail derives its roster from its own membership — the same
    /// three calls `ring_sync`'s tests make and the daemon makes.
    fn node(dir: &std::path::Path, key: &SigningKey, id: NodeId) -> AppState {
        use crate::ring_roster::tests::{member, mesh_of, pubkey_of};
        let state = AppState::new(id, mesh_of(vec![member(id, "a", Some(pubkey_of(key)))]));
        let rail = Arc::new(RingRail::new(dir, Arc::new(key.clone())));
        crate::ring_roster::MeshRosterSource::install(&rail, &state).unwrap();
        state.install_ring_rail(rail);
        state
    }

    /// **The whole contract, and it is no longer a POST.** A claim written
    /// through the ordinary store door is on the ring by the time `broadcast`
    /// returns, and the ring-sync loop has been told to run rather than to
    /// wait out its sixty-second tick.
    ///
    /// Watched RED both ways: deleting the `pump_once` call leaves the row in
    /// the outbox (first assertion); deleting the `notify_one` leaves the
    /// nudge unraised and the timeout below elapses (second). Neither failure
    /// is visible in the other's assertion, which is why both are here.
    #[tokio::test]
    async fn a_claim_write_is_pumped_onto_the_ring_and_the_round_is_nudged() {
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[11u8; 32]);
        let id = NodeId::from_u128(0xA11A5);
        let state = node(dir.path(), &key, id);

        assert!(state
            .inner
            .mesh_store
            .set(
                PUBLIC,
                "claim:1",
                bytes::Bytes::from_static(b"{\"intent\":\"x\"}"),
                id
            )
            .unwrap());
        assert_eq!(
            state.inner.mesh_store.outbox_len().unwrap(),
            1,
            "the store queued the write in its own transaction"
        );

        let nudge = state.ring_write_nudge();
        MeshBroadcaster::new(state.clone())
            .broadcast(PUBLIC, "claim:1")
            .await;

        assert_eq!(
            state.inner.mesh_store.outbox_len().unwrap(),
            0,
            "the claim is on the journal, not still queued"
        );
        assert!(state
            .ring_rail()
            .unwrap()
            .namespaces()
            .unwrap()
            .iter()
            .any(|n| n == PUBLIC));
        tokio::time::timeout(Duration::from_millis(200), nudge.notified())
            .await
            .expect("the ring-sync loop must be told to run now, not in 60s");
    }

    /// The control, and the privacy pin: a PRIVATE claim written on the same
    /// node is not in the outbox, so this call has nothing to drain and raises
    /// no nudge — the namespace gets no journal at all. The refusal is the
    /// store's, not this file's, which is the point (§7.1).
    #[tokio::test]
    async fn a_private_claim_is_not_on_the_ring_and_raises_no_round() {
        assert!(
            commonwealth_state::GOSSIP_EXCLUDED_APP_IDS.contains(&PRIVATE),
            "this test is about an excluded namespace"
        );
        let dir = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[12u8; 32]);
        let id = NodeId::from_u128(0xB0B0);
        let state = node(dir.path(), &key, id);

        assert!(state
            .inner
            .mesh_store
            .set(
                PRIVATE,
                "claim:secret",
                bytes::Bytes::from_static(b"{}"),
                id
            )
            .unwrap());
        assert_eq!(state.inner.mesh_store.outbox_len().unwrap(), 0);

        let nudge = state.ring_write_nudge();
        MeshBroadcaster::new(state.clone())
            .broadcast(PRIVATE, "claim:secret")
            .await;

        assert!(
            !state
                .ring_rail()
                .unwrap()
                .namespaces()
                .unwrap()
                .iter()
                .any(|n| n == PRIVATE),
            "a private namespace has no journal"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), nudge.notified())
                .await
                .is_err(),
            "nothing travelled, so no round was asked for"
        );
        assert!(
            state
                .inner
                .mesh_store
                .get(PRIVATE, "claim:secret")
                .unwrap()
                .is_some(),
            "excluded is not the same as unwritten — the row is still here"
        );
    }
}
