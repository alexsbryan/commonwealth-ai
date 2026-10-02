// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's peer routes as an origin in cw-rails' origin table
//! (pb-mesh-exit-transport, the row's inbound half).
//!
//! cw-rails is the node's one mesh endpoint. A member dialing `cwth/http/0`
//! for one of [`PEER_PREFIXES`] reaches this daemon's internal port through
//! cw-rails, which writes the dialer's verified key (`X-Mesh-Pubkey`) and
//! this registration's tie (`kernel_types::member::ORIGIN_TIE_HEADER`) on
//! the request. The tie is how the internal resolver
//! (`crate::internal_principal`) knows the `x-mesh-*` came from cw-rails and
//! not from a local process typing it: [`PeerOriginTie::holds`] is that
//! check, and nothing else reads the tie.
//!
//! The registration and its renewal are the one register/renew loop
//! (`sovereign_turn_client::rails_origins`), which publishes each claim's
//! tie here. Until the daemon reads cw-rails' roster, a key cw-rails
//! forwards resolves against the daemon's own roster, which names none of
//! cw-rails' members, so a forward through this registration is refused as
//! unverified: the registration answers nothing before the flip.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use oicp_types::origin::{Admit, Framing, OriginRegistration};
use sovereign_turn_client::rails_origins::ClaimsSource;
use subtle::ConstantTimeEq;
use tokio::sync::watch;
use tracing::{debug, info};

use crate::state::AppState;

mod claims;

/// The trace target of every event here.
pub const TRACE_TARGET: &str = "peer_origin";

/// How long the claim holds unrenewed: the work origin's TTL
/// (`crate::work_origin`).
pub const ORIGIN_TTL_SECS: u64 = 60;
/// How often the claim is renewed: cw-rails' default gossip round
/// (commonwealth-rails config.rs `default_gossip_interval`, 10 s), because
/// each renew carries this node's capabilities and cw-rails gossips what the
/// last renew declared, so peers see them no staler than one round.
pub const ORIGIN_RENEW_EVERY: Duration = Duration::from_secs(10);

/// The internal routes a peer dials, each served on the internal port
/// (`crate::server`): the corpus work queue, the merge coordinator's
/// wipe-after-pull, the pipeline pause, knowledge search and index transfer.
pub const PEER_PREFIXES: [&str; 10] = [
    "/internal/corpus/next_unit",
    "/internal/corpus/heartbeat",
    "/internal/corpus/complete_unit",
    "/internal/corpus/ingest_partition",
    "/internal/corpus/canonical",
    "/internal/corpus/partition_evict",
    "/internal/pipeline/pause",
    "/internal/knowledge/search",
    "/internal/index/serve",
    "/internal/index/transfer",
];

/// The registration: [`PEER_PREFIXES`] on `cwth/http/0`, at the internal
/// port, for members only.
pub fn registration(internal_port: u16) -> OriginRegistration {
    OriginRegistration {
        alpn: String::from_utf8_lossy(mesh_reach::alpn::ALPN).into_owned(),
        prefixes: PEER_PREFIXES.iter().map(|p| p.to_string()).collect(),
        port: internal_port,
        admit: Admit::Members(Vec::new()),
        framing: Framing::Http,
        ttl_secs: Some(ORIGIN_TTL_SECS),
        claims: None,
        namespaces: Vec::new(),
    }
}

/// What this registration declares at every register and renew: svrn's own
/// answers ([`claims::svrn_claims`] — hosted corpora, embed model,
/// availability, in-flight and the storage budget left), read from `state`
/// at that moment. The hardware is cw-rails' to measure, and the anchor tier
/// serve's to declare (sovereign-serve rails_mesh.rs `anchor_claims`).
pub fn claims_source(state: AppState) -> ClaimsSource {
    Arc::new(move || {
        let state = state.clone();
        Box::pin(async move {
            let engine = state.inner.node.corpus_engine.clone();
            let atlas = state.inner.node.atlas.clone();
            let now = sovereign_time::unix_now_u64();
            let caps = claims::svrn_claims(engine.as_ref(), atlas.as_ref(), now, &state).await;
            debug!(target: TRACE_TARGET, hosted_corpora = caps.hosted_corpora.len(),
                   embed_model = caps.embed_model.is_some(),
                   availability = caps.inference_availability,
                   storage_remaining_bytes = ?caps.storage_remaining_bytes,
                   "peer origin: declaring svrn's answers about this node");
            caps
        })
    })
}

/// The live claim's tie, as the register/renew loop publishes it. Empty
/// until [`spawn`] runs; `None` inside while no claim holds.
#[derive(Default)]
pub struct PeerOriginTie(OnceLock<watch::Receiver<Option<String>>>);

impl PeerOriginTie {
    /// Whether `presented` is the live claim's tie, compared in constant
    /// time. `false` with no registration, and with a lapsed claim.
    pub fn holds(&self, presented: &str) -> bool {
        let Some(rx) = self.0.get() else {
            return false;
        };
        match rx.borrow().as_deref() {
            Some(tie) => bool::from(tie.as_bytes().ct_eq(presented.as_bytes())),
            None => false,
        }
    }

    /// Install the channel the loop publishes on. `Err` when one is already
    /// installed: one daemon holds one registration.
    pub(crate) fn install(&self, rx: watch::Receiver<Option<String>>) -> Result<(), ()> {
        self.0.set(rx).map_err(|_| ())
    }
}

/// The registration loop, for as long as the daemon that started it runs:
/// dropping it stops the renewals, and cw-rails lets the claim lapse within
/// [`ORIGIN_TTL_SECS`], as the work origin's does.
pub struct PeerOriginHandle {
    register: tokio::task::JoinHandle<()>,
}

impl Drop for PeerOriginHandle {
    fn drop(&mut self) {
        self.register.abort();
    }
}

/// Register [`registration`] with cw-rails at `rails_base` and keep it
/// registered while the handle lives, declaring `claims` at every register
/// and renew and publishing each claim's tie into `tie`. A second call on the
/// same cell registers nothing and says so.
pub fn spawn(
    rails_base: String,
    internal_port: u16,
    tie: &PeerOriginTie,
    claims: ClaimsSource,
) -> Option<PeerOriginHandle> {
    let (tx, rx) = watch::channel(None);
    if tie.install(rx).is_err() {
        tracing::warn!(target: TRACE_TARGET,
                       "peer origin: already registered by this daemon; not registering twice");
        return None;
    }
    info!(target: TRACE_TARGET, rails = %rails_base, internal_port,
          prefixes = ?PEER_PREFIXES,
          "peer origin: registering svrn's peer routes with cw-rails");
    let register = tokio::spawn(
        sovereign_turn_client::rails_origins::keep_registered_declaring(
            rails_base,
            registration(internal_port),
            ORIGIN_TTL_SECS,
            ORIGIN_RENEW_EVERY,
            Some(tx),
            Some(claims),
        ),
    );
    Some(PeerOriginHandle { register })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tied(value: Option<&str>) -> (PeerOriginTie, watch::Sender<Option<String>>) {
        let cell = PeerOriginTie::default();
        let (tx, rx) = watch::channel(value.map(str::to_string));
        cell.install(rx).expect("first install");
        (cell, tx)
    }

    /// The live tie holds; a different one, a prefix of it, and the empty
    /// string do not. Failing input: compare by prefix or by length.
    #[test]
    fn only_the_live_tie_holds() {
        let (cell, _tx) = tied(Some("abc123"));
        assert!(cell.holds("abc123"));
        assert!(!cell.holds("abc124"));
        assert!(!cell.holds("abc"));
        assert!(!cell.holds(""));
    }

    /// No registration, and a lapsed claim, hold no tie at all.
    #[test]
    fn nothing_holds_without_a_live_claim() {
        assert!(!PeerOriginTie::default().holds("abc123"));
        let (cell, tx) = tied(Some("abc123"));
        tx.send_replace(None);
        assert!(!cell.holds("abc123"));
    }

    /// The source reads the node at every call, so what a renew declares is
    /// the state at that renew. Failing input: build the capabilities once
    /// and replay them.
    #[tokio::test]
    async fn the_claims_source_declares_the_state_at_each_call() {
        let state = crate::state::test_app_state();
        let source = claims_source(state.clone());
        state.update_local_availability(0.25).await;
        let first = source().await;
        state.update_local_availability(0.75).await;
        let second = source().await;
        assert_eq!(first.inference_availability, 0.25);
        assert_eq!(second.inference_availability, 0.75);
    }

    /// A second install is refused: one daemon, one registration.
    #[test]
    fn a_second_install_is_refused() {
        let (cell, _tx) = tied(None);
        let (_tx2, rx2) = watch::channel(None);
        assert!(cell.install(rx2).is_err());
    }

    /// Every prefix is a route the internal port mounts, on the internal
    /// ALPN, for members.
    #[test]
    fn the_registration_names_the_peer_routes_for_members() {
        let r = registration(9742);
        assert_eq!(r.alpn, "cwth/http/0");
        assert_eq!(r.port, 9742);
        assert_eq!(r.admit, Admit::Members(Vec::new()));
        assert_eq!(r.prefixes.len(), PEER_PREFIXES.len());
        let server = include_str!("server.rs");
        for prefix in PEER_PREFIXES {
            assert!(
                server.contains(&format!("\"{prefix}")),
                "{prefix} is not a route crate::server mounts"
            );
        }
    }

    /// The merge coordinator's wipe-after-pull (sovereign-grants
    /// `ShardManager::merge_participants`) POSTs this path to each peer over
    /// cw-rails; unregistered, cw-rails answered it 404 and an ephemeral
    /// grant's peer kept the user's chunks until its own pull loop exited
    /// (pc-bare-404s).
    #[test]
    fn the_coordinators_wipe_after_pull_reaches_the_peer() {
        assert!(
            registration(9742)
                .prefixes
                .iter()
                .any(|p| p == "/internal/corpus/partition_evict"),
            "partition_evict must be a registered peer prefix"
        );
    }
}
