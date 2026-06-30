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
        preferred_speed: Speed::Medium,
        model_id: Some("primary".into()),
        max_tokens: Some(256),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    let resp = match inference.complete(&req).await {
        Ok(r) => r.text,
        Err(e) => {
            dbg(&format!("citation: extraction failed: {e} → inconclusive (fall through)"));
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
        if !none && quote_present && answer_in_quote { "GROUNDED" } else { "abstain (fall through to legacy)" }
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
        .filter(|w| w.chars().count() >= 2 && !TINY_STOP.contains(w))
        .map(String::from)
        .collect();
    !words.is_empty() && words.iter().all(|w| q.contains(w.as_str()))
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

fn normalize(s: &str) -> String {
    s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
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
            "Alexander Ossipon, anarchist, nicknamed the Doctor, sat near Mr Verloc."
                .to_string(),
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
        // The measured competence regression: a mis-spaced value not verbatim in
        // its own quote → not grounded → falls through to the correct draft.
        assert!(!answer_supported_by_quote(
            "dancinggirls",
            "photographs of more or less undressed dancing girls in the window"
        ));
    }
}
