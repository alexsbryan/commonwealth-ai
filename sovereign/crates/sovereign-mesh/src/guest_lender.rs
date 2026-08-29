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

/// What this node's guest link is worth RIGHT NOW.
///
/// # Why this is three states and not an `Option`
///
/// It was an `Option<(String, Vec<String>)>`, and `None` meant both "this
/// node has no guest link" and "this node has a live link the lender just
/// refused". Those demand opposite behaviour: the first should route
/// normally, the second must not quietly answer from the local model —
/// that is the silent substitution §18.3 forbids, and it is the SAME defect
/// the two-machine run was convened to catch, reached by a different route.
///
/// Observed live 2026-08-28: the lending node's service manager restarted it
/// (grants are held in RAM), MAC's next four requests got `403`, and every
/// one of them was answered by MAC's own 27B with nothing said. The operator
/// had asked to borrow a model and got their own, and no surface disagreed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantPosture {
    /// No guest link on this node — the overwhelmingly common case. Route
    /// local/peer as if the feature did not exist.
    NoLink,
    /// A link that is live BY ITS OWN TTL, which the lender is nonetheless
    /// not honouring: revoked, the lender restarted, or the tunnel to it
    /// cannot be opened. Never treated as `NoLink`.
    Unusable {
        /// The lender's display URL, for the error the operator reads.
        lender: String,
        /// Why, in the words the operator needs — a status code, or the
        /// transport failure. Carried, not summarised: "refused" and
        /// "unreachable" have different repairs.
        why: String,
    },
    /// A live link the lender is honouring, and what it currently buys.
    Granted { lender: String, ids: Vec<String> },
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

    /// What this node's guest link is worth right now.
    ///
    /// `/v1/models` MUST include a `Granted` posture's ids. The listing's
    /// contract is that it matches what name resolution can actually serve —
    /// omitting a model `locate_named_model` will happily route is the same
    /// lie, in the other direction, that the peer listing was fixed for
    /// (§10.6). `Unusable` is equally load-bearing: it is what stops a
    /// refused grant being served as if it were an absent one.
    async fn posture(&self) -> GrantPosture;

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
    async fn posture(&self) -> GrantPosture {
        GrantPosture::NoLink
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

/// Is `base_url`'s loopback listener still accepting connections?
///
/// A TCP connect, not an HTTP request: the question is whether the BRIDGE is
/// alive, and a request would also exercise the lender, the grant and the
/// network — so a refusal could not be attributed. One connect answers
/// exactly one question (§18.4).
async fn tunnel_is_accepting(base_url: &str) -> bool {
    let Some(authority) = base_url.strip_prefix("http://") else {
        // Not a loopback bridge URL — nothing local to probe, so do not claim
        // it is dead. A link with no `dial=` resolves to the lender's own URL
        // and never reaches this path.
        return true;
    };
    let authority = authority.trim_end_matches('/');
    match tokio::time::timeout(
        Duration::from_millis(750),
        tokio::net::TcpStream::connect(authority),
    )
    .await
    {
        Ok(Ok(_)) => true,
        Ok(Err(e)) => {
            tracing::debug!(
                target: "transport",
                bridge = %base_url,
                error = %e,
                "guest-lender: cached tunnel refused a probe connection"
            );
            false
        }
        Err(_) => {
            tracing::debug!(
                target: "transport",
                bridge = %base_url,
                "guest-lender: cached tunnel did not accept within 750ms"
            );
            false
        }
    }
}

impl StoredGuestLink {
    /// Read `guest.json` from the SAME root `svrn mesh use` writes it to.
    ///
    /// Takes no path ON PURPOSE. It was constructed with the daemon's
    /// `cfg.data.dir` for one afternoon, which is a DIFFERENT directory —
    /// this operator's is `~/.sovereign` while the CLI writes `~/.svrnmesh`
    /// — so the lookup silently found nothing and the whole guest route was
    /// dead with no error anywhere. The unit tests could not see it: they
    /// hand both sides the same tempdir, so the two roots were equal by
    /// construction. Only the two-machine run caught it.
    ///
    /// `svrnmesh_root()` is the SSOT both halves already resolve through, so
    /// there is nothing left for a caller to get wrong (§7.6, §10.6).
    pub fn new() -> Self {
        Self::new_in(sovereign_contracts::rebrand::svrnmesh_root())
    }

    /// Test-only escape hatch. Production must use [`Self::new`] — a root
    /// passed in is a root that can disagree with the writer's.
    pub fn new_in(root: PathBuf) -> Self {
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
            // A CACHED TUNNEL IS A CLAIM, AND IT IS CHECKED BEFORE IT IS USED.
            //
            // Handing back `t.base_url()` on the strength of the key alone
            // assumes the bridge behind it is still accepting. It need not be:
            // the bridge's accept loop can exit, and the `GuestTunnel` can be
            // dropped by a provider rebuild, either of which leaves this entry
            // naming a port that refuses connections. Nothing here noticed,
            // so every later request went to the dead address and surfaced as
            // "the lending node refused the grant" — a true-sounding error
            // about the wrong subject (§18.3).
            //
            // Observed live 2026-08-28: tunnel opened 21:11:10 on port 61564,
            // served the `/v1/models` listing, and at 21:12:39 the completion
            // got connection-refused on that same port while the daemon was
            // still up and the grant still valid.
            if open_for == dial {
                if tunnel_is_accepting(t.base_url()).await {
                    return Some(t.base_url().to_string());
                }
                tracing::warn!(
                    target: "transport",
                    lender = %link.url,
                    stale_bridge = %t.base_url(),
                    "guest-lender: the cached mesh tunnel is no longer accepting —                      reopening rather than sending the request to a dead port"
                );
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
    async fn granted_ids(&self, link: &GuestLink, base: &str) -> Result<Vec<String>, String> {
        if let Some(c) = self.scope.read().await.as_ref() {
            if c.token == link.token && c.fetched_at.elapsed() < SCOPE_TTL {
                return Ok(c.ids.clone());
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
                let status = r.status();
                tracing::info!(
                    lender = %link.url,
                    status = %status,
                    "guest-lender: the lending node refused the grant — it has expired, \
                     been revoked, or the lender restarted (grants are held in memory)"
                );
                // The REASON travels with the failure. Collapsing it to an
                // empty vec here is what made a refused grant indistinguishable
                // from no grant three layers up.
                *self.scope.write().await = None;
                return Err(format!("the lending node answered {status}"));
            }
            Err(e) => {
                tracing::warn!(lender = %link.url, error = %e, "guest-lender: unreachable");
                *self.scope.write().await = None;
                return Err(format!("the lending node was unreachable: {e}"));
            }
        };
        *self.scope.write().await = Some(CachedScope {
            ids: ids.clone(),
            token: link.token.clone(),
            fetched_at: Instant::now(),
        });
        Ok(ids)
    }
}

/// [`StoredGuestLink::resolve`]'s three outcomes, carrying what each needs.
///
/// The public [`GrantPosture`] is this minus the dispatch material; they are
/// derived from one another rather than computed twice (§10.6).
enum Resolved {
    NoLink,
    Unusable {
        lender: String,
        why: String,
    },
    Ok {
        link: GuestLink,
        base: String,
        ids: Vec<String>,
    },
}

impl StoredGuestLink {
    /// The live link, its route, and what it buys — the one place those three
    /// are resolved together, so "what may I name" and "where do I send it"
    /// can never disagree.
    async fn resolve(&self) -> Resolved {
        let Some(link) = guest_link::load_live_in(&self.root, guest_link::now_secs()) else {
            return Resolved::NoLink;
        };
        let Some(base) = self.route_for(&link).await else {
            // A link whose tunnel will not open is NOT the same as no link.
            // Returning `None` here is how an unopenable tunnel used to read
            // as "this node never borrowed anything".
            return Resolved::Unusable {
                lender: link.url.clone(),
                why: "the mesh tunnel to the lending node could not be opened".to_string(),
            };
        };
        match self.granted_ids(&link, &base).await {
            Ok(ids) if ids.is_empty() => Resolved::Unusable {
                lender: link.url.clone(),
                why: "the grant currently covers no models".to_string(),
            },
            Ok(ids) => Resolved::Ok { link, base, ids },
            Err(why) => Resolved::Unusable {
                lender: link.url.clone(),
                why,
            },
        }
    }
}

#[async_trait]
impl GuestLenderSource for StoredGuestLink {
    async fn lender_for(&self, model_id: &str) -> Option<GuestLender> {
        let Resolved::Ok { link, base, ids } = self.resolve().await else {
            return None;
        };
        if !ids.iter().any(|i| i == model_id) {
            return None;
        }
        Some(GuestLender {
            base_url: format!("{}/v1", base.trim_end_matches('/')),
            bearer: link.token.clone(),
            display: link.url.clone(),
        })
    }

    async fn posture(&self) -> GrantPosture {
        match self.resolve().await {
            Resolved::NoLink => GrantPosture::NoLink,
            Resolved::Unusable { lender, why } => GrantPosture::Unusable { lender, why },
            Resolved::Ok { link, ids, .. } => GrantPosture::Granted {
                lender: link.url.clone(),
                ids,
            },
        }
    }

    async fn invalidate(&self) {
        *self.scope.write().await = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE regression, and the one the unit tests were structurally blind to.
    ///
    /// `StoredGuestLink::new()` must read the SAME file `svrn mesh use`
    /// writes. It was built from the daemon's `cfg.data.dir` for one
    /// afternoon — a configurable directory that on the operator's machine
    /// was `~/.sovereign` while the CLI wrote `~/.svrnmesh` — so every
    /// lookup found nothing, silently, and the guest route was dead with no
    /// error on any surface.
    ///
    /// Every other test here passes an explicit tempdir to both halves, so
    /// the two roots are equal by construction and none of them could fail.
    /// This one asserts the PRODUCTION root against the SSOT the CLI
    /// resolves through.
    #[test]
    fn the_default_root_is_the_one_the_cli_writes_to() {
        let expected = sovereign_contracts::rebrand::svrnmesh_root();
        assert_eq!(
            StoredGuestLink::new().root,
            expected,
            "the daemon must read guest.json where `svrn mesh use` put it; a \
             configurable data dir is NOT that place"
        );
        assert_eq!(
            guest_link::path_in(&StoredGuestLink::new().root)
                .file_name()
                .unwrap(),
            "guest.json"
        );
    }

    /// The liveness probe must actually distinguish a live bridge from a dead
    /// one. A probe that returns `true` unconditionally would restore exactly
    /// the bug it was written for, and every other test here would still pass.
    #[tokio::test]
    async fn the_probe_tells_a_live_bridge_from_a_dead_one() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        assert!(
            tunnel_is_accepting(&base).await,
            "a bound listener must probe as accepting"
        );

        drop(listener);
        assert!(
            !tunnel_is_accepting(&base).await,
            "a dropped listener must probe as DEAD — this is the check that \
             stops a cached tunnel handing out a port that refuses connections"
        );
    }

    /// A link with no `dial=` resolves to the lender's own URL and never opens
    /// a local bridge, so there is nothing loopback to probe. The probe must
    /// not report those as dead — that would refuse a perfectly good plaintext
    /// link on the strength of a check that does not apply to it.
    #[tokio::test]
    async fn a_non_bridge_url_is_not_reported_dead() {
        assert!(tunnel_is_accepting("https://lender.example:9741").await);
    }

    #[tokio::test]
    async fn a_node_with_no_link_lends_nothing() {
        assert!(NoGuestLenders.lender_for("anything").await.is_none());
    }

    /// An absent `guest.json` is the overwhelmingly common case and must be a
    /// cheap `None`, never an error or a panic.
    #[tokio::test]
    async fn an_absent_guest_file_resolves_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let src = StoredGuestLink::new_in(dir.path().to_path_buf());
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
        let src = StoredGuestLink::new_in(dir.path().to_path_buf());
        assert!(src.lender_for("some-model").await.is_none());
    }
}
