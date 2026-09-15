// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest-lookup port, the vocabulary it publishes, and the resolver.
//!
//! `sovereign/SERVING_BOUNDARY.md` "The five entries" (a): a guest link is a
//! PIN, not a candidate, so it crosses into Serving through its OWN port
//! ([`GuestLenderSource`]) and never through the roster port. The two ports
//! are deliberately not one: the roster enumerates (`Vec`), the guest lookup
//! resolves a model id (`Option`), and the guest's `invalidate()` on a 401 is
//! a question the roster has no way to ask.
//!
//! # A lender is not a peer
//!
//! A [`GuestLender`] is deliberately NOT an `InferenceVenue`. That type is
//! peer-shaped — a required `NodeId` (a link carries an iroh endpoint pubkey,
//! not a mesh node id), plus `system_ram_gb` / `benchmark` /
//! `current_in_flight` / `gossip_last_seen_unix`, every one a gossip signal a
//! lender has none of and all of them feeding the peer scorer.
//!
//! The semantics differ too, and that is the real reason. Peer routing SCORES
//! candidates; a guest link is a PIN. The operator ran `svrn mesh use` and
//! named the lender, so it is not a candidate to be weighed against peers.
//!
//! [`GrantPosture`] is three states and not an `Option` for the same reason
//! the module below exists: `None` would mean both "this node has no guest
//! link" and "this node has a live link the lender just refused", and those
//! demand opposite behaviour. [`GrantPosture::Unusable`] is NOT
//! [`GrantPosture::NoLink`].
//!
//! # The two reaches, through ports
//!
//! [`StoredGuestLink`] resolves a granted id by reading the holder's link file
//! and, when the link names an iroh endpoint, opening a mesh tunnel. Neither
//! the file (`sovereign_core::guest_link` — the holder's credential, which
//! Serving only consumes; ARCH 12) nor the tunnel (`sovereign_mesh::
//! guest_tunnel`, Fabric reach) is the package's to name, so both arrive
//! through ports: [`GuestLinkReader`] and [`GuestTunnelOpener`], both
//! implemented by the daemon.
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
//! # The lender's `/v1/models` is the authority on scope, not the link
//!
//! A link's `summary` is display only. What a grant actually buys lives in
//! the ISSUING node's store, and `/v1/models` under the bearer returns
//! exactly the granted ids — which is what `svrn mesh use` already verifies
//! against. Caching a scope from the link would be a second answer to that
//! question and would go stale the instant the lender revoked (§10.6).

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::RwLock;

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
/// a file on disk — mirroring `VenueSource`.
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

/// A live guest link, projected to the fields the resolver reads.
///
/// The stored form — the file, its location, its expiry semantics — is the
/// holder's own (`sovereign_core::guest_link`). Serving only consumes it, so
/// it arrives through [`GuestLinkReader`] rather than being named here
/// (ARCH 12: the credential is the holder's, not Serving's). `expires_at` and
/// `summary` are absent on purpose: the reader returns only links within
/// their window, and `summary` is display-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveGuestLink {
    /// The grant token, presented as `Authorization: Bearer`.
    pub token: String,
    /// Base URL of the issuing node's client API — no trailing `/v1`.
    pub url: String,
    /// The lender's iroh dial string, when the plaintext API is not the way
    /// in.
    pub dial: Option<String>,
}

/// Reads this node's live guest link. The daemon implements it over
/// `sovereign_core::guest_link`.
pub trait GuestLinkReader: Send + Sync + std::fmt::Debug {
    /// The link when one is present AND within its stated window; `None`
    /// otherwise.
    fn live_link(&self) -> Option<LiveGuestLink>;
}

/// The default reader: this node holds no guest link.
#[derive(Debug, Default)]
pub struct NoGuestLinks;

impl GuestLinkReader for NoGuestLinks {
    fn live_link(&self) -> Option<LiveGuestLink> {
        None
    }
}

/// A live mesh tunnel to a lender: the local bridge URL requests ride.
pub trait GuestTunnelHandle: Send + Sync + std::fmt::Debug {
    /// The loopback base URL the tunnel serves the lender at.
    fn base_url(&self) -> &str;
}

/// Opens the mesh tunnel a link's iroh dial string names. The daemon
/// implements it over `sovereign_mesh::guest_tunnel` (Fabric reach).
#[async_trait]
pub trait GuestTunnelOpener: Send + Sync + std::fmt::Debug {
    /// Dial `dial` and return a tunnel serving it at a loopback base URL.
    /// `relay_urls` / `discovery` are the guest's OWN `[iroh]` config.
    async fn open(
        &self,
        dial: &str,
        relay_urls: Vec<String>,
        discovery: Option<String>,
    ) -> Result<Arc<dyn GuestTunnelHandle>, String>;
}

/// The default opener: this host opens no tunnels. It REFUSES rather than
/// falling back to the link's plaintext URL, because a link naming an iroh
/// endpoint means the lender's plaintext API is closed on purpose (§18.3).
#[derive(Debug, Default)]
pub struct NoTunnels;

#[async_trait]
impl GuestTunnelOpener for NoTunnels {
    async fn open(
        &self,
        dial: &str,
        _relay_urls: Vec<String>,
        _discovery: Option<String>,
    ) -> Result<Arc<dyn GuestTunnelHandle>, String> {
        Err(format!(
            "this host has no mesh tunnel opener to dial {dial} with"
        ))
    }
}

/// How long a fetched model list stays good. Short, because a grant can be
/// revoked at any moment and the lender is the only one who knows.
const SCOPE_TTL: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct CachedScope {
    /// Model ids the lender served under this bearer.
    ids: Vec<String>,
    /// The token they were fetched for — a re-`mesh use` with a new grant
    /// must not inherit the old grant's scope.
    token: String,
    fetched_at: Instant,
}

/// Resolves a granted model id against the lender, through its two ports.
#[derive(Debug)]
pub struct StoredGuestLink {
    http: reqwest::Client,
    scope: RwLock<Option<CachedScope>>,
    /// The open tunnel, keyed by the dial string it was opened for. A link
    /// re-issued after a lender restart carries NEW ephemeral ports, so the
    /// key is what makes a stale tunnel get replaced instead of reused.
    tunnel: RwLock<Option<(String, Arc<dyn GuestTunnelHandle>)>>,
    /// Reads the holder's link file — `sovereign_core::guest_link`, reached
    /// through the daemon (ARCH 12).
    links: Arc<dyn GuestLinkReader>,
    /// Opens the mesh tunnel — `sovereign_mesh::guest_tunnel`, Fabric reach.
    opener: Arc<dyn GuestTunnelOpener>,
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
    /// Build a resolver over the daemon's link reader and tunnel opener.
    ///
    /// Takes both ports rather than a path ON PURPOSE. It was once
    /// constructed with the daemon's `cfg.data.dir`, which is a DIFFERENT
    /// directory — this operator's is `~/.sovereign` while the CLI writes
    /// `~/.svrnmesh` — so the lookup silently found nothing and the whole
    /// guest route was dead with no error anywhere. The unit tests could not
    /// see it: they handed both sides the same tempdir, so the two roots were
    /// equal by construction. Only the two-machine run caught it. The reader
    /// port owns the root now, and `svrnmesh_root()` is the SSOT both halves
    /// already resolve through (§7.6, §10.6).
    pub fn new(links: Arc<dyn GuestLinkReader>, opener: Arc<dyn GuestTunnelOpener>) -> Self {
        Self {
            http: reqwest::Client::new(),
            scope: RwLock::new(None),
            tunnel: RwLock::new(None),
            links,
            opener,
        }
    }

    /// The base URL for `link`, opening a mesh tunnel first when it names an
    /// iroh endpoint. Mirrors the CLI's `guest_link::open_route`, and for the
    /// same reason: nothing else may turn a link into an address, or a bearer
    /// goes out in plaintext to a mesh that closed plaintext on purpose.
    async fn route_for(&self, link: &LiveGuestLink) -> Option<String> {
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
        let (relay_urls, discovery) = sovereign_contracts::setup_config::SetupConfig::load()
            .map(|c| (c.iroh.relay_urls.clone(), c.iroh.discovery.clone()))
            .unwrap_or_default();
        match self.opener.open(dial, relay_urls, discovery).await {
            Ok(t) => {
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
    async fn granted_ids(&self, link: &LiveGuestLink, base: &str) -> Result<Vec<String>, String> {
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
        link: LiveGuestLink,
        base: String,
        ids: Vec<String>,
    },
}

impl StoredGuestLink {
    /// The live link, its route, and what it buys — the one place those three
    /// are resolved together, so "what may I name" and "where do I send it"
    /// can never disagree.
    async fn resolve(&self) -> Resolved {
        let Some(link) = self.links.live_link() else {
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

    /// The planted NEGATIVE: a node with no link must lend nothing and must
    /// read as `NoLink`, the state that routes local/peer normally.
    #[tokio::test]
    async fn a_node_with_no_link_lends_nothing() {
        assert!(NoGuestLenders.lender_for("anything").await.is_none());
        assert_eq!(NoGuestLenders.posture().await, GrantPosture::NoLink);
    }

    /// The planted POSITIVE: a source holding a live grant returns the
    /// dispatch material for a granted id and `Granted` for its posture. A
    /// port whose only control is the null source would pass even if
    /// `lender_for` never returned `Some`.
    #[tokio::test]
    async fn a_live_grant_resolves_and_reports_granted() {
        let src = StubSource {
            granted: vec!["lent-model".to_string()],
            state: GrantPosture::Granted {
                lender: "https://lender.example:9741".to_string(),
                ids: vec!["lent-model".to_string()],
            },
        };
        let lender = src
            .lender_for("lent-model")
            .await
            .expect("a granted id must resolve to a lender");
        assert_eq!(lender.bearer, "token");
        assert!(src.lender_for("ungranted").await.is_none());
        assert_eq!(
            src.posture().await,
            GrantPosture::Granted {
                lender: "https://lender.example:9741".to_string(),
                ids: vec!["lent-model".to_string()],
            }
        );
    }

    /// `Unusable` is NOT `NoLink` (SERVING_BOUNDARY.md (a)). A refused grant
    /// must not resolve (no silent fallback to the local model), but its
    /// posture must still carry the lender and the reason so the refusal is
    /// visible rather than absent.
    #[tokio::test]
    async fn an_unusable_link_is_not_an_absent_one() {
        let src = StubSource {
            granted: vec![],
            state: GrantPosture::Unusable {
                lender: "https://lender.example:9741".to_string(),
                why: "the lending node answered 403".to_string(),
            },
        };
        assert!(
            src.lender_for("lent-model").await.is_none(),
            "a refused grant must not be served from elsewhere"
        );
        match src.posture().await {
            GrantPosture::NoLink => panic!("Unusable must not read as NoLink"),
            GrantPosture::Unusable { why, .. } => assert!(why.contains("403")),
            GrantPosture::Granted { .. } => panic!("a refused grant is not Granted"),
        }
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

    /// The link port's default answers absence, never a link. The resolver
    /// must read that as `NoLink` — the state that routes local/peer normally.
    #[tokio::test]
    async fn an_absent_link_resolves_to_nothing() {
        let src = StoredGuestLink::new(Arc::new(NoGuestLinks), Arc::new(NoTunnels));
        assert!(src.lender_for("some-model").await.is_none());
        assert_eq!(src.posture().await, GrantPosture::NoLink);
    }

    /// A link whose iroh tunnel cannot be opened is `Unusable`, NOT `NoLink`
    /// (the 2026-08-28 silent-substitution defect reached by another route).
    /// The default opener refuses rather than downgrading to the plaintext URL.
    #[tokio::test]
    async fn an_unopenable_tunnel_is_not_an_absent_link() {
        let links = StubLinks(Some(LiveGuestLink {
            token: "t".into(),
            url: "https://lender.example:9741".into(),
            dial: Some("beef@127.0.0.1:9999".into()),
        }));
        let src = StoredGuestLink::new(Arc::new(links), Arc::new(NoTunnels));
        assert!(
            src.lender_for("some-model").await.is_none(),
            "an unopenable tunnel must not resolve to the lender's plaintext URL"
        );
        match src.posture().await {
            GrantPosture::NoLink => panic!("an unopenable tunnel must not read as NoLink"),
            GrantPosture::Unusable { why, .. } => {
                assert!(why.contains("could not be opened"), "{why}")
            }
            GrantPosture::Granted { .. } => panic!("an unopenable tunnel is not Granted"),
        }
    }

    /// The tunnel opener's planted POSITIVE: an opener that returns a bridge
    /// URL is consulted, so the branch is exercised rather than only the
    /// default that refuses. The `/v1/models` fetch then fails to connect
    /// (nothing is listening), which is `Unusable` — the point is that the
    /// opener was reached and its base URL used.
    #[tokio::test]
    async fn an_opener_that_returns_a_bridge_is_reached() {
        let links = StubLinks(Some(LiveGuestLink {
            token: "t".into(),
            url: "https://lender.example:9741".into(),
            dial: Some("beef@127.0.0.1:9999".into()),
        }));
        let opener = Arc::new(StubOpener::new("http://127.0.0.1:1"));
        let src = StoredGuestLink::new(Arc::new(links), opener.clone());
        assert!(src.lender_for("some-model").await.is_none());
        assert!(
            opener.opened.load(std::sync::atomic::Ordering::SeqCst),
            "a dial-bearing link must reach the tunnel opener"
        );
    }

    #[derive(Debug)]
    struct StubSource {
        granted: Vec<String>,
        state: GrantPosture,
    }

    #[async_trait]
    impl GuestLenderSource for StubSource {
        async fn lender_for(&self, model_id: &str) -> Option<GuestLender> {
            self.granted
                .iter()
                .any(|i| i == model_id)
                .then(|| GuestLender {
                    base_url: "http://127.0.0.1:1/v1".to_string(),
                    bearer: "token".to_string(),
                    display: "https://lender.example:9741".to_string(),
                })
        }
        async fn posture(&self) -> GrantPosture {
            self.state.clone()
        }
        async fn invalidate(&self) {}
    }

    #[derive(Debug)]
    struct StubLinks(Option<LiveGuestLink>);

    impl GuestLinkReader for StubLinks {
        fn live_link(&self) -> Option<LiveGuestLink> {
            self.0.clone()
        }
    }

    #[derive(Debug)]
    struct StubOpener {
        base_url: &'static str,
        opened: std::sync::atomic::AtomicBool,
    }

    impl StubOpener {
        fn new(base_url: &'static str) -> Self {
            Self {
                base_url,
                opened: std::sync::atomic::AtomicBool::new(false),
            }
        }
    }

    #[derive(Debug)]
    struct StubHandle(&'static str);

    impl GuestTunnelHandle for StubHandle {
        fn base_url(&self) -> &str {
            self.0
        }
    }

    #[async_trait]
    impl GuestTunnelOpener for StubOpener {
        async fn open(
            &self,
            _dial: &str,
            _relay_urls: Vec<String>,
            _discovery: Option<String>,
        ) -> Result<Arc<dyn GuestTunnelHandle>, String> {
            self.opened.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Arc::new(StubHandle(self.base_url)))
        }
    }
}
