// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's caller resolution — the local owner, and only the local owner.
//!
//! `quality/DAEMON_CORE.md` §3.3: the turn's `corpus_ceiling` is resolved from
//! a principal, and the daemon had none, so every daemon turn read as
//! *all-corpora-eligible*. This is the missing resolution. It is deliberately
//! the smallest possible one — the daemon is a single-user host, and its own
//! installed corpora are `Org` (`sovereign-tools/src/corpus/manager.rs`), so
//! the value of the owner string is not load-bearing today. It exists so the
//! ceiling is a RESOLVED value rather than an absent one, and so a `Private`
//! corpus that reached this data root from a multi-tenant hub is bounded away
//! from the local owner instead of being visible to everyone.
//!
//! The full design — one `Principal` resolved once at the daemon's edge, the
//! local owner carrying the declared sub-identity the fairness gate buckets on
//! — is `REVIEW-mint-principal` (DC §3.3 "The resolver, decided"). This module
//! is the Wave 0 wiring that keeps the ceiling from defaulting to permissive
//! until that lands.

use sovereign_core::traits::PrincipalResolver;

/// The one principal a single-user daemon serves.
///
/// A named constant, not a literal at the call site: the resolver, the ceiling
/// comparison and any future host that reads the owner must spell the SAME
/// fact (ARCH principle 8 — one decider, one name).
pub(crate) const LOCAL_OWNER: &str = "local-owner";

/// Resolves every conversation this daemon serves to the local owner.
///
/// It answers `Some` for every conversation rather than consulting a store:
/// `PrincipalResolver::principal_for` is synchronous and the daemon's store is
/// single-tenant, so there is no conversation it could fail to attribute. A
/// resolver that genuinely cannot attribute a caller returns `None`, and
/// `build_context` then REFUSES that turn (`PrincipalScope::Unresolved`) rather
/// than defaulting it to every corpus.
pub(crate) struct LocalOwnerPrincipal;

impl PrincipalResolver for LocalOwnerPrincipal {
    fn principal_for(&self, _conversation_id: &str) -> Option<String> {
        Some(LOCAL_OWNER.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the daemon's ceiling depends on: every conversation it
    /// serves resolves, so no turn is `Unresolved`.
    #[test]
    fn every_daemon_conversation_resolves_to_the_local_owner() {
        let r = LocalOwnerPrincipal;
        for id in ["", "c", "0f8fad5b-d9cb-469f-a165-70867728950e"] {
            assert_eq!(
                r.principal_for(id),
                Some(LOCAL_OWNER.to_string()),
                "conversation {id:?} must resolve to the local owner — an \
                 unresolved caller refuses, and the daemon must never refuse \
                 its own owner"
            );
        }
    }
}
