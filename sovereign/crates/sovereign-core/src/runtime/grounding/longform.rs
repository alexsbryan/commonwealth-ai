//! The long-form ladder: per-claim audit → one rewrite → annotate.

use super::*;


/// Long-form ladder: per-claim audit → one rewrite → annotate.
/// An essay with one bad claim is REWRITTEN, not abstained; if the
/// rewrite still carries unsupported claims, they are listed in a
/// visible verification note appended to the answer — the reader sees
/// exactly which assertions didn't verify, instead of either losing
/// the whole essay or trusting it blind.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn gate_longform(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
    progress: Option<&GateProgressSender>,
) -> GateOutcome {
    use crate::types::NarrationPhase;
    let tau = profile.tau;
    let chunks: &[String] = &evidence.chunks;
    // H1's verdict for this turn, for `with_native_verdict` at each of
    // this ladder's seven exits. Telemetry: nothing below reads it.
    let native = evidence.native_verdict.as_ref();
    // T1 P1.4 — split the evidence by provenance once per turn. With no
    // Summary-class chunks (the common case, and every pre-P1.4
    // surface) `leaf_chunks == chunks` and the claim loop below is
    // byte-identical to its pre-P1.4 self. The deterministic checks
    // (name veto, specifics scan, batched pre-pass) always read the
    // Leaf view: they are factual-class by construction.
    let leaf_chunks: Vec<String> = chunks
        .iter()
        .enumerate()
        .filter(|(i, _)| evidence.source_of(*i).may_be_quoted())
        .map(|(_, c)| c.clone())
        .collect();
    let summary_chunks: Vec<String> = chunks
        .iter()
        .enumerate()
        .filter(|(i, _)| !evidence.source_of(*i).may_be_quoted())
        .map(|(_, c)| c.clone())
        .collect();
    // THE AUDITOR IS SHOWN WHAT THE DRAFTER WAS SHOWN. This was a constant
    // (`profile.max_chunks = 8`) on every surface, with no stated rationale,
    // while the drafter received the whole retrieved set — so a claim the
    // drafter grounded in leaf chunk #18 could not be cleared by the judge no
    // matter how well calibrated it was. Measured 2026-08-13 over 18 audit
    // passes: 32 of 57 failed claims (56%) had their support in a retrieved
    // leaf chunk PAST the eighth, and zero passes ever came back clean, so
    // every turn paid a rewrite and a re-audit (note 95b82f97).
    //
    // The window is now the retrieved leaf set itself, and the bound is
    // derived rather than picked: the drafter's evidence already passed
    // `prompt_budget::enforce` for this turn's context window, and a judge
    // prompt is strictly SMALLER than the drafter's (one claim in place of
    // the question, the history and the synthesis instructions), so what fit
    // the drafter fits the judge by construction. There is no separate number
    // to choose.
    //
    // Cost is bounded by a mechanism already default-on: every sibling claim
    // declares the same shared-window prefix (`judge::stable_passages_prefix_len`),
    // so `SOVEREIGN_PREFIX_STATE` — whose only consumer is this gate — pins
    // the evidence state once per turn and restores it for claims 2..N. The
    // turn pays one larger prefill, not N.
    let per_claim_chunks = audit_window(leaf_chunks.len());
    let min_claims = profile.max_claims;
    // Session posture for the judge envelopes, resolved once from the
    // synthesis turn's request; the audit closure captures it by copy.
    let posture = crate::slot_policy::posture_of(base_request);
    // Reference-shadow so the audit closure (called twice: draft +
    // rewrite) captures Copy references, not the Vecs themselves.
    let leaf_chunks = &leaf_chunks;
    let summary_chunks = &summary_chunks;
    let pass = audit_pass::AuditPass {
        inference: inference.clone(),
        searcher: evidence.searcher.clone(),
        question,
        leaf_chunks,
        summary_chunks,
        evidence_labels: evidence.source_labels.clone(),
        per_claim_chunks,
        min_claims,
        tau,
        posture,
        progress,
    };

    let draft_backup = draft.clone();
    let audit_pass::AuditPassOutcome::Judged {
        text,
        audited,
        failed,
        unjudged,
    } = pass.run(draft, audit_pass::PassKind::Draft).await
    else {
        // Claim-list extraction failed — fail open with the draft.
        return GateOutcome {
            // Claim-list extraction failed, so the gate reached no verdict.
            // ARCH §18.2: that is not a pass, and until this rung it released
            // the same bare `String` a verified answer did.
            answer: release_unjudged(
                draft_backup,
                Vec::new(),
                inference,
                base_request.preferred_speed,
                "claim-list extraction failed — gate fell open".to_string(),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "judge_failed_open", "retried": false,
                    "threshold": tau, "mode": "per_claim",
                }),
                native,
            ),
            claims: Vec::new(),
        };
    };
    let n_claims = audited.len();
    if failed.is_empty() && !unjudged.is_empty() {
        // Nothing flagged, but not everything judged: the ladder fell open
        // on `unjudged.len()` claims. That is the fourth verdict, not the
        // first (ARCH §18.1) — the answer ships, the action says Unjudged,
        // and every unjudged row reaches the ledger as FailOpen.
        tracing::warn!(
            target: "grounding_gate",
            event = "judge_failed_open",
            unjudged = unjudged.len(),
            audited = n_claims,
            "longform audit fell open — released without a verdict on every claim"
        );
        emit_gate_progress(
            progress,
            NarrationPhase::ClaimCheckComplete {
                confirmed: n_claims.saturating_sub(unjudged.len()),
                flagged: 0,
            },
        );
        return GateOutcome {
            answer: release_unjudged(
                text,
                Vec::new(),
                inference,
                base_request.preferred_speed,
                format!(
                    "{} of {n_claims} claim(s) could not be judged — gate fell open",
                    unjudged.len()
                ),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "judge_failed_open", "retried": false,
                    "claims_checked": n_claims, "failed_claims": [],
                    "unjudged_claims": unjudged.len(),
                    "claim_check_outcome": "could-not-judge",
                    "threshold": tau, "mode": "per_claim",
                }),
                native,
            ),
            claims: longform_claims(&audited, &failed, &unjudged),
        };
    }
    if failed.is_empty() {
        dbg(&format!("longform released claims={n_claims} failed=0"));
        emit_gate_progress(
            progress,
            NarrationPhase::ClaimCheckComplete {
                confirmed: n_claims,
                flagged: 0,
            },
        );
        return GateOutcome {
            answer: release_held(
                text,
                Vec::new(),
                inference,
                base_request.preferred_speed,
                format!("{n_claims} claim(s) audited, none flagged"),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "released", "retried": false,
                    "claims_checked": n_claims, "failed_claims": [],
                    "threshold": tau, "mode": "per_claim",
                }),
                native,
            ),
            claims: longform_claims(&audited, &failed, &unjudged),
        };
    }
    // ── MARK, DON'T RE-SYNTHESISE ────────────────────────────────────────
    // Two different reasons reach the same release shape, and they are named
    // separately because they are different facts about the turn:
    //
    //   `profile.retry == false`  — a verify-only SURFACE (Refinement). The
    //       caller treats this as "the refined text failed, keep the prior
    //       verified answer" (`runtime/collaboration.rs`).
    //   repair tombstoned         — the surface allows repair, but the repair
    //       LADDER is tombstoned on the default configuration (ECONOMY §9
    //       Phase 4). The draft is released with its failed claims marked.
    //
    // Conflating them under one action string would tell a Refinement
    // consumer that a released, marked knowledge answer was a rejected
    // refinement (ARCH §10.6, one decider one name).
    let repair_armed = config::longform_repair_enabled();
    if !profile.retry || !repair_armed {
        // Verify-only surfaces: annotate the draft with the failed
        // claims — no second synthesis. The caller decides whether
        // an annotated draft is acceptable (Refinement keeps the
        // prior verified answer instead).
        //
        // Tombstoned surfaces: the SAME release shape, and that is the whole
        // point — the replacement was already in production here, so this
        // phase adds no mechanism (ECONOMY §9 Phase 4, "Adds: nothing").
        let action = if profile.retry {
            // Glassbox (#1): the operator is being spared a rewrite + a full
            // re-audit — the two stages that own most of a longform turn
            // (§7.2). A turn that silently skipped them would be
            // indistinguishable from a turn that had nothing to repair.
            // INFO, not DEBUG: once per repaired-turn-that-wasn't.
            tracing::info!(
                target: "grounding_gate",
                event = "repair_tombstoned",
                failed = failed.len(),
                audited = n_claims,
                "longform repair ladder is tombstoned — releasing the audited draft \
                 with its failed claims marked (SOVEREIGN_GATE_LONGFORM_REPAIR=1 re-arms)"
            );
            ACT_ANNOTATED_MARKED
        } else {
            ACT_ANNOTATED_NO_RETRY
        };
        emit_gate_progress(
            progress,
            NarrationPhase::ClaimCheckComplete {
                confirmed: n_claims.saturating_sub(failed.len()),
                flagged: failed.len(),
            },
        );
        // The marking itself. `supported: false` on every failed claim
        // becomes `Verification::FailedOnce` in the epistemic ledger
        // (`runtime/epistemic.rs`), which flips the turn's verdict to
        // `mixed` and renders under the answer. Neither this call nor the
        // ledger consults the repair flag — the mark is a fact about the
        // AUDIT, and the audit is unchanged by construction.
        let claim_records = longform_claims(&audited, &failed, &unjudged);
        let failed_claims: Vec<String> = failed.into_iter().map(|f| f.claim).collect();
        let note = verification_note(&failed_claims);
        return GateOutcome {
            // `append_note` is a no-op on any surface that carries the caveat
            // in its own UI (desktop sets SOVEREIGN_NOTE_AS_METADATA=1); on
            // API/CLI it appends the visible note. Either way a known-failed
            // claim is never released without its caveat (ARCH §18.3).
            answer: release_as_because(
                action,
                append_note(text, &note),
                Vec::new(),
                inference,
                base_request.preferred_speed,
                format!(
                    "{} claim(s) flagged and released with a caveat",
                    failed_claims.len()
                ),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": action.id, "retried": false,
                    "claims_checked": n_claims, "failed_claims": failed_claims,
                    "threshold": tau, "mode": "per_claim",
                }),
                native,
            ),
            claims: claim_records,
        };
    }
    dbg(&format!(
        "longform rewrite: {} failed of {n_claims}",
        failed.len()
    ));
    emit_gate_progress(
        progress,
        NarrationPhase::ClaimRevisionStart {
            failed: failed.len(),
        },
    );
    // Surgical fast path: correct only the failed spans on the fast slot instead
    // of re-synthesising the whole answer on the 35B (measured ~44s → single
    // digits). Falls back to the full re-synthesis below whenever any failed
    // claim can't be confidently located; either way the result runs the same
    // re-audit ladder, so the fabrication guarantee is unchanged.
    // Surgery targets the COMMON case: a mostly-grounded draft with a few
    // unsupported claims. When MOST claims fail the draft is fundamentally
    // broken — a coherent full re-synthesis beats a Frankenstein of patched
    // sentences (and saves little), so cap surgery at a MINORITY of the
    // audited claims. The cap is now the ratio this sentence has always
    // reasoned about; see `surgery_admits` / `SURGICAL_MAX_FAILED_RATIO` for
    // where 0.5 comes from and why it is not a tunable latency knob.
    let max_failed_ratio = surgical_max_failed_ratio();
    let surgery_admitted = surgery_admits(failed.len(), n_claims, max_failed_ratio);
    // Glassbox (#1): the repair pass's routing decision is a decision worth
    // 25-50s of the operator's turn, and until now the only record of it was a
    // `dbg()` that is a no-op outside SOVEREIGN_AGENTIC_KQ_DEBUG=1 — which is
    // the same invisibility G4 was opened to fix. INFO, not DEBUG: it fires
    // once per longform repair (not per claim), and a production log that
    // cannot say why the operator paid for a full re-synthesis is the defect.
    // The strip says WHICH mechanism ran; this says WHY it was allowed to.
    tracing::info!(
        target: "grounding_gate",
        event = "surgical_cap",
        failed = failed.len(),
        audited = n_claims,
        max_failed_ratio,
        surgery_admitted,
        "surgical cap evaluated"
    );
    // Corrected text: surgical span-edits when every failed claim maps, else a
    // full re-synthesis. The surgical arm takes the INCREMENTAL re-audit
    // (only its repaired spans are re-judged); the full-re-synthesis arm —
    // entirely new prose — keeps the full re-audit. On BOTH arms the
    // holistic scan and the deterministic sweeps run over the whole text:
    // the 2026-07-17 scoped re-audit leaked a GK-caveated fabrication
    // (CONFAB-LEAKED 0→1) precisely by skipping that floor, and the floor
    // here is the shared closure body, so no arm can skip it.
    // G4 — the repair pass's own clock, and the ONE place that knows which
    // of its two mechanisms ran. Until now that fact was recorded only by a
    // `dbg()` that is a no-op unless SOVEREIGN_AGENTIC_KQ_DEBUG=1, so on a
    // production turn nothing outside a debug build could say whether the
    // operator paid 43.2s for a full re-synthesis or 5.36s for surgery
    // (NATIVE_GROUNDING_ECONOMY.md §7.3). It is recorded at the branch, from
    // the branch actually taken — never from `surgical_rewrite_enabled()`,
    // which is true on both arms.
    let rewrite_started = std::time::Instant::now();
    let mut rewrite_mechanism = sovereign_contracts::types::StageMechanism::FullResynthesis;
    // `Some(spans)` only on the surgical arm: the re-audit then verifies the
    // repaired spans incrementally instead of re-extracting ~9 claims from a
    // text that is byte-identical outside those spans. The full-re-synthesis
    // arm produces an entirely new text and keeps the full re-audit.
    let mut surgical_spans: Option<Vec<String>> = None;
    let second: String = 'produce: {
        if config::surgical_rewrite_enabled() && surgery_admitted {
            let pairs: Vec<(String, Vec<String>)> = failed
                .iter()
                .map(|f| (f.claim.clone(), f.evidence.clone()))
                .collect();
            if let Some(edited) =
                surgical::surgical_rewrite(inference, base_request, &text, &pairs).await
            {
                dbg(&format!(
                    "surgical rewrite applied — incremental re-audit follows ({} failed of {n_claims}, {} repaired span(s))",
                    failed.len(),
                    edited.repaired_spans.len()
                ));
                rewrite_mechanism = sovereign_contracts::types::StageMechanism::SurgicalRewrite;
                surgical_spans = Some(edited.repaired_spans);
                break 'produce edited.text;
            }
            // Admitted by the cap and still declined: `surgical_rewrite`
            // could not confidently map every failed claim to a span (or
            // over-deleted). That is a DIFFERENT fallback from the cap
            // declining, it costs the same full re-synthesis, and merging
            // the two in the log would make the cap look guilty for a span
            // resolver's miss. Named separately for that reason.
            tracing::info!(
                target: "grounding_gate",
                event = "surgical_unmapped",
                failed = failed.len(),
                audited = n_claims,
                "surgery was admitted but could not map every failed claim — full re-synthesis"
            );
        }
        // Full re-synthesis fallback (flag off, failures are a MAJORITY of the
        // audited claims, or surgery could not confidently map a claim).
        let mut rewrite_req = base_request.clone();
        let base_sys = rewrite_req.system_message.clone().unwrap_or_default();
        rewrite_req.system_message = Some(format!("{base_sys}{}", rewrite_system_note(&failed)));
        rewrite_req.assistant_prefix = Some(LONGFORM_REWRITE_PREFIX.to_string());
        // Budget ~1.5x the draft's token estimate — a faithful rewrite REPLACES
        // a short false claim with a LONGER cited correction, so a 1.0x cap ships
        // truncated; 1.5x stays under the 2x runaway floor and the re-audit still
        // guards the result (history: 2026-06-30 runaway inflation to 23.8k chars,
        // 2026-07-12 truncation at the cap).
        let draft_token_budget = (draft_backup.chars().count() * 3 / 8).max(256);
        rewrite_req.max_tokens = Some(
            rewrite_req
                .max_tokens
                .map_or(draft_token_budget, |m| m.min(draft_token_budget)),
        );
        match gate_call(
            &**inference,
            &rewrite_req,
            sovereign_contracts::types::GateCallMechanism::Rewrite,
        )
        .await
        {
            Ok(resp) => {
                // Truncation trace: the longform rewrite is non-streaming and
                // bypasses synth.truncation — log finish vs cap so a silent
                // Length cut is visible.
                tracing::info!(
                    target: "gate.call",
                    kind = "rewrite",
                    finish = ?resp.finish_reason,
                    completion_tokens = ?resp.completion_tokens,
                    max_tokens = ?rewrite_req.max_tokens,
                    resp_chars = resp.text.chars().count(),
                    "gate internal completion"
                );
                format!("{LONGFORM_REWRITE_PREFIX}{}", resp.text)
            }
            Err(e) => {
                // Rewrite unavailable: release draft 1 WITH the visible
                // verification note (never silently release known-failed
                // claims; never destroy an essay over judge availability).
                tracing::warn!(target: "grounding_gate", error = %e, "longform rewrite failed — annotating draft");
                // The repair pass spent this time and then failed. Attributed,
                // not dropped: an early return is still an execution.
                crate::runtime::stage_ledger::Stage::new(
                    sovereign_contracts::types::StageId::Rewrite,
                    sovereign_contracts::types::StackOwner::Incumbent,
                )
                .mechanism(rewrite_mechanism)
                .cause(sovereign_contracts::types::StageCause::AuditFoundFailures)
                .record(rewrite_started.elapsed().as_millis() as u64);
                emit_gate_progress(
                    progress,
                    NarrationPhase::ClaimCheckComplete {
                        confirmed: n_claims.saturating_sub(failed.len()),
                        flagged: failed.len(),
                    },
                );
                let claim_records = longform_claims(&audited, &failed, &unjudged);
                let failed_claims: Vec<String> = failed.into_iter().map(|f| f.claim).collect();
                let note = verification_note(&failed_claims);
                return GateOutcome {
                    answer: release_as_because(
                        ACT_ANNOTATED_REWRITE_ERROR,
                        append_note(text, &note),
                        Vec::new(),
                        inference,
                        base_request.preferred_speed,
                        format!(
                            "surgical rewrite failed; {} claim(s) flagged and released with a caveat",
                            failed_claims.len()
                        ),
                    ),
                    meta: with_native_verdict(
                        serde_json::json!({
                            "surface": profile.surface.id(),
                            "action": ACT_ANNOTATED_REWRITE_ERROR.id, "retried": false,
                            "claims_checked": n_claims, "failed_claims": failed_claims,
                            "threshold": tau, "mode": "per_claim",
                        }),
                        native,
                    ),
                    claims: claim_records,
                };
            }
        }
    };

    // G4 — the repair pass completed. Recorded BEFORE the re-audit runs, so
    // the two are separate rows: the re-audit's whole existence is caused by
    // this pass having produced new prose, and a strip that merged them would
    // hide the causal chain the operator asked to be able to read.
    crate::runtime::stage_ledger::Stage::new(
        sovereign_contracts::types::StageId::Rewrite,
        sovereign_contracts::types::StackOwner::Incumbent,
    )
    .mechanism(rewrite_mechanism)
    .cause(sovereign_contracts::types::StageCause::AuditFoundFailures)
    .record(rewrite_started.elapsed().as_millis() as u64);

    let second_backup = second.clone();
    // On the incremental arm the re-audit returns only the repaired spans as
    // its audited set; the audit#1 claims whose sentences surgery did NOT
    // touch are still true, this-turn-verified holdings of the released text,
    // so they are carried into the ledger rather than silently dropped
    // (ARCH §18.3 — a shrunken holdings list would read as "less was
    // verified", which is the opposite of what happened).
    let carried_claims: Vec<String> = if surgical_spans.is_some() {
        audited
            .iter()
            .filter(|c| !failed.iter().any(|f| &f.claim == *c))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    match pass
        .run(
            second,
            audit_pass::PassKind::ReAudit {
                incremental: surgical_spans,
            },
        )
        .await
    {
        audit_pass::AuditPassOutcome::Judged {
            text: text2,
            audited: mut audited2,
            failed: failed2,
            unjudged: unjudged2,
        } if failed2.is_empty() => {
            audited2.extend(carried_claims);
            let n2 = audited2.len();
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckComplete {
                    confirmed: n2,
                    flagged: 0,
                },
            );
            GateOutcome {
                answer: release_as_because(
                    ACT_REWRITE_RELEASED,
                    text2,
                    Vec::new(),
                    inference,
                    base_request.preferred_speed,
                    format!("{n2} claim(s) re-audited after rewrite, none flagged"),
                ),
                meta: with_native_verdict(
                    serde_json::json!({
                        "surface": profile.surface.id(),
                        "action": ACT_REWRITE_RELEASED.id, "retried": true,
                        "claims_checked": n2, "failed_claims": [],
                        "threshold": tau, "mode": "per_claim",
                    }),
                    native,
                ),
                claims: longform_claims(&audited2, &failed2, &unjudged2),
            }
        }
        audit_pass::AuditPassOutcome::Judged {
            text: text2,
            audited: mut audited2,
            failed: failed2,
            unjudged: unjudged2,
        } => {
            audited2.extend(carried_claims);
            let n2 = audited2.len();
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckComplete {
                    confirmed: n2.saturating_sub(failed2.len()),
                    flagged: failed2.len(),
                },
            );
            let claim_records = longform_claims(&audited2, &failed2, &unjudged2);
            let failed_claims: Vec<String> = failed2.into_iter().map(|f| f.claim).collect();
            let note = verification_note(&failed_claims);
            GateOutcome {
                answer: release_as_because(
                    ACT_REWRITE_ANNOTATED,
                    append_note(text2, &note),
                    Vec::new(),
                    inference,
                    base_request.preferred_speed,
                    format!(
                        "{} claim(s) still flagged after rewrite, released with a caveat",
                        failed_claims.len()
                    ),
                ),
                meta: with_native_verdict(
                    serde_json::json!({
                        "action": ACT_REWRITE_ANNOTATED.id, "retried": true,
                        "claims_checked": n2, "failed_claims": failed_claims,
                        "threshold": tau, "mode": "per_claim",
                    }),
                    native,
                ),
                claims: claim_records,
            }
        }
        audit_pass::AuditPassOutcome::ExtractionFailed => GateOutcome {
            // The rewrite produced text the gate never re-audited.
            answer: release_as_because(
                ACT_REWRITE_RELEASED_UNVERIFIED,
                second_backup,
                Vec::new(),
                inference,
                base_request.preferred_speed,
                "rewrite released without re-audit".to_string(),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": ACT_REWRITE_RELEASED_UNVERIFIED.id, "retried": true,
                    "threshold": tau, "mode": "per_claim",
                }),
                native,
            ),
            claims: Vec::new(),
        },
    }
}
