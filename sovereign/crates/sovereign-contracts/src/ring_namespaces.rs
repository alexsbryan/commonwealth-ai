// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's own rings, and the one decider over them.
//!
//! Moved from `sovereign_mesh::ring_roster` (phase-b pb-mesh-exit-mesh,
//! phase-b-83 (3)): the daemon and cli-mesh both ask it, and sovereign-mesh
//! cannot name the daemon, so it lives in the contract leaf both link. It is
//! re-exported at its historical path until pb-mesh-dissolve.

/// The daemon's own rings — the namespaces no `roster.json` may narrow,
/// registered with the rail so they outrank the file
/// (`RingRail::default_roster` documents the order).
///
/// A peer's write counts on these rings because its key is in this roster,
/// and the roster heals an unplaceable signer the moment that node advertises
/// a key. A file written by hand — or left behind — on one node would drop
/// peers' writes there and nowhere else, and the ring would stop agreeing
/// across the mesh. Every app ring is answered by the default and may be
/// narrowed. `svrn ring roster` refuses exactly this list, and
/// `a_registered_namespace_ignores_a_hand_roster` pins the reason.
///
/// Every entry is a constant owned by the wire vocabulary of the subsystem
/// that writes the namespace, never a literal repeated here (ARCH §10.6) —
/// that is what makes a rename on the writing side a compile error rather
/// than a namespace that quietly stops replicating.
pub const REGISTERED_NAMESPACES: &[&str] = &[
    // The daemon's own measurements. Not KV-shaped.
    oicp_types::measurements::MEASUREMENTS_APP_ID,
    // The five KV namespaces gossip Step 4 replicated, plus the tracked-article
    // watcher's. Each is a `MeshStore` app_id with a real cross-peer consumer.
    oicp_types::inference_plan::INFERENCE_APP_ID,
    oicp_types::contributions::CONTRIBUTIONS_APP_ID,
    oicp_types::work_queue::PROCESSED_SHARDS_APP_ID,
    crate::peer::NOTES_APP_ID,
    crate::peer::WORK_ATLAS_APP_ID_PUBLIC,
    oicp_types::knowledge::APP_ID_TRACKED,
];

/// Is `namespace` one this daemon writes on its own behalf?
///
/// **THE decider, and the only one.** [`REGISTERED_NAMESPACES`] answers it for
/// every ring whose roster this node derives; `work` is the one namespace that
/// is daemon-written and deliberately NOT on that list, because joining it
/// would flip the work plane's roster to `Derived` and orphan the operator's
/// `rings/work/roster.json`. Two questions, one answer each; this function is
/// where they meet so a caller asking "may a stranger touch this?" does not
/// have to know there are two.
///
/// The one caller today is the guest door: an entry in `[daemon.guest_pages]`
/// naming a namespace this returns `true` for is refused at config load and
/// refused again at the rail route (ARCH 5 — a registry that was wrong must
/// not be the only guard).
pub fn is_daemon_owned(namespace: &str) -> bool {
    REGISTERED_NAMESPACES.contains(&namespace) || namespace == oicp_types::work::WORK_NAMESPACE
}
