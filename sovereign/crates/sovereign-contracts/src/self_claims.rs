// SPDX-License-Identifier: AGPL-3.0-or-later
//! What this node claims about itself — the port Fabric publishes from.
//!
//! `quality/DAEMON_CORE.md` §4.2 decides that gossip must not reach into
//! Serving (the inference store, the availability composite) and the node (the
//! storage budget, the in-flight gauge) to build its own advertisement — the
//! `fabric -> host` backflow in the cluster graph. It inverts into one port
//! Fabric declares and the daemon implements, answering what this node claims
//! right now: availability, in-flight, storage remaining and embed model.
//! Fabric publishes the claims and does not know who computed them.
//!
//! It lives here, beside the rest of the daemon↔package contract, for the same
//! reason [`crate::identity`] does: its two speakers sit in crates that may not
//! name each other — `sovereign-mesh` (Fabric) declares the consumer, and
//! `sovereign-api` (the daemon, until `REVIEW-mint-daemon-move` relocates it)
//! implements the port. One type both can name is what lets the eventual move
//! of Fabric's state leave the implementation untouched.
//!
//! Hosted corpora is deliberately NOT part of this port. It comes from the
//! `CorpusEngine` handle Fabric already legitimately names, not from the node's
//! state, so `capabilities::build_hosted_corpora` stays in `sovereign-mesh`
//! (ralph/DECISIONS.md 2026-09-16, the `SelfClaims` redraw).

use async_trait::async_trait;

use crate::oicp::manifest::EmbedModelInfo;

/// The four answers `capabilities::build_local_capabilities` publishes to the
/// mesh as this node's `NodeCapabilities`.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalClaims {
    /// Inference availability this node advertises, in `[0.0, 1.0]` — the
    /// minimum of the activity report and the yield-to-local-user floor.
    pub availability: f32,
    /// Current local in-flight request count; `None` when no gauge is wired
    /// (storage-only nodes, test harnesses).
    pub in_flight: Option<u32>,
    /// Bytes the storage budget allows above current usage; `None` when no
    /// budget is set.
    pub storage_remaining: Option<u64>,
    /// The embed model this node serves; `None` when none is loaded, which the
    /// collaborative-ingestion planner reads as "don't include me".
    pub embed_model: Option<EmbedModelInfo>,
}

/// The node's answer to "what do you claim about yourself right now?".
///
/// Fabric holds one of these and calls it once per gossip round, immediately
/// before publishing; the daemon implements it. The two methods are separate
/// because the storage-used figure is Fabric's to measure (it walks the
/// `CorpusEngine` once for `hosted_corpora` and shares that walk's sum) while
/// the remembered budget is the node's — so the measurement is handed in and
/// the remaining budget is answered back.
#[async_trait]
pub trait SelfClaims: Send + Sync {
    /// What this node claims right now. An implementation that derives
    /// availability from a time-varying input must recompute it here, at the
    /// moment of publication, rather than cache a value: the yield half has no
    /// transition event to hook, so a node refusing every peer request would
    /// otherwise advertise a stale `1.0` (note 3234d770).
    async fn claims(&self) -> LocalClaims;

    /// Record the storage this node is using, measured by Fabric's engine walk,
    /// so the next [`Self::claims`] reports the right remaining budget. The
    /// write-back rides this port because the measured figure is Fabric's and
    /// the remembered one is the node's.
    fn record_storage_used(&self, used: u64);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    /// A stand-in for the daemon's implementation: the four answers are seeded
    /// and `storage_remaining` is derived from the recorded usage — which is
    /// exactly the write-back contract `record_storage_used` carries.
    struct FakeClaims {
        availability: f32,
        in_flight: Option<u32>,
        embed_model: Option<EmbedModelInfo>,
        budget: Option<u64>,
        used: AtomicU64,
    }

    #[async_trait::async_trait]
    impl SelfClaims for FakeClaims {
        async fn claims(&self) -> LocalClaims {
            LocalClaims {
                availability: self.availability,
                in_flight: self.in_flight,
                storage_remaining: self
                    .budget
                    .map(|b| b.saturating_sub(self.used.load(Ordering::Relaxed))),
                embed_model: self.embed_model.clone(),
            }
        }

        fn record_storage_used(&self, used: u64) {
            self.used.store(used, Ordering::Relaxed);
        }
    }

    fn embed() -> EmbedModelInfo {
        EmbedModelInfo {
            model_id: "qwen3-embedding-0.6b".into(),
            dimensions: 1024,
            pooling: crate::oicp::manifest::PoolingStrategy::Last,
            normalization: crate::oicp::manifest::NormalizationStrategy::Server,
            query_instruction_prefix: String::new(),
        }
    }

    fn fake(budget: Option<u64>) -> FakeClaims {
        FakeClaims {
            availability: 0.4,
            in_flight: Some(3),
            embed_model: Some(embed()),
            budget,
            used: AtomicU64::new(0),
        }
    }

    /// Positive: every answer round-trips through a trait-object call, so
    /// Fabric can hold `&dyn SelfClaims`.
    #[test]
    fn claims_round_trip_through_the_trait_object() {
        let src = fake(Some(100));
        let port: &dyn SelfClaims = &src;
        let claims = futures::executor::block_on(port.claims());
        assert_eq!(claims.availability, 0.4);
        assert_eq!(claims.in_flight, Some(3));
        assert_eq!(claims.embed_model, Some(embed()));
        assert_eq!(claims.storage_remaining, Some(100));
    }

    /// Positive: the storage write-back is observed by the next `claims()`.
    #[test]
    fn record_storage_used_is_observed_by_the_next_claims() {
        let src = fake(Some(100));
        src.record_storage_used(30);
        assert_eq!(
            futures::executor::block_on(src.claims()).storage_remaining,
            Some(70)
        );
        src.record_storage_used(5);
        assert_eq!(
            futures::executor::block_on(src.claims()).storage_remaining,
            Some(95)
        );
    }

    /// Negative: with no budget configured, storage remaining is `None` rather
    /// than a permissive default — absence is reported, never zeroed into "no
    /// headroom" or defaulted into "unlimited" (ARCH 6).
    #[test]
    fn no_budget_answers_none_not_a_default() {
        let src = fake(None);
        src.record_storage_used(999);
        assert_eq!(
            futures::executor::block_on(src.claims()).storage_remaining,
            None
        );
    }

    /// Negative: two implementations do not share the remembered usage — the
    /// port is a handle on the node that owns it, not a global cell.
    #[test]
    fn two_implementations_do_not_share_usage() {
        let a = fake(Some(100));
        let b = fake(Some(100));
        a.record_storage_used(40);
        assert_eq!(
            futures::executor::block_on(a.claims()).storage_remaining,
            Some(60)
        );
        assert_eq!(
            futures::executor::block_on(b.claims()).storage_remaining,
            Some(100)
        );
    }
}
