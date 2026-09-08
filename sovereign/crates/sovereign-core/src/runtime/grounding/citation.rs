// SPDX-License-Identifier: AGPL-3.0-or-later
//! Citation-grounded answering — the *active* grounding primitive.
//!
//! Measured 2026-06-18 on a single sealed literary corpus: on entity-anchored
//! fact questions the small active model (Qwen3.6-35B-**A3B**, ~3B active
//! params) confabulated *despite the answer being verbatim in the retrieved
//! context* — "blowpipe" for the carving knife, "Fyodor" for Stevie — or
//! paraphrased the corpus ("sibling relationship" for "brother"); and the
//! post-hoc substring value-presence verifier then abstained: rightly on the
//! confabulations, wrongly on correct paraphrases and on titles like "Chief
//! Inspector" (which the STOP-list reduces to nothing). 6 of 7 misses had the
//! answer in context — a context-utilisation + verifier-literalism problem,
//! not retrieval.
//!
//! The cure is to make the model **cite before it answers**: copy the exact
//! supporting sentence out of the passages, then answer from it. That
//! (1) forces it to read the retrieved context instead of its parametric
//! memory — it cannot produce "blowpipe" with no sentence to copy it from —
//! and (2) replaces brittle value-substring grounding with *quote-existence*
//! grounding, which a correct title or paraphrase passes. No verifiable
//! supporting quote → honest abstention, so the grounded-or-abstain moat holds
//! by construction: a quote the model cannot find in the passages is exactly an
//! absent answer.
//!
//! This is the attributed-generation / answer-with-citations pattern (Gao et
//! al., ALCE 2023) adapted to the grounded-or-abstain contract and to small
//! local models: one constrained extraction, deterministic verification.

use crate::oicp::ShardingPrivacy;
use crate::slot_policy::Workload;
use crate::traits::InferenceProvider;
use crate::types::{CitationTarget, CompletionRequest, Speed};
use std::sync::Arc;

use super::call_census::gate_call;
use super::config::dbg;
use sovereign_contracts::types::GateCallMechanism;

mod quote_match;

use self::quote_match::{
    extend_mid_token_copy, locate_quote_in_chunks, normalize, snap_answer_case_to_quote, QuoteMatch,
};

/// Total passage budget for the extraction prompt. Full chunks, no per-chunk
/// truncation — that truncation was itself a measured cause of missed answers
/// (the gold token sat at offset ~1900 of a ~2000-char chunk); whole trailing
/// chunks are dropped if the joined set exceeds this. ~28k chars ≈ 7k tokens,
/// inside the 32k-context primary with room for the question + output. A drop
/// is REPORTED, never silent (§18.3): an absence declared over a truncated
/// window is could-not-judge, not "the passages do not answer".
const PASSAGE_CHAR_BUDGET: usize = 28_000;

/// Minimum verbatim word-run accepted as a real quote when the full normalised
/// quote is not a clean substring — tolerates the model trimming or extending
/// the sentence at its edges, without admitting a paraphrase (six consecutive
/// corpus words is a span a confabulation does not produce by accident).
const MIN_VERBATIM_RUN: usize = 6;

/// Longest alphanumeric run `extend_mid_token_copy` will append. A mid-token
/// stop leaves at most a partial word/number to restore; a run longer than this
/// means the "continuation" is some other structure (a hash blob, minified
/// text) — don't guess.
const MAX_TAIL_RUN: usize = 24;

/// A verified quote plus, when it can be attributed to one passage, the human
/// locator for that passage ("CHAPTER VII").
///
/// `locator` is `None` — and the released text simply omits it — whenever the
/// corpus cannot supply one: no section structure, an unjoined `chapters.json`
/// (see `svrn enrich backfill-sections`), or a quote that only matched across
/// a chunk boundary. A missing locator is never faked, because a citation that
/// points a reader at the wrong chapter is worse than one that points nowhere.
///
/// INVARIANT: a `Some(locator)` implies `text` is one contiguous span of one
/// chunk — the source's own characters, not the model's copy of them (see
/// `QuoteMatch::Exact`). A `Partial` run releases the MODEL's span and carries
/// no locator, precisely because nobody has verified that span
/// character-for-character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroundedQuote {
    pub text: String,
    pub locator: Option<String>,
    /// The `(corpus, chunk)` this quote was copied out of — what makes the
    /// released citation openable in a reading surface.
    ///
    /// Governed by the SAME invariant as `locator`, because it is decided from
    /// the same chunk index: `Some` only when the quote matched as one
    /// contiguous span of ONE chunk. A `Partial` run carries no chunk, hence
    /// neither a locator nor a target — it would otherwise open a passage that
    /// does not contain the characters the reader was shown.
    ///
    /// INDEPENDENT of `locator` in both directions, and of
    /// `SOVEREIGN_CITATION_LOCATOR`. That flag governs whether a chapter NAME
    /// is displayed, which is a different question from whether the passage
    /// can be opened; a corpus with no section structure yields `None` locator
    /// and `Some` target, and a synthetic chunk yields the reverse.
    pub target: Option<CitationTarget>,
}

/// Outcome of the citation-grounded answer path.
pub enum CitationOutcome {
    /// Verifiable supporting quotes were found — release this answer.
    ///
    /// `quotes` is a LIST, not a pre-joined string, because the post-hoc
    /// `quote_verification` pass re-checks each `"..."` span in the released
    /// text as ONE contiguous source substring. Two verbatim sentences joined
    /// inside a single pair of quotes match no chunk, so a correct multi-part
    /// citation was demoted to `[unverified excerpt: ...]` — measured on the
    /// arm-C run, 2026-08-05. Each quote must therefore ship as its own span.
    Grounded {
        answer: String,
        quotes: Vec<GroundedQuote>,
        /// Chunks [`PASSAGE_CHAR_BUDGET`] kept out of the window.
        evidence_window_dropped: usize,
    },
    /// The model found no passage to quote (or quoted one not in the
    /// passages) — honest abstention.
    Abstain,
    /// Extraction failed or was unparseable — caller falls through to the
    /// legacy verifier ladder rather than turning a hiccup into a refusal
    /// (fail-open, matching the gate's availability contract).
    Inconclusive,
}

/// Ask the model to quote the supporting sentence and answer from it, then
/// verify the quote is verbatim in the passages. See module docs.
pub async fn citation_grounded_answer(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    chunks: &[String],
    locators: &[Option<String>],
    targets: &[Option<CitationTarget>],
    posture: ShardingPrivacy,
    tau: f64,
) -> CitationOutcome {
    let (passages, evidence_window_dropped) = build_passages(chunks);
    if passages.is_empty() {
        return CitationOutcome::Abstain;
    }
    let multiquote = super::config::citation_multiquote_enabled();
    let q = question.chars().take(300).collect::<String>();
    // The multi-quote contract asks for one PART block per sub-question. The
    // single-sentence contract below cannot express "part one is here, part two
    // is not in the passages", so on a compound question the model takes the
    // whole-question NONE exit — measured 0/14 (see `citation_multiquote_enabled`).
    let prompt = if multiquote {
        format!(
            "PASSAGES:\n{passages}\n\nQUESTION: {q}\n\n\
             The QUESTION may ask for more than one thing. Split it into its parts \
             and handle EACH part on its own. For each part, find the ONE sentence \
             in the PASSAGES that answers THAT part and copy it word for word. \
             Repeat this block once per part, in order:\n\
             PART: <which part of the question this is, in a few words>\n\
             QUOTE: <the sentence, copied verbatim from a passage>\n\
             ANSWER: <the answer to THIS PART only, taken only from the quote \
             above and as concise as the part allows: the single specific fact (a \
             name, term, number, or short phrase), OR every item for a part that \
             asks for several>\n\n\
             If the PASSAGES do not answer a part, write QUOTE: NONE and ANSWER: \
             NONE for THAT PART ONLY. Never answer NONE for a part the passages do \
             answer just because some other part is missing."
        )
    } else {
        format!(
            "PASSAGES:\n{passages}\n\nQUESTION: {q}\n\n\
             Find the ONE sentence in the PASSAGES above that answers the QUESTION \
             and copy it word for word. Then answer from it. Use exactly this format:\n\
             QUOTE: <the sentence, copied verbatim from a passage>\n\
             ANSWER: <the answer, taken only from the quote and as concise as the \
             question allows: the single specific fact (a name, term, number, or short \
             phrase) for a single-answer question, OR every item for a question that \
             asks for several (e.g. \"the three methods\" — list all three)>\n\n\
             If no passage answers the QUESTION, reply with exactly:\n\
             QUOTE: NONE\nANSWER: NONE"
        )
    };
    let req = CompletionRequest {
        prompt,
        system_message: Some(
            "Answer only from the passages. Copy the supporting sentence exactly — \
             never invent or paraphrase it. If the passages do not answer the \
             question, reply NONE."
                .into(),
        ),
        preferred_speed: Speed::Slow,
        // SLOT_POLICY §7: OICP envelope instead of a `model_id: "primary"`
        // pin (a latent privacy hole — see judge.rs). Carries the session
        // posture so the judge offloads only when the turn permits.
        oicp: Some(Workload::Judge.requirements(posture)),
        // One PART block costs roughly what the whole single-quote reply does, so
        // a 3-part question needs headroom or the copy is truncated mid-quote and
        // the verbatim check rejects a real citation.
        max_tokens: Some(if multiquote { 768 } else { 256 }),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    let resp = match gate_call(&**inference, &req, GateCallMechanism::Citation).await {
        Ok(r) => r.text,
        Err(e) => {
            dbg(&format!(
                "citation: extraction failed: {e} → inconclusive (fall through)"
            ));
            return CitationOutcome::Inconclusive;
        }
    };
    // Glassbox for the citation stage (ARCH §9.1). `SOVEREIGN_GATE_AUDIT_FORENSICS`
    // covered the LONG-FORM audit only, so a citation-mode turn — the mode that
    // composes "The passages do not answer: …" — left no record of what the model
    // was shown or what it replied. Same knob, same file, off by default.
    if super::config::audit_forensics_path().is_some() {
        super::gate::audit_forensics(&serde_json::json!({
            "kind": "citation",
            "ts": chrono::Utc::now().to_rfc3339(),
            "question": &q,
            "multiquote": multiquote,
            "n_chunks": chunks.len(),
            "evidence_window_dropped": evidence_window_dropped,
            "passage_chars": passages.chars().count(),
            "passages": &passages,
            "reply": &resp,
        }));
    }
    // Multi-quote contract: one PART block per sub-question, each verified on its
    // own. A model that ignores the format and replies with a bare QUOTE/ANSWER
    // pair falls through to the single-pair parse below, so a format miss
    // degrades to today's behaviour rather than to Inconclusive.
    if multiquote {
        let parts = parse_parts(&resp);
        if !parts.is_empty() {
            return multiquote_outcome(
                inference,
                &parts,
                chunks,
                locators,
                targets,
                posture,
                tau,
                evidence_window_dropped,
            )
            .await;
        }
        dbg("citation: multiquote — reply carried no PART block, using single-pair parse");
    }
    let (quote, answer) = match parse_quote_answer(&resp) {
        Some(qa) => qa,
        None => {
            dbg(&format!(
                "citation: unparseable extraction (raw={:?}) → inconclusive (fall through)",
                resp.chars().take(90).collect::<String>()
            ));
            return CitationOutcome::Inconclusive;
        }
    };
    // Mid-token stop compensation (probed deterministically 2026-07-01): the MTP
    // primary sometimes emits a spontaneous EOS mid-token while copying under a
    // long context — finish=Stop with the token budget unused, leaving
    // "RELATIONAL_EXPRESSIVE_SYSTEM_PROM" or a formula cut at a trailing "∧ ¬".
    // The quote is verified verbatim against the chunks below, so completion is
    // grounded by construction: when the text's occurrence in its source is
    // followed by more alphanumeric characters EVERYWHERE it appears, it stopped
    // mid-token — append that run, copying only from the source (quote-first for
    // the answer, chunks for the quote). Skips the NONE sentinels — an
    // abstention has nothing to complete.
    match verify_pair(inference, None, &quote, &answer, chunks, posture, tau).await {
        Ok((quote, answer, chunk)) => CitationOutcome::Grounded {
            answer,
            quotes: vec![GroundedQuote {
                text: quote,
                locator: locator_at(locators, chunk),
                target: target_at(targets, chunk),
            }],
            evidence_window_dropped,
        },
        Err(_) => CitationOutcome::Abstain,
    }
}

/// The human locator for a verified quote's chunk, or `None`.
///
/// One accessor, so "which label does this quote carry" has a single answer
/// (§10.6). Every path that can fail — the quote matched across chunks, the
/// caller passed no locators, the corpus has no join for that chunk —
/// collapses to `None` here rather than being handled differently at each
/// call site.
fn locator_at(locators: &[Option<String>], chunk: Option<usize>) -> Option<String> {
    if !super::config::citation_locator_enabled() {
        return None;
    }
    locators.get(chunk?)?.clone()
}

/// The `(corpus, chunk)` handle for a verified quote's chunk, or `None`.
///
/// The sibling of [`locator_at`], and one accessor for the same reason
/// (§10.6): "can this quote be opened" gets a single answer, and every way it
/// can fail — matched across chunks, no targets passed, a chunk with no stable
/// row id — collapses to `None` here.
///
/// Deliberately NOT gated on `citation_locator_enabled()`. That flag is the
/// control arm for whether a chapter name is DISPLAYED; it says nothing about
/// whether the passage exists, and switching it off must not silently make
/// citations un-openable as a side effect.
fn target_at(targets: &[Option<CitationTarget>], chunk: Option<usize>) -> Option<CitationTarget> {
    targets.get(chunk?)?.clone()
}

/// Why a `(quote, answer)` pair did not ground. The four are NOT
/// interchangeable and the release depends on telling them apart: three are
/// evidence about the window the model was shown, one is the absence of a
/// verdict (§18.2 — four verdicts, not two; §18.3 — absence is reported).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PairRefusal {
    /// The model answered `NONE` — it read the passages and reported an
    /// absence. Naming that absence in the release is honest.
    DeclaredNone,
    /// The quote is nowhere in the passages. The model had no supporting text
    /// to copy, which is itself evidence the passages do not carry the answer.
    QuoteNotFound,
    /// The quote is verbatim and the pair was refused anyway: the exact-value
    /// veto, or the calibrated probe past `tau`. A verdict ABOUT this evidence,
    /// which is why it may now be NAMED (7a8a2e97, a4f8f2a95).
    Unsupported,
    /// No verdict exists: the probe did not answer. Falls through — the gate
    /// is a quality lever, not an availability risk.
    CouldNotJudge,
}

/// Repair and then verify ONE `(quote, answer)` pair against the passages.
/// `Ok` iff the quote is verbatim in the chunks AND the evidence supports the
/// answer — i.e. this is *the* grounding decision, and both the single-sentence
/// and the multi-quote contract call it, so a part of a compound answer is held
/// to exactly the same bar as a whole one (ARCH_PRINCIPLES §10.6: one decider,
/// one name). `part` labels the glassbox line when the caller is grounding
/// several parts.
///
/// `tau` is the AUDIT PASS's own threshold (`GroundingProfile::tau`,
/// `config.rs:609`, from `grounding_gate_threshold`, `config.rs:56`), applied
/// in its own terms — `violation_prob = 1 - support`, released iff
/// `violation_prob < tau`. No second threshold here (§10.6).
async fn verify_pair(
    inference: &Arc<dyn InferenceProvider>,
    part: Option<&str>,
    quote: &str,
    answer: &str,
    chunks: &[String],
    posture: ShardingPrivacy,
    tau: f64,
) -> Result<(String, String, Option<usize>), PairRefusal> {
    let (quote, answer) = (quote.to_string(), answer.to_string());
    let sentinel = is_none(&quote) || is_none(&answer);
    let quote = match (!sentinel)
        .then(|| extend_mid_token_copy(&quote, chunks.iter().map(String::as_str)))
        .flatten()
    {
        Some(fixed) => {
            dbg(&format!(
                "citation: quote stopped mid-token — completed from chunk (…{:?})",
                fixed
                    .chars()
                    .rev()
                    .take(24)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
            ));
            fixed
        }
        None => quote,
    };
    let answer = match (!sentinel)
        .then(|| {
            extend_mid_token_copy(
                &answer,
                std::iter::once(quote.as_str()).chain(chunks.iter().map(String::as_str)),
            )
        })
        .flatten()
    {
        Some(fixed) => {
            dbg(&format!(
                "citation: answer stopped mid-token — completed to {fixed:?}"
            ));
            fixed
        }
        None => answer,
    };
    // Case fidelity (gen75 step 115: released "¬HN" for the source's "¬Hn"):
    // the copy channel garbles case the way it garbles digits, and the
    // verification below is case-insensitive by design (titles/prose must
    // match regardless of case) — so a case-garbled copy verifies and ships.
    // The quote is verbatim corpus text: when the answer is a case-insensitive
    // copy of a quote span, the quote's casing is ground truth — restore it.
    let answer = match (!sentinel)
        .then(|| snap_answer_case_to_quote(&answer, &quote))
        .flatten()
    {
        Some(fixed) => {
            dbg(&format!(
                "citation: answer case-snapped to the quote's casing → {fixed:?}"
            ));
            fixed
        }
        None => answer,
    };
    // Space fidelity (probe4 2026-07-02: "18seconds"/"21nauticalmiles" for the
    // quote's "18 seconds"/"21 nautical miles" — the copy channel drops spaces
    // the way it drops letters and case; the old space-strict check turned a
    // CORRECT lighthouse answer into a decline). Repair the surface from the
    // quote's exact spacing before verification.
    let answer = match (!sentinel)
        .then(|| respace_answer_from_quote(&answer, &quote))
        .flatten()
    {
        Some(fixed) => {
            dbg(&format!(
                "citation: answer respaced from the quote → {fixed:?}"
            ));
            fixed
        }
        None => answer,
    };
    // Anti-confabulation: the quote must (a) be verbatim in the passages and
    // (b) actually SUPPORT the answer — the model can copy a real-but-
    // insufficient sentence and still confabulate the value (measured: quoted an
    // embassy sentence, answered "Russian embassy" — a country the text
    // withholds).
    let none = is_none(&quote) || is_none(&answer);
    let found = (!none)
        .then(|| locate_quote_in_chunks(&quote, chunks))
        .flatten();
    let quote_present = found.is_some();
    let evidence = evidence_for(&found, &quote, chunks);
    // The exact-value VETO (§7.6) first: it may REFUSE, never APPROVE.
    let veto = (!none && quote_present)
        .then(|| numeric_veto(&answer, &quote, evidence))
        .flatten();
    // The support DECIDER: the gate's OWN calibrated probe, the register
    // `verify_grounding`'s per-claim loop runs — no second, weaker decider for
    // a question the gate already decides (§10.6), open text judged by a judge
    // and not a keyword conjunction (§2.4). Measured on the primary 2026-09-04:
    // #57 paraphrase 0.9993, embassy 0.0043. `None` is judge failure —
    // could-not-judge, never absence (§18.2, §18.3).
    let support = if none || !quote_present || veto.is_some() {
        None
    } else {
        super::judge::claim_chunk_support(&**inference, evidence, &answer, posture).await
    };
    let judge_unavailable = !none && quote_present && veto.is_none() && support.is_none();
    let violation_prob = support.map(|s| 1.0 - s);
    let supported = violation_prob.is_some_and(|vp| vp < tau);
    let decision = match (none, quote_present, veto.is_some(), judge_unavailable) {
        (true, ..) => "declared-none",
        (_, false, ..) => "quote-not-found",
        (.., true, _) => "vetoed (exact value)",
        (.., true) => "could-not-judge (probe unavailable)",
        _ if supported => "GROUNDED",
        _ => "unsupported",
    };
    let match_kind = match &found {
        Some(QuoteMatch::Exact { chunk, .. }) => format!("exact(chunk {chunk})"),
        Some(QuoteMatch::Partial { chunk }) => format!("partial-run(chunk {chunk})"),
        Some(QuoteMatch::AcrossChunks) => "across-chunks".to_string(),
        None => "none".to_string(),
    };
    // Glassbox (§9.1): the decision and the numbers behind it. Only tracing
    // reaches daemon.err — a detached daemon eats eprintln.
    tracing::debug!(
        target: "grounding_gate",
        part = part.unwrap_or("-"),
        quote_present,
        r#match = %match_kind,
        support = support.map(|v| format!("{v:.3}")),
        violation_prob = violation_prob.map(|v| format!("{v:.3}")),
        tau,
        veto = veto.as_deref(),
        decision,
        "citation: pair verdict"
    );
    if super::config::audit_forensics_path().is_some() {
        super::gate::audit_forensics(&serde_json::json!({
            "kind": "citation_part",
            "ts": chrono::Utc::now().to_rfc3339(),
            "part": part,
            "quote": &quote,
            "answer": &answer,
            "sentinel_none": none,
            "quote_present": quote_present,
            "match": &match_kind,
            "evidence": evidence,
            "support": support,
            "violation_prob": violation_prob,
            "tau": tau,
            "veto": &veto,
            "decision": decision,
            "grounded": !none && quote_present && veto.is_none() && supported,
        }));
    }
    if none {
        return Err(PairRefusal::DeclaredNone);
    }
    if !quote_present {
        return Err(PairRefusal::QuoteNotFound);
    }
    if judge_unavailable {
        return Err(PairRefusal::CouldNotJudge);
    }
    if veto.is_some() || !supported {
        return Err(PairRefusal::Unsupported);
    }
    // ONE rule decides both what gets printed and whether it may be attributed,
    // because they are the same question (ARCH_PRINCIPLES §10.6): only a span we
    // can hand back as untouched source text survives the downstream strict
    // re-check, and only a span that survives that re-check may wear a section
    // heading. A `Partial` run and an `AcrossChunks` straddle both still ground —
    // the decision above is untouched — they simply release the model's own span
    // with no locator, exactly as every citation did before locators existed.
    //
    // Measured 2026-08-05 (chaos-saltgrass compound bank): without this, a
    // partial-run match shipped as `CHAPTER III — [unverified excerpt: …]`,
    // asserting confident provenance for a span another checker had just refused.
    let (quote, chunk) = match found {
        Some(QuoteMatch::Exact { chunk, verbatim }) => (verbatim, Some(chunk)),
        _ => (quote, None),
    };
    // Case fidelity, second pass: the earlier snap used the model's copy, whose
    // casing is exactly what the copy channel garbles. For an exact match the
    // SOURCE span is now in hand, and that is the real ground truth the first
    // snap's doc comment assumes. Verification is case-insensitive on both
    // sides, so re-snapping cannot un-ground an answer that just grounded.
    let answer = match chunk
        .is_some()
        .then(|| snap_answer_case_to_quote(&answer, &quote))
        .flatten()
    {
        Some(fixed) => {
            dbg(&format!(
                "citation: answer re-snapped to the SOURCE span's casing → {fixed:?}"
            ));
            fixed
        }
        None => answer,
    };
    Ok((quote, answer, chunk))
}

/// Split a multi-quote reply into `(part_label, quote, answer)` triples, one per
/// `PART:` block. The quote/answer inside a block are read by the very same
/// `parse_quote_answer` the single-sentence contract uses. Returns empty when the
/// reply carries no `PART:` label at all, which the caller treats as "the model
/// ignored the format" and handles as a single pair.
fn parse_parts(resp: &str) -> Vec<(String, String, String)> {
    let low = resp.to_lowercase();
    let mut starts: Vec<usize> = Vec::new();
    let mut from = 0usize;
    while let Some(i) = low[from..].find("part:") {
        let at = from + i;
        starts.push(at);
        from = at + "part:".len();
    }
    let mut out = Vec::new();
    for (n, &s) in starts.iter().enumerate() {
        let end = starts.get(n + 1).copied().unwrap_or(resp.len());
        let block = &resp[s + "part:".len()..end];
        // The label is the remainder of the PART line; QUOTE/ANSWER follow it.
        // An unlabelled block still gets an identity — its ordinal — because the
        // glassbox line and the gap sentence both need to name the part.
        let label = block
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"')
            .trim()
            .to_string();
        let label = if label.is_empty() {
            format!("part {}", n + 1)
        } else {
            label
        };
        if let Some((quote, answer)) = parse_quote_answer(block) {
            out.push((label, quote, answer));
        }
    }
    out
}

/// Verify each part independently and compose the release. A part grounds only
/// on the shared `verify_pair` bar, so nothing enters the answer that a verbatim
/// quote does not support. The parts that do NOT ground are NAMED in the
/// released text rather than dropped — an unanswered half of a compound question
/// is an absence, and absence is reported, never defaulted
/// (ARCH_PRINCIPLES §18.3). All parts ungrounded → `Abstain`, exactly as the
/// single-sentence contract would have done, so the floor is unchanged.
#[allow(clippy::too_many_arguments)]
async fn multiquote_outcome(
    inference: &Arc<dyn InferenceProvider>,
    parts: &[(String, String, String)],
    chunks: &[String],
    locators: &[Option<String>],
    targets: &[Option<CitationTarget>],
    posture: ShardingPrivacy,
    tau: f64,
    evidence_window_dropped: usize,
) -> CitationOutcome {
    let mut grounded: Vec<(String, String, String, Option<usize>)> = Vec::new();
    let mut unanswered: Vec<String> = Vec::new();
    let mut could_not_judge: Vec<String> = Vec::new();
    for (label, quote, answer) in parts {
        match verify_pair(inference, Some(label), quote, answer, chunks, posture, tau).await {
            Ok((quote, answer, chunk)) => grounded.push((label.clone(), quote, answer, chunk)),
            // Three verdicts ABOUT the window the model was shown, all
            // nameable — which they are only because the decider is now the
            // audit's own judge (under the conjunction the third was a
            // fabricated absence, 7a8a2e97, quarantined by a4f8f2a95).
            Err(PairRefusal::DeclaredNone)
            | Err(PairRefusal::QuoteNotFound)
            | Err(PairRefusal::Unsupported) => {
                // …unless the window was truncated: then "the passages do
                // not answer" is a claim about evidence nobody looked at.
                if evidence_window_dropped > 0 {
                    could_not_judge.push(label.clone())
                } else {
                    unanswered.push(label.clone())
                }
            }
            // No verdict exists for this part.
            Err(PairRefusal::CouldNotJudge) => could_not_judge.push(label.clone()),
        }
    }
    dbg(&format!(
        "citation: multiquote parts={} grounded={} unanswered={unanswered:?} \
         could_not_judge={could_not_judge:?} → {}",
        parts.len(),
        grounded.len(),
        if grounded.is_empty() {
            "abstain (fall through to legacy)"
        } else if !could_not_judge.is_empty() {
            "abstain (a part could not be judged — fall through to legacy)"
        } else {
            "GROUNDED"
        }
    ));
    if grounded.is_empty() {
        return CitationOutcome::Abstain;
    }
    // The release has one vocabulary for a missing part — "The passages do not
    // answer: <part>" — and a part nobody could judge has not earned it
    // (§18.3). Fall through to the ladder that audits the draft instead.
    if !could_not_judge.is_empty() {
        tracing::debug!(
            target: "grounding_gate",
            parts = parts.len(),
            grounded = grounded.len(),
            could_not_judge = ?could_not_judge,
            unanswered = ?unanswered,
            evidence_window_dropped,
            "citation multiquote: a part could not be judged — falling through"
        );
        return CitationOutcome::Abstain;
    }
    let mut answer = String::new();
    for (label, _, part_answer, _) in &grounded {
        if !answer.is_empty() {
            answer.push('\n');
        }
        answer.push_str(&format!("{label}: {part_answer}"));
    }
    if !unanswered.is_empty() {
        answer.push_str(&format!(
            "\n\nThe passages do not answer: {}.",
            unanswered.join("; ")
        ));
    }
    let quotes = grounded
        .into_iter()
        .map(|(_, text, _, chunk)| GroundedQuote {
            text,
            locator: locator_at(locators, chunk),
            target: target_at(targets, chunk),
        })
        .collect();
    CitationOutcome::Grounded {
        answer,
        quotes,
        evidence_window_dropped,
    }
}

/// WHICH text the support question is asked against: the matched CHUNK when
/// the quote matched inside one, else the quote. One accessor (§10.6), read by
/// veto and probe alike. Quote-local-only was a measured false-demotion
/// (2026-08-10, chaos-saltgrass, 4 runs); widening keeps the guard, the
/// embassy value being absent from the whole chunk too.
fn evidence_for<'a>(found: &Option<QuoteMatch>, quote: &'a str, chunks: &'a [String]) -> &'a str {
    match found {
        Some(QuoteMatch::Exact { chunk, .. }) | Some(QuoteMatch::Partial { chunk }) => {
            chunks.get(*chunk).map(String::as_str).unwrap_or(quote)
        }
        _ => quote,
    }
}

/// The exact-value VETO: the first COMPLETE number token in `answer` absent
/// from the evidence as a complete number token, else `None`. A veto, not a
/// decider (§7.6): it may only REFUSE. Containment is not enough — "289494"
/// against "…NARA fileUnit 28949423" is a prefix of a different number
/// (2026-07-01). `SOVEREIGN_EXACTVAL_FIX=0` disables it, as it always has.
fn numeric_veto(answer: &str, quote: &str, evidence: &str) -> Option<String> {
    if !super::config::exactval_fix_enabled() {
        return None;
    }
    let (q, e) = (normalize(quote), normalize(evidence));
    answer
        .split(|c: char| !c.is_alphanumeric())
        .find(|w| {
            !w.is_empty()
                && w.chars().all(|c| c.is_ascii_digit())
                && !quote_has_number_token(&q, w)
                && !quote_has_number_token(&e, w)
        })
        .map(str::to_string)
}

/// Repair space-dropped copies: any answer word (≥6 chars) that is absent from
/// the quote as written but present when the quote's spaces are ignored gets
/// replaced by the quote's exactly-spaced span ("18seconds" → "18 seconds").
/// The quote is verified verbatim corpus text, so the respaced form is ground
/// truth. Words the quote doesn't contain either way are left untouched.
fn respace_answer_from_quote(answer: &str, quote: &str) -> Option<String> {
    let qn = normalize(quote);
    let mut out: Vec<String> = Vec::new();
    let mut changed = false;
    for word in answer.split_whitespace() {
        let core: String = word
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        if core.chars().count() >= 6 && !qn.contains(&core) {
            if let Some(spaced) = find_spaced_span(&qn, &core) {
                // Preserve the word's punctuation shell around the respaced core.
                let start = word.to_lowercase().find(core.as_str());
                if let Some(i) = start {
                    let prefix = &word[..i];
                    let suffix = &word[i + core.len()..];
                    out.push(format!("{prefix}{spaced}{suffix}"));
                    changed = true;
                    continue;
                }
            }
        }
        out.push(word.to_string());
    }
    changed.then(|| out.join(" "))
}

/// The quote's exactly-spaced span whose non-space chars equal `token`
/// (both lowercase). None when absent or when the match is embedded in a
/// longer alphanumeric run (complete-run discipline).
fn find_spaced_span(quote_norm: &str, token: &str) -> Option<String> {
    let q: Vec<char> = quote_norm.chars().collect();
    let t: Vec<char> = token.chars().collect();
    for start in 0..q.len() {
        if q[start].is_whitespace()
            || (start > 0
                && q[start - 1].is_alphanumeric()
                && q[start].is_alphanumeric()
                && start_is_mid_run(&q, start))
        {
            continue;
        }
        let mut i = start;
        let mut j = 0;
        while j < t.len() && i < q.len() {
            if q[i].is_whitespace() {
                i += 1;
                continue;
            }
            if q[i] != t[j] {
                break;
            }
            i += 1;
            j += 1;
        }
        if j == t.len() {
            let boundary = i >= q.len() || !q[i].is_alphanumeric();
            let left_ok = start == 0 || !q[start - 1].is_alphanumeric();
            if boundary && left_ok {
                return Some(
                    q[start..i]
                        .iter()
                        .collect::<String>()
                        .trim_end()
                        .to_string(),
                );
            }
        }
    }
    None
}

fn start_is_mid_run(q: &[char], start: usize) -> bool {
    start > 0 && q[start - 1].is_alphanumeric()
}

/// True iff `num` appears in `normalized_quote` as a whole digit-run (bounded by
/// non-digits), not merely as a substring of a longer number. Keeps the citation
/// path from grounding a truncated/altered numeric value against a quote that
/// contains a *different* (longer) number sharing its leading digits.
fn quote_has_number_token(normalized_quote: &str, num: &str) -> bool {
    normalized_quote
        .split(|c: char| !c.is_ascii_digit())
        .any(|tok| tok == num)
}

/// Number the chunks and join them, full text, up to the budget. Returns the
/// window AND how many trailing chunks the budget dropped (§18.3, and see
/// [`PASSAGE_CHAR_BUDGET`]).
fn build_passages(chunks: &[String]) -> (String, usize) {
    let mut out = String::new();
    let mut used = 0usize;
    let mut dropped = 0usize;
    for (i, c) in chunks.iter().enumerate() {
        let c = c.trim();
        if c.is_empty() {
            continue;
        }
        if !out.is_empty() && used + c.len() > PASSAGE_CHAR_BUDGET {
            dropped = chunks.len() - i;
            break;
        }
        out.push_str(&format!("[{}] {}\n\n", i + 1, c));
        used += c.len();
    }
    (out.trim_end().to_string(), dropped)
}

/// `None` when neither label is present (unparseable → inconclusive). The quote
/// runs from after `QUOTE:` to `ANSWER:`; the answer is the first line after
/// `ANSWER:` (later lines are trailing model chatter).
fn parse_quote_answer(resp: &str) -> Option<(String, String)> {
    let low = resp.to_lowercase();
    let q = low.find("quote:")?;
    let a = low.find("answer:")?;
    if a <= q {
        return None;
    }
    let quote = resp[q + "quote:".len()..a]
        .trim()
        .trim_matches('"')
        .trim()
        .to_string();
    let answer_block = resp[a + "answer:".len()..].trim();
    let answer = answer_block
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .trim()
        .to_string();
    Some((quote, answer))
}

fn is_none(s: &str) -> bool {
    let l = s.trim().to_lowercase();
    l.is_empty() || l == "none" || l.starts_with("none ") || l.starts_with("none.")
}

#[cfg(test)]
mod tests;
