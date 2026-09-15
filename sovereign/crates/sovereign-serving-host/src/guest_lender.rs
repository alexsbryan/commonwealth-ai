// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest-lookup port, and the vocabulary it publishes.
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
//! The implementation that reads the link file and opens the tunnel is host
//! wiring and lives beside the rest of the knot; this module carries the port
//! and the published language only, so a lift of the package carries no
//! `guest_tunnel` or `guest_link` reach.

use async_trait::async_trait;

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
}
