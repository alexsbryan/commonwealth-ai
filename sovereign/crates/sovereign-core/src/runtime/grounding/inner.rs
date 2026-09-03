//! `gate_answer_inner` — the verify-only / audit ladder body once the
//! surface and profile are chosen. Split out of mod.rs 2026-09-02 (ARCH §3.1).

use super::*;

/// The gate ladder itself. Callers go through
/// [`gate_answer_with_progress`], which journals the decision — calling
/// this directly would be an unrecorded gate decision, which is the
/// thing the wrapper exists to make impossible.
pub(crate) async fn gate_answer_inner(
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
    // H1's verdict for this turn, bound ONCE at the top so every exit of
    // this ladder can report it (see `with_native_verdict`). It was bound
    // at the decline guard before 2026-08-12, i.e. after four of this
    // function's six exits — which is why those four journaled nothing.
    // Telemetry: nothing below reads this to decide anything.
    let native = evidence.native_verdict.as_ref();
    // T1 P1.4: the short path audits ONE central FACTUAL claim, and its
    // citation / value-presence / name checks are all factual-class — so
    // this whole ladder reads the Leaf view only. A quote or value that
    // exists solely inside a derived RAPTOR summary is LLM prose quoting
    // LLM prose and must not ground a release. With no Summary-class
    // chunks (every pre-P1.4 surface) this is `evidence.chunks` itself.
    let leaf_owned: Vec<String>;
    // Locators travel through the SAME filter as the chunks they name. They
    // are looked up by index, so a Leaf-only chunk view paired with the full
    // locator list would attribute every quote to the wrong passage — the one
    // failure mode a citation label must not have.
    let leaf_locators: Vec<Option<String>>;
    // Targets travel through the SAME filter for the same reason, and with a
    // sharper consequence: a target read off the unfiltered list opens a
    // passage the reader was never shown.
    let leaf_targets: Vec<Option<CitationTarget>>;
    // Custody + URL travel through the SAME filter for the same reason:
    // a custody view read off the unfiltered list would pin a stamp to
    // the wrong passage (or worse, read a stamped chunk as unstamped).
    let leaf_custodies: Vec<Option<crate::types::Custody>>;
    let leaf_urls: Vec<Option<String>>;
    // Grain travels through the SAME filter, for the reason above and one
    // more: it is what the released citation's [`kernel_types::Origin`]
    // carries, so a grain read off the unfiltered list would stamp a quote
    // with another chunk's provenance. Binding it here rather than assuming
    // `Leaf` keeps the seal honest if this filter ever changes (rung
    // nc-20-turn-adoption).
    let leaf_grains: Vec<Grain>;
    // `_urls`: the leaf view's URLs exist for the ledger's locator
    // fallback, which the funnel derives from the FULL evidence; nothing
    // in the ladder reads the filtered view, so it is not bound.
    let (chunks, locators, targets, custodies, _urls, grains): (
        &[String],
        &[Option<String>],
        &[Option<CitationTarget>],
        &[Option<crate::types::Custody>],
        &[Option<String>],
        &[Grain],
    ) = if evidence.has_summary_evidence() {
        let keep: Vec<usize> = (0..evidence.chunks.len())
            .filter(|i| evidence.source_of(*i).may_be_quoted())
            .collect();
        leaf_owned = keep.iter().map(|i| evidence.chunks[*i].clone()).collect();
        leaf_locators = keep
            .iter()
            .map(|i| evidence.chunk_locators.get(*i).cloned().flatten())
            .collect();
        leaf_targets = keep
            .iter()
            .map(|i| evidence.chunk_targets.get(*i).cloned().flatten())
            .collect();
        leaf_custodies = keep
            .iter()
            .map(|i| evidence.chunk_custodies.get(*i).copied().flatten())
            .collect();
        leaf_urls = keep
            .iter()
            .map(|i| evidence.chunk_urls.get(*i).cloned().flatten())
            .collect();
        leaf_grains = keep.iter().map(|i| evidence.source_of(*i)).collect();
        (
            &leaf_owned,
            &leaf_locators,
            &leaf_targets,
            &leaf_custodies,
            &leaf_urls,
            &leaf_grains,
        )
    } else {
        leaf_grains = (0..evidence.chunks.len())
            .map(|i| evidence.source_of(i))
            .collect();
        (
            &evidence.chunks,
            &evidence.chunk_locators,
            &evidence.chunk_targets,
            &evidence.chunk_custodies,
            &evidence.chunk_urls,
            &leaf_grains,
        )
    };
    let entity_anchored = evidence.entity_anchored;
    // Custody refusal (custody.md §4, red R-3). When the stamp machinery
    // ENGAGED this turn — at least one chunk arrived with a stamp — an
    // unstamped chunk in the judged leaf view (sealed/pinned late
    // appends have no source row) must not ground a release: refuse
    // BEFORE any judge call, and let the funnel's ledger
    // (`record_gate_decision`) record the unknown row. Pure-unstamped
    // turns — every pre-custody surface, no stamp anywhere — are
    // untouched: with nothing stamped there is nothing to contrast the
    // unknown against, and this integration stays additive by
    // construction.
    let custody_engaged = evidence.chunk_custodies.iter().any(|c| c.is_some());
    if custody_engaged
        && custodies
            .iter()
            .any(|c| c.map(|c| !c.is_released_class()).unwrap_or(true))
    {
        let unstamped = custodies
            .iter()
            .filter(|c| c.map(|c| !c.is_released_class()).unwrap_or(true))
            .count();
        tracing::info!(
            target: "grounding_gate",
            unstamped,
            stamped = custodies.len() - unstamped,
            "gate refused: evidence holds unknown-provenance chunks (custody.md §4)"
        );
        return GateOutcome {
            answer: abstain(
                grounded_abstention(question, chunks.len().min(12)),
                inference,
                base_request.preferred_speed,
                "evidence holds unknown-provenance chunks (custody.md §4)".to_string(),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "refused_unknown_custody",
                    "retried": false,
                    "violation_prob": null,
                    "threshold": tau,
                    "mode": "custody",
                    "draft": config::debug_enabled().then(|| draft.clone()),
                }),
                native,
            ),
            claims: Vec::new(),
        };
    }
    // Glassbox: whether the call-graph block reached the sealed universe. A
    // code-intel answer whose caller facts land in the verification note is
    // either "the trace was never sealed" or "the judge rejected it" — these
    // two counts tell you which, from any entry path.
    tracing::info!(
        target: "grounding_gate",
        evidence_chunks = chunks.len(),
        has_code_trace = chunks.iter().any(|c| c.contains("Call-graph trace for")),
        trace_labels = evidence
            .source_labels
            .iter()
            .filter(|l| l.starts_with("Call-graph trace for"))
            .count(),
        "gate entry: sealed evidence universe"
    );
    // Verify-correct pivot. gate_longform is the BS-catcher: it extracts each
    // asserted claim, RE-SEARCHES the sealed corpus for that claim's evidence,
    // and REWRITES the ones the corpus won't support — catching the load-bearing-
    // specific confabulation ("Ernest Rhys Jones" for "Ernest Rhys") that the
    // single-claim path waves through. Short factual answers skip it by default
    // (pivot 1_800); `SOVEREIGN_LONGFORM_CHARS` A/Bs routing them through it
    // (0 = always per-claim, the resilient default complex_task already uses) so
    // the architecture catches a model's first-pass BS rather than trusting it.
    let longform_pivot = std::env::var("SOVEREIGN_LONGFORM_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(profile.longform_chars);
    if draft.chars().count() > longform_pivot {
        return gate_longform(
            inference,
            question,
            draft,
            evidence,
            base_request,
            profile,
            progress,
        )
        .await;
    }
    // Glassbox (debug-gated): record the pre-gate draft into the message meta so
    // the measurement layer can tell a gate-killed-CORRECT answer from a
    // confabulation the gate correctly caught — the partition's gate-vs-model
    // split (docs/CHAOS_MEASUREMENT_REDESIGN.md). `None` (→ null) in production:
    // the rejected draft can be the very confab the gate suppressed. Short-path
    // only — gate_longform never produces a clean abstention, so the split there
    // is vacuous. Moved into each diverging return below.
    let draft_for_meta: Option<String> = config::debug_enabled().then(|| draft.clone());
    // Active citation-grounding (entity-anchored fact queries, flag-gated).
    // Replaces generate-then-substring-verify with quote-then-answer: the model
    // must copy a verbatim supporting sentence before it answers, which forces
    // it to read the retrieved context (curing the measured A3B confabulations
    // where the answer was present but unused — "blowpipe" for the carving
    // knife) and grounds by quote-existence rather than value-substring (curing
    // the STOP-list/paraphrase false-negatives that killed "Chief Inspector").
    // Inconclusive (extraction error/unparseable) falls through to the legacy
    // ladder — fail-open, never a refusal from a hiccup. Does not consume the
    // draft, so the fall-through path is unchanged.
    if config::citation_grounding_enabled() && (entity_anchored || config::citation_broad_enabled())
    {
        // G4 — the quote-then-answer path. ECONOMY §4.1 labels it FUNCTION,
        // WRONG TIER: it buys adherence by spending output tokens on a
        // rehearsal, which is a prompt-shaped surrogate for a decode-time
        // constraint. Incumbent tier either way, and it is recorded whether
        // it grounds or falls through, because both cost the same call.
        //
        // THIS ROW EXISTS BECAUSE THE STRIP CAUGHT ITS OWN OMISSION. On the
        // first `citation_grounded` turn measured (2026-08-12) this path was
        // uninstrumented, so 11.08s of gate work landed in the
        // `gate_unattributed` residual and the turn rendered as
        // "no grounding stack ran". That is the defect-detection property
        // working as designed — and the fix is a row, not a smaller residual.
        let citation_started = std::time::Instant::now();
        let citation_outcome = citation::citation_grounded_answer(
            &**inference,
            question,
            chunks,
            locators,
            targets,
            crate::slot_policy::posture_of(base_request),
        )
        .await;
        crate::runtime::stage_ledger::Stage::new(
            sovereign_contracts::types::StageId::Citation,
            sovereign_contracts::types::StackOwner::Incumbent,
        )
        .cause(sovereign_contracts::types::StageCause::EveryTurn)
        .calls(1)
        .record(citation_started.elapsed().as_millis() as u64);
        if let citation::CitationOutcome::Grounded { answer, quotes } = citation_outcome {
            let quote_chars: usize = quotes.iter().map(|q| q.text.len()).sum();
            let located = quotes.iter().filter(|q| q.locator.is_some()).count();
            // The released passages as STRUCTURED rows, so a reading surface
            // can open the one the reader clicked. Until now the gate's
            // citation existed downstream only as prose inside the answer
            // string — which is why the system's best-attested citation, the
            // verbatim gate-verified one, was the only citation in the product
            // a user could not click.
            //
            // A quote with no target is DROPPED rather than emitted with a
            // null handle: a row here is a promise that clicking it opens the
            // passage quoted, and a row that cannot keep that promise is worse
            // than no row (§18.3 — absence is reported, never defaulted). The
            // prose rendering below is unchanged and still shows every quote,
            // so nothing disappears from what the reader can READ.
            // The turn, in kernel vocabulary (rung nc-20-turn-adoption).
            //
            // The seal is the leaf view — what this ladder is allowed to quote.
            // Each released quote is minted through
            // `kernel_types::Citation::pointing_into`, the ONE door: it refuses
            // a quote the seal does not hold verbatim, and refuses one landing
            // in material that may not be quoted. Both rules held here before,
            // as an upstream guarantee stated in three doc comments; they are
            // now a constructor, so no future quote path can skip either.
            //
            // BEHAVIOUR IS UNCHANGED, and that was checked rather than assumed:
            // a `GroundedQuote` carrying `Some(target)` is already one
            // contiguous run of ONE chunk (`QuoteMatch::Exact`), and seal
            // membership is exactly "has a `(corpus, chunk)` handle" — the same
            // predicate the old `target.clone()?` fold applied. What is new is
            // that a drop is a NAMED value carrying the quote and the seal size
            // instead of a `None` vanishing inside a `filter_map`.
            let seal = sealed::SealedEvidence::over(chunks, targets, custodies, grains);
            let mut turn_citations: Vec<kernel_types::Citation> = Vec::new();
            // Human section headings, index-parallel to `turn_citations` — the
            // display half of a citation, which the kernel `Origin` deliberately
            // does not carry (its `Locator` is the machine handle).
            let mut headings: Vec<Option<String>> = Vec::new();
            let mut refusals: Vec<kernel_types::Refused> = Vec::new();
            for q in &quotes {
                // No handle => no seal member => no row, exactly as the
                // `target.clone()?` fold decided before. Counted as a refusal
                // so the trace below distinguishes it from a quote the member
                // did not hold.
                let Some(target) = q.target.as_ref() else {
                    refusals.push(kernel_types::Refused::NotInSeal {
                        quote: q.text.clone(),
                        sealed_len: 0,
                    });
                    continue;
                };
                match seal.cite(target, q.text.as_str()) {
                    Ok(c) => {
                        turn_citations.push(c);
                        headings.push(q.locator.clone());
                    }
                    Err(r) => refusals.push(r),
                }
            }
            tracing::debug!(
                target: "grounding.seal",
                sealed = seal.len(),
                unhandled = seal.unhandled(),
                quotes = quotes.len(),
                cited = turn_citations.len(),
                refused = refusals.len(),
                why = ?refusals.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "citation release: quotes checked against the sealed leaf view"
            );
            dbg(&format!(
                "citation: GROUNDED → release (answer={:?} quotes={} located={located}/{} \
                 quote_chars={quote_chars})",
                answer.chars().take(60).collect::<String>(),
                quotes.len(),
                quotes.len()
            ));
            // Release the grounded value WITH its supporting quote as a
            // citation: glassbox (the user sees the exact sentence that grounds
            // the answer) AND a bare value ("the Doctor") is otherwise mis-read
            // as an abstention by the downstream answer/abstain classifier, which
            // wants a fuller response. The terse `answer` is what was verified
            // against the quote.
            //
            // EACH quote gets its OWN `"..."` span. The post-hoc
            // `quote_verification` pass re-checks a quoted span as one
            // contiguous source substring, so joining two verbatim sentences
            // inside one pair of quotes makes a correct citation fail that
            // re-check and ship as `[unverified excerpt: ...]` — measured on the
            // first arm-C run, 2026-08-05, where it hid a genuinely grounded
            // two-part answer behind an "unverified" label.
            //
            // The locator goes OUTSIDE the quote marks. That same re-check
            // reads whatever sits between the quotes as source text, so a
            // heading placed inside them would be read as part of the quote
            // and fail verbatim verification — the label would break the
            // citation it was added to explain.
            //
            // A quote with no locator renders exactly as it always did. The
            // corpus may have no section structure at all, or an unjoined
            // manifest, or the quote may have matched only across a chunk
            // boundary, or only as a partial run inside one; none of those
            // licence inventing a chapter.
            //
            // The renderer does NOT have to ask whether the post-hoc
            // `quote_verification` pass will demote a span before labelling it:
            // `GroundedQuote` guarantees a `Some(locator)` is source text
            // copied out of a single chunk, which that pass cannot demote. The
            // guarantee is upstream and structural, because a check here would
            // be a second decider re-deriving the first's verdict
            // (ARCH_PRINCIPLES §10.6). Measured 2026-08-05, before that
            // guarantee existed: a run-only match shipped as
            // `CHAPTER III — [unverified excerpt: …]`.
            let rendered = quotes
                .iter()
                .map(|q| {
                    let text = format!(
                        "\"{}\"",
                        q.text
                            .chars()
                            .take(CITATION_QUOTE_DISPLAY_CHARS)
                            .collect::<String>()
                    );
                    match &q.locator {
                        Some(loc) => format!("{loc} — {text}"),
                        None => text,
                    }
                })
                .collect::<Vec<_>>()
                .join("\n  ");
            let cited = format!("{answer}\n\nGrounded in the source:\n  {rendered}");
            // Second-opinion fabrication guard: the citation path grounds the
            // asserted VALUE against a quote, but a confabulated quote wearing a
            // real-passage shape can still slip a fabricated named entity
            // through (measured: "David Hart, COO of Knowledge Process Software"
            // over Enron evidence). Scan the asserted answer holistically; on a
            // flag, correct-or-abstain instead of releasing the fabrication.
            if let Some(guarded) = short_specifics_guard(
                inference,
                question,
                &answer,
                chunks,
                evidence.searcher.as_ref(),
                base_request,
                profile,
                native,
            )
            .await
            {
                return guarded;
            }
            // ── Release (rung nc-20-turn-adoption) ───────────────────────────
            //
            // The composed text becomes a `Draft`, whose text CANNOT BE READ,
            // and the only exit from a `Draft` is a release that says what is
            // known about it. This turn's verdict is a pass and it is one the
            // gate genuinely established: every released quote was re-checked
            // verbatim against the seal three lines up. The fold is
            // `Judgement::roll_up` inside `Draft::release` — one reducer, not a
            // second one written here (ARCH §10.6).
            //
            // Nothing about the released STRING changes: `Answer::text` is the
            // `cited` value that used to be assigned to `GateOutcome::text`
            // directly. What changes is that it can no longer be assigned
            // WITHOUT a judgement, because there is no other door.
            let verdict_reason = kernel_types::Reason::new(format!(
                "{} of {} released quote(s) verified verbatim against {} sealed chunk(s)",
                turn_citations.len(),
                quotes.len(),
                seal.len()
            ))
            .unwrap_or_else(|| kernel_types::Reason::literal("quotes verified against the seal"));
            // Through the same door every other exit uses. This site was
            // already correct before rung 9.2 and was the only one that was;
            // routing it through `release_held` too is what makes "one
            // decider" true rather than "one decider plus the original"
            // (ARCH §10.6).
            let released: kernel_types::Answer = release_held(
                cited,
                turn_citations,
                inference,
                base_request.preferred_speed,
                verdict_reason.to_string(),
            );
            // The wire rows are PROJECTED from the released answer rather than
            // assembled beside it: one decider for "what did this turn cite"
            // (ARCH §10.6). Before this, `meta["citations"]` and the answer's
            // own citations were two hand-built lists that happened to agree.
            let released_citations =
                crate::types::EpistemicState::citations_of(&released, &headings);
            let openable = released_citations.len();
            tracing::debug!(
                target: "grounding.seal",
                verdict = %released.judgement().verdict(),
                citations = released.citations().len(),
                openable,
                custody = ?released.evidence_custody().map(|c| c.as_str()),
                "citation release: answer sealed with its judgement"
            );
            return GateOutcome {
                // Was `released.text().to_string()` — the `Answer` was built
                // correctly here and then thrown away on the next line, which
                // is what made this the only judged exit of sixteen.
                answer: released,
                meta: with_native_verdict(
                    serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "citation_grounded",
                    "retried": false,
                    "mode": "citation",
                    "quote_chars": quote_chars,
                    "quotes": quotes.len(),
                    // How many released quotes carry a section locator. The
                    // gate already knew this — it was computed for a `dbg`
                    // line above and then dropped, so "did this answer tell
                    // the reader WHERE to look" existed only as prose in the
                    // released text and as a debug string.
                    //
                    // That gap is load-bearing, not cosmetic. The situated
                    // bench's `cites_a_source` criterion was reduced to
                    // re-deriving it with an LLM judge reading the answer, and
                    // measured 5/7 against this count's 7/7 (2026-08-05): it
                    // credited the one answer that did NOT disclose a gap and
                    // declined both that did, because a trailing "the passages
                    // do not answer X" distracted it. A fact the system
                    // computes must not be re-litigated downstream by a weaker
                    // decider (§10.6) — so it ships here, deterministically.
                    //
                    // `located <= quotes`, and `located == 0` is a legitimate
                    // reading, not a failure: a corpus with no section
                    // structure, an unjoined manifest, or a quote that matched
                    // only across chunks all release with no locator by design
                    // (see `gate_evidence_locators`).
                    "located": located,
                    // The openable passages, in release order. Rides the meta
                    // blob because the epistemic assembler already receives it
                    // — no new parameter through the handler chain for data
                    // the gate has already finished computing.
                    //
                    // `openable <= quotes`, and it is INDEPENDENT of `located`
                    // in both directions: a corpus with no section structure
                    // yields openable quotes with no chapter name, and a
                    // synthetic chunk yields the reverse. Reading either as a
                    // proxy for the other would misreport both.
                    "citations": released_citations,
                    "openable": openable,
                    "draft": draft_for_meta,
                    }),
                    native,
                ),
                claims: vec![GateClaim {
                    text: answer,
                    supported: true,
                    failed_once: false,
                    unjudged: false,
                    violation_prob: None,
                    // Filled post-gate, display only (see GateClaim::address).
                    address: None,
                }],
            };
        }
        // Not tightly grounded (no quote, quote not verbatim, or answer not
        // supported by its own quote) → fall through to the legacy verify →
        // retry → abstain ladder. Citation is purely ADDITIVE: it can only
        // upgrade a legacy abstention into a grounded release — it never causes
        // an abstention itself nor replaces a correct draft, so the legacy path
        // stays the honesty floor (measured 1.00).
        dbg("citation: not tightly grounded → fall through to legacy ladder");
    }
    let mut text = draft;
    let mut action = ACT_RELEASED;
    let mut retried = false;
    let mut final_vp: Option<f64> = None;
    // Why `final_vp` is what it is. A vp of 0.0 from a path the gate
    // never ran (long-form out-of-scope, no input) is NOT a pass —
    // without this the UI rendered it as `Supported` (ARCH §18.1).
    let mut final_outcome: Option<judge::ClaimCheckOutcome> = None;
    // Whether the short path actually extracted and judged a claim —
    // gates the ClaimCheckComplete frame (a NO_CLAIM release audited
    // nothing, so reporting "1 claim confirmed" would be a lie).
    let mut claim_audited = false;
    // Retained per-claim record for the epistemic ledger (at most one
    // on the single-claim path). Mirrors the narration frames but
    // SURVIVES the return — the frames are transient by design.
    let mut gate_claims: Vec<GateClaim> = Vec::new();
    dbg(&format!(
        "gate_answer entity_anchored={entity_anchored} chunks={} draft={:?}",
        chunks.len(),
        text.chars().take(240).collect::<String>()
    ));
    // Structural exemption-closing: strip any GK caveat before extraction so the
    // asserted claim is actually verified rather than exempted as NO_CLAIM. This
    // runs UNCONDITIONALLY now (not just entity_anchored): gate_answer only fires
    // on the gated path, where documents WERE retrieved (gate_on requires
    // documents_found > 0), so the grounding contract applies — a "from general
    // knowledge" escape hatch must not ship confident specifics the retrieved
    // evidence can't support (observed 2026-07-01: "Winnie's former lover was
    // Eddie Henderson", a name absent from the Secret-Agent corpus, shipped under
    // a GK caveat on a non-entity-anchored question). If the GK content is
    // genuinely unsupported the gate now abstains — an honest "the sources don't
    // cover it" beats a labelled-but-confident fabrication. strip_gk_caveat is a
    // no-op when there is no caveat, so grounded answers are unaffected. The
    // released `text` is unchanged; only what the verifier reads is de-caveated.
    // Env-gated (SOVEREIGN_EXACTVAL_FIX=0 restores the prior entity_anchored-only
    // strip) for a clean replay A/B.
    let verify_text = if entity_anchored || config::exactval_fix_enabled() {
        strip_gk_caveat(&text)
    } else {
        text.clone()
    };
    // G4 — the short path's assurance stage. Two-stage generative critic:
    // incumbent tier by construction (ECONOMY §4.1 labels it FUNCTION,
    // WRONG TIER).
    let verify_started = std::time::Instant::now();
    let verify_outcome = verify_grounding(
        inference,
        question,
        &verify_text,
        chunks,
        entity_anchored,
        evidence.searcher.as_ref(),
        crate::slot_policy::posture_of(base_request),
    )
    .await;
    crate::runtime::stage_ledger::Stage::new(
        sovereign_contracts::types::StageId::Verify,
        sovereign_contracts::types::StackOwner::Incumbent,
    )
    .mechanism(sovereign_contracts::types::StageMechanism::PerClaimJudge)
    .cause(sovereign_contracts::types::StageCause::EveryTurn)
    .record(verify_started.elapsed().as_millis() as u64);
    match verify_outcome {
        Some(v) => {
            final_vp = Some(v.violation_prob);
            final_outcome = Some(v.outcome);
            dbg(&format!(
                "  verify: vp={:.3} tau={tau} claim={:?}",
                v.violation_prob,
                v.claim
                    .as_deref()
                    .map(|c| c.chars().take(70).collect::<String>())
            ));
            // Short path audits one central claim — surface it and its
            // verdict on the progress channel (extraction + judging is
            // one verify call here, so the frames land together).
            if let Some(c) = v.claim.as_deref() {
                claim_audited = true;
                gate_claims.push(GateClaim {
                    text: c.to_string(),
                    supported: v.violation_prob < tau,
                    // Filled post-gate, display only (see GateClaim::address).
                    address: None,
                    failed_once: v.violation_prob >= tau,
                    unjudged: false,
                    violation_prob: Some(v.violation_prob),
                });
                emit_gate_progress(
                    progress,
                    NarrationPhase::ClaimCheckStart {
                        claims: vec![wire_claim(c)],
                        recheck: false,
                    },
                );
                emit_gate_progress(
                    progress,
                    NarrationPhase::ClaimVerdict {
                        index: 0,
                        supported: v.violation_prob < tau,
                    },
                );
            }
            if v.violation_prob >= tau {
                if let Some(claim) = v.claim.clone() {
                    if !profile.retry {
                        // Verify-only surfaces (Refinement): no second
                        // synthesis — the caller decides what replaces
                        // the failed text (typically: keep the prior
                        // verified answer).
                        text = grounded_abstention(&claim, chunks.len().min(12));
                        action = ACT_ABSTAINED_NO_RETRY;
                        emit_gate_progress(
                            progress,
                            NarrationPhase::ClaimCheckComplete {
                                confirmed: 0,
                                flagged: 1,
                            },
                        );
                        return GateOutcome {
                            answer: release_as(
                                action,
                                text,
                                Vec::new(),
                                inference,
                                base_request.preferred_speed,
                            ),
                            meta: with_native_verdict(
                                serde_json::json!({
                                                "surface": profile.surface.id(),
                                                "action": action.id,
                                                "retried": false,
                                                "violation_prob": final_vp,
                                "claim_check_outcome": final_outcome,
                                                "threshold": tau,
                                                "mode": "single_claim",
                                                "draft": draft_for_meta,
                                            }),
                                native,
                            ),
                            claims: gate_claims,
                        };
                    }
                    // Env-gated retry floor: the retry below is a SECOND full
                    // 35B synthesis, justified only when the evidence could
                    // ground a better answer (the good-evidence-but-bad-draft
                    // case the retry exists for). When the best retrieval
                    // similarity is below the floor, the evidence can't ground an
                    // answer — the retry would near-certainly fail again after
                    // paying for it (the observed 50-160s slow-abstention) — so
                    // abstain now. This never changes the answer/abstain DECISION
                    // on a turn the gate already failed; it only skips a wasted
                    // retry (gates COST, not competence), so it can't trigger the
                    // Critic-as-gate over-abstain regression. Default-off no-op.
                    if let (Some(floor), Some(sim)) = (retry_floor_env(), evidence.top_similarity) {
                        if sim < floor {
                            tracing::info!(
                                target: "grounding_gate",
                                top_similarity = sim,
                                retry_floor = floor,
                                vp = v.violation_prob,
                                "grounding gate: retry skipped — evidence below retry floor, abstaining without a second synthesis"
                            );
                            text = grounded_abstention(&claim, chunks.len().min(12));
                            action = ACT_ABSTAINED_WEAK_EVIDENCE;
                            emit_gate_progress(
                                progress,
                                NarrationPhase::ClaimCheckComplete {
                                    confirmed: 0,
                                    flagged: 1,
                                },
                            );
                            return GateOutcome {
                                answer: release_as(
                                    action,
                                    text,
                                    Vec::new(),
                                    inference,
                                    base_request.preferred_speed,
                                ),
                                meta: with_native_verdict(
                                    serde_json::json!({
                                                        "surface": profile.surface.id(),
                                                        "action": action.id,
                                                        "retried": false,
                                                        "violation_prob": final_vp,
                                    "claim_check_outcome": final_outcome,
                                                        "threshold": tau,
                                                        "top_similarity": sim,
                                                        "retry_floor": floor,
                                                        "mode": "single_claim",
                                                        "draft": draft_for_meta,
                                                    }),
                                    native,
                                ),
                                claims: gate_claims,
                            };
                        }
                    }
                    retried = true;
                    // G4 — the retry ladder. ECONOMY §4.1: INCUMBENCY, no
                    // grounding function; it is the control loop of the
                    // rewrite. Clocked from here through the re-verify.
                    let retry_started = std::time::Instant::now();
                    emit_gate_progress(progress, NarrationPhase::ClaimRevisionStart { failed: 1 });
                    let mut retry_req = base_request.clone();
                    let base_sys = retry_req.system_message.clone().unwrap_or_default();
                    retry_req.system_message = Some(format!(
                        "{base_sys}{}",
                        retry_system_note(&claim, &v.claim_evidence)
                    ));
                    retry_req.assistant_prefix = None;
                    match gate_call(
                        &**inference,
                        &retry_req,
                        sovereign_contracts::types::GateCallMechanism::Retry,
                    )
                    .await
                    {
                        Ok(resp) => {
                            // Truncation trace (2026-06-30): the gate's non-streaming
                            // retry bypasses the synth.truncation glassbox — log its
                            // finish vs cap so a silent Length cut here is visible.
                            tracing::info!(
                                target: "gate.call",
                                kind = "retry",
                                finish = ?resp.finish_reason,
                                completion_tokens = ?resp.completion_tokens,
                                max_tokens = ?retry_req.max_tokens,
                                resp_chars = resp.text.chars().count(),
                                "gate internal completion"
                            );
                            let second = resp.text;
                            // Same structural strip on the retry, matching the
                            // first-pass strip above (env-gated): the documented
                            // leak is a retry that re-asserts the fabrication
                            // wearing the GK caveat and slips the exemption.
                            let verify_second = if entity_anchored || config::exactval_fix_enabled()
                            {
                                strip_gk_caveat(&second)
                            } else {
                                second.clone()
                            };
                            emit_gate_progress(
                                progress,
                                NarrationPhase::ClaimCheckStart {
                                    claims: vec![wire_claim(&claim)],
                                    recheck: true,
                                },
                            );
                            let reverify_outcome = verify_grounding(
                                inference,
                                question,
                                &verify_second,
                                chunks,
                                entity_anchored,
                                evidence.searcher.as_ref(),
                                crate::slot_policy::posture_of(base_request),
                            )
                            .await;
                            // The retry pass (re-synthesis + its re-verify)
                            // is done. One row: it is one mechanism, and the
                            // re-verify exists only because the retry ran.
                            crate::runtime::stage_ledger::Stage::new(
                                sovereign_contracts::types::StageId::Retry,
                                sovereign_contracts::types::StackOwner::Incumbent,
                            )
                            .cause(sovereign_contracts::types::StageCause::ViolationOverThreshold)
                            .calls(2)
                            .record(retry_started.elapsed().as_millis() as u64);
                            match reverify_outcome {
                                Some(v2) if v2.violation_prob < tau => {
                                    final_vp = Some(v2.violation_prob);
                                    final_outcome = Some(v2.outcome);
                                    if v2.claim.is_none() && released_pure_decline(&second) {
                                        // The retry asserted NOTHING — a pure
                                        // decline extracted as NO_CLAIM (vp=0).
                                        // Releasing it "supported" forges a
                                        // Verified holding for a claim the
                                        // final text no longer asserts
                                        // (observed: ood-table-salt shipped
                                        // verdict `grounded` on "I don't have
                                        // reliable information on this.",
                                        // 2026-07-20). A 0-assertion decline
                                        // is an abstention — same contract as
                                        // the NO_CLAIM decline guard below.
                                        text = second;
                                        action = ACT_ABSTAINED_DECLINE;
                                        emit_gate_progress(
                                            progress,
                                            NarrationPhase::ClaimVerdict {
                                                index: 0,
                                                supported: false,
                                            },
                                        );
                                    } else {
                                        text = second;
                                        action = ACT_RETRY_RELEASED;
                                        if let Some(rec) = gate_claims.first_mut() {
                                            rec.supported = true;
                                            rec.violation_prob = Some(v2.violation_prob);
                                        }
                                        emit_gate_progress(
                                            progress,
                                            NarrationPhase::ClaimVerdict {
                                                index: 0,
                                                supported: true,
                                            },
                                        );
                                    }
                                }
                                Some(v2) => {
                                    final_vp = Some(v2.violation_prob);
                                    final_outcome = Some(v2.outcome);
                                    text = grounded_abstention(&claim, chunks.len().min(12));
                                    action = ACT_ABSTAINED;
                                    if let Some(rec) = gate_claims.first_mut() {
                                        rec.violation_prob = Some(v2.violation_prob);
                                    }
                                    emit_gate_progress(
                                        progress,
                                        NarrationPhase::ClaimVerdict {
                                            index: 0,
                                            supported: false,
                                        },
                                    );
                                }
                                None => {
                                    // Retry verdict unavailable — fail open
                                    // on the retry (written under the
                                    // grounding constraint; safer than
                                    // draft 1). Unless the retry is a pure
                                    // decline: nothing is asserted, so there
                                    // is nothing to fail open ON — it's an
                                    // abstention (same contract as above).
                                    text = second;
                                    if released_pure_decline(&text) {
                                        action = ACT_ABSTAINED_DECLINE;
                                    } else {
                                        action = ACT_RETRY_RELEASED_UNVERIFIED;
                                    }
                                    if let Some(rec) = gate_claims.first_mut() {
                                        rec.violation_prob = None;
                                    }
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
                            action = ACT_ABSTAINED_RETRY_ERROR;
                        }
                    }
                }
            }
        }
        None => {
            action = ACT_JUDGE_FAILED_OPEN;
        }
    }
    // Terminal progress frame for the short path. Only when a claim
    // was actually audited (NO_CLAIM releases verified nothing) and
    // only on the verdicts this fall-through exit owns — the abstain
    // early-returns above emit their own completion frames.
    if claim_audited {
        // Reads the action's REACH rather than its spelling. The old form
        // matched four string arms and a `starts_with` prefix — §2.1's smell,
        // and a fifth action id would have fallen into `_ => (0, 0)` silently.
        let (confirmed, flagged) = match action.reach {
            GateReach::Held if action.id.starts_with("retry_") => (1, 1),
            GateReach::Held => (1, 0),
            GateReach::Unjudged if action.id.starts_with("retry_") => (1, 1),
            GateReach::Declined => (0, 1),
            GateReach::Flawed | GateReach::Unjudged => (0, 0),
        };
        if confirmed + flagged > 0 {
            emit_gate_progress(
                progress,
                NarrationPhase::ClaimCheckComplete { confirmed, flagged },
            );
        }
    }
    dbg(&format!(
        "verdict action={} retried={retried} vp={final_vp:?} tau={tau}",
        action.id
    ));
    tracing::info!(
        target: "grounding_gate",
        action = action.id,
        retried,
        vp = ?final_vp,
        tau,
        top_similarity = ?evidence.top_similarity,
        "grounding gate verdict"
    );
    // Fragment guard (gen75c: the answer to a three-variable code question was
    // the single word "Start", released via NO_CLAIM — a fragment extracts no
    // claim, so the verify ladder waves it through). A released answer this
    // short, with no grounding suffix and no decline shape, answers nothing:
    // convert it to the honest abstention instead of shipping noise. Terse
    // GROUNDED answers are unaffected — the citation path formats them with
    // their supporting quote, well past this floor.
    if action == ACT_RELEASED
        && text.trim().chars().count() < 15
        && !text.contains("Grounded in the source")
        && question.trim().chars().count() > 40
    {
        dbg(&format!(
            "fragment guard: released text {:?} answers nothing — abstaining",
            text.trim()
        ));
        return GateOutcome {
            answer: abstain(
                grounded_abstention(question, chunks.len().min(12)),
                inference,
                base_request.preferred_speed,
                "released text answers nothing — fragment guard".to_string(),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": "abstained_fragment",
                    "retried": retried,
                    "violation_prob": final_vp,
                    "claim_check_outcome": final_outcome,
                    "threshold": tau,
                    "mode": "single_claim",
                    "draft": draft_for_meta,
                }),
                native,
            ),
            claims: gate_claims,
        };
    }
    // Decline guard (EPISTEMIC_STATE P0, 2026-07-20): a NO_CLAIM release whose
    // text is a pure provenance-flagged decline asserts nothing — it IS an
    // abstention, and releasing it ships the wrong epistemic standing
    // downstream (verdict `Unverified` instead of `CannotKnowFromHere`; the
    // coverage probe never fires; the gap mis-routes as ClaimUncovered —
    // observed on `ood-australia-capital` over 10 retrieved distractors).
    // Reclassify the ACTION only: the model's own decline prose is already the
    // honest user-facing abstention, so the text ships unchanged. Caveated
    // parametric answers are excluded by `released_pure_decline`; audited
    // claims (`claim_audited`) exclude every turn that asserted something.
    //
    // P1 (`NATIVE_GROUNDING_PARITY_PLAN.md` §4.1): the zoo decides this on
    // BOTH arms. H1's verdict rides the turn as telemetry and is reported
    // beside the decision, never in place of it — see `abstention_action`
    // for why the typed shortcut was retired and when it comes back.
    let reclassify = (action == ACT_RELEASED && !claim_audited)
        .then(|| abstention_action(&text))
        .flatten();
    if let Some(reclassified) = reclassify {
        dbg("decline guard: NO_CLAIM release is a pure decline — reclassifying as abstention");
        tracing::info!(
            target: "grounding_gate",
            gate_action = reclassified,
            // What H1 scored, when it ran. `None` on every flag-off turn.
            // Named `native_*` so no reader can mistake it for the decider.
            native_answerability = native.map(|v| v.answerability),
            native_decision = native.map(|v| v.to_gate_action()),
            "grounding gate: released text is a 0-holding decline — action reclassified to abstained_decline"
        );
        return GateOutcome {
            answer: abstain(
                text,
                inference,
                base_request.preferred_speed,
                "released text is a 0-holding decline".to_string(),
            ),
            meta: with_native_verdict(
                serde_json::json!({
                    "surface": profile.surface.id(),
                    "action": reclassified,
                    "retried": retried,
                    "violation_prob": final_vp,
                    "claim_check_outcome": final_outcome,
                    "threshold": tau,
                    "mode": "single_claim",
                    "draft": draft_for_meta,
                }),
                // This site carried the pair inline from 2026-07-20 — the
                // seed for `with_native_verdict`, now one of its callers.
                // Absent turns changed shape here (`null` → `not_computed`);
                // the reason is in that constant's docs.
                native,
            ),
            claims: gate_claims,
        };
    }
    // Second-opinion fabrication guard on a RELEASED single-claim answer — the
    // per-claim verify grounds the load-bearing value but is blind to fabricated
    // SUPPORTING specifics (a cited flag/number/entity absent from the
    // evidence). Skip when the path already abstained (nothing asserted). On a
    // flag: correct-or-abstain via one grounded rewrite.
    // Skip when the gate did not release an asserted answer — the verdict is
    // read off the action rather than re-derived from its spelling.
    if matches!(action.reach, GateReach::Held | GateReach::Flawed) {
        if let Some(guarded) = short_specifics_guard(
            inference,
            question,
            &text,
            chunks,
            evidence.searcher.as_ref(),
            base_request,
            profile,
            native,
        )
        .await
        {
            return guarded;
        }
    }
    GateOutcome {
        answer: release_as(
            action,
            text,
            Vec::new(),
            inference,
            base_request.preferred_speed,
        ),
        meta: with_native_verdict(
            serde_json::json!({
                "surface": profile.surface.id(),
                "action": action.id,
                "retried": retried,
                "violation_prob": final_vp,
                    "claim_check_outcome": final_outcome,
                "threshold": tau,
                "mode": "single_claim",
                "draft": draft_for_meta,
            }),
            native,
        ),
        claims: gate_claims,
    }
}
