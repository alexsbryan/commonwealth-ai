// SPDX-License-Identifier: AGPL-3.0-or-later
//! The endpoint's self-heal (phase-b pb-rails-parity): the reachability
//! watchdog ([`crate::iroh_watchdog`]) over THIS process's one endpoint, and
//! the rebuild it escalates to — what the inference daemon runs over its own
//! endpoint (daemon.rs's watchdog spawn), here so the flip turns nothing off.
//!
//! A rebuild re-binds with the same node key and relay posture
//! ([`RailsNode::bind_endpoint`], the one binder) and swaps the endpoint, the
//! dial-by-key transport over it and the acceptor in front of it as ONE value
//! ([`LiveEndpoint`]), so no reader can hold a transport over one endpoint
//! while the acceptor listens on another. Runtime readers go through
//! [`RailsDaemon::endpoint`] and [`RailsDaemon::transport`];
//! `RailsNode::endpoint` is the endpoint the node bound at start.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, PoisonError};

use commonwealth_core::mesh::Mesh;
use commonwealth_media::origins::OriginRegistry;
use commonwealth_transport::iroh::{format_dial_string, Endpoint, IrohAcceptor, IrohTransport};
use commonwealth_transport::PeerTransport;
use tokio::sync::RwLock;

use crate::iroh_watchdog::{
    self, ReachPathObservation, ReachPathsFn, RebuildFn, WatchdogConfig, WatchdogHandle,
};
use crate::{acceptor, Config, RailsDaemon, RailsNode};

/// The endpoint a running daemon serves on, with its transport and acceptor.
pub struct LiveEndpoint(std::sync::RwLock<Live>);

struct Live {
    endpoint: Endpoint,
    transport: Arc<dyn PeerTransport>,
    _acceptor: IrohAcceptor,
}

impl Live {
    /// The transport and the acceptor over `endpoint`. The acceptor installs
    /// its ALPN hook on `origins`, which replaces the previous endpoint's.
    fn over(endpoint: Endpoint, mesh: Arc<RwLock<Mesh>>, origins: OriginRegistry) -> Self {
        Self {
            transport: Arc::new(IrohTransport::new(endpoint.clone())),
            _acceptor: acceptor::spawn(endpoint.clone(), mesh, origins),
            endpoint,
        }
    }
}

impl LiveEndpoint {
    pub(crate) fn stand(
        endpoint: Endpoint,
        mesh: Arc<RwLock<Mesh>>,
        origins: OriginRegistry,
    ) -> Self {
        Self(std::sync::RwLock::new(Live::over(endpoint, mesh, origins)))
    }

    pub fn endpoint(&self) -> Endpoint {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .endpoint
            .clone()
    }

    pub fn transport(&self) -> Arc<dyn PeerTransport> {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .transport
            .clone()
    }

    /// Put `next` in place and hand back the endpoint it replaced. The old
    /// acceptor stops as it drops here.
    fn replace(&self, next: Live) -> Endpoint {
        let old = std::mem::replace(
            &mut *self.0.write().unwrap_or_else(PoisonError::into_inner),
            next,
        );
        old.endpoint
    }
}

/// The watchdog's last-resort recovery: bind a fresh endpoint with this
/// node's key, swap it in, close the old one, and hand the fresh one back
/// for the watchdog to judge from here on.
pub async fn rebuild(daemon: &RailsDaemon) -> Result<Endpoint, String> {
    let fresh = RailsNode::bind_endpoint(&daemon.node.key, &daemon.node.config)
        .await
        .map_err(|e| e.to_string())?;
    let old = daemon.live.replace(Live::over(
        fresh.clone(),
        daemon.mesh.clone(),
        daemon.origins.clone(),
    ));
    tracing::info!(
        target: "rails",
        old = ?format_dial_string(&old.addr()),
        fresh = ?format_dial_string(&fresh.addr()),
        "self-heal: endpoint rebuilt and swapped in; closing the old one"
    );
    old.close().await;
    Ok(fresh)
}

/// The watchdog's tunables for this node's relay posture, as the daemon sets
/// them: the self-discovery probe runs only with n0 discovery, and relay-home
/// is a health signal only when the node uses a relay at all.
pub fn watchdog_config(config: &Config) -> WatchdogConfig {
    let relay = config.relay_config();
    let mut cfg = WatchdogConfig::from_env();
    cfg.self_probe = relay.n0_services;
    cfg.relays_expected = relay.n0_services || !relay.relay_urls.is_empty();
    cfg
}

/// Spawn the watchdog over the daemon's live endpoint, with [`rebuild`] as
/// its recovery and the roster's keyed members as its peer-path term. Its
/// status is what `/v1/mesh/status` reports as `self_reachability`.
pub fn spawn_watchdog(daemon: Arc<RailsDaemon>, cfg: WatchdogConfig) -> WatchdogHandle {
    let heal = daemon.clone();
    let rebuild: RebuildFn = Arc::new(move || {
        let daemon = heal.clone();
        let fut: Pin<Box<dyn Future<Output = Result<Endpoint, String>> + Send>> =
            Box::pin(async move { rebuild(&daemon).await });
        fut
    });
    let eye = daemon.clone();
    let peer_paths: ReachPathsFn = Arc::new(move |endpoint| {
        let daemon = eye.clone();
        let fut: Pin<Box<dyn Future<Output = Vec<ReachPathObservation>> + Send>> =
            Box::pin(async move {
                iroh_watchdog::observe_peer_paths(&daemon.mesh, daemon.node.self_id, &endpoint)
                    .await
            });
        fut
    });
    let handle = iroh_watchdog::spawn(daemon.endpoint(), rebuild, Some(peer_paths), cfg);
    if daemon.reachability.set(handle.status_arc()).is_err() {
        tracing::warn!(target: "rails", "self-heal: a watchdog was already spawned; status reads the first");
    }
    handle
}
