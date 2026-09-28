// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The minimal rails daemon** — the process that IS your address on the
//! mesh, with media registered on it and nothing else.
//!
//! # What a shim author installs
//!
//! A Jellyswarrm-shaped shim beside a media server needs four things from a
//! mesh and none of the rest of one: an identity other members can dial, a
//! roster that says who those members are, a way for a member's player to
//! reach this machine's origin, and a way for this machine to reach theirs.
//! `cw-rails join <invite>` and `cw-rails run` are that, and the shim sees
//! three loopback routes plus one header:
//!
//! - `GET  /v1/mesh/status` — who I am, who is on the roster, who offers media.
//! - `GET  /v1/mesh/media[?peer=]` — the catalogue, or a loopback URL that
//!   reaches one member's origin by key.
//! - `POST /v1/mesh/media/fanout` — the same request to every offering member,
//!   one attributed row each.
//! - `X-Mesh-Member` / `X-Mesh-Node` / `X-Mesh-Pubkey` on every request the
//!   local origin receives from the mesh.
//!
//! No key, no relay, no port-forward, no VPN.
//!
//! # What it deliberately does NOT do
//!
//! **It founds and admits without a full daemon** (phase-b pb-membership,
//! reversing five-programs-21). `cw-rails found` starts a mesh ([`found`]),
//! `/v1/mesh/status` carries its invite as `join_link`, and `/internal/join`
//! admits through the membership decider the inference daemon uses
//! ([`internal`]). Its mesh is keyed by this process's own node key.
//!
//! **It speaks mDNS by key only.** `run --mdns` advertises this member's node
//! pubkey on the LAN, and a join whose invite carries no dial dials a keyed
//! founder it finds there over iroh; with none, the join is refused by name
//! (see [`join`], [`lan`]). A daemon's plaintext mDNS port is never dialed.
//!
//! **It has nothing Jellyfin in it.** The shim is a separate distribution —
//! GPL-2 against this repo's AGPL — and the rails are the product here.
//!
//! # Why it is its own binary
//!
//! `commonwealth-api`'s dependency closure is 743 crates (it carries the
//! corpus engine, arrow, and the sovereign runtime). The set this composes —
//! `commonwealth-core`, `-transport`, `-media`, `-discovery` and their leaves
//! — is 319 (`cargo tree --edges normal`, both measured 2026-09-11; the lift
//! sandbox resolves 518 packages, which is the same closure plus the
//! dev-dependencies a third party carries with the tests). `scripts/cw-rails-lift.sh --sandbox` is the instrument that
//! proves the difference is real rather than a crate-name count: it copies
//! the closure outside the repo, builds it there, and runs THIS daemon
//! against a live mesh.
//!
//! Everything below is glue. Identity, the endpoint, the acceptor, the
//! bridge, dial-by-key, the `Mesh` merge and its proofs, the wire structs and
//! all three media questions are owned elsewhere and composed here.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use commonwealth_core::ids::{NodeId, NodePubkey};
use commonwealth_core::mesh::Mesh;
use commonwealth_transport::fanout::InflightGauge;
use commonwealth_transport::iroh::{
    build_relayed_endpoint, Endpoint, IrohAcceptor, IrohTransport, SecretKey, ALPN, MEDIA_ALPN,
};
use commonwealth_transport::PeerTransport;
use ed25519_dalek::SigningKey;
use tokio::sync::{Mutex, RwLock};

pub mod acceptor;
pub mod api;
pub mod cli;
pub mod config;
pub mod found;
pub mod gossip;
pub mod identity;
pub mod internal;
pub mod join;
pub mod kv;
pub mod lan;
pub mod known;
pub mod ledger;
pub mod membership;
pub mod origins;
pub mod presence;
pub mod rail;
pub mod work;

pub use config::Config;

/// Why a start did not happen. Every variant names the thing that was absent
/// or wrong; none of them is a fall-back.
#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    #[error("{0}")]
    Config(#[from] config::ConfigRefusal),
    #[error("{0}")]
    Store(#[from] identity::StoreRefusal),
    #[error("{0}")]
    Join(#[from] join::JoinRefusal),
    #[error("{0}")]
    Found(#[from] found::FoundRefusal),
    #[error("the iroh endpoint would not bind: {0}")]
    Endpoint(String),
    #[error("the mesh store would not open: {0}")]
    KvStore(#[from] commonwealth_state::Error),
    #[error("could not listen on {0}: {1}")]
    Listen(SocketAddr, std::io::Error),
    #[error(
        "`listen` resolved to {0}, which is not loopback — this API has no auth \
         because loopback IS the auth, so binding it anywhere else would publish it"
    )]
    NotLoopback(SocketAddr),
    #[error(
        "{0} is held by another cw-rails on this data root — stop that cw-rails, \
         or give this one its own --data-dir"
    )]
    RootHeld(PathBuf),
    #[error("could not open the data-root lock {0}: {1}")]
    RootLock(PathBuf, std::io::Error),
    #[error(
        "{0} is no longer the lock this cw-rails claimed ({1}) — its data root is gone \
         or replaced, so it exits rather than acknowledge writes it cannot keep"
    )]
    RootLost(PathBuf, String),
}

impl Refusal {
    /// The process exit code this refusal is worth. `3` is could-not-judge —
    /// a precondition of the run is absent, so nothing was attempted and
    /// nothing is claimed. Everything else is `1`: the daemon ran and refused.
    pub fn exit_code(&self) -> u8 {
        match self {
            // A data dir that cannot be created is the host, not the mesh.
            Refusal::Store(identity::StoreRefusal::DataDir(_, _)) => 3,
            _ => 1,
        }
    }
}

/// The file `run` holds for its whole life, so ONE cw-rails serves a data
/// root (fp-solo-lift). The lock itself is `host_kit::RunLock` (pb-hostkit).
pub const ROOT_LOCK: &str = "rails.lock";

/// Claim `<data_dir>/rails.lock` through the host kit's one lock. The returned
/// claim IS the lock: drop it and the root is free, which the OS also does
/// when the process dies.
pub fn claim_root(data_dir: &Path) -> Result<host_kit::RunLock, Refusal> {
    std::fs::create_dir_all(data_dir)
        .map_err(|e| identity::StoreRefusal::DataDir(data_dir.to_path_buf(), e))?;
    match host_kit::RunLock::acquire(data_dir, ROOT_LOCK) {
        Ok(lock) => {
            tracing::info!(lock = %lock.path().display(), "cw-rails: data root claimed");
            Ok(lock)
        }
        Err(host_kit::RunLockError::Held { path }) => {
            tracing::warn!(lock = %path.display(), "cw-rails: data root held by another cw-rails");
            Err(Refusal::RootHeld(path))
        }
        Err(host_kit::RunLockError::Unopenable { path, source }) => {
            tracing::warn!(lock = %path.display(), error = %source, "cw-rails: root lock would not open");
            Err(Refusal::RootLock(path, source))
        }
        Err(host_kit::RunLockError::Unlockable { path, source }) => {
            tracing::warn!(lock = %path.display(), error = %source, "cw-rails: root lock would not take");
            Err(Refusal::RootLock(path, source))
        }
    }
}

/// How often `run` checks that `rails.lock` is still the file it claimed.
pub const ROOT_WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Resolve when `<data_dir>/rails.lock` is no longer `held` — unlinked (the
/// root was deleted) or replaced — compared by (dev, ino) every `every`.
/// cw-rails owns its root and its lifetime (principle 12): nothing else
/// reaps a cw-rails whose root is gone, so it reaps itself (five-programs-66).
#[cfg(unix)]
pub async fn root_lost(
    data_dir: &Path,
    held: &host_kit::RunLock,
    every: std::time::Duration,
) -> Refusal {
    use std::os::unix::fs::MetadataExt;
    let path = data_dir.join(ROOT_LOCK);
    let ours = match held.metadata() {
        Ok(m) => (m.dev(), m.ino()),
        Err(e) => return Refusal::RootLost(path, format!("the held lock cannot be read: {e}")),
    };
    tracing::info!(lock = %path.display(), every = ?every, "cw-rails: watching the data root's lock");
    let mut tick = tokio::time::interval(every);
    loop {
        tick.tick().await;
        let why = match std::fs::metadata(&path) {
            Ok(m) if (m.dev(), m.ino()) == ours => continue,
            Ok(_) => "another file now sits at that path".to_string(),
            Err(e) => format!("the path is gone: {e}"),
        };
        tracing::error!(lock = %path.display(), why = %why, "cw-rails: the data root's lock is no longer ours — exiting");
        return Refusal::RootLost(path, why);
    }
}

/// No (dev, ino) to compare off unix: the watch never fires, and says so.
#[cfg(not(unix))]
pub async fn root_lost(
    data_dir: &Path,
    _held: &host_kit::RunLock,
    _every: std::time::Duration,
) -> Refusal {
    tracing::warn!(lock = %data_dir.join(ROOT_LOCK).display(), "cw-rails: no root-loss watch on this platform");
    std::future::pending().await
}

/// Identity plus the ONE long-lived iroh endpoint. Both verbs need this much;
/// only `run` needs the rest.
///
/// The endpoint is built once and shared: the join handshake, every outbound
/// gossip round, the acceptor and every media bridge ride the same one. A
/// second endpoint would be a second node key on the wire and a second set of
/// hole-punched paths for the same member.
pub struct RailsNode {
    pub data_dir: PathBuf,
    pub config: Config,
    pub key: SigningKey,
    pub self_id: NodeId,
    pub endpoint: Endpoint,
}

impl RailsNode {
    pub async fn bind(data_dir: PathBuf, config: Config) -> Result<Self, Refusal> {
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| identity::StoreRefusal::DataDir(data_dir.clone(), e))?;
        let key = commonwealth_transport::identity::load_or_generate_node_key(&data_dir);
        let self_id = identity::load_or_generate_node_id(&data_dir)?;
        // Two ALPNs at bind: `cwth/http/0` carries join and gossip,
        // `cwth/media/0` carries a member's player. `cwth/app/0` is the third
        // and is NOT here, because whether this node serves it changes while
        // the daemon runs — the acceptor adds and removes it as the app
        // registry fills and empties. The acceptor routes by ALPN *and* by
        // who dialed — see `acceptor`.
        let endpoint = build_relayed_endpoint(
            SecretKey::from_bytes(&key.to_bytes()),
            vec![ALPN.to_vec(), MEDIA_ALPN.to_vec()],
            &config.relay_config(),
        )
        .await
        .map_err(Refusal::Endpoint)?;
        tracing::info!(
            target: "rails",
            node_id = %self_id,
            name = %config.name,
            pubkey = %hex::encode(commonwealth_transport::identity::node_pubkey(&key).0),
            dial = ?commonwealth_transport::iroh::format_dial_string(&endpoint.addr()),
            "rails: endpoint bound"
        );
        Ok(Self {
            data_dir,
            config,
            key,
            self_id,
            endpoint,
        })
    }

    pub fn pubkey(&self) -> NodePubkey {
        commonwealth_transport::identity::node_pubkey(&self.key)
    }

    /// The self-only roster a node with no active mesh runs on. Its invite
    /// key is discarded, so nobody can join it, and it is never persisted.
    pub fn solo_mesh(&self) -> Mesh {
        commonwealth_discovery::membership::init_mesh_with_identity(
            "solo",
            &self.config.name,
            Vec::new(),
            self.self_id,
            Some(self.pubkey()),
            false,
        )
        .0
    }
}

/// The running daemon: a node, the mesh it converges, and the three listeners
/// that make it reachable.
pub struct RailsDaemon {
    pub node: RailsNode,
    /// The one roster. Every reader clones what it needs out of the lock
    /// before dialing anything — no lock is held across a round trip.
    pub mesh: Arc<RwLock<Mesh>>,
    pub transport: Arc<dyn PeerTransport>,
    /// Local-clock time we last had contact with each peer, keyed by node id.
    /// Offline decay measures THIS, never the peer's own gossiped
    /// `last_seen` — a clock-skewed live peer must not read as stale.
    pub contacts: Arc<Mutex<HashMap<NodeId, u64>>>,
    /// In-flight media fan-outs, reported by `/v1/mesh/status`.
    pub gauge: InflightGauge,
    /// What this node publishes as named HTTP apps. Claims only — a rails
    /// node has no `[iroh.apps]` equivalent and deliberately does not grow
    /// one (see `acceptor`), so everything here arrived through the loopback
    /// publish API and goes away with the process that took it.
    pub published_apps: commonwealth_media::PublishedApps,
    /// Every origin this endpoint serves to members — its own, the app entry
    /// (`published_apps`), and each program's registration. The acceptor
    /// table, the advertised ALPNs and the gossiped claims are read from it
    /// (see [`origins`]).
    pub origins: commonwealth_media::origins::OriginRegistry,
    /// The ring rail: journals under THIS process's data root (`rings/`),
    /// signed with the node key, membership as every ring's default roster.
    /// The doors over it are [`rail`]. Constructed in `start`, so it lives
    /// exactly as long as the daemon.
    pub rail: Arc<commonwealth_rail::RingRail>,
    /// The live lane's arrived-payload buffer — delivery, not record;
    /// nothing here reaches a store, a journal or a disk. See
    /// [`rail::LiveBuffer`].
    pub rail_live: rail::LiveBuffer,
    /// The mesh store, projected from `rail`'s journals and pumped back onto
    /// them; served at `/v1/mesh/kv/*`. See [`kv`].
    pub kv: Arc<kv::KvHost>,
    /// The presence poll's last reading (`presence::run_forever` writes it,
    /// the gossip round stamps it into `NodeCapabilities::media_available`,
    /// `GET /v1/mesh/media/presence` serves it). `None` is "nobody answered",
    /// never "free" — the poll publishes its arms, not a guess.
    pub media_presence: Arc<std::sync::RwLock<Option<f32>>>,
    /// Where `POST /internal/gossip` is served. Ephemeral loopback, reachable
    /// only through the acceptor.
    pub internal_addr: SocketAddr,
    /// No active mesh: the roster is this node alone, in memory only, and
    /// neither the gossip round nor the presence poll ticks (five-programs-63).
    /// Live, because the membership doors ([`membership`]) move a running
    /// node between solo and meshed; read it with [`RailsDaemon::is_solo`].
    solo: AtomicBool,
    /// The raw invite key: present only on the member that founded its mesh
    /// (`cw-rails found` or `POST /v1/mesh/create`) or rotated its invite.
    /// Read it with [`RailsDaemon::join_key`].
    join_key: std::sync::RwLock<Option<String>>,
    /// Serializes the membership verbs with each other and with the gossip
    /// round's write of `mesh.json`, so a round that began before a `leave`
    /// cannot write the left mesh back after the doors removed it.
    pub(crate) verbs: Mutex<()>,
    /// mDNS: `None` unless `run --mdns` asked for it, then the running
    /// advertisement or the reason there is none (see [`lan`]).
    pub lan: Option<Result<lan::Lan, String>>,
    _internal: tokio::task::JoinHandle<()>,
    _acceptor: IrohAcceptor,
}

impl RailsDaemon {
    /// Start the two inbound halves — the internal gossip listener and the
    /// iroh acceptor in front of it — around an already-loaded mesh.
    pub async fn start(node: RailsNode, mesh: Mesh) -> Result<Self, Refusal> {
        let mesh = Arc::new(RwLock::new(mesh));
        let contacts: Arc<Mutex<HashMap<NodeId, u64>>> = Arc::new(Mutex::new(HashMap::new()));
        let transport: Arc<dyn PeerTransport> = Arc::new(IrohTransport::new(node.endpoint.clone()));

        // The ring rail's storage (five-programs fp-44): one journal
        // directory per ring namespace under THIS process's data dir,
        // signed with the same identity key the endpoint proves — rails'
        // own, not the daemon's (one loader, two data dirs). Its lines verify
        // because rails joined as a member with that key, and render as the
        // daemon's person while both keep the hostname default name (see
        // `rail::tests::rails_and_the_daemon_sign_with_two_keys_under_one_person`). Membership
        // is every ring's default roster, derived at read time; the mesh
        // Arc below is what the source reads, held weakly beside the rail.
        let rail = Arc::new(commonwealth_rail::RingRail::new(
            &node.data_dir,
            Arc::new(node.key.clone()),
        ));
        rail::MembershipRosterSource::install(&rail, &mesh, node.self_id, Some(node.pubkey()));
        // The mesh store over those journals (fp-77). Empty until `run`'s
        // pump task projects the journals on disk into it.
        let kv = Arc::new(kv::KvHost::new(
            rail.clone(),
            mesh.clone(),
            node.self_id,
            Some(node.pubkey()),
        )?);

        let (internal_addr, internal) = internal::serve(
            mesh.clone(),
            contacts.clone(),
            node.self_id,
            node.data_dir.clone(),
        )
        .await?;

        let join_key = identity::load_join_key(&node.data_dir)?;
        let origins = commonwealth_media::origins::OriginRegistry::new(
            commonwealth_media::PublishedApps::default(),
        );
        let published_apps = origins.apps().clone();
        origins::stand_own(
            &origins,
            internal_addr,
            node.config.media.origin,
            node.config.media.allow.clone(),
            // Read once, here, from the same data dir that holds the node key.
            // Not from `rails.toml`: `MediaSection` is `deny_unknown_fields`,
            // so a new key there would make an UN-upgraded daemon refuse to
            // boot rather than ignore it -- see `commonwealth_media::declared`.
            commonwealth_media::read_declared_in(&commonwealth_media::dir_under(&node.data_dir)),
        )
        .expect("an empty registry holds the endpoint's own origins");
        let acceptor = acceptor::spawn(node.endpoint.clone(), mesh.clone(), origins.clone());

        Ok(Self {
            node,
            mesh,
            transport,
            contacts,
            gauge: Arc::new(AtomicUsize::new(0)),
            published_apps,
            origins,
            rail,
            rail_live: rail::LiveBuffer::default(),
            kv,
            media_presence: Arc::new(std::sync::RwLock::new(None)),
            internal_addr,
            solo: AtomicBool::new(false),
            join_key: std::sync::RwLock::new(join_key),
            verbs: Mutex::new(()),
            lan: None,
            _internal: internal,
            _acceptor: acceptor,
        })
    }

    /// Load the persisted mesh and start — or, with none on disk, start SOLO
    /// (five-programs-63): a self-only roster held in memory and never
    /// written to `mesh.json`, so a later `cw-rails join` still finds no mesh
    /// in its way and the next start is meshed. The stores keep their rows
    /// across that move: the local-only rehydrate keys on the node key, which
    /// a join does not change.
    pub async fn start_from_disk(node: RailsNode) -> Result<Self, Refusal> {
        if let Some(mesh) = identity::load_mesh(&node.data_dir)? {
            tracing::info!(target: "rails", mode = "meshed", mesh = %mesh.name,
                           members = mesh.members.len(), "rails: starting meshed");
            return Self::start(node, mesh).await;
        }
        tracing::info!(target: "rails", mode = "solo",
                       absent = %identity::mesh_file(&node.data_dir).display(),
                       "rails: no mesh, starting solo — self-only roster, no gossip, no peers");
        let mesh = node.solo_mesh();
        let daemon = Self::start(node, mesh).await?;
        daemon.solo.store(true, Ordering::SeqCst);
        Ok(daemon)
    }

    /// No active mesh: this node alone, in memory only.
    pub fn is_solo(&self) -> bool {
        self.solo.load(Ordering::SeqCst)
    }

    /// The invite key this node holds for its active mesh, if it holds one.
    pub fn join_key(&self) -> Option<String> {
        self.join_key
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Hold `join_key` as the active mesh's invite key (a rotate).
    pub(crate) fn set_join_key(&self, join_key: Option<String>) {
        *self
            .join_key
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = join_key;
    }

    /// Replace what this node is a member of — the one place the live
    /// membership state changes. The caller holds `verbs` and has already
    /// written (or cleared) the files at the root, so a restart comes up in
    /// the same state this leaves.
    pub(crate) async fn swap_membership(&self, mesh: Mesh, join_key: Option<String>, solo: bool) {
        tracing::info!(target: "rails", mesh = %mesh.name, mesh_id = %mesh.id, solo,
                       holds_invite = join_key.is_some(), "membership: the active mesh changed");
        *self.mesh.write().await = mesh;
        self.set_join_key(join_key);
        self.solo.store(solo, Ordering::SeqCst);
        // Contacts are evidence about the members of the mesh they were seen
        // on; carried across they would decay the next roster's rows wrongly.
        self.contacts.lock().await.clear();
        if self.lan.is_some() {
            tracing::warn!(target: "rails", "membership: mDNS still advertises the mesh it was started on — restart `cw-rails run --mdns` to announce this one");
        }
    }

    /// Announce this member on the LAN by key (`run --mdns`). A solo node
    /// has no mesh to announce, and a LAN that refuses multicast is a named
    /// absence on `/v1/mesh/status`, never an exit.
    pub async fn advertise_on_lan(&mut self) {
        let lan = if self.is_solo() {
            Err("solo: there is no mesh to advertise".to_string())
        } else {
            lan::advertise(&self.node, &*self.mesh.read().await)
        };
        if let Err(why) = &lan {
            tracing::warn!(target: "rails", why = %why, "lan: not advertising on mDNS");
        }
        self.lan = Some(lan);
    }

    /// Serve the loopback API and gossip forever. Returns only on a listener
    /// failure; a gossip round that fails is a logged round, not an exit.
    pub async fn run(self) -> Result<(), Refusal> {
        let listen = api::bind_addr(self.node.config.listen);
        let daemon = Arc::new(self);
        // Ready means projected (phase-b-5): a door that answered before the
        // journals were folded would read absent for rows they hold.
        daemon.kv.project_all_on_disk().await;
        let api = api::serve(daemon.clone(), listen).await?;
        // Both run from the start because a membership door can end solo
        // while this runs; each skips its tick while solo, which has no one to
        // gossip with and no roster to stamp a presence reading into. The
        // presence poll (five-programs fp-46): the reading lands in
        // `media_presence`, which the gossip round and the presence route
        // read. A tick that cannot ask is a logged `None`, never an exit.
        let gossip = tokio::spawn(gossip::run_forever(daemon.clone()));
        let presence = tokio::spawn(presence::run_forever(daemon.clone()));
        // The mesh store's pump (fp-77): drain the outbox onto the journals
        // every tick.
        let kv_pump = tokio::spawn(kv::run_forever(daemon.kv.clone()));
        // The contributions ledger's retention sweep (fp-78), over that store.
        let ledger_gc = tokio::spawn(ledger::run_retention_gc(daemon.kv.store.clone()));
        tracing::info!(
            target: "rails",
            api = %listen,
            internal = %daemon.internal_addr,
            members = daemon.mesh.read().await.members.len(),
            solo = daemon.is_solo(),
            "rails: serving"
        );
        // The API task owns the process's lifetime; a gossip round that fails
        // is a logged round, never an exit.
        let _ = api.await;
        gossip.abort();
        presence.abort();
        kv_pump.abort();
        ledger_gc.abort();
        Ok(())
    }

    /// The roster as the media questions see it, cloned out of the lock.
    pub async fn roster(
        &self,
    ) -> Vec<(
        commonwealth_media::MediaCandidate,
        commonwealth_transport::PeerContact,
    )> {
        commonwealth_media::roster_of(&*self.mesh.read().await)
    }

    /// The live iroh path to every member that has a key. One `path_to` per
    /// member; nothing is dialed that is not already connected.
    pub async fn paths(&self) -> Vec<(NodeId, commonwealth_media::PeerTransportPath)> {
        let keyed: Vec<(NodeId, NodePubkey)> = {
            let mesh = self.mesh.read().await;
            mesh.members
                .values()
                .filter(|m| m.removed_at.is_none())
                .filter_map(|m| m.node_pubkey.map(|k| (m.node_id, k)))
                .collect()
        };
        let mut out = Vec::new();
        for (id, key) in keyed {
            if let Some(p) = commonwealth_media::path_to(&self.node.endpoint, &key.0).await {
                out.push((id, p));
            }
        }
        out
    }
}

/// Note local-clock contact with a peer. The ONE writer of the contact map,
/// so inbound gossip, outbound gossip and the decay pass cannot disagree
/// about what "we heard from them" means (ARCH §10.6).
pub async fn note_contact(contacts: &Arc<Mutex<HashMap<NodeId, u64>>>, peer: NodeId, now: u64) {
    contacts.lock().await.insert(peer, now);
}
