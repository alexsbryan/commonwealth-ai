//! Fabric's part of the node's state — the node id, the roster reader, the
//! ring rail and its write nudge, the replicated KV, transport, the
//! convergence recorder, the fan-out gauge and the RPC-over-iroh flag.
//!
//! svrn holds no mesh of its own (pb-mesh-exit-transport): cw-rails is the
//! node's one mesh endpoint, so the roster is read through the membership port
//! and every peer is reached through the transport the distribution composed.
//! Nothing here holds a key, a roster copy, a liveness map or a gossip clock;
//! gossip, admission and the ring round are cw-rails'.

use std::sync::Arc;

use arc_swap::ArcSwap;

use commonwealth_core::ids::NodeId;

use commonwealth_transport::{PeerContact, PeerTransport};
use sovereign_contracts::identity::IdentityReader;
use sovereign_contracts::membership::{MembershipReader, NoMembership};
use sovereign_contracts::peer::ConvergenceRecord;

/// How this node reaches mesh peers, published as a **reader**: the daemon
/// seeds it at construction with the transport its distribution composed
/// (cw-rails' reach door) and a test may publish its own (DC §4.2
/// "Construction is staged, and parts are total").
#[derive(Clone)]
pub struct TransportReader(Arc<ArcSwap<Arc<dyn PeerTransport>>>);

impl TransportReader {
    /// Seed the reader with the transport the node starts life with.
    pub fn new(transport: Arc<dyn PeerTransport>) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(transport)))
    }

    /// The transport right now.
    pub fn current(&self) -> Arc<dyn PeerTransport> {
        Arc::clone(&self.0.load())
    }

    /// Publish a new transport.
    pub fn publish(&self, transport: Arc<dyn PeerTransport>) {
        self.0.store(Arc::new(transport));
    }
}

impl Default for TransportReader {
    fn default() -> Self {
        Self::new(Arc::new(commonwealth_transport::IpTransport::default()))
    }
}

/// Everything Fabric's part is constructed with (DC §4.2 "Construction is
/// staged, and parts are total"): the values that exist before the part is
/// built. The daemon gathers them and passes them to `AppState::new…`; a test
/// takes `Default`.
#[derive(Default)]
pub struct FabricSeed {
    /// The shared notes-rail convergence recorder. One instance, so the
    /// daemon-side writers and `/status`'s reader cannot disagree.
    pub convergence: Option<Arc<ConvergenceRecord>>,
    /// The ring rail's storage (cw-rails' journals, through the port).
    pub ring_rail: Option<Arc<dyn crate::rail_port::RingRailPort>>,
    /// The node's peer transport: cw-rails' reach door in production.
    pub peer_transport: TransportReader,
    /// The membership reader: cw-rails' roster in production. `None` reads
    /// an empty roster ([`NoMembership`]).
    pub membership: Option<Arc<dyn MembershipReader<Dial = PeerContact>>>,
}

/// Fabric's fields, held as `AppStateInner::fabric`.
pub struct FabricPart {
    /// This node's identity, published as a **watch** rather than copied as a
    /// value (`sovereign_contracts::identity::IdentityReader`; DC §4.2
    /// "Identity is a reader, not a value").
    pub identity: IdentityReader,
    /// The one read of membership (`sovereign_contracts::membership`):
    /// cw-rails' roster, the node's one (pb-mesh-exit-transport).
    pub membership: Arc<dyn MembershipReader<Dial = PeerContact>>,
    /// The ring rail's storage: where each ring namespace's journal lives,
    /// and how this node signs the ops it writes — cw-rails' doors through
    /// the port (`crate::rail_port`). A daemon with no data directory has
    /// nowhere to put a ledger, and the rail then REFUSES rather than
    /// answering from an empty in-memory one (ARCH §18.3).
    pub ring_rail: Option<Arc<dyn crate::rail_port::RingRailPort>>,
    /// "A local write is queued; make it travel now." ONE `Notify` per
    /// daemon, raised by the KV pump after an append and by the work atlas's
    /// broadcaster when a claim is written. Always present: a nudge nobody is
    /// waiting on stores one permit and costs nothing.
    pub ring_write_nudge: Arc<tokio::sync::Notify>,
    /// How this daemon reaches mesh peers — the PeerTransport seam. Every
    /// route handler that dials a peer resolves URLs through this.
    pub peer_transport: TransportReader,
    /// True when an RPC route over the mesh tunnel reaches this node's ggml
    /// rpc-server. Drives the additive `rpc_worker.iroh` flag on `/status`.
    pub rpc_iroh_accept: std::sync::atomic::AtomicBool,
    /// Concurrent **outbound** peer knowledge fan-out requests in flight from
    /// this node. Maintained by `commonwealth_transport::fanout`'s guard,
    /// which holds this same `Arc`. Read via
    /// [`crate::state::AppState::fanout_inflight_count`].
    pub fanout_inflight: commonwealth_transport::fanout::InflightGauge,
    /// The notes-rail convergence recorder (order commons-fluency fix 9).
    /// `None` when the boot has no rails.
    pub convergence: Option<Arc<ConvergenceRecord>>,
}

impl FabricPart {
    /// Assemble Fabric's part from values that all exist before it (DC §4.2
    /// "Construction is staged, and parts are total").
    pub fn new(self_node_id: NodeId, seed: FabricSeed) -> Self {
        let membership = seed.membership.unwrap_or_else(|| {
            Arc::new(NoMembership::<PeerContact>::default())
                as Arc<dyn MembershipReader<Dial = PeerContact>>
        });
        Self {
            identity: IdentityReader::new(self_node_id),
            membership,
            ring_rail: seed.ring_rail,
            ring_write_nudge: Arc::new(tokio::sync::Notify::new()),
            peer_transport: seed.peer_transport,
            rpc_iroh_accept: std::sync::atomic::AtomicBool::new(false),
            fanout_inflight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            convergence: seed.convergence,
        }
    }

    /// The ring rail's storage, or `None` if the daemon has none.
    pub fn ring_rail(&self) -> Option<Arc<dyn crate::rail_port::RingRailPort>> {
        self.ring_rail.clone()
    }

    /// The wake-up that makes a local store write travel now.
    pub fn ring_write_nudge(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.ring_write_nudge)
    }

    /// Snapshot of the active [`PeerTransport`]. Cheap (one atomic load +
    /// Arc clone); call per dial, don't cache across awaits.
    pub fn peer_transport(&self) -> Arc<dyn PeerTransport> {
        self.peer_transport.current()
    }

    /// Fabric's peer-transport reader — the owner's write handle.
    pub fn peer_transport_reader(&self) -> TransportReader {
        self.peer_transport.clone()
    }

    /// This node's NodeId, by value. Cheap (atomic load + Arc deref).
    pub fn self_node_id(&self) -> NodeId {
        self.identity.current()
    }

    /// Fabric's identity, published as a watch.
    pub fn identity_reader(&self) -> IdentityReader {
        self.identity.clone()
    }

    /// The convergence recorder the daemon's sink/poller writers stamp and
    /// `/status` reads — ONE instance, set at construction.
    pub fn convergence_recorder(&self) -> Option<Arc<ConvergenceRecord>> {
        self.convergence.clone()
    }

    /// Record whether an RPC route over the mesh tunnel reaches a local ggml
    /// rpc-server.
    pub fn set_rpc_iroh_accept(&self, on: bool) {
        self.rpc_iroh_accept
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether `/status` may honestly advertise `rpc_worker.iroh: true`.
    pub fn rpc_iroh_accept(&self) -> bool {
        self.rpc_iroh_accept
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The online members whose capability record says they can anchor. The
    /// daemon's `eligible_anchors` delegates so the roster decision has one
    /// implementation: `sovereign_contracts::membership::eligible_anchors`
    /// over this part's membership reader, the one serve's discovery loop
    /// applies to whatever roster it is handed (pb-serve-distributes).
    pub async fn eligible_anchors(&self) -> Vec<NodeId> {
        sovereign_contracts::membership::eligible_anchors(&self.membership.members().await)
    }
}
