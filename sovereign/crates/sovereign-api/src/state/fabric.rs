//! Fabric's part of the node's state — the node id with its public key, the
//! dial-info provider and dial signer, the roster, the ring rail and its write
//! nudge, the replicated KV, transport, clock, gossip's liveness maps, the
//! mutation persistence hook, the convergence recorder, the fan-out gauge, the
//! mesh-app registry and port map, the contribution emitter and the RPC-over-iroh
//! flag.
//!
//! DC §4.2 assigns these twenty to Fabric, whose home is `sovereign-mesh`, which
//! `sovereign-api` may not name (`[[forbid]] sovereign-api -> sovereign-*`,
//! `ARCH_LAYERS.toml:702`). Until `REVIEW-mint-daemon-move` relocates it this
//! part is scaffolding carried on `AppStateInner`; the route shells read it
//! directly rather than through delegating accessors.

use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::sync::RwLock;

use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{IrohDialInfo, Mesh};
use commonwealth_core::Clock;
use commonwealth_state::{ContributionEmitter, MeshStore};
use commonwealth_transport::PeerTransport;
use sovereign_core::identity::IdentityReader;
use sovereign_meshapp_registry::proxy::AppPortMap;
use sovereign_meshapp_registry::registry::AppRegistry;

use super::{ConvergenceRecord, MeshMutationHook};

/// Signs this node's dial info — `(version, relay, addrs) -> hex sig`. The
/// daemon builds it from the node `SigningKey`; `AppState` never holds raw key
/// material.
pub type DialSigner =
    dyn Fn(u64, Option<String>, Vec<std::net::SocketAddr>) -> String + Send + Sync;

/// The provider that yields this node's CURRENT iroh dial info (relay URL +
/// direct addrs), pulled fresh each gossip round. Type-erased so this crate
/// needs no iroh dependency; the daemon owns the endpoint.
pub type DialInfoProvider = dyn Fn() -> IrohDialInfo + Send + Sync;

/// The node's wall-clock source, published as a **reader** rather than installed
/// into the part afterwards: the daemon seeds it with [`commonwealth_core::SystemClock`]
/// at construction and the deterministic test harness swaps a per-node
/// `TestClock` (and skews it) during life. A reader, not a value, so a consumer
/// observes the swap (DC §4.2 "Construction is staged, and parts are total").
#[derive(Clone)]
pub struct ClockReader(Arc<ArcSwap<Arc<dyn Clock>>>);

impl ClockReader {
    /// Seed the reader with the clock the node starts life with.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(clock)))
    }

    /// The clock right now.
    pub fn current(&self) -> Arc<dyn Clock> {
        Arc::clone(&self.0.load())
    }

    /// Publish a new clock (the harness's per-node `TestClock`).
    pub fn publish(&self, clock: Arc<dyn Clock>) {
        self.0.store(Arc::new(clock));
    }
}

impl Default for ClockReader {
    fn default() -> Self {
        Self::new(Arc::new(commonwealth_core::SystemClock))
    }
}

/// How this node reaches mesh peers, published as a **reader**: the bootstrap
/// seeds it with an `IpTransport` at construction and the iroh reachability
/// watchdog swaps in a `RoutedTransport` (and a fresh endpoint) during life
/// without a restart (DC §4.2 "Construction is staged, and parts are total").
#[derive(Clone)]
pub struct PeerTransportReader(Arc<ArcSwap<Arc<dyn PeerTransport>>>);

impl PeerTransportReader {
    /// Seed the reader with the transport the node starts life with.
    pub fn new(transport: Arc<dyn PeerTransport>) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(transport)))
    }

    /// The transport right now.
    pub fn current(&self) -> Arc<dyn PeerTransport> {
        Arc::clone(&self.0.load())
    }

    /// Publish a new transport (the iroh routed transport, or a fresh endpoint).
    pub fn publish(&self, transport: Arc<dyn PeerTransport>) {
        self.0.store(Arc::new(transport));
    }
}

impl Default for PeerTransportReader {
    fn default() -> Self {
        Self::new(Arc::new(commonwealth_transport::IpTransport::default()))
    }
}

/// This node's live iroh dial info, published as a **reader**: the endpoint
/// binds after the node is constructed, and the reachability watchdog swaps in
/// a fresh endpoint during life (DC §4.2 "Construction is staged, and parts are
/// total"). `None` until iroh binds — absence reported, never invented.
#[derive(Clone, Default)]
pub struct DialInfoReader(Arc<ArcSwap<Option<Arc<DialInfoProvider>>>>);

impl DialInfoReader {
    /// This node's dial info right now, or `None` when iroh is off.
    pub fn current(&self) -> Option<IrohDialInfo> {
        let guard = self.0.load();
        let provider = (**guard).as_ref()?;
        Some(provider())
    }

    /// Publish the provider that reads from the live endpoint.
    pub fn publish(&self, provider: Arc<DialInfoProvider>) {
        self.0.store(Arc::new(Some(provider)));
    }
}

/// Everything Fabric's part is constructed with (DC §4.2 "Construction is
/// staged, and parts are total"): the values that exist before the part is
/// built. The daemon gathers them and passes them to `AppState::new…`; a test
/// takes `Default`. The three readers are created first and shared, so an owner
/// may publish through its own handle while the part reads — the in-flight
/// gauge's shape (`sovereign-contracts/src/in_flight.rs`).
#[derive(Default)]
pub struct FabricSeed {
    /// The shared notes-rail convergence recorder. One instance, so the
    /// daemon-side writers and `/status`'s reader cannot disagree.
    pub convergence: Option<Arc<ConvergenceRecord>>,
    /// This node's Ed25519 identity pubkey, from `<data_dir>/node_key`.
    pub self_node_pubkey: Option<commonwealth_core::ids::NodePubkey>,
    /// The dial-info signing closure, built from the node `SigningKey`.
    pub self_dial_signer: Option<Arc<DialSigner>>,
    /// The ring rail's storage, built from the data dir and identity key.
    pub ring_rail: Option<Arc<commonwealth_rail::RingRail>>,
    /// The mutation persistence hook, installed at construction rather than
    /// through the `Arc::get_mut` installer that could silently no-op.
    pub mesh_mutation_hook: Option<MeshMutationHook>,
    /// The node's clock, created first and shared with the harness.
    pub clock: ClockReader,
    /// The node's peer transport, created first and shared with the watchdog.
    pub peer_transport: PeerTransportReader,
    /// The node's live iroh dial info, created first and shared with the
    /// endpoint owner.
    pub dial_info: DialInfoReader,
}

/// Fabric's twenty fields, held as `AppStateInner::fabric`.
pub struct FabricPart {
    /// This node's identity, published as a **watch** rather than copied as a
    /// value (`sovereign_core::identity::IdentityReader`, a re-export of the
    /// contract type; DC §4.2
    /// "Identity is a reader, not a value"). `join_mesh` swaps the placeholder
    /// id for the founder-assigned one atomically after the handshake, and a
    /// consumer that holds this handle observes the swap — one that copied a
    /// `NodeId` would keep the placeholder, so gossip would never find our own
    /// member record (it indexes by `self_node_id`) and `corpus_collaborate`
    /// would 500 with "local node not found in mesh".
    pub identity: IdentityReader,
    pub mesh: Arc<RwLock<Mesh>>,
    /// This node's Ed25519 identity pubkey (see
    /// `commonwealth_core::ids::NodePubkey`). Set at construction by the
    /// embedded daemon from `<data_dir>/node_key`; `None` in tests and on
    /// daemons that don't manage an identity key. Gossip stamps it into our own
    /// `MemberRecord` every round so in-place upgrades publish the key without a
    /// rejoin.
    pub self_node_pubkey: Option<commonwealth_core::ids::NodePubkey>,
    /// Provider yielding this node's CURRENT iroh dial info (relay URL
    /// + direct addrs), pulled fresh each gossip round and stamped into
    /// our own `MemberRecord` (Track W2). A reader created before the
    /// part and published by the daemon that owns the endpoint: the relay
    /// and hole-punched addrs appear and change over the endpoint's
    /// lifetime, and the reachability watchdog swaps in a fresh one. `None`
    /// when iroh access is off.
    pub dial_info: DialInfoReader,
    /// Closure that signs this node's dial info (relay_url + direct addrs)
    /// for the gossip self-stamp — `(version, relay, addrs) -> hex sig`.
    /// Set at construction from the node `SigningKey`, so `AppState`
    /// never holds raw key material and `commonwealth-api` needs no crypto
    /// dependency. `None` when the daemon has no identity key. See
    /// [`crate::state::AppState::sign_dial_info`].
    pub self_dial_signer: Option<Arc<DialSigner>>,
    /// The ring rail's storage: where each ring namespace's journal lives,
    /// and how this node signs the ops it writes. Set at construction; a
    /// daemon with no data directory has nowhere to put a ledger, and the
    /// rail then REFUSES rather than inventing a location or answering from
    /// an empty in-memory one (ARCH §18.3).
    ///
    /// The signer is a closure-shaped seam for the same reason
    /// [`Self::self_dial_signer`] is: `AppState` never holds raw key
    /// material and this crate needs no crypto dependency.
    pub ring_rail: Option<Arc<commonwealth_rail::RingRail>>,
    /// "A local write is queued; run the ring round now rather than at the
    /// next sixty-second tick."
    ///
    /// ONE `Notify` per daemon, held here rather than passed around, because
    /// three unrelated places raise or wait on it — the KV pump after an
    /// append, `ring_sync`'s loop as its early wake-up, and the work atlas's
    /// broadcaster when a claim is written for a peer to see. A second
    /// `Notify` would be a second answer to "what wakes replication" (ARCH
    /// §10.6), and threading one through three constructors is how the second
    /// one gets minted.
    ///
    /// Always present: a nudge nobody is waiting on stores one permit and
    /// costs nothing, so there is no `Option` and no "the daemon has not
    /// installed it yet" branch to get wrong.
    pub ring_write_nudge: Arc<tokio::sync::Notify>,
    /// How this daemon reaches mesh peers — the PeerTransport seam.
    /// Every route handler that dials a peer resolves URLs through
    /// this instead of formatting `http://{ip}:{port}` inline, so a
    /// future transport (dial-by-key iroh) slots in without touching
    /// call sites. Seeded with `IpTransport::default()` (client port
    /// 9741); the embedded daemon seeds one configured with its resolved
    /// client port, and the iroh watchdog publishes a `RoutedTransport`
    /// through the same reader. A reader created before the part, not a
    /// slot filled later.
    pub peer_transport: PeerTransportReader,
    /// Wall-clock source. Seeded with [`commonwealth_core::SystemClock`]; the
    /// test harness publishes a per-node [`commonwealth_core::TestClock`]
    /// through the reader to drive skew scenarios deterministically. A reader
    /// created before the part, not a slot filled later.
    pub clock: ClockReader,
    /// Local-observation liveness map: `node_id -> local-clock seconds at which
    /// we last observed this peer's gossiped record advance (or reached it
    /// directly). Offline-decay measures staleness against THIS, never the
    /// peer's own gossiped `last_seen` — so a clock-skewed peer can't flap
    /// Offline (the "~9 min flap"). Ephemeral; rebuilt after restart as gossip
    /// re-observes peers.
    pub peer_last_contact: std::sync::RwLock<std::collections::HashMap<NodeId, u64>>,
    /// Selection-fairness map: `node_id -> local-clock seconds at which we last
    /// SPENT A ROUND'S SLOT on this peer, whether or not the dial worked.
    ///
    /// A SEPARATE CLOCK FROM `peer_last_contact` BECAUSE THEY ANSWER DIFFERENT
    /// QUESTIONS, and one field answering both is what starved a live peer.
    /// `peer_last_contact` is LIVENESS EVIDENCE and must stay success-only —
    /// the comment on its stamp site in `gossip::run_one_round` records the
    /// 2026-07-29 false-Offline that cost a distributed 122B its remote shard.
    /// But `select_round_peers` orders by most-stale-first, so using evidence
    /// as the ordering key means a peer that never answers never advances and
    /// therefore holds its slot for ever.
    ///
    /// MEASURED on RuggedFox 2026-09-09, `FANOUT = 2`: 74 gossip dials to each
    /// of the same two unreachable peers in twenty minutes, and ZERO to the
    /// other five members — one of whose daemons was up throughout.
    /// `select_round_peers`' own bound ("a peer waits at most
    /// `ceil(n / FANOUT)` rounds, because every round it goes unpicked it
    /// moves up the order") holds only if being PICKED advances your key. For
    /// a peer that does not answer, it did not.
    ///
    /// Ephemeral like `peer_last_contact`; rebuilt after restart.
    pub peer_last_attempt: std::sync::RwLock<std::collections::HashMap<NodeId, u64>>,
    /// Which peers we have CONFIRMED are running a post-credential-split
    /// build, by having merged a gossip payload from them that carried a
    /// `mesh_secret`.
    ///
    /// Absence is not "pre-split", it is "unknown", and both are treated as
    /// unsafe by `EmbeddedDaemon::rotate_invite` — the fail-safe direction.
    /// A peer we have not gossiped with since boot could be either, and
    /// rotating on that assumption is what partitions a mesh. One gossip
    /// round per peer clears it, so the conservative window is short.
    ///
    /// Deliberately NOT on `MemberRecord`: this is our own local observation
    /// of a peer's payload, not a claim the peer makes about itself and not
    /// something another node may assert on its behalf (ARCH §18.1 — never
    /// let the subject supply the field a guard reads).
    pub peer_post_split: std::sync::RwLock<std::collections::HashMap<NodeId, bool>>,
    /// True when this node's iroh acceptor routes the RPC ALPN to a local
    /// ggml rpc-server — i.e. a cross-network host can genuinely reach our
    /// RPC worker through the mesh tunnel. Drives the additive
    /// `rpc_worker.iroh` flag on `/status`. Set by the daemon when it
    /// installs iroh access; stays false on plaintext meshes or when the
    /// iroh kill-switch is on, so we never advertise a path that isn't
    /// actually accepting.
    pub rpc_iroh_accept: std::sync::atomic::AtomicBool,
    /// Distributed KV store for mesh apps.
    pub mesh_store: Arc<MeshStore>,
    /// Registry of known mesh apps (gossiped).
    pub app_registry: Arc<AppRegistry>,
    /// Map of locally running app ports for the proxy layer.
    pub app_port_map: AppPortMap,
    /// Concurrent **outbound** peer knowledge fan-out requests in flight from
    /// this node (one per peer a knowledge search is currently querying). The
    /// glassbox companion to the inbound `peer_sched` admission gauge: it makes
    /// `BoundedFanOut` a live runtime signal, not just a source-unit-tested
    /// property of `select_fanout_corpora`. 0 when no fan-out is in progress.
    /// Maintained by `commonwealth_transport::fanout`'s guard, which holds
    /// this same `Arc` — the counter is shared, not owned, so the fan-out
    /// serves a process with no `AppState` at all. Read via
    /// [`crate::state::AppState::fanout_inflight_count`].
    pub fanout_inflight: commonwealth_transport::fanout::InflightGauge,
    /// Optional callback fired after any `Mesh` mutation by the
    /// route handlers. Set by the embedded daemon to the
    /// `persist::save` function so `/internal/join` accepts survive
    /// a founder restart immediately (not just on the next gossip
    /// tick). `None` in tests and in the standalone Commonwealth
    /// daemon, where persistence is managed elsewhere.
    pub on_mesh_mutation: Option<MeshMutationHook>,
    /// The notes-rail convergence recorder (order commons-fluency
    /// fix 9). One shared instance set at construction so the
    /// daemon-side publish sink and ingest poller stamp the same record
    /// `/status` reads. `None` when the boot has no rails.
    pub convergence: Option<Arc<ConvergenceRecord>>,

    /// Dimensional contribution emitter. Each route handler records
    /// `LedgerEvent`s through this on completion (per write site
    /// listed in the Mesh Health design). Cheap to clone; emission
    /// is `tokio::spawn`-friendly. The emitter holds its own handle
    /// to `MeshStore` so it survives `AppState` clones and can be
    /// passed into spawned tasks without lifetime gymnastics.
    pub contribution_emitter: ContributionEmitter,
}
