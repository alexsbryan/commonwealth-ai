// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rerank kind's role, and the one decider for whether a provider serves
//! it — here so svrn answers from a provider (its own or a loopback to serve)
//! without linking the crates that load the kind (pb-serve-distributes).
//! `sovereign_inference::served_kind::RERANK` names its roles from here, and
//! `sovereign_compute::assembly::serves_rerank` re-exports the decider.

use crate::traits::InferenceProvider;

/// The rerank kind's slot role, and the role of the compute child that hosts it.
pub const RERANK_ROLE: &str = "rerank";

/// Does `provider` — what the serving assembly installed — serve the rerank
/// kind: a rerank slot its engine holds, or a compute child in the kind's
/// child role? Answered from what loaded, never from the config: a rerank
/// install that failed leaves the lane unarmed, so no search pays the
/// cross-encoder's overfetch for a reranker that is not there (ARCH §6).
pub fn serves_rerank(provider: &dyn InferenceProvider) -> bool {
    let in_process = provider
        .resident_slots()
        .iter()
        .any(|s| s.role == RERANK_ROLE);
    let child = provider
        .compute_children()
        .iter()
        .any(|c| c.role == RERANK_ROLE);
    tracing::debug!(
        target: "served_kind",
        in_process,
        child,
        "does this process serve the rerank kind"
    );
    in_process || child
}
