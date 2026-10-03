// SPDX-License-Identifier: AGPL-3.0-or-later
//! The enrich run's consent grant: what the egress gate in
//! [`super::DaemonInferenceClient::complete`] consults before a payload goes
//! to a remote provider.
//!
//! The gate landed with order deep-research-t2a and its grant surface did
//! not, so from then until 2026-10-03 nothing called `with_consent` and every
//! `provider:model` dispatch to a remote host refused, including the
//! `providers.toml` setups that had worked before the boundary existed.
//!
//! There is one carrier, `SVRNMESH_EGRESS_CONSENT` (legacy spelling
//! `SOVEREIGN_EGRESS_CONSENT`, both read through `svrnmesh_env`):
//! - The `--consent <class>` flag on the front doors (`svrn-ingest ingest`,
//!   `enrich build`) exports it.
//! - Every `enrich` child that a pipeline shells out to inherits it.
//! - Setting it by hand is the same act as passing the flag.
//!
//! Its value is the release floor, one of `public-web | peer | personal`.
//! The grant is scoped to the corpus being enriched and is built at the one
//! construction path every enrich verb shares
//! ([`super::DaemonInferenceClient::from_enrich_config`]).

use sovereign_contracts::egress::ConsentGrant;
use sovereign_contracts::rebrand::svrnmesh_env;
use sovereign_contracts::types::Custody;

/// Parse a consent class from the CLI or the carrier. The closed set is the
/// three releasable custodies. `unknown` is never a floor anyone can grant.
pub fn parse_consent_class(raw: &str) -> Result<Custody, String> {
    match Custody::parse_wire(raw.trim()) {
        Some(c) if c != Custody::Unknown => Ok(c),
        _ => Err(format!(
            "unknown consent class {raw:?}: the closed set is public-web | peer | personal"
        )),
    }
}

/// Export the floor for this process and every enrich child it spawns.
///
/// Literal names here and in [`run_consent`] so the env gate's scan observes
/// both sites. The `SVRNMESH_` spelling is the one `svrnmesh_env` reads
/// first, so an export cannot be shadowed by a stale legacy value.
pub fn export_run_consent(floor: Custody) {
    std::env::set_var("SVRNMESH_EGRESS_CONSENT", floor.as_str());
    tracing::info!(
        target: "egress",
        floor = floor.as_str(),
        "enrich: consent grant exported for this run and its children"
    );
}

/// The grant this enrich run carries, or `None` (default-deny, the
/// pre-existing behaviour) when the carrier is absent.
pub fn run_consent(corpus_id: &str) -> Option<ConsentGrant> {
    let raw = svrnmesh_env("EGRESS_CONSENT")?;
    consent_from_value(&raw.to_string_lossy(), corpus_id)
}

/// A value outside the closed set is `None` as well, with a warning: a typo
/// must never widen what leaves the machine.
pub(crate) fn consent_from_value(raw: &str, corpus_id: &str) -> Option<ConsentGrant> {
    match parse_consent_class(raw) {
        Ok(floor) => {
            tracing::info!(
                target: "egress",
                corpus = corpus_id,
                floor = floor.as_str(),
                "enrich: consent grant in force; remote providers may receive payloads \
                 at or below this custody"
            );
            Some(ConsentGrant {
                run_id: format!("enrich:{corpus_id}"),
                granted_at_unix: chrono::Utc::now().timestamp(),
                release_floor: floor,
            })
        }
        Err(e) => {
            tracing::warn!(
                target: "egress",
                corpus = corpus_id,
                error = %e,
                "enrich: ignoring an unparseable consent carrier; default-deny stands"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_closed_set_parses_and_unknown_is_never_grantable() {
        assert_eq!(parse_consent_class("personal"), Ok(Custody::Personal));
        assert_eq!(parse_consent_class(" peer "), Ok(Custody::Peer));
        assert_eq!(parse_consent_class("public-web"), Ok(Custody::PublicWeb));
        assert!(parse_consent_class("unknown").is_err());
        assert!(parse_consent_class("public_web").is_err());
        assert!(parse_consent_class("").is_err());
    }

    #[test]
    fn a_garbled_carrier_grants_nothing() {
        assert!(consent_from_value("persnal", "c").is_none());
    }

    #[test]
    fn a_grant_is_scoped_to_the_corpus_it_enriches() {
        let g = consent_from_value("personal", "wessex-hoard").expect("a grant");
        assert_eq!(g.run_id, "enrich:wessex-hoard");
        assert_eq!(g.release_floor, Custody::Personal);
        assert!(g.covers(Custody::Personal) && g.covers(Custody::PublicWeb));
    }
}
