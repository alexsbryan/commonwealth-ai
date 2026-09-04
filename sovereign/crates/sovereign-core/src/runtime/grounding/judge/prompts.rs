//! Extracted from judge.rs (2026-09-03, ARCH §3.1) — see the judge façade.
use std::sync::Arc;
use crate::oicp::ShardingPrivacy;
use crate::slot_policy::Workload;
use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};
use crate::runtime::grounding::call_census::gate_call;
use crate::runtime::grounding::config::dbg;
use crate::runtime::grounding::search::SealedEvidenceSearch;
use sovereign_contracts::types::GateCallMechanism;
use super::*;


/// System turn for claim extraction — step 1 of the two-step gate.
pub const CLAIM_EXTRACTION_SYSTEM: &str =
    "You extract claims precisely. Reply with one sentence or NO_CLAIM.";

/// Render step 1's prompt — the claim the gate will then verify.
///
/// **The one renderer, for the gate and for the bench critic alike.**
/// Step 2 (`chunk_judge_prompt`) was unified for exactly this reason: a
/// duplicate literal in two crates is a claim that holds only while
/// nobody edits one side. Step 1 was left duplicated and duly diverged —
/// production grew the `entity_anchored` branch below while the bench
/// critic kept the unanchored rule, so `tau` was calibrated on a prompt
/// production does not send for entity-anchored turns (measured
/// 2026-08-19). Callers pass their own `entity_anchored`; the STRING is
/// no longer forkable.
///
/// `entity_anchored` turns keep the GK-attribution exemption narrow:
/// outside knowledge cannot establish a fact about the corpus's own
/// world, so a general-knowledge-caveated in-world assertion must still
/// be extracted and verified.
pub fn claim_extraction_prompt(question: &str, answer: &str, entity_anchored: bool) -> String {
    let no_claim_rule = if entity_anchored {
        "Reply with exactly NO_CLAIM if the assistant declined or said the \
         information is not in its sources. If the assistant asserted a fact \
         while attributing it to general knowledge, still state that claim."
    } else {
        "Reply with exactly NO_CLAIM if the assistant declined, said the information \
         is not in its sources, or explicitly attributed the fact to general \
         knowledge rather than the sources."
    };
    format!(
        "A user asked: {}\n\nAn assistant answered:\n\"\"\"\n{}\n\"\"\"\n\n\
         State the single central factual claim the assistant asserts as its answer, \
         as one short standalone sentence that names BOTH sides of the relation \
         (who/what is claimed to be/do what). Do not add qualifiers or sources.\n\
         {no_claim_rule}",
        question.chars().take(400).collect::<String>(),
        answer.chars().take(2000).collect::<String>(),
    )
}
