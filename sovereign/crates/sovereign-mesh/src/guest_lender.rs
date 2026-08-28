// SPDX-License-Identifier: AGPL-3.0-or-later
//! Resolving a granted model id to the node that lent it.
//!
//! # Why the DAEMON holds the guest link
//!
//! `svrn chat ask` is a surface: the turn runs on the daemon and its result
//! arrives as a value (`chat_cmd/ask.rs` module docs). So the CLI cannot
//! "borrow a model" by repointing its own base URL — doing that sends the
//! whole CONVERSATION to the lender, where `/v1/conversations` is in no
//! `Scope` and is not served on the guest listener at all. Observed on the
//! wire 2026-08-28 (live bar 3.3): `POST <bridge>/v1/conversations -> 403`.
//!
//! A guest's conversation is their own state and must never leave their
//! machine. Only the completion crosses. That means the guest's OWN daemon
//! holds the link, runs the turn locally, and dispatches the named model here.
//!
//! # A lender is not a peer
//!
//! Deliberately NOT expressed as a [`PeerInferenceEndpoint`]. That type is
//! peer-shaped — a required `NodeId` (a link carries an iroh endpoint pubkey,
//! not a mesh node id), plus `system_ram_gb` / `benchmark` /
//! `current_in_flight` / `gossip_last_seen_unix`, every one a gossip signal a
//! lender has none of and all of them feeding `select_peer`'s scorer.
//!
//! The semantics differ too, and that is the real reason. Peer routing SCORES
//! candidates; a guest link is a PIN. The operator ran `svrn mesh use` and
//! named the lender, so it is not a candidate to be weighed against peers.
//!
//! # Two things that must differ from `provider_for_peer`
//!
//! Both are live defects if the peer path is copied:
//!
//! 1. **Send the real model id.** `provider_for_peer` calls
//!    `with_placeholder_model_id()`, which puts the receiver on its
//!    explicit-name path, resolves to nobody and 503s — and a placeholder
//!    cannot satisfy the lender's scope check, which matches on the model
//!    NAME (proven live: bar 3.4 refuses an ungranted id with
//!    `model_not_granted`).
//! 2. **Do not stamp `X-Node-Id`.** The peer path stamps it deliberately, so
//!    the peer's pause / foreground-yield / `max_peer_inflight` ceiling
//!    engage. A guest is not a node; stamping would run the lender's PEER
//!    admission on a non-peer and mis-attribute the traffic in its tally.
//!
//! [`PeerInferenceEndpoint`]: crate::daemon::PeerInferenceEndpoint
//!
//! # The lender's `/v1/models` is the authority on scope, not the link
//!
//! A link's `summary` is display only. What a grant actually buys lives in
//! the ISSUING node's store, and `/v1/models` under the bearer returns
//! exactly the granted ids — which is what `svrn mesh use` already verifies
//! against. Caching a scope from the link would be a second answer to that
//! question and would go stale the instant the lender revoked (§10.6).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::RwLock;

use sovereign_core::guest_link::{self, GuestLink};

/// How long a fetched model list stays good. Short, because a grant can be
/// revoked at any moment and the lender is the only one who knows.
const SCOPE_TTL: Duration = Duration::from_secs(60);

/// A lender this node holds a live guest link with, resolved to something
/// dispatchable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestLender {
    /// Where to send `/v1/chat/completions` — the tunnel's local bridge when
    /// the link carries a dial string, else the link's plain URL. Never
    /// `link.url` when a dial is present: that mesh closed its plaintext
    /// ingress on purpose and there is no plaintext fallback (§18.3).
    pub base_url: String,
    /// The grant token, presented as `Authorization: Bearer`.
    pub bearer: String,
    /// The lender's advertised URL, for glassbox and attribution. Display
    /// only — never used to build a request.
    pub display: String,
}

/// "Do I hold a live grant for this model id?"
///
/// A trait so the dispatch path can be tested without a lender, a tunnel, or
/// a file on disk — mirroring `PeerEndpointSource`.
#[async_trait]
pub trait GuestLenderSource: Send + Sync + std::fmt::Debug {
    /// The lender to dispatch `model_id` to, or `None` to fall through to the
    /// ordinary local/peer resolution.
    async fn lender_for(&self, model_id: &str) -> Option<GuestLender>;

    /// Every model id this node's guest link currently buys, with the
    /// lender's display name.
    ///
    /// `/v1/models` MUST include these. The listing's contract is that it
    /// matches what name resolution can actually serve — omitting a model
    /// `locate_named_model` will happily route is the same lie, in the other
    /// direction, that the peer listing was fixed for (§10.6).
    async fn granted_models(&self) -> Option<(String, Vec<String>)>;

    /// Called when the lender refuses a dispatch with 401. The grant is gone —
    /// expired, revoked, or the lender restarted (its store is RAM-only) — so
    /// the cached scope must not keep claiming the model is reachable.
    async fn invalidate(&self);
}

/// The null source: a node with no guest link, which is almost every node.
#[derive(Debug, Default)]
pub struct NoGuestLenders;

#[async_trait]
impl GuestLenderSource for NoGuestLenders {
    async fn lender_for(&self, _model_id: &str) -> Option<GuestLender> {
        None
    }
    async fn granted_models(&self) -> Option<(String, Vec<String>)> {
        None
    }
    async fn invalidate(&self) {}
}

#[derive(Debug)]
struct CachedScope {
    /// Model ids the lender served under this bearer.
    ids: Vec<String>,
    /// The token they were fetched for — a re-`mesh use` with a new grant
    /// must not inherit the old grant's scope.
    token: String,
    fetched_at: Instant,
}

/// Reads `<root>/guest.json` and resolves granted ids against the lender.
#[derive(Debug)]
pub struct StoredGuestLink {
    root: PathBuf,
    http: reqwest::Client,
    scope: RwLock<Option<CachedScope>>,
    /// The open tunnel, keyed by the dial string it was opened for. A link
    /// re-issued after a lender restart carries NEW ephemeral ports, so the
    /// key is what makes a stale tunnel get replaced instead of reused.
    tunnel: RwLock<Option<(String, Arc<crate::guest_tunnel::GuestTunnel>)>>,
}

impl StoredGuestLink {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            http: reqwest::Client::new(),
            scope: RwLock::new(None),
            tunnel: RwLock::new(None),
        }
    }

    /// The base URL for `link`, opening a mesh tunnel first when it names an
    /// iroh endpoint. Mirrors the CLI's `guest_link::open_route`, and for the
    /// same reason: nothing else may turn a link into an address, or a bearer
    /// goes out in plaintext to a mesh that closed plaintext on purpose.
    async fn route_for(&self, link: &GuestLink) -> Option<String> {
        let Some(dial) = link.dial.as_deref() else {
            return Some(link.url.clone());
        };
        if let Some((open_for, t)) = self.tunnel.read().await.as_ref() {
            if open_for == dial {
                return Some(t.base_url().to_string());
            }
        }
        // The guest's OWN iroh posture, not the lender's: a node that severed
        // n0 discovery must not be put back on it by accepting a lend.
        let (relay_urls, discovery) = sovereign_core::setup_config::SetupConfig::load()
            .map(|c| (c.iroh.relay_urls.clone(), c.iroh.discovery.clone()))
            .unwrap_or_default();
        match crate::guest_tunnel::GuestTunnel::open(dial, relay_urls, discovery.as_deref()).await {
            Ok(t) => {
                let t = Arc::new(t);
                let base = t.base_url().to_string();
                *self.tunnel.write().await = Some((dial.to_string(), t));
                tracing::info!(
                    target: "transport",
                    lender = %link.url,
                    bridge = %base,
                    "guest-lender: opened the mesh tunnel to a lending node"
                );
                Some(base)
            }
            Err(e) => {
                // No plaintext fallback. A link naming an iroh endpoint means
                // the lender's plaintext API is closed; sending the bearer to
                // `link.url` anyway would defeat the reason it is closed.
                tracing::warn!(
                    target: "transport",
                    lender = %link.url,
                    error = %e,
                    "guest-lender: could not open the mesh tunnel — the model will \
                     resolve as unavailable rather than being served from elsewhere"
                );
                None
            }
        }
    }

    /// Model ids this grant currently buys, straight from the lender.
    async fn granted_ids(&self, link: &GuestLink, base: &str) -> Vec<String> {
        if let Some(c) = self.scope.read().await.as_ref() {
            if c.token == link.token && c.fetched_at.elapsed() < SCOPE_TTL {
                return c.ids.clone();
            }
        }
        let url = format!("{}/v1/models", base.trim_end_matches('/'));
        let ids: Vec<String> = match self
            .http
            .get(&url)
            .bearer_auth(&link.token)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| {
                    v.get("data").and_then(|d| d.as_array()).map(|rows| {
                        rows.iter()
                            .filter_map(|m| m.get("id").and_then(|i| i.as_str()))
                            .map(str::to_string)
                            .collect()
                    })
                })
                .unwrap_or_default(),
            Ok(r) => {
                tracing::info!(
                    lender = %link.url,
                    status = %r.status(),
                    "guest-lender: the lending node refused the grant — it has expired, \
                     been revoked, or the lender restarted (grants are held in memory)"
                );
                Vec::new()
            }
            Err(e) => {
                tracing::warn!(lender = %link.url, error = %e, "guest-lender: unreachable");
                Vec::new()
            }
        };
        *self.scope.write().await = Some(CachedScope {
            ids: ids.clone(),
            token: link.token.clone(),
            fetched_at: Instant::now(),
        });
        ids
    }
}

impl StoredGuestLink {
    /// The live link, its route, and what it buys — the one place those three
    /// are resolved together, so "what may I name" and "where do I send it"
    /// can never disagree.
    async fn resolve(&self) -> Option<(GuestLink, String, Vec<String>)> {
        let link = guest_link::load_live_in(&self.root, guest_link::now_secs())?;
        let base = self.route_for(&link).await?;
        let ids = self.granted_ids(&link, &base).await;
        Some((link, base, ids))
    }
}

#[async_trait]
impl GuestLenderSource for StoredGuestLink {
    async fn lender_for(&self, model_id: &str) -> Option<GuestLender> {
        let (link, base, ids) = self.resolve().await?;
        if !ids.iter().any(|i| i == model_id) {
            return None;
        }
        Some(GuestLender {
            base_url: format!("{}/v1", base.trim_end_matches('/')),
            bearer: link.token.clone(),
            display: link.url.clone(),
        })
    }

    async fn granted_models(&self) -> Option<(String, Vec<String>)> {
        let (link, _, ids) = self.resolve().await?;
        if ids.is_empty() {
            return None;
        }
        Some((link.url.clone(), ids))
    }

    async fn invalidate(&self) {
        *self.scope.write().await = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_node_with_no_link_lends_nothing() {
        assert!(NoGuestLenders.lender_for("anything").await.is_none());
    }

    /// An absent `guest.json` is the overwhelmingly common case and must be a
    /// cheap `None`, never an error or a panic.
    #[tokio::test]
    async fn an_absent_guest_file_resolves_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let src = StoredGuestLink::new(dir.path().to_path_buf());
        assert!(src.lender_for("some-model").await.is_none());
    }

    /// An EXPIRED link must not resolve, without any network call — the
    /// window is the guest's own half of the contract.
    #[tokio::test]
    async fn an_expired_link_resolves_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        guest_link::save_in(
            dir.path(),
            &GuestLink {
                token: "t".into(),
                // Unroutable on purpose: if expiry were not checked FIRST this
                // would hang or error rather than returning a clean None.
                url: "http://127.0.0.1:1".into(),
                dial: None,
                expires_at: 1,
                summary: None,
            },
        )
        .unwrap();
        let src = StoredGuestLink::new(dir.path().to_path_buf());
        assert!(src.lender_for("some-model").await.is_none());
    }
}
