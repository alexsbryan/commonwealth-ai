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

use commonwealth_core::capabilities::OriginKind;
use commonwealth_core::ids::NodeId;
use commonwealth_core::mesh::{IrohDialInfo, Mesh};
use commonwealth_core::Clock;
use commonwealth_media::fanout::{FanoutRequest, FanoutResponse};
use commonwealth_media::{MediaOffer, MediaReach, MediaReachRefusal, PeerTransportPath};
use commonwealth_state::{ContributionEmitter, MeshStore};
use commonwealth_transport::PeerTransport;
use sovereign_core::identity::IdentityReader;
use sovereign_core::peer::ConvergenceRecord;
use sovereign_meshapp_registry::proxy::AppPortMap;
use sovereign_meshapp_registry::registry::AppRegistry;

/// Callback the route handlers fire whenever they mutate `Mesh` —
/// `/internal/join` (accepting a new member), `/internal/gossip`
/// (merging a peer's view). `sovereign-mesh::EmbeddedDaemon` installs
/// a hook that persists `mesh.json` synchronously so a restart within
/// the gossip interval never forgets a mutation. Tests leave this
/// `None` and rely on their assertions without touching disk.
///
/// Moved here with the part at domains `dm-daemon-api-edge` (b): the hook is
/// Fabric's own persistence seam and the loops that fire it now hold the part.
pub type MeshMutationHook = std::sync::Arc<dyn Fn(&Mesh, NodeId) + Send + Sync>;

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

    /// Publish a new transport (the iroh routed transport, or a fresh endpoint).
    pub fn publish(&self, transport: Arc<dyn PeerTransport>) {
        self.0.store(Arc::new(transport));
    }
}

impl Default for TransportReader {
    fn default() -> Self {
        Self::new(Arc::new(commonwealth_transport::IpTransport::default()))
    }
}

/// The active mesh's cached plaintext join key, published as a **reader**
/// rather than copied into each holder: `create_mesh` / `join_mesh` /
/// `try_resume` seed it, `rotate_invite` swaps in the new key, and `leave`
/// clears it — while the share UI's `current_invite` reads through the same
/// handle. Fabric owns the key; the daemon's orchestration observes it through
/// this handle instead of a second copy that could go stale (DC §4.1 "the
/// membership operations ... are Fabric adopting, persisting and gossiping its
/// own roster and identity"; DC §4.2 "Construction is staged, and parts are
/// total").
#[derive(Clone, Default)]
pub struct JoinKeyReader(Arc<ArcSwap<Option<String>>>);

impl JoinKeyReader {
    /// Seed the reader with the key the active mesh starts life with.
    pub fn new(key: Option<String>) -> Self {
        Self(Arc::new(ArcSwap::from_pointee(key)))
    }

    /// The cached plaintext right now, if any.
    pub fn current(&self) -> Option<String> {
        (**self.0.load()).clone()
    }

    /// Publish the active mesh's key (or clear it on leave/park).
    pub fn publish(&self, key: Option<String>) {
        self.0.store(Arc::new(key));
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

/// What [`FabricPart::forget_member`] retired. Moved here with the tombstone
/// core at domains `REVIEW-build-daemon-membership-lifecycle` (DC §4.1: the
/// roster mutations are Fabric's). The daemon's HTTP shell serializes it; the
/// wire shape is unchanged.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ForgottenMember {
    pub name: String,
    pub node_id: NodeId,
    /// The row was one of a colliding pair — this call was a repair rather
    /// than a removal. Reported so the CLI can say which it did.
    pub was_aliased: bool,
    /// Already a tombstone when we got here; nothing was written. Distinct
    /// from a fresh retirement so a caller never reports work it did not do.
    pub already_retired: bool,
}

/// Why [`FabricPart::forget_member`] refused. The daemon maps each arm onto its
/// own `MeshError` at the boundary (DC §4.1: "`MeshError` maps at the daemon
/// boundary"), so Fabric does not name the host's error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgetMemberError {
    /// No member row matched the query.
    UnknownMember(String),
    /// The query resolved to this node's own row.
    CannotForgetSelf,
    /// The member is online and unaliased; a `force` is required.
    MemberStillLive(String),
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
    pub peer_transport: TransportReader,
    /// The node's live iroh dial info, created first and shared with the
    /// endpoint owner.
    pub dial_info: DialInfoReader,
    /// The active mesh's cached plaintext join key, created first and shared
    /// with the daemon's membership orchestration (DC §4.1: Fabric owns the
    /// key; the daemon observes it through this reader).
    pub join_key: JoinKeyReader,
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
    pub peer_transport: TransportReader,
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

    /// The active mesh's cached plaintext join key, held as a **reader**
    /// created before the part and shared with the daemon's membership
    /// orchestration (DC §4.1: Fabric owns the roster, identity and join key;
    /// the daemon observes them through readers rather than a second copy).
    /// Seeded on `create_mesh`/`join_mesh`/`try_resume`, swapped by
    /// `rotate_invite`, cleared on `leave`/`park`.
    pub join_key: JoinKeyReader,

    /// Dimensional contribution emitter. Each route handler records
    /// `LedgerEvent`s through this on completion (per write site
    /// listed in the Mesh Health design). Cheap to clone; emission
    /// is `tokio::spawn`-friendly. The emitter holds its own handle
    /// to `MeshStore` so it survives `AppState` clones and can be
    /// passed into spawned tasks without lifetime gymnastics.
    pub contribution_emitter: ContributionEmitter,
}

/// The accessors the three mesh loops call — moved here from `AppState` with
/// the part at domains `dm-daemon-api-edge` (b), so a loop can hold Fabric's
/// part without naming the daemon's `AppState` (which `sovereign-mesh` may not
/// do: `[[forbid]] sovereign-mesh -> sovereign-daemon`). `AppState`'s own
/// methods now delegate here, so there is one implementation of each read.
impl FabricPart {
    /// Assemble Fabric's part from values that all exist before it (DC §4.2
    /// "Construction is staged, and parts are total"). The daemon gathers the
    /// seed and calls this before `AppState`, so the part can be held across a
    /// stop; the tests take [`FabricSeed::default`].
    pub fn new(
        self_node_id: NodeId,
        mesh: Mesh,
        mesh_store: Arc<MeshStore>,
        app_registry: Arc<AppRegistry>,
        seed: FabricSeed,
    ) -> Self {
        let contribution_emitter = ContributionEmitter::new((*mesh_store).clone(), self_node_id);
        Self {
            identity: IdentityReader::new(self_node_id),
            mesh: Arc::new(RwLock::new(mesh)),
            self_node_pubkey: seed.self_node_pubkey,
            dial_info: seed.dial_info,
            self_dial_signer: seed.self_dial_signer,
            ring_rail: seed.ring_rail,
            ring_write_nudge: Arc::new(tokio::sync::Notify::new()),
            peer_transport: seed.peer_transport,
            clock: seed.clock,
            peer_last_contact: std::sync::RwLock::new(std::collections::HashMap::new()),
            peer_last_attempt: std::sync::RwLock::new(std::collections::HashMap::new()),
            peer_post_split: std::sync::RwLock::new(std::collections::HashMap::new()),
            rpc_iroh_accept: std::sync::atomic::AtomicBool::new(false),
            mesh_store,
            app_registry,
            app_port_map: AppPortMap::new(),
            fanout_inflight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            on_mesh_mutation: seed.mesh_mutation_hook,
            convergence: seed.convergence,
            contribution_emitter,
            join_key: seed.join_key,
        }
    }

    /// This node's identity pubkey, if the node has one.
    pub fn self_node_pubkey(&self) -> Option<commonwealth_core::ids::NodePubkey> {
        self.self_node_pubkey
    }

    /// This node's current iroh dial info, if iroh access is on. Pulled live
    /// from the endpoint each call through the reader.
    pub fn self_iroh_dialinfo(&self) -> Option<IrohDialInfo> {
        self.dial_info.current()
    }

    /// Fabric's dial-info reader — the owner's write handle.
    pub fn dial_info_reader(&self) -> DialInfoReader {
        self.dial_info.clone()
    }

    /// Sign this node's dial info (hex), or `None` if the node has no identity
    /// key (iroh disabled / pre-identity build).
    pub fn sign_dial_info(
        &self,
        version: u64,
        relay_url: Option<&str>,
        direct_addrs: &[std::net::SocketAddr],
    ) -> Option<String> {
        let signer = self.self_dial_signer.clone()?;
        Some(signer(
            version,
            relay_url.map(|s| s.to_string()),
            direct_addrs.to_vec(),
        ))
    }

    /// The ring rail's storage, or `None` if the daemon has none.
    pub fn ring_rail(&self) -> Option<Arc<commonwealth_rail::RingRail>> {
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

    /// Snapshot of the active [`Clock`]. Cheap (one atomic load + Arc clone);
    /// call per timestamp, don't cache across awaits.
    pub fn clock(&self) -> Arc<dyn Clock> {
        self.clock.current()
    }

    /// Fabric's clock reader — the owner's write handle.
    pub fn clock_reader(&self) -> ClockReader {
        self.clock.clone()
    }

    /// Record that we just observed `peer`'s liveness at local time `now_secs`.
    pub fn observe_peer_contact(&self, peer: NodeId, now_secs: u64) {
        self.peer_last_contact
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(peer, now_secs);
    }

    /// Record that this round SPENT A SLOT dialing `peer`.
    pub fn note_peer_attempt(&self, peer: NodeId, now_secs: u64) {
        self.peer_last_attempt
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(peer, now_secs);
    }

    /// When a round last spent a slot on `peer`, initializing to `now_secs`
    /// when we have never dialed it.
    pub fn peer_attempt_or_init(&self, peer: NodeId, now_secs: u64) -> u64 {
        *self
            .peer_last_attempt
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(peer)
            .or_insert(now_secs)
    }

    /// Local-observation time for `peer`, initializing it to `now_secs`.
    pub fn peer_contact_or_init(&self, peer: NodeId, now_secs: u64) -> u64 {
        *self
            .peer_last_contact
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(peer)
            .or_insert(now_secs)
    }

    /// Record which credential generation `peer` is running.
    pub fn observe_peer_split_generation(&self, peer: NodeId, post_split: bool) {
        self.peer_post_split
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(peer, post_split);
    }

    /// Whether we have positively confirmed `peer` is post-credential-split.
    pub fn peer_confirmed_post_split(&self, peer: NodeId) -> bool {
        self.peer_split_generation(peer).unwrap_or(false)
    }

    /// What we actually know about `peer`'s credential generation, WITHOUT
    /// collapsing the two ways of not knowing into one.
    pub fn peer_split_generation(&self, peer: NodeId) -> Option<bool> {
        self.peer_post_split
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&peer)
            .copied()
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

    /// Record whether the iroh acceptor routes the RPC ALPN to a local
    /// ggml rpc-server.
    pub fn set_rpc_iroh_accept(&self, on: bool) {
        self.rpc_iroh_accept
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether `/status` may honestly advertise `rpc_worker.iroh: true`.
    pub fn rpc_iroh_accept(&self) -> bool {
        self.rpc_iroh_accept
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The online members whose capability record says they can anchor —
    /// Fabric's own roster read, moved here from `EmbeddedDaemon` at domains
    /// `REVIEW-build-daemon-embedded-split` (DC §4.1 "report reach"). The
    /// daemon's `eligible_anchors` delegates so the roster decision has one
    /// implementation.
    pub async fn eligible_anchors(&self) -> Vec<NodeId> {
        let mesh = self.mesh.read().await;
        mesh.members
            .values()
            .filter(|m| {
                matches!(
                    m.status,
                    commonwealth_core::mesh::NodeStatus::Online
                        | commonwealth_core::mesh::NodeStatus::Busy
                )
            })
            .filter(|m| m.capabilities.anchor.as_ref().is_some_and(|a| a.can_anchor))
            .map(|m| m.node_id)
            .collect()
    }

    /// The members offering an origin of `kind`, with the live path to each.
    /// `paths` is the daemon's iroh transport snapshot (Fabric does not own the
    /// endpoint), passed in so the roster projection stays Fabric's.
    pub async fn origin_offers(
        &self,
        kind: OriginKind,
        paths: &[(NodeId, PeerTransportPath)],
    ) -> Vec<MediaOffer> {
        let self_id = self.identity.current();
        let roster = {
            let mesh = self.mesh.read().await;
            commonwealth_media::roster_of(&mesh)
        };
        commonwealth_media::offers(self_id, &roster, paths, kind)
    }

    /// Mint (or reuse) the loopback bridge to `query`'s origin of `kind`.
    pub async fn origin_reach(
        &self,
        query: &str,
        kind: OriginKind,
        paths: &[(NodeId, PeerTransportPath)],
    ) -> Result<MediaReach, MediaReachRefusal> {
        let self_id = self.identity.current();
        let roster = {
            let mesh = self.mesh.read().await;
            commonwealth_media::roster_of(&mesh)
        };
        commonwealth_media::reach(self_id, &roster, query, &self.peer_transport(), paths, kind)
            .await
    }

    /// Ask every selected member the same request through its own bridge for
    /// the requested origin kind, concurrently, one row per member.
    pub async fn origin_fanout(
        &self,
        req: FanoutRequest,
    ) -> Result<FanoutResponse, MediaReachRefusal> {
        let self_id = self.identity.current();
        let roster = {
            let mesh = self.mesh.read().await;
            commonwealth_media::roster_of(&mesh)
        };
        commonwealth_media::fanout::fanout(
            self_id,
            &roster,
            req,
            self.peer_transport(),
            self.fanout_inflight.clone(),
        )
        .await
    }

    /// The active mesh's cached plaintext join key, right now. Fabric owns the
    /// key (DC §4.1); the daemon's `current_invite` reads it through here.
    pub fn join_key(&self) -> Option<String> {
        self.join_key.current()
    }

    /// Publish the active mesh's cached plaintext join key — seeded by
    /// `create_mesh`/`join_mesh`/`try_resume`, swapped by `rotate_invite`,
    /// cleared by `leave`/`park`. The daemon holds a handle to the same reader.
    pub fn publish_join_key(&self, key: Option<String>) {
        self.join_key.publish(key);
    }

    /// Adopt an authoritative mesh (a join handshake's snapshot, or a resume)
    /// and the identity the mesh assigned us, in ONE step. Fabric owns the
    /// roster and the identity (DC §4.1); the daemon calls this instead of
    /// writing `fabric.mesh` and `identity_reader().publish` itself.
    pub async fn adopt(&self, mesh: Mesh, node_id: NodeId) {
        *self.mesh.write().await = mesh;
        self.identity.publish(node_id);
    }

    /// Retire one member row: tombstone it in Fabric's roster and report what
    /// happened. The daemon persists the result and lets the ordinary gossip
    /// round carry the removal mesh-wide (DC §4.1: the roster mutations are
    /// Fabric's; `MeshError` maps at the daemon boundary).
    ///
    /// # Why a tombstone rather than a delete
    ///
    /// Deleting the row locally would work until the next gossip round, when
    /// a peer still holding it hands it straight back. `removed_at` is the
    /// mesh's removal primitive and it converges: it wins the
    /// [`commonwealth_core::mesh::MemberRecord::effective_at`] LWW against
    /// any older `last_seen`, and it is what `leave` already uses.
    ///
    /// It is also self-limiting in the right way. A GHOST — a stale row for a
    /// machine that re-registered under a new node_id — has nothing left to
    /// defend it, so the tombstone sticks. A row belonging to a daemon that
    /// is genuinely alive gets re-announced on that node's next round, since
    /// a node is authoritative for itself. The repair therefore cannot evict
    /// a live member even by mistake, which is why `force` is a guard against
    /// operator surprise rather than against damage.
    pub async fn forget_member(
        &self,
        query: &str,
        force: bool,
        now: u64,
    ) -> Result<ForgottenMember, ForgetMemberError> {
        let self_id = self.identity.current();
        let mut mesh = self.mesh.write().await;

        let aliased: std::collections::HashSet<NodeId> = mesh
            .aliased_endpoint_keys()
            .into_iter()
            .flat_map(|a| a.members.into_iter().map(|(id, _)| id))
            .collect();

        let target = mesh
            .members
            .values()
            .find(|m| commonwealth_core::mesh::member_matches(m.node_id, &m.name, query))
            .map(|m| m.node_id)
            .ok_or_else(|| ForgetMemberError::UnknownMember(query.to_string()))?;

        if target == self_id {
            return Err(ForgetMemberError::CannotForgetSelf);
        }

        let record = mesh.members.get(&target).expect("just resolved");
        let was_aliased = aliased.contains(&target);
        let name = record.name.clone();

        if !record.is_active() {
            // Idempotent: already retired, nothing to do and nothing to
            // report as if it had happened.
            return Ok(ForgottenMember {
                name,
                node_id: target,
                was_aliased,
                already_retired: true,
            });
        }
        let live = record.status == commonwealth_core::mesh::NodeStatus::Online;
        if live && !was_aliased && !force {
            return Err(ForgetMemberError::MemberStillLive(name));
        }
        let record = mesh.members.get_mut(&target).expect("just resolved");
        record.removed_at = Some(now);
        record.last_seen = record.last_seen.max(now);
        record.status = commonwealth_core::mesh::NodeStatus::Offline;
        Ok(ForgottenMember {
            name,
            node_id: target,
            was_aliased,
            already_retired: false,
        })
    }
}
