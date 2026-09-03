//! The gate ladder: entry, verdict projection, decision journaling, and the
//! short-path specifics guard. Callers go through `gate_answer_with_progress`;
//! the body of the ladder lives in `inner` (short) and `longform` (essays).

use super::*;

pub(crate) async fn gate_answer(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
) -> GateOutcome {
    gate_answer_with_progress(
        inference,
        question,
        draft,
        evidence,
        base_request,
        profile,
        None,
    )
    .await
}

/// `gate_answer` plus a live claim-check progress channel (see
/// `GateProgressSender`). The streaming spawns call this form; all
/// other surfaces keep the plain `gate_answer` signature.
///
/// This wrapper is also the ONE funnel through which every gate decision
/// reaches the local grounding journal (VERIFIER_V0.md §6.1, phase 0) —
/// wrapping rather than instrumenting each of the inner ladder's return
/// sites, so no exit path can forget to record (ARCH §10.6). It stamps
/// `episode_id` into the outcome meta, which the daemon persists with
/// the message row: that id is the join between the journal line, the
/// stored claim/answer text, and any future escalation line.
pub(crate) async fn gate_answer_with_progress(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    draft: String,
    evidence: &EvidenceContext,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
    progress: Option<&GateProgressSender>,
) -> GateOutcome {
    let started = std::time::Instant::now();
    // G4 — open the gate's attribution window. This funnel is the only
    // place that holds the gate's wall clock independently of the stage
    // rows recorded inside it, so it is the only place that can compute
    // the in-gate residual. See `runtime::stage_ledger::gate_close`: a
    // gate mechanism that runs while recording no row shows up there as
    // seconds nobody claimed, which is how "a mechanism with no row is a
    // defect in the strip" becomes detectable outside a debug build.
    let ledger_window = crate::runtime::stage_ledger::gate_open();
    // D0 — open the per-CALL census (`call_census`). Same funnel, same
    // reason as the stage ledger above, one grain finer: the ledger says
    // which STAGE spent the seconds, this says which model CALL. They are
    // separate instruments on purpose — see `call_census`'s module docs for
    // why merging them would break the ledger's residual arithmetic.
    let census = CallCensus::new();
    let mut outcome = census
        .clone()
        .scope(gate_answer_inner(
            inference,
            question,
            draft,
            evidence,
            base_request,
            profile,
            progress,
        ))
        .await;
    let gate_ms = started.elapsed().as_millis() as u64;
    crate::runtime::stage_ledger::gate_close(ledger_window, gate_ms);
    record_gate_decision(&mut outcome, evidence, profile, gate_ms, census.take());
    outcome
}

/// Build the journal line for one gate decision and hand it to the
/// grounding stream. Metadata only, by construction: the claim, answer
/// and chunk text stay where they already live (conversation store,
/// corpus) — the line carries identity, scores, and what the gate did,
/// with the evidence as `(corpus, chunk_id)` handles from
/// `EvidenceContext::chunk_targets`. The append runs on a dropped-handle
/// blocking task and swallows IO errors into a `tracing::warn!`, so
/// journaling can neither delay nor fail a turn (the next-edit journal's
/// contract, note 43770c85 rule 4).
/// Project a gate decision onto the journal's four-valued verdict.
///
/// Pure so it can be watched fail. `claim_check_measured` is the guard
/// that the ladder's `violation_prob` is a MEASUREMENT rather than a
/// placeholder: `verify_grounding` returns `violation_prob: 0.0` from
/// three paths that never ran a check — no input, a long-form answer
/// outside the single-claim gate's scope, and NO_CLAIM (the assistant
/// declined, which is an honesty SUCCESS, not an audited claim that
/// passed). Until 2026-08-19 all three landed in the `Supported` arm, so
/// a turn the gate never evaluated was rendered to the user as verified.
/// Four verdicts, not two (ARCH §18.1); absence reported, never
/// defaulted (§18.3).
///
/// `claims_all_supported` is `Some(all_supported)` when the per-claim
/// ladder produced verdicts, `None` when it produced none.
pub(crate) fn project_verdict(
    violation_prob: Option<f64>,
    claim_check_measured: bool,
    tau: f64,
    claims_all_supported: Option<bool>,
) -> sovereign_contracts::types::GateJudgeVerdict {
    use sovereign_contracts::types::GateJudgeVerdict;
    match violation_prob {
        // A vp from a path that never judged is a fact about the
        // instrument, not a verdict about the answer.
        Some(_) if !claim_check_measured => GateJudgeVerdict::CouldNotJudge,
        Some(vp) if vp >= tau => GateJudgeVerdict::Unsupported,
        Some(_) => GateJudgeVerdict::Supported,
        None => match claims_all_supported {
            Some(true) => GateJudgeVerdict::Supported,
            Some(false) => GateJudgeVerdict::Unsupported,
            None => GateJudgeVerdict::CouldNotJudge,
        },
    }
}

fn record_gate_decision(
    outcome: &mut GateOutcome,
    evidence: &EvidenceContext,
    profile: &GroundingProfile,
    gate_ms: u64,
    calls: Vec<sovereign_contracts::types::GateCallRow>,
) {
    #[cfg(not(test))]
    use sovereign_contracts::types::{grounding_journal_append, journal_dir};
    use sovereign_contracts::types::{EvidenceRef, GroundingDecisionLine, GroundingLine};
    let mut d = GroundingDecisionLine::new(profile.surface.id(), profile.tau, gate_ms);
    // The per-call census (D0). The journal line below is the exact join for
    // the census script; these two surfaces exist because a reader should
    // not have to open a file (the log line) or replay a turn (the meta
    // summary) to learn which mechanism owns the gate's seconds (ARCH §9).
    if !calls.is_empty() {
        let call_ms: u64 = calls.iter().map(|c| c.ms).sum();
        let mut by_mech: std::collections::BTreeMap<&'static str, (u32, u64)> =
            std::collections::BTreeMap::new();
        for c in &calls {
            let e = by_mech.entry(c.mechanism.label()).or_insert((0, 0));
            e.0 += 1;
            e.1 += c.ms;
        }
        let breakdown = by_mech
            .iter()
            .map(|(m, (n, ms))| format!("{m}x{n}={ms}ms"))
            .collect::<Vec<_>>()
            .join(" ");
        tracing::info!(
            target: "grounding_gate",
            gate_ms,
            calls = calls.len(),
            call_ms,
            // gate_ms minus the model calls: deterministic gate work plus
            // anything a mechanism spent without going through the funnel.
            unattributed_ms = gate_ms.saturating_sub(call_ms),
            breakdown = %breakdown,
            "gate call census"
        );
        // The compact form on the outcome's meta: counts and milliseconds
        // per mechanism, never the rows. Small enough to ride the message
        // row, and it is what makes the census assertable in-process — a
        // task-local that silently failed to install would pass every unit
        // test of the funnel while recording nothing in production, so the
        // instrument is checked on the real path (ARCH §18.4).
        if let Some(m) = outcome.meta.as_object_mut() {
            m.insert(
                "gate_call_ms".to_string(),
                serde_json::Value::Object(
                    by_mech
                        .iter()
                        .map(|(k, (_, ms))| ((*k).to_string(), serde_json::json!(ms)))
                        .collect(),
                ),
            );
            m.insert(
                "gate_call_n".to_string(),
                serde_json::Value::Object(
                    by_mech
                        .iter()
                        .map(|(k, (n, _))| ((*k).to_string(), serde_json::json!(n)))
                        .collect(),
                ),
            );
        }
    }
    d.calls = calls;
    d.entity_anchored = evidence.entity_anchored;
    d.claim_audited = !outcome.claims.is_empty();
    let meta = outcome.meta.as_object();
    d.action = meta
        .and_then(|m| m.get("action"))
        .and_then(|v| v.as_str())
        .map(str::to_string);
    d.retried = meta
        .and_then(|m| m.get("retried"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    d.violation_prob = meta
        .and_then(|m| m.get("violation_prob"))
        .and_then(|v| v.as_f64());
    // Verdict, from what the ladder reported. The vp comparison mirrors
    // the gate's own `>= tau` act condition; paths that judge without a
    // vp (citation-grounded) speak through their claim verdicts; a path
    // with neither judged nothing — could-not-judge, never a pass
    // (ARCH §18.1).
    // Did the gate actually judge, or is this a placeholder? Three paths
    // return `violation_prob: 0.0` WITHOUT running a check — no input,
    // long-form out-of-scope, and NO_CLAIM (a decline, i.e. an honesty
    // success). Before 2026-08-19 all three fell into the `Some(_) =>
    // Supported` arm below, so a turn the gate never evaluated was
    // rendered to the user as `Supported` — the exact overclaim the
    // comment above forbids. `gate_outcome` is written beside
    // `violation_prob` by every meta site; absent (older rows, or a path
    // that predates it) is treated as measured, preserving prior
    // behaviour rather than silently reclassifying history.
    let claim_check_measured = meta
        .and_then(|m| m.get("claim_check_outcome"))
        .and_then(|v| v.as_str())
        .map(|s| s == "measured")
        .unwrap_or(true);
    d.verdict = project_verdict(
        d.violation_prob,
        claim_check_measured,
        profile.tau,
        // A claim the judge never reached makes the whole check
        // could-not-judge (ARCH §18.1): `None` here projects to
        // CouldNotJudge, never to Supported.
        (!outcome.claims.is_empty() && !outcome.claims.iter().any(|c| c.unjudged))
            .then(|| outcome.claims.iter().all(|c| c.supported)),
    );
    d.chunks = evidence.chunks.len();
    d.evidence = evidence
        .chunk_targets
        .iter()
        .flatten()
        .map(|t| EvidenceRef {
            corpus: t.corpus_id.clone(),
            chunk: t.chunk_id,
        })
        .collect();
    d.evidence_unresolved = d.chunks.saturating_sub(d.evidence.len());
    d.top_similarity = evidence.top_similarity;
    if let Some(m) = outcome.meta.as_object_mut() {
        m.insert(
            "episode_id".to_string(),
            serde_json::Value::String(d.episode_id.clone()),
        );
    }
    // The per-chunk custody ledger the judge's evidence universe held
    // (custody.md §5, reds R-2/R-3): emitted for EVERY decision through
    // this funnel, so a refusal is auditable in the same shape as a
    // release. Emitted only when the stamp machinery engaged (at least
    // one stamped chunk) — a turn with no stamp anywhere carries no
    // custody record, and fabricating all-unknown rows would misread
    // every pre-custody surface as a refusal case.
    if evidence.chunk_custodies.iter().any(|c| c.is_some()) {
        let ledger: Vec<serde_json::Value> = (0..evidence.chunks.len())
            .map(|i| {
                let custody = evidence
                    .chunk_custodies
                    .get(i)
                    .copied()
                    .flatten()
                    .unwrap_or(crate::types::Custody::Unknown);
                // The chunk's stable id when it has one, else its URL —
                // else an index fallback, labeled as such (a store chunk
                // has no chunk id in this slice).
                let locator = evidence
                    .chunk_targets
                    .get(i)
                    .cloned()
                    .flatten()
                    .map(|t| t.chunk_id.to_string())
                    .or_else(|| evidence.chunk_urls.get(i).cloned().flatten())
                    .unwrap_or_else(|| format!("chunk-{i}"));
                let row = sovereign_contracts::types::ChunkCustody::new(
                    locator,
                    custody,
                    evidence.chunk_urls.get(i).cloned().flatten(),
                );
                serde_json::to_value(row).unwrap_or(serde_json::Value::Null)
            })
            .collect();
        if let Some(m) = outcome.meta.as_object_mut() {
            m.insert(
                "chunk_custody".to_string(),
                serde_json::Value::Array(ledger),
            );
        }
    }
    // Structural backstop for the H1 telemetry pair (ARCH §10 — make it
    // structural, not remembered). Every `GateOutcome` site in this file
    // builds its meta through `with_native_verdict`; this funnel is what a
    // future site that forgets trips on, because every outcome reaches it.
    // Warns, never panics: the gate is a quality lever, not an availability
    // risk, and a missing key is itself the readable "not attached" state.
    if let Some(m) = outcome.meta.as_object() {
        if !m.contains_key("native_answerability") || !m.contains_key("native_decision") {
            tracing::warn!(
                target: "grounding_gate",
                action = ?d.action,
                "gate outcome reached the journal with no H1 telemetry — a GateOutcome site skipped with_native_verdict"
            );
        }
    }
    let line = GroundingLine::Decision(d);
    // The line is BUILT under test — every branch above this point is
    // exercised — but not WRITTEN. Unit tests drive this funnel with mock
    // providers at millisecond gate times, and appending those to the
    // operator's real journal corrupts the one stream the latency census
    // reads by index: one `cargo test -p sovereign-core --lib grounding::`
    // run added 12 synthetic turns to `grounding-2026-08-13.jsonl`, four of
    // them with `gate_ms: 0`. A measurement instrument that its own test
    // suite writes into is not an instrument (ARCH §18.4).
    #[cfg(test)]
    let _ = line;
    #[cfg(not(test))]
    drop(tokio::task::spawn_blocking(move || {
        if let Err(e) = grounding_journal_append(&journal_dir(), &line) {
            tracing::warn!(target: "grounding_gate", error = %e, "grounding journal append failed");
        }
    }));
}

/// Second-opinion fabrication guard for the SHORT gate path (single-claim +
/// citation). Those paths verify the LOAD-BEARING value but are structurally
/// blind to fabricated SUPPORTING specifics — a named person/flag/number/quote
/// the answer cites to `[Source: …]` that is absent from the evidence (observed
/// 2026-07-01 on thin evidence: "David Hart, COO of Knowledge Process Software"
/// shipped by the citation path, and tokei "--files"/"--sort"/".tokeignore"
/// specifics padded onto a grounded top-line). Runs the holistic specifics scan
/// on an already-RELEASED short answer; on a flag it routes into ONE corrective
/// retry (each flagged specific re-searched so the rewrite has the truth) and
/// re-scans the result, abstaining only if the rewrite still fabricates.
///
/// Never a blunt abstention: a truly-grounded specific gets its passage back and
/// the rewrite keeps it (self-correcting away a false positive), and a
/// mostly-grounded answer with one bad specific is rewritten, not discarded.
/// Returns `None` to leave the release unchanged — disabled, no-retry surface,
/// abstention-shaped answer, judge failure, or a clean scan.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn short_specifics_guard(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    released: &str,
    chunks: &[String],
    searcher: Option<&Arc<dyn SealedEvidenceSearch>>,
    base_request: &CompletionRequest,
    profile: &GroundingProfile,
    // H1's verdict for the turn, threaded in for `with_native_verdict`
    // alone: this guard takes `chunks`, not the `EvidenceContext`, so the
    // two outcomes it can return had no way to report the instrument.
    // Read by nothing that decides — see `with_native_verdict`.
    native: Option<&crate::types::GroundingVerdict>,
) -> Option<GateOutcome> {
    // Only on retry-capable surfaces: the guard's whole remedy is a corrective
    // re-synthesis. Verify-only surfaces have no second synthesis to give.
    if !profile.retry {
        return None;
    }
    // Deterministic sentence sweep FIRST — receipt-grade hits (a code
    // identifier or in-world name attribution absent from the entire
    // evidence) trigger the corrective retry regardless of the LLM-scan flag.
    // gen75e s34: the `cmd_init`/`found.rs` ghosts shipped in a 1,504-char
    // answer — UNDER the 1,800 longform pivot, where none of the longform
    // vetoes run. The short path needs the same receipts-grade guard.
    let hay_lower = chunks.join(" ").to_lowercase();
    // Budget for the LLM scan paths (initial when the flag is on; the
    // post-retry re-scan always).
    let budget = claim_budget(released.chars().count(), 3);
    let mut swept: Vec<String> = Vec::new();
    for sentence in released.split(['.', '\n']) {
        let sentence = sentence.trim();
        if sentence.chars().count() < 20 {
            continue;
        }
        if let Some(x) = judge::absent_identifier_attribution(sentence, &hay_lower)
            .or_else(|| judge::absent_name_attribution(sentence, &hay_lower))
        {
            if !swept.contains(&x) {
                swept.push(x);
            }
        }
    }
    let specifics = if !swept.is_empty() {
        dbg(&format!(
            "short sweep VETOED {swept:?} (absent from evidence)"
        ));
        swept
            .iter()
            .map(|x| {
                format!("The answer references \"{x}\", which does not appear in the sources.")
            })
            .collect()
    } else {
        if !short_specifics_scan_enabled() {
            return None;
        }
        // Nothing asserted → nothing to fabricate-check (fail-open latency skip).
        if answer_declines(released) {
            return None;
        }
        // Small budget floored at 3 so even a terse citation answer ("David Hart")
        // gets a real check; scales modestly on longer short answers.
        let specifics = scan_unsupported_specifics(
            inference,
            question,
            released,
            chunks,
            &[],
            budget,
            crate::slot_policy::posture_of(base_request),
        )
        .await?;
        if specifics.is_empty() {
            return None; // clean — release unchanged
        }
        specifics
    };
    // Corrective evidence per flagged specific — the same material the long-form
    // rewrite gets, and the self-correction for a false positive (a real
    // specific's grounding passage comes back, so the rewrite keeps it).
    let mut corrective: Vec<String> = Vec::new();
    if let Some(s) = searcher {
        for spec in specifics.iter().take(4) {
            if let Some(hit) = s.search(spec).await.into_iter().next() {
                corrective.push(hit);
            }
        }
    }
    let joined = specifics.join("\"; \"");
    dbg(&format!(
        "short_specifics_guard: {} flagged specific(s) [{:?}] → corrective retry",
        specifics.len(),
        joined.chars().take(90).collect::<String>()
    ));
    let mut retry_req = base_request.clone();
    let base_sys = retry_req.system_message.clone().unwrap_or_default();
    retry_req.system_message = Some(format!(
        "{base_sys}{}",
        retry_system_note(&joined, &corrective)
    ));
    retry_req.assistant_prefix = None;
    let second = match gate_call(
        &**inference,
        &retry_req,
        sovereign_contracts::types::GateCallMechanism::ShortGuardRetry,
    )
    .await
    {
        Ok(r) => r.text,
        Err(e) => {
            tracing::warn!(
                target: "grounding_gate",
                error = %e,
                "short specifics guard retry failed — keeping prior release"
            );
            return None; // fail-open: keep the original release
        }
    };
    // Re-scan the rewrite. Still fabricating → abstain; clean → release the
    // corrected answer. A re-scan judge failure falls open to keep the rewrite
    // (written under the corrective note, no worse than the flagged draft).
    match scan_unsupported_specifics(
        inference,
        question,
        &second,
        chunks,
        &[],
        budget,
        crate::slot_policy::posture_of(base_request),
    )
    .await
    {
        Some(v) if !v.is_empty() => {
            tracing::info!(
                target: "grounding_gate",
                action = ACT_ABSTAINED_SPECIFICS.id,
                flagged = specifics.len(),
                "short specifics guard: rewrite still fabricates — abstaining"
            );
            let claims = specifics
                .iter()
                .map(|s| GateClaim {
                    text: s.clone(),
                    supported: false,
                    failed_once: true,
                    unjudged: false,
                    violation_prob: None,
                    // Filled post-gate, display only (see GateClaim::address).
                    address: None,
                })
                .collect();
            Some(GateOutcome {
                answer: abstain(
                    grounded_abstention("", chunks.len().min(12)),
                    inference,
                    base_request.preferred_speed,
                    "second-opinion guard flagged fabricated specifics".to_string(),
                ),
                meta: with_native_verdict(
                    serde_json::json!({
                        "surface": profile.surface.id(),
                        "action": "abstained_specifics",
                        "retried": true,
                        "flagged_specifics": specifics,
                        "mode": "short_specifics",
                    }),
                    native,
                ),
                claims,
            })
        }
        _ => {
            tracing::info!(
                target: "grounding_gate",
                action = ACT_RETRY_RELEASED_SPECIFICS.id,
                flagged = specifics.len(),
                "short specifics guard: corrective rewrite released"
            );
            let claims = specifics
                .iter()
                .map(|s| GateClaim {
                    text: s.clone(),
                    supported: true,
                    failed_once: true,
                    unjudged: false,
                    violation_prob: None,
                    // Filled post-gate, display only (see GateClaim::address).
                    address: None,
                })
                .collect();
            Some(GateOutcome {
                answer: release_as(
                    ACT_RETRY_RELEASED_SPECIFICS,
                    second,
                    Vec::new(),
                    inference,
                    base_request.preferred_speed,
                ),
                meta: with_native_verdict(
                    serde_json::json!({
                        "surface": profile.surface.id(),
                        "action": "retry_released_specifics",
                        "retried": true,
                        "flagged_specifics": specifics,
                        "mode": "short_specifics",
                    }),
                    native,
                ),
                claims,
            })
        }
    }
}

/// Fold a long-form audit's outcome into retained per-claim records
/// for the epistemic ledger: audited claims get their final verdict;
/// synthetic failures (specifics scan, sentence sweep) that never
/// appeared in the extracted list are appended as unsupported records.
pub(crate) fn longform_claims(
    audited: &[String],
    failed: &[FailedClaim],
    unjudged: &[String],
) -> Vec<GateClaim> {
    let mut out: Vec<GateClaim> = audited
        .iter()
        .map(|c| {
            let is_failed = failed.iter().any(|f| &f.claim == c);
            // A claim the judge never reached is neither supported nor
            // failed. It shipped because the ladder fails open per claim,
            // and the record must say so (ARCH §18.3): the ledger renders
            // it FailOpen, and the verdict projection treats the whole
            // check as could-not-judge.
            let is_unjudged = !is_failed && unjudged.iter().any(|u| u == c);
            GateClaim {
                text: c.clone(),
                supported: !is_failed && !is_unjudged,
                failed_once: is_failed,
                unjudged: is_unjudged,
                violation_prob: None,
                // Filled post-gate, display only (see GateClaim::address).
                address: None,
            }
        })
        .collect();
    for f in failed {
        if !audited.iter().any(|c| c == &f.claim) {
            out.push(GateClaim {
                text: f.claim.clone(),
                supported: false,
                failed_once: true,
                unjudged: false,
                violation_prob: None,
                // Filled post-gate, display only (see GateClaim::address).
                address: None,
            });
        }
    }
    out
}

/// Append one audit-forensics record when `SOVEREIGN_GATE_AUDIT_FORENSICS`
/// names a file (see `config::audit_forensics_path` for why it is off by
/// default). Synchronous and best-effort: this runs only on a deliberate
/// diagnostic run, and an IO failure there must be visible rather than
/// silently producing a short file that reads as "no failures" (ARCH §18.3).
pub(crate) fn audit_forensics(record: &serde_json::Value) {
    let Some(path) = config::audit_forensics_path() else {
        return;
    };
    use std::io::Write;
    let line = match serde_json::to_string(record) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "audit forensics record not serialisable");
            return;
        }
    };
    let opened = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    match opened {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{line}") {
                tracing::warn!(target: "grounding_gate", error = %e, path = %path.display(), "audit forensics append failed");
            }
        }
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, path = %path.display(), "audit forensics file not writable");
        }
    }
}

/// The audit window: how many leaf chunks each claim's judge prompt carries.
///
/// **Derived, never a constant** (ARCH §18.6). The auditor is shown what the
/// drafter was shown, so the bound IS the retrieved leaf set — the drafter's
/// evidence already passed `prompt_budget::enforce` for this turn's context
/// window, and a judge prompt is strictly smaller than the drafter's. There is
/// no separate number to choose, and reintroducing one (the removed
/// `profile.max_chunks = 8`) silently narrows the auditor's view below the
/// drafter's without any surface saying so.
///
/// `max(1)` only guards the empty case: a claim loop over zero evidence still
/// needs a non-zero take() bound.
pub(crate) fn audit_window(leaf_chunk_count: usize) -> usize {
    leaf_chunk_count.max(1)
}
