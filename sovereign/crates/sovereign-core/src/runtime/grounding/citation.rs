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

use super::call_census::gate_call;
use super::config::dbg;
use sovereign_contracts::types::GateCallMechanism;

/// Total passage budget for the extraction prompt. Full chunks, no per-chunk
/// truncation — that truncation was itself a measured cause of missed answers
/// (the gold token sat at offset ~1900 of a ~2000-char chunk); whole trailing
/// chunks are dropped if the joined set exceeds this. ~28k chars ≈ 7k tokens,
/// inside the 32k-context primary with room for the question + output.
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
    inference: &dyn InferenceProvider,
    question: &str,
    chunks: &[String],
    locators: &[Option<String>],
    targets: &[Option<CitationTarget>],
    posture: ShardingPrivacy,
) -> CitationOutcome {
    let passages = build_passages(chunks);
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
    let resp = match gate_call(inference, &req, GateCallMechanism::Citation).await {
        Ok(r) => r.text,
        Err(e) => {
            dbg(&format!(
                "citation: extraction failed: {e} → inconclusive (fall through)"
            ));
            return CitationOutcome::Inconclusive;
        }
    };
    // Multi-quote contract: one PART block per sub-question, each verified on its
    // own. A model that ignores the format and replies with a bare QUOTE/ANSWER
    // pair falls through to the single-pair parse below, so a format miss
    // degrades to today's behaviour rather than to Inconclusive.
    if multiquote {
        let parts = parse_parts(&resp);
        if !parts.is_empty() {
            return multiquote_outcome(&parts, chunks, locators, targets);
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
    match verify_pair(None, &quote, &answer, chunks) {
        Some((quote, answer, chunk)) => CitationOutcome::Grounded {
            answer,
            quotes: vec![GroundedQuote {
                text: quote,
                locator: locator_at(locators, chunk),
                target: target_at(targets, chunk),
            }],
        },
        None => CitationOutcome::Abstain,
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

/// Repair and then verify ONE `(quote, answer)` pair against the passages.
/// `Some` iff the quote is verbatim in the chunks AND the answer is supported by
/// that quote — i.e. this is *the* grounding decision, and both the
/// single-sentence and the multi-quote contract call it, so a part of a compound
/// answer is held to exactly the same bar as a whole one (ARCH_PRINCIPLES §10.6:
/// one decider, one name). `part` labels the glassbox line when the caller is
/// grounding several parts; `None` on the single-quote path keeps that output
/// byte-identical to what it has always emitted.
fn verify_pair(
    part: Option<&str>,
    quote: &str,
    answer: &str,
    chunks: &[String],
) -> Option<(String, String, Option<usize>)> {
    let label = part.map(|p| format!("[part {p}] ")).unwrap_or_default();
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
    // withholds). Glassbox via tracing (a detached daemon's eprintln is lost —
    // only the tracing subscriber reaches daemon.err and the desktop panel).
    let none = is_none(&quote) || is_none(&answer);
    let found = (!none)
        .then(|| locate_quote_in_chunks(&quote, chunks))
        .flatten();
    let quote_present = found.is_some();
    // Support is checked against the quote FIRST, then widened to the chunk
    // the quote matched in. Quote-local-only was a measured false-demotion
    // surface (2026-08-10, chaos-saltgrass present-stolen-object, 4
    // consecutive runs): the draft answered "The Lyle-Hannett chronometer"
    // and quoted the adjacent sentence "…the chronometer was gone…" — the
    // maker's name sits one sentence earlier IN THE SAME CHUNK, verbatim,
    // yet the part was dropped and released as "The passages do not
    // answer: …", replacing a correct draft with a wrong verdict. Widening
    // to the MATCHED CHUNK keeps the anti-confabulation guard intact: the
    // embassy case this check was built on (quoted an embassy sentence,
    // answered "Russian embassy" — a country the text withholds) still
    // fails, because the value is absent from the whole chunk, not just
    // the quote. Same repair family as
    // `verify_answer_against_turn_evidence` (the 600-char split): verify
    // against what the evidence holds, not against the narrower rendering.
    // `AcrossChunks` matches keep the quote-local rule — there is no
    // single chunk to widen into.
    let answer_in_quote = quote_present && answer_supported_by_quote(&answer, &quote);
    let answer_in_matched_chunk = !answer_in_quote
        && match &found {
            Some(QuoteMatch::Exact { chunk, .. }) | Some(QuoteMatch::Partial { chunk }) => chunks
                .get(*chunk)
                .map(|c| answer_supported_by_quote(&answer, c))
                .unwrap_or(false),
            _ => false,
        };
    let supported = answer_in_quote || answer_in_matched_chunk;
    dbg(&format!(
        "citation: {label}quote={:?} answer={:?} | present={} match={} answer_in_quote={} answer_in_matched_chunk={} → {}",
        quote.chars().take(100).collect::<String>(),
        answer.chars().take(50).collect::<String>(),
        quote_present,
        match &found {
            Some(QuoteMatch::Exact { chunk, .. }) => format!("exact(chunk {chunk})"),
            Some(QuoteMatch::Partial { chunk }) => format!("partial-run(chunk {chunk})"),
            Some(QuoteMatch::AcrossChunks) => "across-chunks".to_string(),
            None => "none".to_string(),
        },
        answer_in_quote,
        answer_in_matched_chunk,
        if !none && quote_present && supported {
            "GROUNDED"
        } else {
            "abstain (fall through to legacy)"
        }
    ));
    if none || !quote_present || !supported {
        return None;
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
    Some((quote, answer, chunk))
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
fn multiquote_outcome(
    parts: &[(String, String, String)],
    chunks: &[String],
    locators: &[Option<String>],
    targets: &[Option<CitationTarget>],
) -> CitationOutcome {
    let mut grounded: Vec<(String, String, String, Option<usize>)> = Vec::new();
    let mut unanswered: Vec<String> = Vec::new();
    for (label, quote, answer) in parts {
        match verify_pair(Some(label), quote, answer, chunks) {
            Some((quote, answer, chunk)) => grounded.push((label.clone(), quote, answer, chunk)),
            None => unanswered.push(label.clone()),
        }
    }
    dbg(&format!(
        "citation: multiquote parts={} grounded={} unanswered={:?} → {}",
        parts.len(),
        grounded.len(),
        unanswered,
        if grounded.is_empty() {
            "abstain (fall through to legacy)"
        } else {
            "GROUNDED"
        }
    ));
    if grounded.is_empty() {
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
    CitationOutcome::Grounded { answer, quotes }
}

/// Is the answer's content actually in the cited quote? Closes the gap between
/// "the quote is real" and "the quote supports THIS answer". Uses only a *light*
/// function-word stop — content words like "chief"/"inspector"/"doctor" are
/// kept (the all-chunks value check's STOP-list wrongly dropped them, which is
/// what killed correct title answers), so the asserted value must genuinely
/// appear in the sentence the model copied.
fn answer_supported_by_quote(answer: &str, quote: &str) -> bool {
    const TINY_STOP: &[&str] = &[
        "the", "of", "a", "an", "is", "was", "to", "in", "and", "by", "at", "on", "with", "for",
    ];
    let q = normalize(quote);
    let words: Vec<String> = answer
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| {
            // Pure-digit answers ("4", a single-chunk count) are valid COMPLETE
            // number tokens — the >=2-char rule (there to drop tiny stopwords)
            // must not swallow them, or a single-digit answer can never ground
            // via the citation path and always falls through to the noisier
            // legacy vp ladder. Observed 2026-07-08: answer "4" vs quote
            // "…chunks_created >= 4;" logged present=true but answer_in_quote=false
            // → false abstain → the "sources don't cover it" evidence-denial. The
            // !is_empty guard matters: chars().all() is vacuously true for the
            // empty strings split() emits between consecutive delimiters.
            let pure_digit = !w.is_empty() && w.chars().all(|c| c.is_ascii_digit());
            pure_digit || (w.chars().count() >= 2 && !TINY_STOP.contains(w))
        })
        .map(String::from)
        .collect();
    !words.is_empty()
        && words.iter().all(|w| {
            if w.chars().all(|c| c.is_ascii_digit()) && super::config::exactval_fix_enabled() {
                // Numeric value: it must be a COMPLETE number token in the quote,
                // not a partial digit-run of a longer number. Plain substring
                // containment accepts a TRUNCATED value — the model answered
                // "289494" citing a quote that reads "…NARA fileUnit 28949423",
                // and "289494" is a prefix substring of "28949423", so it slipped
                // through as grounded. A prefix of a number is a different number.
                quote_has_number_token(&q, w)
            } else {
                // Space-tolerant containment: the MTP copy channel drops spaces
                // ("18seconds" for the quote's "18 seconds"; the measured
                // "dancinggirls") — a mis-spaced COPY of quote text is grounded
                // content wearing a typo, and `respace_answer_from_quote`
                // repairs the surface after verification. A compound absent
                // from the quote even space-blind ("50minutes" with no
                // "50 minutes" anywhere) still fails.
                q.contains(w.as_str())
                    || q.split_whitespace()
                        .collect::<String>()
                        .contains(w.as_str())
            }
        })
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

/// Number the chunks and join them, full text, up to the budget.
fn build_passages(chunks: &[String]) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for (i, c) in chunks.iter().enumerate() {
        let c = c.trim();
        if c.is_empty() {
            continue;
        }
        if !out.is_empty() && used + c.len() > PASSAGE_CHAR_BUDGET {
            break;
        }
        out.push_str(&format!("[{}] {}\n\n", i + 1, c));
        used += c.len();
    }
    out.trim_end().to_string()
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

/// The grounded completion of a mid-token generation stop, if one is warranted.
/// Tries `sources` in order (first source containing the text decides — pass the
/// verified quote before the chunks so declared provenance wins). Within that
/// source, every occurrence must agree:
/// - any occurrence followed by a token boundary → the text IS a complete token
///   there → `None` (nothing to fix);
/// - all occurrences followed by the SAME alphanumeric run (≤ `MAX_TAIL_RUN`) →
///   `Some(text + run)`;
/// - disagreeing or oversized continuations → `None` (ambiguous — don't guess).
/// Whitespace-run tolerant (a quote's single spaces match a chunk's newlines),
/// case-exact (the text is a copy; a case drift means it is not this span).
fn extend_mid_token_copy<'a>(
    text: &str,
    sources: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    let needle = text.trim_end();
    if needle.is_empty() {
        return None;
    }
    for src in sources {
        let conts = continuations_after(src, needle);
        if conts.is_empty() {
            continue; // not in this source — try the next
        }
        if conts.iter().any(|c| c.is_empty()) {
            return None; // complete token somewhere — no truncation to repair
        }
        let first = &conts[0];
        if conts.iter().all(|c| c == first) && first.chars().count() <= MAX_TAIL_RUN {
            return Some(format!("{needle}{first}"));
        }
        return None; // ambiguous continuations in the provenance source
    }
    None
}

/// The QUOTE-cased span the answer is a case-garbled copy of, if any: the
/// answer occurs in the quote under case-insensitive (and whitespace-tolerant)
/// matching, and the quote's exact-case span differs. Returns `None` when the
/// answer isn't a quote span or is already exact. Restoring the quote's casing
/// can only make the answer MORE faithful to the verified source text — it
/// also repairs de-capitalized proper nouns, not just formula variables.
fn snap_answer_case_to_quote(answer: &str, quote: &str) -> Option<String> {
    let q: Vec<char> = quote.chars().collect();
    let n: Vec<char> = answer.trim().chars().collect();
    if n.is_empty() {
        return None;
    }
    for start in 0..q.len() {
        if let Some(end) = ci_ws_match_at(&q, start, &n) {
            let span: String = q[start..end].iter().collect();
            return (span != answer.trim()).then_some(span);
        }
    }
    None
}

/// `whitespace_tolerant_match_at`, case-insensitively.
fn ci_ws_match_at(h: &[char], start: usize, n: &[char]) -> Option<usize> {
    let mut i = start;
    let mut j = 0usize;
    let eq = |a: char, b: char| a == b || a.to_lowercase().eq(b.to_lowercase());
    while j < n.len() {
        if n[j].is_whitespace() {
            if i >= h.len() || !h[i].is_whitespace() {
                return None;
            }
            while i < h.len() && h[i].is_whitespace() {
                i += 1;
            }
            while j < n.len() && n[j].is_whitespace() {
                j += 1;
            }
        } else {
            if i >= h.len() || !eq(h[i], n[j]) {
                return None;
            }
            i += 1;
            j += 1;
        }
    }
    Some(i)
}

/// The alphanumeric run immediately following each whitespace-tolerant
/// occurrence of `needle` in `hay` (empty string = the occurrence ends at a
/// token boundary). Runs are truncated at `MAX_TAIL_RUN + 1` chars so an
/// oversized continuation is detectable without unbounded collection.
fn continuations_after(hay: &str, needle: &str) -> Vec<String> {
    let h: Vec<char> = hay.chars().collect();
    let n: Vec<char> = needle.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < h.len() {
        if let Some(end) = whitespace_tolerant_match_at(&h, i, &n) {
            let mut run = String::new();
            let mut k = end;
            while k < h.len() && h[k].is_alphanumeric() && run.chars().count() <= MAX_TAIL_RUN {
                run.push(h[k]);
                k += 1;
            }
            out.push(run);
        }
        i += 1;
    }
    out
}

/// Match `needle` at `h[start..]` treating any whitespace run as equivalent to
/// any other. Returns the hay index one past the match.
fn whitespace_tolerant_match_at(h: &[char], start: usize, n: &[char]) -> Option<usize> {
    let mut i = start;
    let mut j = 0usize;
    while j < n.len() {
        if n[j].is_whitespace() {
            if i >= h.len() || !h[i].is_whitespace() {
                return None;
            }
            while i < h.len() && h[i].is_whitespace() {
                i += 1;
            }
            while j < n.len() && n[j].is_whitespace() {
                j += 1;
            }
        } else {
            if i >= h.len() || h[i] != n[j] {
                return None;
            }
            i += 1;
            j += 1;
        }
    }
    Some(i)
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where a verified quote was found — and, when the passage can be quoted back
/// verbatim, the source's OWN text for that span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuoteMatch {
    /// The WHOLE quote sits in ONE passage as one contiguous run. `verbatim` is
    /// the source's own characters for that run, and it is what the release
    /// prints — see `verify_pair`. Because it is a substring of a chunk, the
    /// downstream strict re-check (`quote_verification::verify_quotes`, which
    /// demands one contiguous source substring) cannot demote it. This is the
    /// ONLY match that may carry a section locator.
    Exact { chunk: usize, verbatim: String },
    /// Only a run of ≥`MIN_VERBATIM_RUN` consecutive words matched — the model's
    /// span diverges from the source somewhere, so it is NOT a contiguous source
    /// substring even though it is grounded. Carries no locator: the strict
    /// re-check will rewrite this span to `[unverified excerpt: …]`, and a
    /// heading on an unverified excerpt claims more than the text it labels.
    Partial { chunk: usize },
    /// Verbatim only across the joined passages, so no single chunk owns it.
    /// Still grounded (the text is corpus text either way); simply not
    /// attributable to one source, and reported as such rather than being
    /// assigned to whichever chunk happens to be first.
    AcrossChunks,
}

/// Is `quote` a verbatim span of the passages, and if so, where? Full
/// normalised substring, or a run of ≥`MIN_VERBATIM_RUN` consecutive words
/// (the model trimmed the edges). A paraphrase or a fabricated "quote"
/// matches neither.
///
/// THE GROUNDING DECISION IS UNCHANGED by returning a location. The set of
/// quotes that ground is exactly what it was before locators existed: pass 2
/// below IS the original per-chunk test, and pass 3 IS the original joined-
/// haystack test. Pass 1 only *refines* a match pass 2 would have accepted
/// anyway — it never admits one pass 2 would reject, because it is gated on the
/// same `hay.contains(&q)`. Tightening to per-chunk-only would have moved a
/// fabrication guard while claiming to add a label (ARCH_PRINCIPLES §10.6 —
/// one decider; this is the same decider, saying more).
fn locate_quote_in_chunks(quote: &str, chunks: &[String]) -> Option<QuoteMatch> {
    let q = normalize(quote);
    let words: Vec<&str> = q.split(' ').filter(|w| !w.is_empty()).collect();
    if words.len() < 3 {
        return None; // too short to be a genuine supporting sentence
    }
    let present_in = |hay: &str| -> bool {
        hay.contains(&q)
            || (words.len() >= MIN_VERBATIM_RUN
                && words
                    .windows(MIN_VERBATIM_RUN)
                    .any(|w| hay.contains(&w.join(" "))))
    };
    let normalised: Vec<String> = chunks.iter().map(|c| normalize(c)).collect();
    // Pass 1 — the whole quote in one passage, recovered as the SOURCE's own
    // characters. `hay.contains(&q)` is the cheap gate; `exact_span_in` then
    // re-finds the span in the raw chunk so we can hand back untouched source
    // text rather than the model's copy of it.
    for (i, hay) in normalised.iter().enumerate() {
        if hay.contains(&q) {
            if let Some(verbatim) = exact_span_in(&chunks[i], quote) {
                return Some(QuoteMatch::Exact { chunk: i, verbatim });
            }
        }
    }
    // Pass 2 — the original per-chunk decision, unchanged.
    for (i, hay) in normalised.iter().enumerate() {
        if present_in(hay) {
            return Some(QuoteMatch::Partial { chunk: i });
        }
    }
    // Pass 3 — the original joined-haystack decision, unchanged.
    present_in(&normalize(&chunks.join(" "))).then_some(QuoteMatch::AcrossChunks)
}

/// The source's OWN text for the span `quote` occupies in `chunk`, if the whole
/// quote sits there as one contiguous run (case-insensitively, any whitespace
/// run matching any other).
///
/// Why the source's characters and not the model's: what we print is what the
/// downstream strict re-check reads back. A copy that differs from the source in
/// case alone passes the citation path's case-insensitive test and then FAILS
/// the strict re-check, which is case-sensitive — and the answer ships with a
/// confident section heading glued to an `[unverified excerpt: …]`. Returning
/// the source span makes "the released quote is verbatim" structural rather than
/// hoped-for (ARCH_PRINCIPLES §7).
fn exact_span_in(chunk: &str, quote: &str) -> Option<String> {
    let n: Vec<char> = quote.trim().chars().collect();
    if n.is_empty() {
        return None;
    }
    let h: Vec<char> = chunk.chars().collect();
    (0..h.len())
        .find_map(|start| ci_ws_match_at(&h, start, &n).map(|end| (start, end)))
        .map(|(start, end)| h[start..end].iter().collect())
}

#[cfg(test)]
mod tests;
