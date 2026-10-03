//! Claim-audit replay harness: the exact prompts and judges the gate runs,
//! exposed for offline extraction (bench verifier, Stream B corruption
//! harness) so offline claims are in the register the verifier sees at
//! runtime. Delegates to the same judge primitives — re-implementing the
//! prompt in a script is the drift this seam exists to prevent.

use super::*;

/// The gate's claim-extraction primitive, exported for callers OUTSIDE the
/// gate (`svrn bench verifier extract-claims`, the Stream B corruption
/// harness). Delegates to the same `judge::extract_claim_list` the longform
/// gate runs, so offline-extracted claims are in the exact register the
/// verifier sees at runtime — re-implementing the prompt in a script is the
/// drift this seam exists to prevent (VERIFIER_V0.md §3 Stream B).
pub async fn extract_claim_list(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    answer: &str,
    max_claims: usize,
    posture: crate::oicp::ShardingPrivacy,
) -> Option<Vec<String>> {
    judge::extract_claim_list(inference, question, answer, max_claims, posture).await
}

/// The gate's per-chunk support primitive, exported for the same reason as
/// [`extract_claim_list`]: the bench faithfulness lane (T1 P0.3) judges
/// RAPTOR-summary claims against member-chunk texts, and it must do so in
/// the exact register the runtime gate uses — passage cap, prompt, and
/// forced-choice normalization included — or lane rates stop predicting
/// gate behavior. Returns support in [0,1]; `None` = judge failure.
pub async fn claim_chunk_support(
    inference: &Arc<dyn InferenceProvider>,
    passage: &str,
    claim: &str,
    posture: crate::oicp::ShardingPrivacy,
) -> Option<f64> {
    judge::claim_chunk_support(&**inference, passage, claim, posture).await
}

/// The gate's JOINT per-claim register, exported for the judge-replay
/// harness (`svrn bench judge-replay`) — the third seam in the
/// [`extract_claim_list`] / [`claim_chunk_support`] family, for the same
/// reason: an offline verdict transfers to the production gate only if it was
/// produced by the EXACT production register (family renderer, system turn,
/// forced-choice normalization). `replay_` prefix because `judge::
/// claim_violation_joint` is already imported unqualified in this module;
/// this is pure delegation, not a second implementation (ARCH §10.6).
///
/// `chunks` is shared window + appended claim-conditioned passages, in that
/// order; `n_stable` is the shared-window length — exactly the
/// (`judged`, `n_shared`) pair the longform loop passes at its own call site.
pub async fn replay_claim_violation_joint(
    inference: &Arc<dyn InferenceProvider>,
    claim: &str,
    chunks: &[String],
    n_stable: usize,
    posture: crate::oicp::ShardingPrivacy,
) -> Option<f64> {
    claim_violation_joint(inference, claim, chunks, chunks.len(), n_stable, posture).await
}

/// The joint register's PROMPT, without the model call — the replay
/// harness's bit-stability surface: two builds whose rendered bytes differ
/// are different judge configurations whatever their diff says. Delegates to
/// the one renderer ([`judge::EvidenceFamily`]).
pub fn replay_render_claim_prompt(
    shared: &[String],
    appended: &[String],
    claim: &str,
) -> (String, Option<usize>) {
    judge::replay_render_claim_prompt(shared, appended, claim)
}

/// The BATCHED support register, exported for the judge-replay harness
/// (order `audit-economy` D1: the batched text-A/B verdict is recalibrated
/// offline against the calibrated per-claim register before
/// `SOVEREIGN_GATE_BATCH_VERIFY` can flip). Pure delegation; `shared` is the
/// full shared window (the batched pre-pass judges the family window only —
/// exactly what `gate_longform` passes at its own call site). Returns one
/// entry per claim; `None` = no clean aligned verdict for that row.
pub async fn replay_claims_support_batched(
    inference: &Arc<dyn InferenceProvider>,
    claims: &[String],
    shared: &[String],
    posture: crate::oicp::ShardingPrivacy,
) -> Vec<Option<bool>> {
    judge::claims_support_batched(inference, claims, shared, shared.len(), posture).await
}

/// The batched register's PROMPT, without the model call — the replay
/// harness's bit-stability surface for the batched shape. Delegates to the
/// one renderer ([`judge::EvidenceFamily`]).
pub fn replay_render_batched_claims_prompt(
    shared: &[String],
    claims: &[String],
) -> (String, Option<usize>) {
    judge::replay_render_batched_claims_prompt(shared, claims)
}

/// The system turn every forced-choice judge call carries, behind an
/// accessor so the replay harness fingerprints WHATEVER constant this build
/// compiled in — the constant's *name* is exactly what judge-register lands
/// change (land C renames `CHUNK_JUDGE_SYSTEM` to `GATE_EVIDENCE_SYSTEM`),
/// and a harness naming one of them would silently stop compiling against
/// the other side of the very comparison it exists to make.
pub fn replay_judge_system_turn() -> &'static str {
    CHUNK_JUDGE_SYSTEM
}

/// The holistic specifics scan, exported for the judge-replay harness.
/// `evidence_chunks` is what the production call site passes: the leaf
/// window followed by the summary chunks (`gate_longform`'s
/// `scan_evidence`). Pure delegation; see [`replay_claim_violation_joint`].
pub async fn replay_scan_unsupported_specifics(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    answer: &str,
    leaf_chunks: &[String],
    summary_chunks: &[String],
    max_items: usize,
    posture: crate::oicp::ShardingPrivacy,
) -> Option<Vec<String>> {
    scan_unsupported_specifics(
        inference,
        question,
        answer,
        leaf_chunks,
        summary_chunks,
        max_items,
        posture,
    )
    .await
}
