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
//! **It does not admit joiners.** There is no `/internal/join` here and no
//! invite minting: a mesh is founded and grown by a full daemon, and this
//! process joins one. That is what keeps it small enough to lift.
//!
//! **It does not join over LAN/mDNS.** An invite without an iroh dial string
//! is refused by name (see [`join`]).
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
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
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
pub mod gossip;
pub mod identity;
pub mod internal;
pub mod join;

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
    #[error(
        "no mesh at {0} — join first: `cw-rails join <invite>`. \
         An invite comes from a member of the mesh you want to be on."
    )]
    NoMesh(PathBuf),
    #[error("the iroh endpoint would not bind: {0}")]
    Endpoint(String),
    #[error("could not listen on {0}: {1}")]
    Listen(SocketAddr, std::io::Error),
    #[error(
        "`listen` resolved to {0}, which is not loopback — this API has no auth \
         because loopback IS the auth, so binding it anywhere else would publish it"
    )]
    NotLoopback(SocketAddr),
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
    /// Where `POST /internal/gossip` is served. Ephemeral loopback, reachable
    /// only through the acceptor.
    pub internal_addr: SocketAddr,
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

        let (internal_addr, internal) = internal::serve(
            mesh.clone(),
            contacts.clone(),
            node.self_id,
            node.data_dir.clone(),
        )
        .await?;

        let published_apps = commonwealth_media::PublishedApps::default();
        let acceptor = acceptor::spawn(
            node.endpoint.clone(),
            internal_addr,
            mesh.clone(),
            node.config.media.origin,
            node.config.media.allow.clone(),
            // Read once, here, from the same data dir that holds the node key.
            // Not from `rails.toml`: `MediaSection` is `deny_unknown_fields`,
            // so a new key there would make an UN-upgraded daemon refuse to
            // boot rather than ignore it -- see `commonwealth_media::declared`.
            commonwealth_media::read_declared_in(&commonwealth_media::dir_under(&node.data_dir)),
            published_apps.clone(),
        );

        Ok(Self {
            node,
            mesh,
            transport,
            contacts,
            gauge: Arc::new(AtomicUsize::new(0)),
            published_apps,
            internal_addr,
            _internal: internal,
            _acceptor: acceptor,
        })
    }

    /// Load the persisted mesh and start. The refusal when there is none is
    /// the one a first-time operator will see, so it names the next command.
    pub async fn start_from_disk(node: RailsNode) -> Result<Self, Refusal> {
        let Some(mesh) = identity::load_mesh(&node.data_dir)? else {
            return Err(Refusal::NoMesh(identity::mesh_file(&node.data_dir)));
        };
        Self::start(node, mesh).await
    }

    /// Serve the loopback API and gossip forever. Returns only on a listener
    /// failure; a gossip round that fails is a logged round, not an exit.
    pub async fn run(self) -> Result<(), Refusal> {
        let listen = api::bind_addr(self.node.config.listen);
        let daemon = Arc::new(self);
        let api = api::serve(daemon.clone(), listen).await?;
        let gossip = tokio::spawn(gossip::run_forever(daemon.clone()));
        tracing::info!(
            target: "rails",
            api = %listen,
            internal = %daemon.internal_addr,
            members = daemon.mesh.read().await.members.len(),
            "rails: serving"
        );
        // The API task owns the process's lifetime; a gossip round that fails
        // is a logged round, never an exit.
        let _ = api.await;
        gossip.abort();
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
