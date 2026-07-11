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
use crate::types::{CompletionRequest, Speed};

use super::config::dbg;

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

/// Outcome of the citation-grounded answer path.
pub enum CitationOutcome {
    /// A verifiable supporting quote was found — release this answer.
    Grounded { answer: String, quote: String },
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
    posture: ShardingPrivacy,
) -> CitationOutcome {
    let passages = build_passages(chunks);
    if passages.is_empty() {
        return CitationOutcome::Abstain;
    }
    let prompt = format!(
        "PASSAGES:\n{passages}\n\nQUESTION: {q}\n\n\
         Find the ONE sentence in the PASSAGES above that answers the QUESTION \
         and copy it word for word. Then answer from it. Use exactly this format:\n\
         QUOTE: <the sentence, copied verbatim from a passage>\n\
         ANSWER: <the answer, taken only from the quote and as concise as the \
         question allows: the single specific fact (a name, term, number, or short \
         phrase) for a single-answer question, OR every item for a question that \
         asks for several (e.g. \"the three methods\" — list all three)>\n\n\
         If no passage answers the QUESTION, reply with exactly:\n\
         QUOTE: NONE\nANSWER: NONE",
        q = question.chars().take(300).collect::<String>(),
    );
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
        max_tokens: Some(256),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    let resp = match inference.complete(&req).await {
        Ok(r) => r.text,
        Err(e) => {
            dbg(&format!(
                "citation: extraction failed: {e} → inconclusive (fall through)"
            ));
            return CitationOutcome::Inconclusive;
        }
    };
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
    let quote_present = !none && quote_present_in_chunks(&quote, chunks);
    let answer_in_quote = quote_present && answer_supported_by_quote(&answer, &quote);
    dbg(&format!(
        "citation: quote={:?} answer={:?} | present={} answer_in_quote={} → {}",
        quote.chars().take(100).collect::<String>(),
        answer.chars().take(50).collect::<String>(),
        quote_present,
        answer_in_quote,
        if !none && quote_present && answer_in_quote {
            "GROUNDED"
        } else {
            "abstain (fall through to legacy)"
        }
    ));
    if none || !quote_present || !answer_in_quote {
        return CitationOutcome::Abstain;
    }
    CitationOutcome::Grounded { answer, quote }
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

/// Is `quote` a verbatim span of the passages? Full normalised substring, or a
/// run of ≥`MIN_VERBATIM_RUN` consecutive words (the model trimmed the edges).
/// A paraphrase or a fabricated "quote" matches neither.
fn quote_present_in_chunks(quote: &str, chunks: &[String]) -> bool {
    let q = normalize(quote);
    let words: Vec<&str> = q.split(' ').filter(|w| !w.is_empty()).collect();
    if words.len() < 3 {
        return false; // too short to be a genuine supporting sentence
    }
    let hay = normalize(&chunks.join(" "));
    if hay.contains(&q) {
        return true;
    }
    words.len() >= MIN_VERBATIM_RUN
        && words
            .windows(MIN_VERBATIM_RUN)
            .any(|w| hay.contains(&w.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks() -> Vec<String> {
        vec![
            "The blended noises of the enormous town sank to a murmur. Chief Inspector \
             Heat of the Special Crimes Department changed his tone. His wife, examining \
             the sharp edge of the carving knife, placed it on the dish."
                .to_string(),
            "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc.".to_string(),
        ]
    }

    #[test]
    fn parses_quote_and_answer() {
        let r = "QUOTE: Chief Inspector Heat of the Special Crimes Department changed his tone.\nANSWER: Heat is a Chief Inspector.";
        let (q, a) = parse_quote_answer(r).unwrap();
        assert!(q.starts_with("Chief Inspector Heat"));
        assert_eq!(a, "Heat is a Chief Inspector.");
    }

    #[test]
    fn unparseable_is_none() {
        assert!(parse_quote_answer("I think the answer is Chief Inspector").is_none());
    }

    #[test]
    fn none_sentinel_detected() {
        let (q, a) = parse_quote_answer("QUOTE: NONE\nANSWER: NONE").unwrap();
        assert!(is_none(&q) && is_none(&a));
    }

    #[test]
    fn verbatim_quote_present() {
        // Exact copy of the sentence whose answer ("Chief Inspector") the
        // STOP-list verifier wrongly killed — the quote itself is present.
        assert!(quote_present_in_chunks(
            "Chief Inspector Heat of the Special Crimes Department changed his tone.",
            &chunks()
        ));
        assert!(quote_present_in_chunks(
            "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc.",
            &chunks()
        ));
    }

    #[test]
    fn trimmed_edges_still_match_via_run() {
        // Model dropped the leading clause but copied a long verbatim run.
        assert!(quote_present_in_chunks(
            "Heat of the Special Crimes Department changed his tone today",
            &chunks()
        ));
    }

    #[test]
    fn fabricated_quote_rejected() {
        // A plausible but invented sentence shares no 6-word run.
        assert!(!quote_present_in_chunks(
            "Winnie killed Verloc with a blowpipe in the parlour.",
            &chunks()
        ));
        // A paraphrase of a real sentence also fails (not verbatim).
        assert!(!quote_present_in_chunks(
            "Stevie was the younger sibling of Winnie Verloc.",
            &chunks()
        ));
    }

    #[test]
    fn answer_must_be_in_its_own_quote() {
        // Title/name answers — present in their quote (the light stop keeps
        // "chief"/"inspector"/"doctor", unlike the all-chunks value check).
        assert!(answer_supported_by_quote(
            "Chief Inspector",
            "Chief Inspector Heat of the Special Crimes Department changed his tone."
        ));
        assert!(answer_supported_by_quote(
            "the Doctor",
            "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc."
        ));
        // The measured moat break: a confabulated value ("Russian") pinned to a
        // real-but-insufficient quote that never names the country.
        assert!(!answer_supported_by_quote(
            "Russian embassy",
            "Ever since the time of the late Baron Stott-Wartenheim, employed by the Embassy."
        ));
        // Space-dropped copies are grounded content wearing a typo: verification
        // is space-tolerant (the old space-strict rule turned a CORRECT
        // lighthouse answer into a decline), and respace_answer_from_quote
        // repairs the surface from the quote before release.
        assert!(answer_supported_by_quote(
            "dancinggirls",
            "photographs of more or less undressed dancing girls in the window"
        ));
        assert_eq!(
            respace_answer_from_quote(
                "dancinggirls",
                "photographs of more or less undressed dancing girls in the window"
            )
            .as_deref(),
            Some("dancing girls")
        );
        // Numeric truncation (measured 2026-07-01): a TRUNCATED number must not
        // ground against a quote that contains a *longer* number sharing its
        // leading digits. "289494" is a prefix substring of "28949423" but a
        // different value.
        assert!(!answer_supported_by_quote(
            "289494",
            "U.S. Air Force Project Blue Book UFO case file (15 scanned pages; NARA fileUnit 28949423)."
        ));
        // The complete, correct number still grounds.
        assert!(answer_supported_by_quote(
            "28949423",
            "U.S. Air Force Project Blue Book UFO case file (15 scanned pages; NARA fileUnit 28949423)."
        ));
        // A whole-token year grounds normally.
        assert!(answer_supported_by_quote(
            "Deloitte 2025",
            "review of Deloitte's performance during the engagement for the 2025 audit"
        ));
        // Single-digit answer (measured 2026-07-08 class-A evidence-denial): "4"
        // is a valid COMPLETE number token in the quote. The old >=2-char word
        // filter dropped it, emptied the word list, and returned false → a false
        // abstain that surfaced as "the sources don't cover it".
        assert!(answer_supported_by_quote(
            "4",
            "assert!(result.chunks_created >= 4);"
        ));
        // …but a single digit that is NOT a complete token in the quote (or is a
        // prefix of a longer number) still fails — no free pass from the exemption.
        assert!(!answer_supported_by_quote(
            "5",
            "assert!(result.chunks_created >= 4);"
        ));
        assert!(!answer_supported_by_quote(
            "2",
            "the NARA fileUnit 28949423 has 24 pages"
        ));
    }

    // ── mid-token stop compensation (probed deterministically 2026-07-01:
    //    finish=Stop at 99/256 tokens, answer cut mid-symbol) ──

    #[test]
    fn completes_the_mid_symbol_answer_from_the_chunk() {
        // The observed failure: chaos rebaseline step 127 / replay step 21.
        let chunk = "two prompt forms: `RELATIONAL_BASE_SYSTEM_PROMPT` (full) and\n\
                     `RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT` (compact — situated-handler default).";
        let fixed =
            extend_mid_token_copy("RELATIONAL_EXPRESSIVE_SYSTEM_PROM", std::iter::once(chunk));
        assert_eq!(
            fixed.as_deref(),
            Some("RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT")
        );
    }

    #[test]
    fn completes_the_dangling_formula_operator() {
        // Chaos rebaseline step 173: the answer stopped at a trailing "¬".
        let quote = "Then simply define Hn+1 := ¬H1 ∧ … ∧ ¬Hn and add this new hypothesis.";
        let fixed = extend_mid_token_copy("Hn+1 := ¬H1 ∧ … ∧ ¬", std::iter::once(quote));
        assert_eq!(fixed.as_deref(), Some("Hn+1 := ¬H1 ∧ … ∧ ¬Hn"));
    }

    #[test]
    fn complete_token_anywhere_means_no_extension() {
        // "1968" ends at a boundary in one occurrence — it is a real token; the
        // longer "19685" elsewhere must not trigger an extension.
        let chunk = "launched in 1968. Production reached 19685 units.";
        assert_eq!(extend_mid_token_copy("1968", std::iter::once(chunk)), None);
    }

    #[test]
    fn disagreeing_continuations_do_not_extend() {
        let chunk = "PREFIXalpha here, PREFIXbeta there.";
        assert_eq!(
            extend_mid_token_copy("PREFIX", std::iter::once(chunk)),
            None
        );
    }

    #[test]
    fn truncated_number_completes_to_the_source_value() {
        // The NARA class: "289494" cut from "28949423" — unanimous continuation
        // restores the real value (verification then passes on the full number).
        let quote = "NARA fileUnit 28949423.";
        assert_eq!(
            extend_mid_token_copy("289494", std::iter::once(quote)).as_deref(),
            Some("28949423")
        );
    }

    #[test]
    fn whitespace_runs_are_equivalent() {
        // The answer copies a quote whose source chunk breaks the line mid-span.
        let chunk = "the relational voice\ncontract has two prompt\n  forms in FOOBA";
        let fixed = extend_mid_token_copy(
            "voice contract has two prompt forms in FOO",
            std::iter::once(chunk),
        );
        assert_eq!(
            fixed.as_deref(),
            Some("voice contract has two prompt forms in FOOBA")
        );
    }

    #[test]
    fn oversized_continuation_is_not_guessed() {
        let chunk = "hash watched959ee8a8f330aabbccddeeff00112233445566778899 end";
        assert_eq!(
            extend_mid_token_copy("watched", std::iter::once(chunk)),
            None
        );
    }

    #[test]
    fn absent_text_and_sentinels_are_untouched() {
        assert_eq!(
            extend_mid_token_copy("missing", std::iter::once("no match here")),
            None
        );
        assert_eq!(extend_mid_token_copy("", std::iter::once("anything")), None);
    }

    // ── case fidelity (gen75 step 115: "¬HN" released for the source's "¬Hn") ──

    #[test]
    fn case_garbled_formula_snaps_to_quote_casing() {
        let quote = "Then simply define Hn+1 := ¬H1 ∧ … ∧ ¬Hn and add this new hypothesis.";
        let fixed = snap_answer_case_to_quote("Hn+1 := ¬H1 ∧ … ∧ ¬HN", quote);
        assert_eq!(fixed.as_deref(), Some("Hn+1 := ¬H1 ∧ … ∧ ¬Hn"));
    }

    #[test]
    fn decapitalized_proper_noun_is_restored() {
        let quote = "Chief Inspector Heat of the Special Crimes Department changed his tone.";
        assert_eq!(
            snap_answer_case_to_quote("chief inspector heat", quote).as_deref(),
            Some("Chief Inspector Heat")
        );
    }

    #[test]
    fn exact_case_and_non_span_answers_are_untouched() {
        let quote = "Then simply define Hn+1 := ¬H1 ∧ … ∧ ¬Hn here.";
        assert_eq!(
            snap_answer_case_to_quote("Hn+1 := ¬H1 ∧ … ∧ ¬Hn", quote),
            None
        );
        assert_eq!(
            snap_answer_case_to_quote("something else entirely", quote),
            None
        );
    }

    #[test]
    fn space_dropped_lighthouse_answer_respaces_from_quote() {
        // probe4 verbatim: correct values, spaces eaten by the copy channel.
        let quote = "The light's characteristic signal is one white flash every 18                      seconds, visible for 21 nautical miles in clear weather.";
        let ans = "one white flash every 18seconds; 21nauticalmiles";
        assert!(answer_supported_by_quote(ans, quote));
        assert_eq!(
            respace_answer_from_quote(ans, quote).as_deref(),
            Some("one white flash every 18 seconds; 21 nautical miles")
        );
    }

    #[test]
    fn fabricated_compound_still_fails_space_blind() {
        // "50minutes" has no "50 minutes" in the quote either way.
        let quote = "The sighting was reported at dawn and lasted briefly.";
        assert!(!answer_supported_by_quote("50minutes", quote));
        assert_eq!(respace_answer_from_quote("50minutes", quote), None);
    }

    #[test]
    fn whitespace_differences_still_case_snap() {
        // The answer collapses the quote's line break; casing still restores.
        let quote = "the RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT\n(compact) form";
        assert_eq!(
            snap_answer_case_to_quote(
                "the relational_expressive_system_prompt (compact) form",
                quote
            )
            .as_deref(),
            Some("the RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT\n(compact) form")
        );
    }
}
