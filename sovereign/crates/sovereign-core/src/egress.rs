// SPDX-License-Identifier: AGPL-3.0-or-later
//! The egress boundary — the ONE choke point every remote-model call
//! and every search-query egress passes through (order
//! deep-research-t2a, R10).
//!
//! Two facts are enforced here, structurally, never by a model:
//!
//! 1. **One construction site.** Every HTTP client that can carry a
//!    payload to a third party is built by [`search_client`] or
//!    [`model_client`] in THIS file. The F26 census
//!    (`sovereign-core/tests/f26_egress_census.rs`) is the build gate:
//!    a `reqwest::Client` construction anywhere else in the workspace
//!    fails the census, and this file is registered `Boundary`.
//!
//! 2. **One release rule** ([`verify`], one decider, one name — ARCH
//!    §10.6). A payload leaves this machine iff:
//!      - its custody is `PublicWeb` (the bar's unconditional
//!        release — web material is egress-releasable), OR
//!      - a run-scoped [`ConsentGrant`] covers its custody (the
//!        operator's typed grant, default-deny, recorded in the run
//!        manifest), OR
//!      - it is a QUERY formed verbatim by the user (`user_formed` —
//!        the user's own words leaving at the user's own action; the
//!        chat tool path).
//!    Everything else refuses, typed, naming what was withheld.
//!    `Unknown` custody always refuses (custody.rs: `Unknown` never
//!    rides a released record).
//!
//! The consent grant is run-scoped and never a model judgment (§7.6):
//! the CLI's `--consent <class>` flag builds it once at launch, it is
//! frozen into the run's charter (FR-3), carried by the port, and
//! recorded in `manifest.json` (`Manifest.consent`).
//!
//! Every egress event — released or refused — is traced at
//! `tracing=debug` under this module's target: provider, payload
//! class, exact-payload size, custody proof, and (when one released
//! it) the grant's run id + release floor.

use std::fmt;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::types::{Custody, SearchPrivacy};


/// A run-scoped typed consent grant: the operator's release of a
/// custody floor for ONE run. Default-deny — the absence of a grant
/// releases nothing but public-web material. Recorded in the run
/// manifest; never produced or amended by a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ConsentGrant {
    /// The run this grant is scoped to (e.g. `dr-1786720584`).
    pub run_id: String,
    /// When the operator granted it (unix seconds).
    pub granted_at_unix: i64,
    /// The most restrictive custody class this grant releases:
    /// `personal` covers all classes, `peer` covers peer + public-web,
    /// `public-web` covers public-web only. A grant never releases
    /// `unknown` provenance.
    pub release_floor: Custody,
}

impl ConsentGrant {
    /// True iff this grant releases payloads of the given custody: a
    /// payload releases when it is AT MOST as restrictive as the
    /// floor (`restrictiveness(payload) <= restrictiveness(floor)`).
    /// Floor `public-web` (0) therefore releases public-web only;
    /// floor `personal` (2) releases every non-unknown class. The
    /// inverse comparison would let a public-web grant release
    /// personal payloads — the test
    /// `grant_floor_covers_and_refuses_by_class` pins the correct
    /// direction.
    pub fn covers(&self, payload: Custody) -> bool {
        // ONE implementation of the custody ordering (ARCH §10.6). The
        // comparison used to be a private `restrictiveness` fn in this file;
        // it moved to `kernel_types::Custody` at rung nc-11-answer, when the
        // MESH boundary (`PeerAnswer::bound_for_peer`) needed the same
        // question answered and a second copy would have been the second
        // decider. This method keeps its name — it is the third-party-egress
        // spelling of the question — and delegates the rule.
        payload.released_by(self.release_floor)
    }
}

/// What is crossing the boundary. The caller declares every field by
/// code (never by a model); `verify` decides on the declaration.
#[derive(Debug, Clone)]
pub struct EgressPayload<'a> {
    /// The egress's privacy posture — consulted at the boundary
    /// (`Local` never leaves; `External { provider }` names the
    /// third party in the trace and the decision).
    pub privacy: SearchPrivacy,
    /// The payload's custody class. `PublicWeb` releases
    /// unconditionally; anything else needs the grant (or the
    /// user-formed-query clause).
    pub custody: Custody,
    /// What is leaving: `"chunk"` | `"query"` | `"url"`.
    pub what: &'a str,
    /// Where it is leaving to: the provider id for `External`
    /// egress (the backend's stable audit id), or a host.
    pub target: &'a str,
    /// The exact payload content — the object of the decision (a
    /// chunk, the query text, the URL). Traced truncated; the full
    /// payload leaves only when the gate releases it.
    pub detail: &'a str,
    /// True iff the payload is a query formed verbatim by the user
    /// (their own words leaving at their own action — the chat tool
    /// path). Machine-formed queries (the loop's gap templates) are
    /// never user-formed and need the run's grant.
    pub user_formed: bool,
}

/// The typed refusal: what was withheld, why, and whether a grant
/// existed. The caller surfaces the message verbatim so the operator
/// sees exactly what the boundary refused and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressRefusal {
    pub custody: Custody,
    pub what: String,
    pub target: String,
    pub grant_present: bool,
    pub reason: String,
}

impl fmt::Display for EgressRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "egress refused: {what} with {custody} custody to {target} — {reason} (grant {grant})",
            what = self.what,
            custody = self.custody,
            target = self.target,
            reason = self.reason,
            grant = if self.grant_present {
                "present but insufficient"
            } else {
                "absent — default-deny"
            },
        )
    }
}

/// The release rule — one decider, one name. Returns the typed
/// refusal when the payload may not leave; every path traces at
/// `tracing=debug`.
pub fn verify(
    payload: &EgressPayload<'_>,
    grant: Option<&ConsentGrant>,
) -> Result<(), EgressRefusal> {
    // The privacy posture is consulted AT the boundary: Local egress
    // never leaves the node (nothing to gate), Mesh rides the estate's
    // own transport (out of this HTTP boundary's scope), External is
    // third-party egress and faces the release rule.
    let provider = match payload.privacy {
        SearchPrivacy::Local => {
            debug!(
                target: "sovereign_core::egress",
                what = payload.what,
                custody = %payload.custody,
                "egress: privacy Local — no third-party egress"
            );
            return Ok(());
        }
        SearchPrivacy::Mesh => {
            debug!(
                target: "sovereign_core::egress",
                what = payload.what,
                custody = %payload.custody,
                "egress: privacy Mesh — peer transport, outside the HTTP boundary"
            );
            return Ok(());
        }
        SearchPrivacy::External { provider } => provider,
    };

    // Unknown provenance never egresses — refuses before any clause.
    if payload.custody == Custody::Unknown {
        return Err(refusal(
            payload,
            grant,
            "unknown provenance never egresses".to_string(),
        ));
    }

    // The bar's unconditional release: public-web custody.
    if payload.custody == Custody::PublicWeb {
        debug!(
            target: "sovereign_core::egress",
            provider = %provider,
            what = payload.what,
            custody = %payload.custody,
            payload_chars = payload.detail.len(),
            detail = %truncate(payload.detail, 200),
            "egress released — public-web custody"
        );
        return Ok(());
    }

    // The operator's run-scoped grant.
    if let Some(g) = grant {
        if g.covers(payload.custody) {
            debug!(
                target: "sovereign_core::egress",
                provider = %provider,
                what = payload.what,
                custody = %payload.custody,
                payload_chars = payload.detail.len(),
                run = %g.run_id,
                release_floor = %g.release_floor,
                detail = %truncate(payload.detail, 200),
                "egress released — run consent grant"
            );
            return Ok(());
        }
    }

    // The user's own words, formed verbatim by the user — the chat
    // tool path's release. Machine-formed payloads never hit this
    // clause.
    if payload.what == "query" && payload.user_formed {
        debug!(
            target: "sovereign_core::egress",
            provider = %provider,
            what = payload.what,
            custody = %payload.custody,
            payload_chars = payload.detail.len(),
            detail = %truncate(payload.detail, 200),
            "egress released — user-formed query"
        );
        return Ok(());
    }

    let reason = match (payload.what, grant) {
        (_, None) => {
            "no run consent grant — the boundary is default-deny for non-public-web payloads"
                .to_string()
        }
        (_, Some(g)) => format!(
            "grant {run} covers up to {floor}, not {custody}",
            run = g.run_id,
            floor = g.release_floor,
            custody = payload.custody,
        ),
    };
    Err(refusal(payload, grant, reason))
}

fn refusal(
    payload: &EgressPayload<'_>,
    grant: Option<&ConsentGrant>,
    reason: String,
) -> EgressRefusal {
    let refusal = EgressRefusal {
        custody: payload.custody,
        what: payload.what.to_string(),
        target: payload.target.to_string(),
        grant_present: grant.is_some(),
        reason,
    };
    debug!(
        target: "sovereign_core::egress",
        custody = %refusal.custody,
        what = %refusal.what,
        target = %refusal.target,
        grant_present = refusal.grant_present,
        reason = %refusal.reason,
        "egress refused — {refusal}"
    );
    refusal
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…[{} chars total]", s.chars().count())
    }
}

/// The boundary's search client factory — the ONE construction site
/// for clients that carry search-query egress. Callers (the
/// deep-research port, the chat web tools, the knowledge-lookup
/// tool) build through here and pass the client into their
/// orchestrator; the census counts this site `Boundary`.
pub fn search_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
}

/// The boundary's remote-model client factory — the ONE construction
/// site for clients that carry chunk payloads to remote providers
/// (the enrich `--provider` dispatch, the t2b frontier judge). The
/// caller still declares payload custody + grant and calls [`verify`]
/// before any request is built; this factory only builds the client.
///
/// The timeout is the CALLER's policy, passed in: enrich's chat path
/// needs its documented 1800s hang headroom (Phase-1 extract on a
/// 27B model can legitimately run 5–15 minutes; a shorter ceiling
/// silently killed real SEP campaign requests, verified 2026-04-25),
/// while a t2b judge call would pass something tighter. The census
/// counts this one construction site `Boundary` regardless of the
/// timeout passed.
pub fn model_client(timeout: std::time::Duration) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder().timeout(timeout).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(custody: Custody, what: &'static str, user_formed: bool) -> EgressPayload<'static> {
        EgressPayload {
            privacy: SearchPrivacy::External {
                provider: "duckduckgo",
            },
            custody,
            what,
            target: "duckduckgo",
            detail: "the exact payload",
            user_formed,
        }
    }

    #[test]
    fn public_web_always_releases_without_a_grant() {
        assert!(verify(&payload(Custody::PublicWeb, "chunk", false), None).is_ok());
        assert!(verify(&payload(Custody::PublicWeb, "url", false), None).is_ok());
    }

    #[test]
    fn personal_chunk_refuses_without_a_grant() {
        let err = verify(&payload(Custody::Personal, "chunk", false), None)
            .expect_err("personal chunk must refuse");
        assert!(err.to_string().contains("personal"), "{}", err);
        assert!(err.to_string().contains("default-deny"), "{}", err);
        assert!(!err.grant_present);
    }

    #[test]
    fn grant_floor_covers_and_refuses_by_class() {
        let public = ConsentGrant {
            run_id: "dr-test".into(),
            granted_at_unix: 1,
            release_floor: Custody::PublicWeb,
        };
        // floor public-web: public-web payloads release, personal refuse.
        assert!(verify(&payload(Custody::PublicWeb, "chunk", false), Some(&public)).is_ok());
        let err = verify(&payload(Custody::Personal, "chunk", false), Some(&public))
            .expect_err("floor public-web must refuse personal");
        assert!(
            err.to_string()
                .contains("covers up to public-web, not personal"),
            "{}",
            err
        );
        assert!(err.grant_present);

        let personal = ConsentGrant {
            run_id: "dr-test".into(),
            granted_at_unix: 1,
            release_floor: Custody::Personal,
        };
        assert!(verify(&payload(Custody::Personal, "chunk", false), Some(&personal)).is_ok());
        assert!(verify(&payload(Custody::Peer, "chunk", false), Some(&personal)).is_ok());
    }

    #[test]
    fn unknown_custody_never_egresses() {
        assert!(verify(&payload(Custody::Unknown, "chunk", false), None).is_err());
        let g = ConsentGrant {
            run_id: "dr-test".into(),
            granted_at_unix: 1,
            release_floor: Custody::Personal,
        };
        assert!(
            verify(&payload(Custody::Unknown, "chunk", false), Some(&g)).is_err(),
            "a grant never releases unknown provenance"
        );
    }

    #[test]
    fn user_formed_query_releases_machine_formed_query_refuses() {
        assert!(verify(&payload(Custody::Personal, "query", true), None).is_ok());
        let err = verify(&payload(Custody::Personal, "query", false), None)
            .expect_err("a machine-formed query needs the run's grant");
        assert!(err.to_string().contains("default-deny"), "{}", err);
    }

    #[test]
    fn local_privacy_never_touches_the_release_rule() {
        let mut p = payload(Custody::Unknown, "chunk", false);
        p.privacy = SearchPrivacy::Local;
        // Local egress is not third-party egress — nothing to gate.
        assert!(verify(&p, None).is_ok());
    }
}
