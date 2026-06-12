// SPDX-License-Identifier: AGPL-3.0-or-later
//! Production grounding gate — the runtime port of the chaos-bench
//! critic (`bench_cmd/live_runner.rs::verify_grounding`), wired into
//! the KQ synthesis stream as **hold → gate → retry → abstain**.
//!
//! Mechanism (validated on two corpora, 2026-06-11): the synthesized
//! answer's single central claim is extracted (one small completion),
//! then checked per-retrieved-chunk with a forced-choice logprob pass
//! ("does THIS passage support THIS claim"); `violation_prob = 1 −
//! max(per-chunk support)`. On the contamination-free holdout bank the
//! verdicts separated cleanly: fabricated claims scored 0.96–1.00,
//! the highest-scoring CORRECT answer 0.45 — and every gated answer on
//! both banks contained genuine confabulation (zero false positives).
//!
//! Gate semantics, in order:
//!   1. `vp < τ`  → release the answer unchanged.
//!   2. `vp ≥ τ`  → ONE retry: re-synthesize with the failed claim
//!      quoted back as a constraint (minimal best-of-N — the second
//!      draft knows exactly which assertion failed verification).
//!   3. retry still `≥ τ` → release a grounded abstention instead.
//!      The user gets "your sources don't establish this" — never the
//!      confabulation.
//!
//! Long-form answers (past the profile's `longform_chars` pivot) take
//! the per-claim ladder instead: audit (each claim judged against the
//! prompt snapshot ∪ claim-conditioned sealed search) → ONE rewrite
//! fed the failed claims' corrective passages → visible verification
//! note on anything still unverified. An essay is never abstained.
//!
//! Surfaces, budgets, and the env surface (`SOVEREIGN_GROUNDING_GATE`,
//! per-surface overrides, `SOVEREIGN_GV_THRESHOLD`) live in
//! `config.rs` (`GateSurface`/`GroundingProfile`/
//! `grounding_gate_flags`); judges in `judge.rs` (prompts byte-pinned
//! to the bench critic); sealed claim search in `search.rs`.
//!
//! Scope guards (same as the bench critic): declines and explicitly
//! GK-attributed answers extract as NO_CLAIM and pass (the honest
//! OOD-caveat case must not be gated) — except on entity-anchored
//! questions, where a GK caveat cannot exempt an in-world claim.

mod config;
mod judge;
mod search;

pub(crate) use config::{dbg, grounding_gate_enabled, GateSurface, GroundingProfile};
// Registry export: consumed by the config-module coverage test today;
// the docs flag table renders from it (same contract as
// `retrieval_pipeline_flags`).
#[allow(unused_imports)]
pub use config::grounding_gate_flags;
#[allow(unused_imports)]
pub(crate) use judge::{verify_grounding, GateVerdict};
// `ClaimSearcher` is constructed via `Runtime::claim_searcher`; the
// type re-exports are for call sites that name them.
#[allow(unused_imports)]
pub(crate) use search::{AttachedAssetSearcher, ClaimSearcher, SealedEvidenceSearch};

use std::collections::HashSet;
use std::sync::Arc;

use crate::traits::InferenceProvider;
use crate::types::CompletionRequest;

use judge::{claim_violation_joint, extract_claim_list};

/// WHAT one released answer is verified against — the sealed evidence
/// universe for one turn. Owned values throughout (the gate runs in
/// spawned stream tasks that hold no `&Runtime`).
pub(crate) struct EvidenceContext {
    /// Prompt-snapshot evidence the draft was synthesized from.
    pub chunks: Vec<String>,
    /// Claim-conditioned widening WITHIN the sealed universe.
    /// `None` = the snapshot IS the universe (e.g. tool transcripts).
    pub searcher: Option<Arc<dyn SealedEvidenceSearch>>,
    /// In-world question: a general-knowledge attribution cannot
    /// exempt a claim from extraction (see `verify_grounding`).
    pub entity_anchored: bool,
}

/// One audit-failed claim plus the claim-conditioned passages its
/// targeted search returned — the rewrite's correction material.
struct FailedClaim {
    claim: String,
    evidence: Vec<String>,
}

/// The grounded abstention released when both drafts fail the gate.
/// Names what was claimed and what was checked — glassbox, not a bare
/// refusal.
pub(crate) fn grounded_abstention(claim: &str, chunks_checked: usize) -> String {
    format!(
        "Your sources don't establish this. The draft answer asserted that {claim} \
         but none of the {chunks_checked} retrieved passages support that, so I'm \
         not presenting it as fact. If this is in your sources somewhere, try \
         rephrasing with the specific names or terms involved; otherwise this may \
         simply not be recorded there.",
        claim = claim.trim_end_matches('.'),
    )
}

/// System-message suffix for the single gated retry. Quotes the failed
/// claim back — the second draft knows exactly which assertion failed
/// verification and must either ground it or drop it.
pub(crate) fn retry_system_note(claim: &str, corrective: &[String]) -> String {
    const RETRY_EVIDENCE_PER_CLAIM: usize = 2;
    const RETRY_EVIDENCE_CHARS: usize = 700;
    let mut note = format!(
        "\n\nGROUNDING CHECK FAILED on your previous draft. It asserted: \"{claim}\" — \
         no retrieved passage supports that assertion."
    );
    if corrective.is_empty() {
        note.push_str(
            " Write a new answer using ONLY what the passages state. If the passages \
             do not contain the asked-for fact, say plainly that the sources do not \
             state it. Do not repeat the unsupported assertion.",
        );
    } else {
        // Parity with the long-form rewrite (measured v13c–v15): a
        // retry told only WHICH assertion failed, with no passages
        // stating the truth, can only delete and disclaim.
        note.push_str("\n  What the sources actually say on this point:");
        for p in corrective.iter().take(RETRY_EVIDENCE_PER_CLAIM) {
            let trimmed: String = p.chars().take(RETRY_EVIDENCE_CHARS).collect();
            note.push_str(&format!("\n  | {}", trimmed.replace('\n', "\n  | ")));
        }
        note.push_str(
            "\nWrite a new answer using ONLY what the passages state — if the \
             passages above contain the asked-for fact, state it (with citations); \
             do not repeat the unsupported assertion.",
        );
    }
    note
}

/// Final outcome of a full gate ladder over one draft answer.
pub(crate) struct GateOutcome {
    pub text: String,
    /// `grounding_gate` metadata for the message (action, retried,
    /// violation_prob / failed_claims, threshold).
    pub meta: serde_json::Value,
}

/// The complete gate ladder, shared by every gated surface (see
/// `GateSurface`): short answers go through the single-claim
/// verify → retry → abstain ladder; long-form answers (past the
/// profile's `longform_chars` pivot) go through the per-claim
/// audit → rewrite → annotate ladder. Fail-open on judge failure
/// everywhere — the gate is a quality lever, not an availability
/// risk.
pub(crate) async fn gate_answer(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
) -> GateOutcome {
    let tau = profile.tau;
    let chunks: &[String] = &evidence.chunks;
    let entity_anchored = evidence.entity_anchored;
    if draft.chars().count() > profile.longform_chars {
        return gate_longform(inference, question, draft, evidence, base_request, profile).await;
    }
    let mut text = draft;
    let mut action = "released";
    let mut retried = false;
    let mut final_vp: Option<f64> = None;
    match verify_grounding(
        inference,
        question,
        &text,
        chunks,
        entity_anchored,
        evidence.searcher.as_ref(),
    )
    .await
    {
        Some(v) => {
            final_vp = Some(v.violation_prob);
            if v.violation_prob >= tau {
                if let Some(claim) = v.claim.clone() {
                    if !profile.retry {
                        // Verify-only surfaces (Refinement): no second
                        // synthesis — the caller decides what replaces
                        // the failed text (typically: keep the prior
                        // verified answer).
                        text = grounded_abstention(&claim, chunks.len().min(12));
                        action = "abstained_no_retry";
                        return GateOutcome {
                            text,
                            meta: serde_json::json!({
                                "surface": profile.surface.id(),
                                "action": action,
                                "retried": false,
                                "violation_prob": final_vp,
                                "threshold": tau,
                                "mode": "single_claim",
                            }),
                        };
                    }
                    retried = true;
                    let mut retry_req = base_request.clone();
                    let base_sys = retry_req.system_message.clone().unwrap_or_default();
                    retry_req.system_message = Some(format!(
                        "{base_sys}{}",
                        retry_system_note(&claim, &v.claim_evidence)
                    ));
                    retry_req.assistant_prefix = None;
                    match inference.complete(&retry_req).await {
                        Ok(resp) => {
                            let second = resp.text;
                            match verify_grounding(
                                inference,
                                question,
                                &second,
                                chunks,
                                entity_anchored,
                                evidence.searcher.as_ref(),
                            )
                            .await
                            {
                                Some(v2) if v2.violation_prob < tau => {
                                    final_vp = Some(v2.violation_prob);
                                    text = second;
                                    action = "retry_released";
                                }
                                Some(v2) => {
                                    final_vp = Some(v2.violation_prob);
                                    text = grounded_abstention(&claim, chunks.len().min(12));
                                    action = "abstained";
                                }
                                None => {
                                    // Retry verdict unavailable — fail open
                                    // on the retry (written under the
                                    // grounding constraint; safer than
                                    // draft 1).
                                    text = second;
                                    action = "retry_released_unverified";
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                target: "grounding_gate",
                                error = %e,
                                "gated retry synthesis failed — releasing abstention"
                            );
                            text = grounded_abstention(&claim, chunks.len().min(12));
                            action = "abstained_retry_error";
                        }
                    }
                }
            }
        }
        None => {
            action = "judge_failed_open";
        }
    }
    dbg(&format!("verdict action={action} retried={retried} vp={final_vp:?} tau={tau}"));
    tracing::info!(
        target: "grounding_gate",
        action,
        retried,
        vp = ?final_vp,
        tau,
        "grounding gate verdict"
    );
    GateOutcome {
        text,
        meta: serde_json::json!({
            "surface": profile.surface.id(),
            "action": action,
            "retried": retried,
            "violation_prob": final_vp,
            "threshold": tau,
            "mode": "single_claim",
        }),
    }
}

/// Decode-committed opening for the long-form rewrite. Instruction-only
/// shape rules measured non-compliant (v14: the rewrite still led with
/// "I do not have access to passages detailing…" despite an explicit
/// "do not open with what the passages lack" rule — same ~60%
/// instruction-wall as the GK caveat). Committing the opening forces
/// the rewrite to continue into the supported account; the abstain
/// read of a disclaimer-led head disappears structurally. Like
/// GK_CAVEAT_PREFIX, assistant_prefix is decode-commit only — the
/// caller must prepend it to the returned text.
pub const LONGFORM_REWRITE_PREFIX: &str =
    "From the retrieved sources, here is what can be established:\n\n";

/// Rewrite-request system note: every failed claim, each with the
/// passages its targeted corpus search returned (when any). The
/// correction material is the point — v13c/v14/v14b measured that a
/// rewrite told only WHICH assertions failed, with no passages
/// stating the truth, can only delete and disclaim.
fn rewrite_system_note(failed: &[FailedClaim]) -> String {
    const REWRITE_EVIDENCE_PER_CLAIM: usize = 2;
    const REWRITE_EVIDENCE_CHARS: usize = 700;
    let list = failed
        .iter()
        .map(|f| {
            let mut entry = format!("- \"{}\"", f.claim);
            if f.evidence.is_empty() {
                entry.push_str(
                    "\n  (no corpus passage states this — remove it, or say the \
                     sources do not establish it)",
                );
            } else {
                entry.push_str("\n  What the sources actually say on this point:");
                for p in f.evidence.iter().take(REWRITE_EVIDENCE_PER_CLAIM) {
                    let trimmed: String = p.chars().take(REWRITE_EVIDENCE_CHARS).collect();
                    entry.push_str(&format!("\n  | {}", trimmed.replace('\n', "\n  | ")));
                }
            }
            entry
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "\n\nGROUNDING AUDIT FAILED on your previous draft. These assertions did not \
         verify against the sources:\n{list}\n\
         Rewrite the answer: keep everything the sources support. For each failed \
         assertion that has corrective passages above, REPLACE it with what those \
         passages actually state, citing them — do not merely delete it. Structure \
         the rewrite as an ANSWER, not a disclaimer: open directly with the \
         supported account, organized to address the question. Do not open with \
         what the sources lack, and do not enumerate the removed assertions in the \
         body. If material gaps remain, note them briefly in a single short \
         paragraph at the end."
    )
}

/// Cross-passage support check for ONE long-form claim: the top
/// passages are presented TOGETHER and the judge answers whether they
/// jointly state or imply the claim. Long-form synthesis legitimately
/// assembles claims across passages — per-chunk max-support is
/// structurally biased against exactly that (the bench critic's
/// documented blind spot; measured v13: a correct maximal essay was
/// rewritten into hedging because its synthesis claims had no single

/// Long-form ladder: per-claim audit → one rewrite → annotate.
/// An essay with one bad claim is REWRITTEN, not abstained; if the
/// rewrite still carries unsupported claims, they are listed in a
/// visible verification note appended to the answer — the reader sees
/// exactly which assertions didn't verify, instead of either losing
/// the whole essay or trusting it blind.
async fn gate_longform(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
) -> GateOutcome {
    let tau = profile.tau;
    let chunks: &[String] = &evidence.chunks;
    let per_claim_chunks = profile.max_chunks;
    let max_claims = profile.max_claims;
    let audit = |text: String| {
        let inference = inference.clone();
        let searcher = evidence.searcher.clone();
        async move {
            let claims = extract_claim_list(&inference, question, &text).await?;
            let mut failed: Vec<FailedClaim> = Vec::new();
            for claim in claims.iter().take(max_claims) {
                // Claim-conditioned retrieval: verify against the
                // sealed CORPUS, not just the prompt snapshot. Hits
                // go first (most relevant to THIS claim) and the cap
                // widens by their count, so they never displace a
                // prompt chunk the old audit would have judged.
                let extra = match &searcher {
                    Some(s) => {
                        let hits = s.search(claim).await;
                        if !hits.is_empty() {
                            dbg(&format!(
                                "claim_search hits={} for {:?}",
                                hits.len(),
                                claim.chars().take(60).collect::<String>()
                            ));
                        }
                        hits
                    }
                    None => Vec::new(),
                };
                let dedup: HashSet<String> = extra
                    .iter()
                    .map(|c| c.chars().take(120).collect::<String>())
                    .collect();
                let mut judged: Vec<String> = extra.clone();
                judged.extend(
                    chunks
                        .iter()
                        .filter(|c| !dedup.contains(&c.chars().take(120).collect::<String>()))
                        .cloned(),
                );
                let cap = per_claim_chunks + extra.len();
                match claim_violation_joint(&inference, claim, &judged, cap).await {
                    Some(vp) => {
                        dbg(&format!("longform claim vp={vp:.3} {claim:?}"));
                        if vp >= tau {
                            failed.push(FailedClaim {
                                claim: claim.clone(),
                                evidence: extra,
                            });
                        }
                    }
                    None => {} // unverifiable claim — fail open per claim
                }
            }
            Some((text, claims.len(), failed))
        }
    };

    let draft_backup = draft.clone();
    let Some((text, n_claims, failed)) = audit(draft).await else {
        // Claim-list extraction failed — fail open with the draft.
        return GateOutcome {
            text: draft_backup,
            meta: serde_json::json!({
                "surface": profile.surface.id(),
                "action": "judge_failed_open", "retried": false,
                "threshold": tau, "mode": "per_claim",
            }),
        };
    };
    if failed.is_empty() {
        dbg(&format!("longform released claims={n_claims} failed=0"));
        return GateOutcome {
            text,
            meta: serde_json::json!({
                "surface": profile.surface.id(),
                "action": "released", "retried": false,
                "claims_checked": n_claims, "failed_claims": [],
                "threshold": tau, "mode": "per_claim",
            }),
        };
    }
    if !profile.retry {
        // Verify-only surfaces: annotate the draft with the failed
        // claims — no second synthesis. The caller decides whether
        // an annotated draft is acceptable (Refinement keeps the
        // prior verified answer instead).
        let failed_claims: Vec<String> = failed.into_iter().map(|f| f.claim).collect();
        let note = format!(
            "\n\n---\n*Verification note: the following could not be \
             confirmed against your sources — treat as unverified:*\n{}",
            failed_claims
                .iter()
                .map(|c| format!("- {c}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        return GateOutcome {
            text: format!("{text}{note}"),
            meta: serde_json::json!({
                "surface": profile.surface.id(),
                "action": "annotated_no_retry", "retried": false,
                "claims_checked": n_claims, "failed_claims": failed_claims,
                "threshold": tau, "mode": "per_claim",
            }),
        };
    }
    dbg(&format!("longform rewrite: {} failed of {n_claims}", failed.len()));
    let mut rewrite_req = base_request.clone();
    let base_sys = rewrite_req.system_message.clone().unwrap_or_default();
    rewrite_req.system_message = Some(format!("{base_sys}{}", rewrite_system_note(&failed)));
    rewrite_req.assistant_prefix = Some(LONGFORM_REWRITE_PREFIX.to_string());
    match inference.complete(&rewrite_req).await {
        Ok(resp) => {
            let second = format!("{LONGFORM_REWRITE_PREFIX}{}", resp.text);
            let second_backup = second.clone();
            match audit(second).await {
                Some((text2, n2, failed2)) if failed2.is_empty() => GateOutcome {
                    text: text2,
                    meta: serde_json::json!({
                        "surface": profile.surface.id(),
                        "action": "rewrite_released", "retried": true,
                        "claims_checked": n2, "failed_claims": [],
                        "threshold": tau, "mode": "per_claim",
                    }),
                },
                Some((text2, n2, failed2)) => {
                    let failed_claims: Vec<String> =
                        failed2.into_iter().map(|f| f.claim).collect();
                    let note = format!(
                        "\n\n---\n*Verification note: the following could not be \
                         confirmed against your sources — treat as unverified:*\n{}",
                        failed_claims
                            .iter()
                            .map(|c| format!("- {c}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    GateOutcome {
                        text: format!("{text2}{note}"),
                        meta: serde_json::json!({
                            "action": "rewrite_annotated", "retried": true,
                            "claims_checked": n2, "failed_claims": failed_claims,
                            "threshold": tau, "mode": "per_claim",
                        }),
                    }
                }
                None => GateOutcome {
                    text: second_backup,
                    meta: serde_json::json!({
                        "surface": profile.surface.id(),
                        "action": "rewrite_released_unverified", "retried": true,
                        "threshold": tau, "mode": "per_claim",
                    }),
                },
            }
        }
        Err(e) => {
            // Rewrite unavailable: release draft 1 WITH the visible
            // verification note (never silently release known-failed
            // claims; never destroy an essay over judge availability).
            tracing::warn!(target: "grounding_gate", error = %e, "longform rewrite failed — annotating draft");
            let failed_claims: Vec<String> = failed.into_iter().map(|f| f.claim).collect();
            let note = format!(
                "\n\n---\n*Verification note: the following could not be \
                 confirmed against your sources — treat as unverified:*\n{}",
                failed_claims
                    .iter()
                    .map(|c| format!("- {c}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            GateOutcome {
                text: format!("{text}{note}"),
                meta: serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "annotated_rewrite_error", "retried": false,
                    "claims_checked": n_claims, "failed_claims": failed_claims,
                    "threshold": tau, "mode": "per_claim",
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::error::{Error, Result};
    use crate::types::{Depth, ProviderCapabilities};
    use crate::types::CompletionResponse;
    use futures::Stream;
    use std::pin::Pin;

    /// Prompt-routing mock for the gate's judge calls: claim
    /// extraction returns a fixed claim; every forced-choice support
    /// check returns `support` (as a logprob A/B distribution).
    struct GateMock {
        support: bool,
    }

    #[async_trait::async_trait]
    impl crate::traits::InferenceProvider for GateMock {
        async fn complete(
            &self,
            request: &crate::types::CompletionRequest,
        ) -> Result<CompletionResponse> {
            let text = if request
                .structured_output
                .as_ref()
                .map(|s| s.to_string().contains("x_forced_choice"))
                .unwrap_or(false)
            {
                if self.support {
                    r#"{"A": 0.98, "B": 0.02}"#.to_string()
                } else {
                    r#"{"A": 0.02, "B": 0.98}"#.to_string()
                }
            } else if request.prompt.contains("single central factual claim") {
                "The shop is located on Crescent Lane.".to_string()
            } else {
                "unexpected synthesis call".to_string()
            };
            Ok(CompletionResponse {
                text,
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "gate-mock".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }

        async fn complete_stream(
            &self,
            _request: &crate::types::CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented("GateMock: no streaming".into()))
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![])
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: true,
                relative_speed: crate::types::Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    fn refinement_evidence() -> EvidenceContext {
        EvidenceContext {
            chunks: vec!["The shop sits on Harbour Row, by the quay.".to_string()],
            searcher: None,
            entity_anchored: false,
        }
    }

    /// The Phase-6 invariant's gate half: verify-only (retry: false)
    /// on an unsupported claim must return `abstained_no_retry` — the
    /// caller (collaboration refinement) keeps the verified original.
    #[tokio::test]
    async fn verify_only_failure_is_abstained_no_retry() {
        let inference: Arc<dyn crate::traits::InferenceProvider> =
            Arc::new(GateMock { support: false });
        let profile = GateSurface::Refinement.profile();
        assert!(!profile.retry);
        let outcome = gate_answer(
            &inference,
            "Where is the shop?",
            "The shop is on Crescent Lane.".to_string(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
        )
        .await;
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("abstained_no_retry")
        );
        assert!(outcome.text.starts_with("Your sources don't establish"));
    }

    /// Supported claims release unchanged under verify-only.
    #[tokio::test]
    async fn verify_only_supported_claim_releases() {
        let inference: Arc<dyn crate::traits::InferenceProvider> =
            Arc::new(GateMock { support: true });
        let profile = GateSurface::Refinement.profile();
        let draft = "The shop is on Harbour Row.".to_string();
        let outcome = gate_answer(
            &inference,
            "Where is the shop?",
            draft.clone(),
            &refinement_evidence(),
            &CompletionRequest::default(),
            &profile,
        )
        .await;
        assert_eq!(
            outcome.meta.get("action").and_then(|a| a.as_str()),
            Some("released")
        );
        assert_eq!(outcome.text, draft);
    }

    fn fc(claim: &str, evidence: &[&str]) -> FailedClaim {
        FailedClaim {
            claim: claim.to_string(),
            evidence: evidence.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn rewrite_note_includes_corrective_passages() {
        let note = rewrite_system_note(&[fc(
            "Verloc instructs The Professor to bomb the Observatory.",
            &["Mr Vladimir tells Verloc the attack must be against the first meridian."],
        )]);
        assert!(note.contains("Verloc instructs The Professor"));
        assert!(
            note.contains("first meridian"),
            "corrective passage must reach the rewrite prompt"
        );
        assert!(note.contains("REPLACE"));
    }

    #[test]
    fn rewrite_note_marks_claims_with_no_corpus_support() {
        let note = rewrite_system_note(&[fc("Mrs Veronica Verloc shoots her husband.", &[])]);
        assert!(note.contains("no corpus passage states this"));
    }

    #[test]
    fn rewrite_note_caps_evidence_per_claim_and_length() {
        let long = "x".repeat(2_000);
        let note = rewrite_system_note(&[fc("c", &[&long, &long, &long])]);
        // 2 passages max, 700 chars each — the note stays prompt-sized.
        assert!(note.matches("  | ").count() <= 2);
        assert!(note.len() < 2_200);
    }

    #[test]
    fn rewrite_note_commits_answer_shape_rules() {
        let note = rewrite_system_note(&[fc("c", &[])]);
        assert!(note.contains("Do not open with what the sources lack"));
    }
}
