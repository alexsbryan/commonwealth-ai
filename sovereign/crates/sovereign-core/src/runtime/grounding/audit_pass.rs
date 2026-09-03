// SPDX-License-Identifier: AGPL-3.0-or-later
//! One long-form audit pass, as a value with a plan and an execution.
//!
//! `gate_longform` runs this once on the draft and once more on a rewrite.
//! Until 2026-09-02 it was a 650-line closure inside a 7,400-line file with
//! four concerns interleaved — per-claim decisions, IO scheduling, the pricing
//! of an optimisation, and telemetry — and every test of it was an integration
//! test. The shape now: everything fixed for the turn is [`AuditPass`]; what
//! each claim gets is decided ONCE, before any IO, as a [`ClaimDisposition`]
//! (pure, table-testable); the pass returns an [`AuditPassOutcome`] whose
//! variants are the ways a pass can end. The failure space is the product of
//! those two enums, and each variant has a test.

use super::*;
use crate::types::NarrationPhase;

/// Which pass this is. The draft's audit extracts claims; the rewrite's
/// re-audit may instead judge the repaired spans AS the claim list
/// (`incremental`, order audit-economy D4) — they are the only new prose the
/// surgery produced. Everything else runs unchanged on the full text either
/// way: the deterministic vetoes, the claim-conditioned search, the holistic
/// specifics scan and the sentence sweep. That "everything else" is the
/// 2026-07-17 lesson made structural: the scoped re-audit that leaked
/// (CONFAB-LEAK 0→1) skipped the holistic floor; this one cannot, because the
/// floor is the same code path, not a second copy.
pub(super) enum PassKind {
    Draft,
    ReAudit { incremental: Option<Vec<String>> },
}

/// What one extracted claim gets. Decided deterministically from the claim
/// text and the lowercased evidence, before any model call or search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ClaimDisposition {
    /// Honesty meta-language ("the system does not have access to X") is not
    /// a world-claim; auditing it prosecutes the answer's own honesty.
    /// Ships unflagged, no search, no judge.
    Exempt,
    /// An in-world attribution naming a person or identifier absent from the
    /// ENTIRE evidence is fabricated. Fails without asking the yes-biased
    /// judge (measured: "Betty Alexander sent an email…" cleared at vp=0.010
    /// with the name nowhere in the corpus). Searched, for corrective
    /// material; never judged.
    Vetoed { kind: &'static str, token: String },
    /// Everything else: searched, then judged against window + hits.
    Judge,
}

/// One decider for the per-claim plan (ARCH §10.6). The fan-out and the
/// judging loop both consume its output.
pub(super) fn dispositions(claims: &[String], hay_lower: &str) -> Vec<ClaimDisposition> {
    claims
        .iter()
        .map(|c| {
            if judge::is_self_referential_decline(c) {
                return ClaimDisposition::Exempt;
            }
            if let Some(n) = judge::absent_name_attribution(c, hay_lower) {
                return ClaimDisposition::Vetoed {
                    kind: "person",
                    token: n,
                };
            }
            if let Some(i) = judge::absent_identifier_attribution(c, hay_lower) {
                return ClaimDisposition::Vetoed {
                    kind: "identifier",
                    token: i,
                };
            }
            ClaimDisposition::Judge
        })
        .collect()
}

/// How a pass ends. Closed: a caller that matches this cannot forget a case.
pub(super) enum AuditPassOutcome {
    /// Claim extraction returned nothing; no claim was judged. The caller
    /// releases the text as UNJUDGED (ARCH §18.2 — not a pass).
    ExtractionFailed,
    /// Every claim met its disposition. `unjudged` names the claims whose
    /// judge returned no verdict, so the exit can say `judge_failed_open`
    /// rather than `released`.
    Judged {
        text: String,
        audited: Vec<String>,
        failed: Vec<FailedClaim>,
        unjudged: Vec<String>,
    },
}

/// Everything one audit pass needs that is fixed for the turn.
pub(super) struct AuditPass<'a> {
    pub inference: Arc<dyn InferenceProvider>,
    pub searcher: Option<Arc<dyn SealedEvidenceSearch>>,
    pub question: &'a str,
    /// Factual-class evidence; the deterministic checks read only this.
    pub leaf_chunks: &'a [String],
    /// Summary-class evidence; admitted for thematic claims and the scan.
    pub summary_chunks: &'a [String],
    pub evidence_labels: Vec<String>,
    pub per_claim_chunks: usize,
    pub min_claims: usize,
    pub tau: f64,
    pub posture: sovereign_contracts::oicp::ShardingPrivacy,
    pub progress: Option<&'a GateProgressSender>,
}

impl AuditPass<'_> {
    /// Run one pass over `text`.
    pub(super) async fn run(&self, text: String, kind: PassKind) -> AuditPassOutcome {
        let inference = self.inference.clone();
        let searcher = self.searcher.clone();
        let evidence_labels = &self.evidence_labels;
        let leaf_chunks = self.leaf_chunks;
        let summary_chunks = self.summary_chunks;
        let per_claim_chunks = self.per_claim_chunks;
        let min_claims = self.min_claims;
        let posture = self.posture;
        let tau = self.tau;
        let question = self.question;
        let progress = self.progress;
        let recheck = matches!(kind, PassKind::ReAudit { .. });
        let incremental = match kind {
            PassKind::Draft => None,
            PassKind::ReAudit { incremental } => incremental,
        };
        // G4 — this pass's own clock and model-call count. `recheck`
        // selects the stage: the SAME code is the draft's audit and the
        // rewrite's re-audit, and the strip has to tell them apart
        // because the second one exists only because the rewrite ran.
        // Counted, not inferred: every model call below increments this
        // where it is made, so a call added later without a bump shows
        // up as an undercount against the gate's own clock rather than
        // as a plausible number.
        let audit_started = std::time::Instant::now();
        let mut model_calls: u32 = 0;
        let audit_stage = if recheck {
            sovereign_contracts::types::StageId::ReAudit
        } else {
            sovereign_contracts::types::StageId::Audit
        };
        let audit_cause = if recheck {
            sovereign_contracts::types::StageCause::RewriteProducedNewProse
        } else {
            sovereign_contracts::types::StageCause::EveryTurn
        };
        // Budget scales with THIS text's length — audited afresh for the
        // draft and again for the (possibly different-length) rewrite.
        let budget = claim_budget(text.chars().count(), min_claims);
        let is_incremental = incremental.is_some();
        let claims = match incremental {
            // INCREMENTAL: the repaired spans ARE the claim list — no
            // extraction call. A span sentence is judged whole, which is
            // the conservative direction (a sentence carrying one
            // unsupported clause fails entirely and gets annotated).
            Some(spans) => spans,
            None => {
                model_calls += 1;
                let extracted =
                    extract_claim_list(&inference, question, &text, budget, posture).await;
                let Some(claims) = extracted else {
                    // Extraction failed; the time was spent, so it is
                    // attributed rather than dropped (ARCH §18.3).
                    crate::runtime::stage_ledger::Stage::new(
                        audit_stage,
                        sovereign_contracts::types::StackOwner::Incumbent,
                    )
                    .cause(audit_cause)
                    .calls(model_calls)
                    .record(audit_started.elapsed().as_millis() as u64);
                    return AuditPassOutcome::ExtractionFailed;
                };
                claims
            }
        };
        // Progress: the extracted claim list opens (or re-opens,
        // on the rewrite's re-audit) the desktop's check panel.
        emit_gate_progress(
            progress,
            NarrationPhase::ClaimCheckStart {
                claims: claims.iter().take(budget).map(|c| wire_claim(c)).collect(),
                recheck,
            },
        );
        let mut failed: Vec<FailedClaim> = Vec::new();
        // Claims whose judge returned NO verdict. Counted so the exit
        // below can say `judge_failed_open` instead of `released`.
        let mut unjudged: Vec<String> = Vec::new();
        // Forensics header: the evidence window this pass judges against,
        // written once so the per-claim records below stay small. No-op
        // unless SOVEREIGN_GATE_AUDIT_FORENSICS names a file.
        let audit_id = uuid::Uuid::new_v4().to_string();
        if config::audit_forensics_path().is_some() {
            audit_forensics(&serde_json::json!({
                "kind": "audit",
                "audit_id": audit_id,
                "run": crate::run_identity::run_id(),
                "ts": chrono::Utc::now().to_rfc3339(),
                "recheck": recheck,
                "incremental": is_incremental,
                "question": question,
                "answer": text,
                "answer_chars": text.chars().count(),
                "budget": budget,
                "tau": tau,
                "n_claims_extracted": claims.len(),
                "claims": claims.iter().take(budget).collect::<Vec<_>>(),
                "per_claim_chunks": per_claim_chunks,
                "leaf_chunks": leaf_chunks,
                "summary_chunks": summary_chunks,
                "evidence_labels": evidence_labels,
            }));
        }
        // Evidence + labels, lowercased once, for the deterministic
        // in-world attribution veto below.
        let hay_lower = {
            let mut h = leaf_chunks.join(" ").to_lowercase();
            for l in evidence_labels {
                h.push(' ');
                h.push_str(&l.to_lowercase());
            }
            h
        };
        // Batched support pre-pass (SOVEREIGN_GATE_BATCH_VERIFY, default OFF):
        // one call judges all claims with the evidence prefilled ONCE, so the
        // N per-claim re-prefills of the same evidence collapse to one on the
        // prefix-cache-vetoed qwen35moe. Indexed by the same enumerate() index
        // as the loop below. Empty when the flag is off → the loop runs exactly
        // as before. GATED on claim count: with only a few claims the single
        // batched prefill does not amortise (measured net-negative below ~6
        // claims), so small answers keep the per-claim path.
        let claim_texts: Vec<String> = claims.iter().take(budget).cloned().collect();
        // WHAT EACH CLAIM GETS, decided here, once, before any IO. The
        // fan-out below and the judging loop both read this vector, so there
        // is one predicate rather than two that must be kept in step.
        let dispositions = dispositions(&claim_texts, &hay_lower);
        let shadow_mode = config::gate_batch_shadow_enabled();
        // The batched pass is STUDY apparatus (BATCH_VERIFY / BATCH_SHADOW,
        // both default OFF) and runs only under those flags, above their
        // amortisation threshold. It no longer doubles as a TRIAGE for the
        // corpus fan-out. That ladder was retired 2026-09-02 (issue #57): a
        // model call spent to skip deterministic searches measured 185 s on
        // the reporter's box against half a second of searching, and no
        // per-turn pricing of it survived a second corpus. The fan-out below
        // is concurrent under one permit, which bounds the cost the triage
        // was trying to dodge — deterministically, with no model in the loop.
        let mut prefetched: Vec<(usize, Vec<String>)> = Vec::new();
        let batched_support: Vec<Option<bool>> = if (config::gate_batch_verify_enabled()
            || shadow_mode)
            && claim_texts.len() >= config::gate_batch_min_claims()
        {
            model_calls += 1;
            judge::claims_support_batched(
                &inference,
                &claim_texts,
                &leaf_chunks,
                per_claim_chunks,
                posture,
            )
            .await
        } else {
            Vec::new()
        };
        // THE FAN-OUT, RUN CONCURRENTLY (issue #57). Below, the audit walks
        // claims IN ORDER and each one may search the sealed corpus. Those
        // searches were serial, and `claims x corpora` sequential round
        // trips is the only multiplicative term in the turn — the reason
        // the ladder exists at all. But a claim's hits depend on that claim
        // alone, so the SEARCHES have no order between them even though the
        // judging does: hoist them here, run them with bounded concurrency,
        // and let the loop below consume the results. The worst case collapses from the
        // SUM of the searches toward the SLOWEST ONE, and no claim loses a
        // search or a verdict — this is a scheduling change, not a
        // semantic one (the same argument `corpus_search.rs` makes for the
        // turn's own fan-out, which got this treatment in 2026-06).
        //
        // Both this and the loop read `dispositions`, so nothing can be
        // searched here that the loop would have skipped. A claim NOT
        // prefetched still falls through to the loop's own `s.search(...)`.
        if let Some(s) = &searcher {
            let want: Vec<(usize, String)> = claims
                .iter()
                .take(budget)
                .enumerate()
                .filter(|(i, _)| dispositions[*i] == ClaimDisposition::Judge)
                .map(|(i, c)| (i, c.clone()))
                .collect();
            if !want.is_empty() {
                use futures::StreamExt as _;
                let t_fan = std::time::Instant::now();
                let n = want.len();
                let fetched: Vec<(usize, Vec<String>)> =
                    futures::stream::iter(want.into_iter().map(|(i, c)| {
                        let s = std::sync::Arc::clone(s);
                        async move { (i, s.search(&c).await) }
                    }))
                    .buffered(config::claim_search_concurrency())
                    .collect()
                    .await;
                let fan_ms = t_fan.elapsed().as_millis() as u64;
                tracing::info!(
                    target: "grounding_gate",
                    event = "claim_search_fanout",
                    run = crate::run_identity::run_id(),
                    searches = n,
                    concurrency = config::claim_search_concurrency(),
                    elapsed_ms = fan_ms,
                    per_search_ms = fan_ms / n.max(1) as u64,
                    "claim search fan-out: every claim that needs one, at once rather than in turn"
                );
                prefetched.extend(fetched);
            }
        }
        for (claim_idx, claim) in claims.iter().take(budget).enumerate() {
            // Jurisdiction: honesty meta-language is not a world-claim —
            // "the system does not have access to X" can never be stated
            // by a passage, and auditing it prosecutes the answer's own
            // honesty (observed: refined honest declines rejected at vp
            // 0.85–0.98 on exactly these sentences). Deterministic shape
            // check; see is_self_referential_decline.
            if dispositions[claim_idx] == ClaimDisposition::Exempt {
                dbg(&format!(
                    "longform claim EXEMPT — self-referential decline: {claim:?}"
                ));
                audit_forensics(&serde_json::json!({
                    "kind": "claim", "audit_id": audit_id,
                    "claim_idx": claim_idx, "claim": claim,
                    "mechanism": "exempt_self_referential",
                    "failed": false, "vp": serde_json::Value::Null,
                }));
                // Exempt = ships unflagged; stamp the row so the
                // panel never shows a permanently-pending claim.
                emit_gate_progress(
                    progress,
                    NarrationPhase::ClaimVerdict {
                        index: claim_idx,
                        supported: true,
                    },
                );
                continue;
            }
            // Deterministic pre-check: an in-world attribution naming a
            // person absent from the ENTIRE evidence is fabricated — do
            // not ask the yes-biased joint judge (measured: "Betty
            // Alexander sent an email…" cleared at vp=0.010 despite the
            // name existing nowhere in the corpus; it shipped in 3 runs).
            if let ClaimDisposition::Vetoed { kind, token: name } = &dispositions[claim_idx] {
                dbg(&format!(
                    "longform claim VETOED — in-world attribution names {kind} {name:?}, absent from evidence: {claim:?}"
                ));
                emit_gate_progress(
                    progress,
                    NarrationPhase::ClaimVerdict {
                        index: claim_idx,
                        supported: false,
                    },
                );
                let extra = match &searcher {
                    Some(s) => s.search(claim).await,
                    None => Vec::new(),
                };
                audit_forensics(&serde_json::json!({
                    "kind": "claim", "audit_id": audit_id,
                    "claim_idx": claim_idx, "claim": claim,
                    "mechanism": "deterministic_veto",
                    "veto_kind": kind, "veto_token": &name,
                    "failed": true, "vp": serde_json::Value::Null,
                    "extra": &extra,
                }));
                failed.push(FailedClaim {
                    claim: claim.clone(),
                    evidence: extra,
                });
                continue;
            }
            // Claim-conditioned retrieval: verify against the
            // sealed CORPUS, not just the prompt snapshot. The
            // SHARED prompt window goes first and claim-specific
            // hits are APPENDED after it, so every per-claim judge
            // prompt shares one byte-stable evidence prefix — the
            // pinned-prefix state cache (SOVEREIGN_PREFIX_STATE)
            // can restore that prefix instead of re-prefilling the
            // ~10K-token evidence per claim on prefix-cache-vetoed
            // hybrids (hits-FIRST ordering diverged the prompts at
            // the first passage and thrashed the pin). Duplicates
            // resolve in favor of the shared copy; novel hits widen
            // the cap by their count, so they never displace a
            // shared chunk the old audit would have judged.
            // The claim-conditioned hits were fetched by the fan-out above
            // and are appended below, after the shared window exists.
            //
            // T1 P1.4 class policy: FACTUAL/SPECIFIC claims verify
            // against Leaf evidence only (a derived summary must
            // never be the source-of-truth for a fact); THEMATIC/
            // STRUCTURAL claims may additionally rest on Summary-
            // class chunks — appended AFTER the leaf window, and the
            // judge is told the leaf window is the family boundary
            // (`n_stable` below), so the leaf prefix stays byte-stable
            // across both classes. With no summaries in evidence the
            // window is exactly the pre-P1.4 one.
            let factual = summary_chunks.is_empty()
                || judge::claim_is_factual_specific(&inference, claim).await;
            let mut shared: Vec<String> =
                leaf_chunks.iter().take(per_claim_chunks).cloned().collect();
            if !factual {
                shared.extend(summary_chunks.iter().take(per_claim_chunks).cloned());
                dbg(&format!(
                    "longform claim THEMATIC — {} summary chunk(s) admitted as evidence: {claim:?}",
                    summary_chunks.len().min(per_claim_chunks)
                ));
            }
            let seen: HashSet<String> = shared
                .iter()
                .map(|c| c.chars().take(120).collect::<String>())
                .collect();
            // Every sibling claim-check declares this same shared-window
            // boundary (judge::stable_passages_prefix_len), so the engine
            // pins the evidence state once per turn and restores it for
            // claims 2..N — including claims that append extra hits.
            let n_shared = shared.len();
            // The FAMILY BOUNDARY every sibling judge declares is the leaf
            // window alone. Summary chunks (thematic claims) and re-searched
            // hits sit after it, so a thematic claim restores the same pin
            // its factual siblings use instead of re-learning the whole
            // window (measured 24.7 s per thematic claim before 2026-09-01,
            // issue #57). The batched pass declares the same boundary.
            let n_stable = leaf_chunks.len().min(per_claim_chunks);
            // SHADOW (SOVEREIGN_GATE_CLAIM_SEARCH_SHADOW, default OFF):
            // keep a copy of the prompt-only window so the SAME claim can
            // be re-judged without the re-searched hits. Unlike the
            // single-claim path, `claim_violation_joint` judges all
            // passages in ONE forced-choice — there is no per-chunk max to
            // decompose — so the counterfactual costs one extra call per
            // claim. That call's passages are exactly the pinned shared
            // prefix, so it restores rather than re-prefills.
            let shadow_claim_search = config::claim_search_shadow_enabled();
            let shared_only: Option<Vec<String>> = if shadow_claim_search {
                Some(shared.clone())
            } else {
                None
            };
            // The fan-out above already searched this claim when it was
            // eligible; reuse those hits. A claim it did not prefetch still
            // searches here, so a predicate mismatch costs latency, never a
            // verdict.
            let prefetched_hits: Option<Vec<String>> = prefetched
                .iter()
                .find(|(idx, _)| *idx == claim_idx)
                .map(|(_, hits)| hits.clone());
            let extra: Vec<String> = match (prefetched_hits, &searcher) {
                (Some(hits), _) => hits,
                (None, Some(s)) => {
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
                (None, None) => Vec::new(),
            };
            let mut judged = shared;
            judged.extend(
                extra
                    .iter()
                    .filter(|c| !seen.contains(&c.chars().take(120).collect::<String>()))
                    .cloned(),
            );
            let cap = judged.len();
            // ASYMMETRIC TRUST (order audit-economy D2; the shape D0
            // pre-registered and D1 recalibrated): the batched verdict may
            // only CLEAR. A batch "supported" releases the claim without a
            // per-claim call — the error class that creates, false-
            // "supported", is exactly what the D1 replay priced (catch
            // 0.950/clear 1.000, zero (c)-class loss on the pinned set,
            // fc58319d). A batch "unsupported" is NOT a released flag:
            // the batched text A/B is not calibrated against tau, so it
            // falls through to the calibrated `claim_violation_joint`
            // over `judged` (shared + re-searched extras) — flags stay
            // fully calibrated by construction and the rescue search
            // stays judgeable. A parse gap (None) falls through the same
            // way. The earlier both-directions shape (flag at vp 1.0 on
            // batch "unsupported") shipped uncalibrated flags and threw
            // away the rescue extras it had already searched.
            // VERDICT source, gated on the batch flags only.
            let batch_v = if config::gate_batch_verify_enabled() || shadow_mode {
                batched_support.get(claim_idx).and_then(|v| *v)
            } else {
                None
            };
            let vp_opt = if shadow_mode {
                // SHADOW: keep BASELINE behavior (calibrated per-claim) but log
                // the batched verdict alongside so batch-vs-calibrated agreement
                // can be scored without changing any answer.
                model_calls += 1;
                let cal =
                    claim_violation_joint(&inference, claim, &judged, cap, n_stable, posture).await;
                dbg(&format!(
                    "shadow claim {claim_idx}: batch={batch_v:?} cal_vp={cal:?} cal_supported={:?}",
                    cal.map(|vp| vp < tau)
                ));
                cal
            } else {
                match batch_v {
                    // Batch: supported → cleared (vp below tau).
                    Some(true) => Some(0.0),
                    // Batch: unsupported OR parse gap → the calibrated
                    // forced-choice decides, over shared + extras.
                    _ => {
                        model_calls += 1;
                        claim_violation_joint(&inference, claim, &judged, cap, n_stable, posture)
                            .await
                    }
                }
            };
            // The counterfactual, logged next to the production verdict.
            // Nothing here feeds `vp_opt` — the released answer is
            // untouched; this only prices what the re-search bought.
            if let (Some(so), Some(vp), false) = (shared_only.as_ref(), vp_opt, extra.is_empty()) {
                model_calls += 1;
                let vp_wo =
                    claim_violation_joint(&inference, claim, so, so.len(), n_stable, posture).await;
                match vp_wo {
                    Some(vp_wo) => tracing::info!(
                        target: "grounding_gate",
                        event = "claim_search_shadow",
                        claim = %claim.chars().take(90).collect::<String>(),
                        extras = extra.len(),
                        n_shared,
                        vp_production = format!("{vp:.3}").as_str(),
                        vp_chunks_only = format!("{vp_wo:.3}").as_str(),
                        delta = format!("{:.3}", vp_wo - vp).as_str(),
                        tau = format!("{tau:.3}").as_str(),
                        verdict_flips = (vp < tau) != (vp_wo < tau),
                        rescued = (vp < tau) && (vp_wo >= tau),
                        newly_failed = (vp >= tau) && (vp_wo < tau),
                        "claim search shadow: with re-search vs prompt chunks alone (no answer changed)"
                    ),
                    None => tracing::info!(
                        target: "grounding_gate",
                        event = "claim_search_shadow",
                        claim = %claim.chars().take(90).collect::<String>(),
                        extras = extra.len(),
                        vp_production = format!("{vp:.3}").as_str(),
                        vp_chunks_only = "unavailable",
                        "claim search shadow: counterfactual judge returned no verdict"
                    ),
                }
            }
            // Forensics: the claim, the mechanism that judged it, and the
            // passages it was judged against — the record D1 needs to ask
            // whether the audit's own verdict was right.
            if config::audit_forensics_path().is_some() {
                audit_forensics(&serde_json::json!({
                    "kind": "claim", "audit_id": audit_id,
                    "claim_idx": claim_idx, "claim": claim,
                    // The DECIDER: only a batch "supported" releases a
                    // verdict; batch "unsupported"/gap rows are decided by
                    // the calibrated judge (asymmetric trust, D2).
                    "mechanism": if batch_v == Some(true) && !shadow_mode { "batched" } else { "per_claim_judge" },
                    "failed": vp_opt.map(|vp| vp >= tau),
                    "vp": vp_opt,
                    "tau": tau,
                    "factual_class": factual,
                    "n_shared": n_shared,
                    "extra": &extra,
                }));
            }
            match vp_opt {
                Some(vp) => {
                    dbg(&format!("longform claim vp={vp:.3} {claim:?}"));
                    emit_gate_progress(
                        progress,
                        NarrationPhase::ClaimVerdict {
                            index: claim_idx,
                            supported: vp < tau,
                        },
                    );
                    if vp >= tau {
                        failed.push(FailedClaim {
                            claim: claim.clone(),
                            evidence: extra,
                        });
                    }
                }
                None => {
                    // The judge returned no verdict (provider error,
                    // admission-queue shed, parse gap). The ladder fails
                    // open per claim — the row resolves and the text
                    // ships — but the claim is RECORDED as unjudged, so
                    // the audit exits `judge_failed_open` and the ledger
                    // says FailOpen, never Verified (ARCH §18.3). Issue
                    // #57: eight shed judges had released as eight
                    // verified holdings on a `grounded` turn.
                    unjudged.push(claim.clone());
                    tracing::warn!(
                        target: "grounding_gate",
                        event = "claim_unjudged",
                        claim = %claim.chars().take(90).collect::<String>(),
                        "per-claim judge returned no verdict — the claim ships unflagged and is recorded as unjudged"
                    );
                    emit_gate_progress(
                        progress,
                        NarrationPhase::ClaimVerdict {
                            index: claim_idx,
                            supported: true,
                        },
                    );
                }
            }
        }
        // Holistic supporting-specifics scan: catches the fabricated
        // details the load-bearing claim extraction misses (misattribution,
        // fake values, phantom section refs). One extra judge pass over the
        // WHOLE text vs the FULL evidence; its findings join `failed` and
        // ride the same rewrite/annotate path. Each flagged specific gets a
        // claim-conditioned search so the rewrite has corrective material —
        // which ALSO self-corrects a false positive: a truly-grounded
        // specific gets its grounding passage back, so the rewrite keeps it.
        // THE SCAN SEES THE SUMMARY CHUNKS TOO, and the asymmetry that
        // makes this safe under Fix B is the whole argument.
        //
        // Fix B (2026-06-17, see `GateEvidenceParts`) bars an abstractive
        // summary from being the source-of-truth a FACTUAL CLAIM is
        // verified against — "a fabrication grounding a fabrication". That
        // hazard is about ACCEPTING a claim on paraphrase evidence. This
        // scan does not accept anything: its entire output is ACCUSATIONS
        // ("these statements are unsupported"). Showing it a summary can
        // only ever WITHDRAW an accusation about text the system itself put
        // in the drafter's prompt; it can never let a fabrication through,
        // because nothing here clears a claim.
        //
        // Withholding them did not protect the invariant, it manufactured
        // false alarms: measured 2026-08-13, 16 of 20 summary-grounded
        // failures came from this scan flagging content stated verbatim in
        // a Summary chunk the drafter had been given — "Key figures such as
        // John Martin Fischer and Paul Russell advance strategies like
        // reasons-responsiveness" flagged as a fabricated specific while
        // sitting word for word in summary #29 (note 95b82f97).
        //
        // Operator, 2026-08-13: "epistemic honesty is the point" — a
        // summary is a legitimate evidence node when its provenance is
        // inspectable, and the answer to a paraphrase is traceability, not
        // blindness. The per-claim judge's factual-claim admission needs
        // that traceability carried explicitly (RaptorNode::quote_spans)
        // and is NOT changed here; this site needs only to stop accusing
        // the drafter of inventing what we handed it.
        if specifics_scan_enabled() {
            model_calls += 1;
            if let Some(specifics) = scan_unsupported_specifics(
                &inference,
                question,
                &text,
                leaf_chunks,
                summary_chunks,
                budget,
                posture,
            )
            .await
            {
                for spec in specifics {
                    // Citations are validated by the deterministic snap pass
                    // BEFORE this audit — a scan finding about a passage
                    // header is out of its jurisdiction (observed 2026-07-01:
                    // the scan flagged REAL label citations, which then read
                    // as self-indictment in the verification note).
                    //
                    // BOTH header shapes, not just one. The 2026-07-01 fix
                    // named `[Source:` and stopped there, but `formatters.rs`
                    // emits `[Web: title]` for live web-fetch results from the
                    // same builder — so the system went on flagging its own
                    // passage headers as fabricated specifics. Measured
                    // 2026-08-13: `[Web: compatibilism]` and
                    // `[Web: experimental-philosophy]` fired on 3 of 8 desktop
                    // audit passes, each one triggering the repair chain
                    // (note 95b82f97). One rule, every header the system writes.
                    let low = spec.to_lowercase();
                    if low.contains("[source:") || low.contains("[web:") {
                        continue;
                    }
                    // Same jurisdiction rule as the claim loop: the
                    // answer's own honesty meta-language is exempt.
                    if judge::is_self_referential_decline(&spec) {
                        continue;
                    }
                    // Skip specifics already surfaced by the per-claim audit.
                    if failed
                        .iter()
                        .any(|f| f.claim.contains(&spec) || spec.contains(&f.claim))
                    {
                        continue;
                    }
                    let corrective = match &searcher {
                        Some(s) => s.search(&spec).await,
                        None => Vec::new(),
                    };
                    dbg(&format!(
                        "specifics_scan flagged {:?} (corrective_hits={})",
                        spec.chars().take(60).collect::<String>(),
                        corrective.len()
                    ));
                    audit_forensics(&serde_json::json!({
                        "kind": "claim", "audit_id": audit_id,
                        "claim_idx": serde_json::Value::Null, "claim": &spec,
                        "mechanism": "specifics_scan",
                        "failed": true, "vp": serde_json::Value::Null,
                        "extra": &corrective,
                    }));
                    failed.push(FailedClaim {
                        claim: spec,
                        evidence: corrective,
                    });
                }
            }
        }
        // Sentence-level identifier sweep: the vetoes above only see
        // EXTRACTED claims, and ghost identifiers ride non-load-bearing
        // sentences the extractor never surfaces (gen75d s2: `cmd_init` /
        // `found.rs`, receipt-absent from the corpus, released inside a
        // rewrite despite the claim-level veto). Sweep every sentence of
        // the text with the same scoped checks; hits become synthetic
        // failed claims and ride the existing rewrite/annotate ladder.
        for sentence in text.split(['.', '\n']) {
            let sentence = sentence.trim();
            if sentence.chars().count() < 20 {
                continue;
            }
            let hit = judge::absent_identifier_attribution(sentence, &hay_lower)
                .or_else(|| judge::absent_name_attribution(sentence, &hay_lower));
            if let Some(ident) = hit {
                if failed.iter().any(|f| f.claim.contains(&ident)) {
                    continue;
                }
                dbg(&format!(
                    "longform sentence sweep VETOED {ident:?} (absent from evidence)"
                ));
                let synthetic = format!(
                    "The answer references \"{ident}\", which does not appear in the sources."
                );
                let extra = match &searcher {
                    Some(s) => s.search(&synthetic).await,
                    None => Vec::new(),
                };
                audit_forensics(&serde_json::json!({
                    "kind": "claim", "audit_id": audit_id,
                    "claim_idx": serde_json::Value::Null, "claim": &synthetic,
                    "mechanism": "identifier_sweep",
                    "veto_token": &ident, "sentence": sentence,
                    "failed": true, "vp": serde_json::Value::Null,
                    "extra": &extra,
                }));
                failed.push(FailedClaim {
                    claim: synthetic,
                    evidence: extra,
                });
            }
        }
        let audited: Vec<String> = claims.into_iter().take(budget).collect();
        // G4 — the pass is done. Recorded HERE, at the exit of the code
        // that ran it, with the mechanism it actually used: this is an
        // incumbent per-claim generative audit, and it says so because
        // it just performed one, not because a flag says the ladder is
        // on.
        crate::runtime::stage_ledger::Stage::new(
            audit_stage,
            sovereign_contracts::types::StackOwner::Incumbent,
        )
        // The ReAudit stage now has two arms; the row records the arm
        // actually taken, at the site that took it (the strip's honesty
        // rule — never inferred from a flag).
        .mechanism(if is_incremental {
            sovereign_contracts::types::StageMechanism::IncrementalReVerify
        } else {
            sovereign_contracts::types::StageMechanism::PerClaimJudge
        })
        .cause(audit_cause)
        .calls(model_calls)
        .record(audit_started.elapsed().as_millis() as u64);
        audit_forensics(&serde_json::json!({
            "kind": "audit_result",
            "audit_id": audit_id,
            "recheck": recheck,
            "audited": audited.len(),
            "failed": failed.len(),
            "ratio": if audited.is_empty() { 0.0 } else { failed.len() as f64 / audited.len() as f64 },
            "zero_failure": failed.is_empty(),
            "model_calls": model_calls,
            "audit_ms": audit_started.elapsed().as_millis() as u64,
            "failed_claims": failed.iter().map(|f| &f.claim).collect::<Vec<_>>(),
            "unjudged": unjudged.len(),
        }));
        AuditPassOutcome::Judged {
            text,
            audited,
            failed,
            unjudged,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::types::{CompletionResponse, Depth, ProviderCapabilities, Speed};
    use futures::Stream;
    use std::pin::Pin;
    use std::sync::Mutex;

    /// The per-claim plan, one row per variant, using the judge module's own
    /// fixtures: a decline is exempt, an in-world artifact attribution by a
    /// name the evidence never mentions is vetoed, and ordinary prose about a
    /// name the evidence does mention is judged.
    #[test]
    fn every_disposition_variant_is_reachable_and_nothing_else_is() {
        let hay = "frankfurt cases are the primary compatibilist response.".to_string();
        let claims = vec![
            "I don't have access to the shop's opening hours.".to_string(),
            "Betty Alexander sent an email about the schedule.".to_string(),
            "Harry Frankfurt designed cases about responsibility.".to_string(),
        ];
        let d = dispositions(&claims, &hay);
        assert_eq!(
            d.len(),
            claims.len(),
            "one disposition per claim, index-aligned"
        );
        assert_eq!(d[0], ClaimDisposition::Exempt, "{:?}", claims[0]);
        assert_eq!(
            d[1],
            ClaimDisposition::Vetoed {
                kind: "person",
                token: "Betty Alexander".to_string()
            },
            "{:?}",
            claims[1]
        );
        assert_eq!(d[2], ClaimDisposition::Judge, "{:?}", claims[2]);
        assert!(dispositions(&[], &hay).is_empty());
    }

    /// The veto is decided before any IO and never reaches the judge; the
    /// same claim with its name present in the evidence is judged.
    #[test]
    fn a_vetoed_claim_becomes_judged_once_the_evidence_names_the_person() {
        let claim = vec!["Betty Alexander sent an email about the schedule.".to_string()];
        assert!(matches!(
            dispositions(&claim, "unrelated evidence with no such person")[0],
            ClaimDisposition::Vetoed { kind: "person", .. }
        ));
        assert_eq!(
            dispositions(&claim, "betty alexander sent an email about the schedule")[0],
            ClaimDisposition::Judge
        );
    }

    /// The three registers one audit pass issues, told apart at the REQUEST
    /// boundary rather than by matching prompt prose — the same discipline
    /// `judge.rs`'s family tests use, and the reason this fixture does not
    /// go stale when a prompt is reworded.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Register {
        /// Claim extraction: its own system turn, generative.
        ClaimList,
        /// The calibrated per-claim judge: the only forced choice, one token.
        ForcedChoice,
        /// The holistic specifics scan: the judges' system turn, generative.
        SpecificsScan,
    }

    fn register_of(r: &CompletionRequest) -> Register {
        if r.max_tokens == Some(1) && r.structured_output.is_some() {
            return Register::ForcedChoice;
        }
        match r.system_message.as_deref() {
            Some(s) if s.contains("extract claims") => Register::ClaimList,
            _ => Register::SpecificsScan,
        }
    }

    /// A gate-shaped provider: it answers each register with a scripted reply
    /// and records which registers actually ran, so a test can assert on the
    /// SHAPE of the pass and not only on its result.
    struct ScriptedAudit {
        /// Claim extraction's reply — one claim per line.
        claims: String,
        /// p(A) every forced choice reports. `support`, not violation:
        /// `claim_violation_joint` returns `1 - a/(a+b)`.
        support: f64,
        /// The specifics scan's reply — verbatim spans of the answer.
        scan: String,
        seen: Mutex<Vec<Register>>,
    }

    #[async_trait::async_trait]
    impl InferenceProvider for ScriptedAudit {
        async fn complete(&self, r: &CompletionRequest) -> Result<CompletionResponse> {
            let reg = register_of(r);
            self.seen.lock().unwrap().push(reg);
            let text = match reg {
                Register::ClaimList => self.claims.clone(),
                Register::ForcedChoice => {
                    format!(r#"{{"A": {}, "B": {}}}"#, self.support, 1.0 - self.support)
                }
                Register::SpecificsScan => self.scan.clone(),
            };
            Ok(CompletionResponse {
                text,
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "scripted".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
            })
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            unimplemented!("the audit pass never streams")
        }
        async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            unimplemented!("the audit pass never embeds")
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 32_768,
                supports_structured_output: true,
                relative_speed: Speed::Slow,
                relative_reasoning: Depth::Deep,
            }
        }
    }

    /// **GR-08 — the holistic specifics scan is NOT conditioned on the
    /// per-claim verdicts, and that independence is what holds the line the
    /// truncation lift gave up.**
    ///
    /// Note 95b82f97 (2026-08-13) established with a watched specimen that A
    /// CUT CHUNK MANUFACTURES ABSENCES — "The Luck Objection" flagged as a
    /// fabricated specific while sitting verbatim at offset 1,497 of a chunk
    /// cut at 1,500 — and land B removed the per-chunk cap for the per-claim
    /// judge on that argument. Note 1aeadf1a records the symmetric hazard the
    /// same change bought: A RESTORED CHUNK MANUFACTURES PRESENCES. On
    /// `longneg-distract-evidence-chain`, chunk[9] is 2,040 chars, so land B
    /// surfaced a 540-char tail reading "the boat-hook had been standing
    /// against his own barrow of salvage gear by the door" — and the per-claim
    /// judge then CLEARED "a heavy salvage-pattern boat-hook", a phrase with
    /// ZERO hits in the evidence on either arm. Restoring the tail did not
    /// restore support; it supplied a confusor.
    ///
    /// The seat ruled land B stands (1aeadf1a): re-truncating re-buys
    /// accidental catches at the price of systematic false failures, which is
    /// the judge-gaming direction ARCH §18.6 exists to block. The ruling rests
    /// on the flip needing TWO defects — the confusor, and a composite claim
    /// clearing while carrying an unsupported specific — and on the second one
    /// having an INDEPENDENT reader. The specifics scan is that reader.
    ///
    /// So the property under test is not "the judge gets it right". It is that
    /// a turn whose every extracted claim the calibrated judge SUPPORTED is
    /// still scanned holistically, and that the scan's findings still become
    /// failures. Condition the scan on the per-claim audit having already
    /// found something and the seat's ruling loses its second defect, silently.
    #[tokio::test]
    async fn the_specifics_scan_runs_when_every_claim_cleared() {
        // The reader of this test needs to know the scan was ON, not assume
        // it: with `SOVEREIGN_SPECIFICS_SCAN=0` in the environment the pass
        // below legitimately produces no finding and this test would be
        // reporting the flag, not the invariant (ARCH §18.1, four verdicts).
        assert!(
            specifics_scan_enabled(),
            "SOVEREIGN_SPECIFICS_SCAN is off in this environment — this test \
             cannot judge the invariant, it can only report the flag"
        );

        // The specimen, reduced to its two moving parts. The evidence carries
        // the confusor vocabulary ("boat-hook", "salvage gear") one clause
        // apart; the answer asserts a specific ("salvage-pattern") the
        // evidence never states.
        const ANSWER: &str = "A heavy salvage-pattern boat-hook stood by the door.";
        const FABRICATED: &str = "heavy salvage-pattern boat-hook";
        let leaf = vec![
            "The boat-hook had been standing against his own barrow of salvage gear \
             by the door."
                .to_string(),
        ];
        assert!(
            !leaf[0].to_lowercase().contains("salvage-pattern"),
            "the fixture's fabricated specific must be absent from the evidence \
             or there is nothing for the scan to catch"
        );

        let provider = Arc::new(ScriptedAudit {
            claims: ANSWER.to_string(),
            // The judge CLEARS the composite claim — the measured behaviour
            // this test exists because of. vp = 1 - 0.99 = 0.01, well under tau.
            support: 0.99,
            scan: FABRICATED.to_string(),
            seen: Mutex::new(Vec::new()),
        });
        let inference: Arc<dyn InferenceProvider> = provider.clone();
        let pass = AuditPass {
            inference,
            searcher: None,
            question: "What stood by the door?",
            leaf_chunks: &leaf,
            summary_chunks: &[],
            evidence_labels: Vec::new(),
            per_claim_chunks: 8,
            min_claims: 1,
            tau: 0.9,
            posture: sovereign_contracts::oicp::ShardingPrivacy::LocalOnly,
            progress: None,
        };

        let AuditPassOutcome::Judged {
            audited,
            failed,
            unjudged,
            ..
        } = pass.run(ANSWER.to_string(), PassKind::Draft).await
        else {
            panic!("claim extraction was scripted to succeed — the pass must reach Judged");
        };

        // The premise, asserted rather than assumed: the per-claim ladder
        // found NOTHING. Without this the test below could pass because the
        // judge flagged the claim, which is the opposite of the specimen.
        assert_eq!(audited.len(), 1, "one claim was extracted and audited");
        assert!(
            unjudged.is_empty(),
            "the scripted judge returned a verdict for every claim; an unjudged \
             claim means this test measured a parse gap, not the invariant"
        );
        let registers = provider.seen.lock().unwrap().clone();
        assert!(
            registers.contains(&Register::ForcedChoice),
            "the calibrated per-claim judge never ran, so 'every claim cleared' \
             is vacuous here: {registers:?}"
        );

        // THE INVARIANT. The only producer of this failure is the holistic
        // scan — the per-claim judge said supported.
        assert!(
            registers.contains(&Register::SpecificsScan),
            "the holistic specifics scan did not run on a turn whose claims all \
             cleared. That is the exact condition note 1aeadf1a's seat ruling \
             depends on: land B's confusor only flips an answer when a composite \
             claim clears AND nothing independent reads its specifics. \
             Registers: {registers:?}"
        );
        assert_eq!(
            failed.len(),
            1,
            "the scan's finding did not become a failure. The per-claim judge \
             cleared the composite claim (vp 0.01 < tau 0.9); the fabricated \
             specific {FABRICATED:?} is absent from the evidence and the scan \
             flagged it, so the pass must carry exactly one failure. Got: {:?}",
            failed.iter().map(|f| &f.claim).collect::<Vec<_>>()
        );
        assert_eq!(
            failed[0].claim, FABRICATED,
            "the failure must be the scan's span, verbatim from the answer"
        );
    }
}
