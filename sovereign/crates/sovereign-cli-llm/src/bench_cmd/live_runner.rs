// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reusable live-path runner for grounded-calibration benches (chaos-monkey
//! and the Fidelity Flywheel).
//!
//! Drives the SAME desktop chat path (`Runtime::handle_message_stream`), sealed
//! to one corpus via `enabled_corpora`, then recovers the retrieved chunks +
//! routing provenance from the persisted assistant message. Every probe set
//! (I1–I5) flows through this one runner, so the loop exercises the real router
//! + retrieval + synthesis — not a stub. Generalized out of the chaos bench's
//! `run_synth` so reuse is proven from day one.
//!
//! The forced-choice judges (`classify_abstain`, `classify_caveat`) live here
//! too: they are the *observation* step that turns a free-text reply into the
//! `AgentAction` / caveat signal the (pure) verifier consumes. They are
//! objective single-letter classifications the chaos bench already gates on.

use futures::StreamExt as _;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};

use crate::chat_cmd::bootstrap::ChatSession;

/// What the live path produced for one probe.
///
/// (Phase 5 will add `coarse_intent` here — recovered from the persisted
/// `provenance` metadata — once the F-MISROUTE register check actually reads
/// it; kept minimal until then so there's no speculative dead field.)
pub struct LiveAnswer {
    /// Think-stripped visible answer (what a user would read).
    pub visible: String,
    /// Retrieved chunk text recovered from the persisted assistant message.
    pub retrieved_chunk_texts: Vec<String>,
}

/// Drive the desktop chat path, sealed to `corpus` via `enabled_corpora`.
/// Best-effort: a seeding/stream failure degrades to an empty answer (the
/// caller scores it as an abstention / miss) rather than aborting the battery.
pub async fn run_live(session: &ChatSession, corpus: &str, question: &str) -> LiveAnswer {
    let conv_id = uuid::Uuid::new_v4().to_string();
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Seal retrieval to the bank's corpus so ABSENT questions genuinely have
    // nothing to find.
    let _ = session.store.insert_empty_conversation(&conv_id, created_at, None).await;
    let _ = session
        .store
        .set_conversation_enabled_corpora(&conv_id, Some(vec![corpus.to_string()]))
        .await;

    let raw = match session.runtime.handle_message_stream(question, &conv_id).await {
        Ok(handle) => {
            let mut stream = handle.stream;
            let mut buf = String::new();
            while let Some(item) = stream.next().await {
                match item {
                    Ok(chunk) => buf.push_str(&chunk),
                    Err(e) => {
                        eprintln!("    [live] stream error: {e}");
                        break;
                    }
                }
            }
            buf
        }
        Err(sovereign_core::error::Error::NotImplemented(_)) => {
            match session.runtime.handle_message(question, &conv_id).await {
                Ok(resp) => resp.message.content,
                Err(e) => {
                    eprintln!("    [live] fallback failed: {e}");
                    String::new()
                }
            }
        }
        Err(e) => {
            eprintln!("    [live] stream start: {e}");
            String::new()
        }
    };

    // Recover retrieved chunk text from the persisted assistant message.
    //
    // FULL text, not the metadata snippet: `project_retrieved_chunks`
    // truncates `snippet` to 200 chars, and the deterministic chaos
    // checks (`citation_faithful`, `verify_grounding`) substring-match
    // signature quotes against these texts — against snippets, every
    // ProvenanceTrap quote missed and the direct lane scored
    // citation-fidelity 0.00 while the bridge lane (which resolves
    // full text via `read_get_chunk`) scored 0.75 on identical
    // behaviour (2026-06-10 transport-delta finding). Resolve each
    // (corpus_id, chunk_id) through the corpus index, mirroring the
    // bridge; fall back to the snippet only when resolution fails.
    let chunk_refs: Vec<serde_json::Value> = session
        .store
        .get_conversation(&conv_id)
        .await
        .ok()
        .and_then(|c| c.messages.last().and_then(|m| m.metadata.clone()))
        .and_then(|m| m.get("retrieved_chunks").and_then(|v| v.as_array()).cloned())
        .unwrap_or_default();
    let mut retrieved_chunk_texts = Vec::with_capacity(chunk_refs.len());
    for c in &chunk_refs {
        let resolved = match (
            c.get("corpus_id").and_then(|v| v.as_str()),
            c.get("chunk_id").and_then(|v| v.as_u64()),
        ) {
            (Some(cid), Some(chid)) => match session.corpus_engine.open_index_for_corpus(cid).await
            {
                Ok(index) => index
                    .chunks_by_ids(&[chid])
                    .await
                    .ok()
                    .and_then(|mut rows| rows.pop())
                    .map(|row| row.content),
                Err(_) => None,
            },
            _ => None,
        };
        let text = resolved.or_else(|| {
            ["text", "content", "passage_preview", "preview", "snippet"]
                .iter()
                .find_map(|k| c.get(*k).and_then(|v| v.as_str()))
                .map(str::to_string)
        });
        if let Some(t) = text {
            retrieved_chunk_texts.push(t);
        }
    }

    let visible = strip_think(&raw);
    LiveAnswer { visible, retrieved_chunk_texts }
}

/// Drive the BARE model — the "true baseline" control. NONE of Commonwealth's
/// value-add: no system prompt, no retrieval injection, no router / synthesis /
/// presenter pipeline. Just `{user: question} → model → answer`, at the same
/// model + temperature as `run_live`, so the ONLY variables removed are our
/// prompting and retrieval. The delta (`run_live` − `run_naked`) is exactly the
/// measured value-add. `retrieved_chunk_texts` is empty by definition (no
/// retrieval), so grounding sub-metrics (citation_fidelity, distractor) score
/// against an empty set — that's the point: the naked model has no sources.
pub async fn run_naked(
    provider: &dyn InferenceProvider,
    model: &str,
    question: &str,
    max_tokens: usize,
) -> LiveAnswer {
    let req = CompletionRequest {
        prompt: question.to_string(),
        system_message: None,
        preferred_speed: Speed::Slow,
        max_tokens: Some(max_tokens),
        temperature: Some(0.0),
        model_id: Some(model.to_string()),
        ..Default::default()
    };
    let raw = match provider.complete(&req).await {
        Ok(resp) => resp.text,
        Err(e) => {
            eprintln!("    [naked] complete failed: {e}");
            String::new()
        }
    };
    LiveAnswer {
        visible: strip_think(&raw),
        retrieved_chunk_texts: Vec::new(),
    }
}

/// Remove `<think>…</think>` reasoning blocks; keep the visible answer.
pub fn strip_think(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("</think>") {
            rest = &rest[start + end + "</think>".len()..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Forced-choice answer-vs-abstain classifier. `Some(true)` = abstained.
pub async fn classify_abstain(judge: &dyn InferenceProvider, model: &str, answer: &str) -> Option<bool> {
    if answer.trim().is_empty() {
        return Some(true);
    }
    // Window the answer HEAD + TAIL, not head-only. A leaked/verbose
    // reply often reaches its actual verdict ("the text never names
    // him") in the final sentences; a head-only window classified the
    // preamble instead of the conclusion (2026-06-10 fabrication
    // burn-down: all 6 absent_adjacent "answers" were scored off
    // deliberation the judge never saw the end of).
    let windowed: String = {
        let chars: Vec<char> = answer.chars().collect();
        if chars.len() <= 1700 {
            answer.to_string()
        } else {
            let head: String = chars[..400].iter().collect();
            let tail: String = chars[chars.len() - 1200..].iter().collect();
            format!("{head}\n[…]\n{tail}")
        }
    };
    let prompt = format!(
        "An assistant was asked a question and replied:\n\n\"\"\"\n{windowed}\n\"\"\"\n\n\
         Did the assistant provide a substantive answer, or did it decline / say it \
         does not have the information?\n\n\
         Answer with exactly one letter — A = gave a substantive answer, B = declined / lacks the information."
    );
    forced_choice_ab(judge, model, &prompt).await.map(|(a, b)| b > a)
}

/// Forced-choice provenance-caveat classifier for out-of-domain answers.
/// `Some(true)` = the reply explicitly flags the answer is general knowledge /
/// NOT drawn from the provided passages.
pub async fn classify_caveat(judge: &dyn InferenceProvider, model: &str, answer: &str) -> Option<bool> {
    if answer.trim().is_empty() {
        return Some(false);
    }
    let prompt = format!(
        "An assistant answered a question whose answer is NOT in the provided source passages \
         (it comes from the assistant's general knowledge). Here is the reply:\n\n\"\"\"\n{}\n\"\"\"\n\n\
         Did the assistant EXPLICITLY flag that this answer comes from its general knowledge / is \
         NOT drawn from the provided passages or corpus?\n\n\
         Answer with exactly one letter — A = yes, it flagged the answer as general knowledge / not from the sources, B = no, it gave the answer with no such provenance caveat.",
        answer.chars().take(1200).collect::<String>()
    );
    forced_choice_ab(judge, model, &prompt).await.map(|(a, b)| a > b)
}

/// EXTERNAL grounding-verifier — the tier-agnostic abstention lever from the
/// situated-harness study. Returns `Some(true)` when the answer commits the
/// adjacent-fabrication failure: it ASSERTS a specific fact as if established by
/// the retrieved passages when that fact is NOT actually in them. The caller
/// gates such an answer to a grounded abstention. Crucially this is EXTERNAL —
/// it judges the answer against the chunks the model already had — so it can
/// make the present-vs-absent call the model itself cannot (the reason a blunt
/// abstain-prompt over-triggered), and it works identically for any model tier.
///
/// NOT a violation (returns `Some(false)`): the fact IS in the passages; the
/// answer explicitly flags it as general-knowledge / not-in-sources (the honest
/// OOD-caveat case — must NOT be gated); or the answer already declines.
/// Returns the continuous violation probability `P(A)` from the
/// forced-choice pass; the CALLER owns the gate threshold. Returning
/// the probability (rather than a pre-thresholded bool) is what makes
/// a single `--gv-shadow` bench run yield the full threshold curve
/// offline — the 2026-06-10 gate@0.50 run cost 2h and answered only
/// one point on it (honesty 0.18→0.45 but competence 0.50→0.33,
/// 14/24 answerable falsely gated).
pub async fn verify_grounding(
    judge: &dyn InferenceProvider,
    model: &str,
    question: &str,
    answer: &str,
    chunks: &[String],
) -> Option<f64> {
    if answer.trim().is_empty() || chunks.is_empty() {
        return Some(0.0);
    }
    // Scope: the gate exists to catch a CRISP ungrounded factual
    // assertion (a name, a date, an identification). A long-form
    // synthesis answer makes dozens of claims — reducing it to one
    // extracted claim and gating the whole reply on that single
    // check is the wrong instrument (observed: essay answers
    // degenerate to a meta-claim no single chunk supports, and a
    // correct essay gets suppressed). Long-form replies pass through
    // ungated; per-claim auditing of essays is separate machinery.
    if answer.chars().count() > 1_800 {
        eprintln!("    [gv] long-form answer ({} chars) — out of gate scope", answer.chars().count());
        return Some(0.0);
    }
    // Two-step, decomposed (2026-06-10 iteration C). The earlier
    // single-pass design asked one forced-choice token to BOTH locate
    // the answer's claim AND search ~24k chars of passages for support
    // — measured on the shadow-run sweeps as inseparable distributions
    // (fabricated relations assembled from real chunk entities scored
    // LOW; correct answers scored HIGH). Decomposed, each step is a
    // task the mechanism-fidelity `attribution_support` class already
    // validates models do well via logprobs:
    //   1. extract the single central claim the answer asserts;
    //   2. per-chunk forced-choice "does THIS passage support THIS
    //      claim" — violation_prob = 1 − max(per-chunk support).
    // Cross-passage assembly is the known blind spot of per-chunk
    // checking; accepted for v1 (the bank's fabrications are
    // single-relation claims).
    let claim_prompt = format!(
        "A user asked: {}\n\nAn assistant answered:\n\"\"\"\n{}\n\"\"\"\n\n\
         State the single central factual claim the assistant asserts as its answer, \
         as one short standalone sentence that names BOTH sides of the relation \
         (who/what is claimed to be/do what). Do not add qualifiers or sources.\n\
         Reply with exactly NO_CLAIM if the assistant declined, said the information \
         is not in its sources, or explicitly attributed the fact to general \
         knowledge rather than the sources.",
        question.chars().take(400).collect::<String>(),
        answer.chars().take(2000).collect::<String>(),
    );
    let claim_req = CompletionRequest {
        prompt: claim_prompt,
        system_message: Some("You extract claims precisely. Reply with one sentence or NO_CLAIM.".into()),
        preferred_speed: Speed::Medium,
        max_tokens: Some(64),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        model_id: Some(model.to_string()),
        ..Default::default()
    };
    let claim = match judge.complete(&claim_req).await {
        Ok(resp) => {
            let t = resp.text.trim().to_string();
            if t.is_empty() || t.to_uppercase().contains("NO_CLAIM") {
                eprintln!("    [gv] claim=NO_CLAIM → violation_prob=0.000");
                return Some(0.0);
            }
            // (A CIRCULAR category for vacuous self-confirmation claims
            // was tried 2026-06-10 and REVERTED: the extra instruction
            // bled into NO_CLAIM behaviour and the circular fabrication
            // came through UNGATED — worse than the 0.31-0.57 vp the
            // plain extraction gives it. Don't reintroduce as prompt
            // text; if circularity matters later, detect it in code.)
            t
        }
        Err(e) => {
            eprintln!("    [gv] claim extraction failed: {e}");
            return None;
        }
    };

    let mut max_support: f64 = 0.0;
    let mut checked = 0usize;
    for c in chunks.iter().take(12) {
        let passage: String = c.chars().take(2_400).collect();
        let prompt = format!(
            "PASSAGE:\n\"\"\"\n{passage}\n\"\"\"\n\n\
             CLAIM: {claim}\n\n\
             Does the passage state or clearly imply this claim? Paraphrase counts; \
             the passage merely mentioning the people or things involved, without \
             establishing the claimed connection between them, does NOT count.\n\n\
             Answer with exactly one letter — A = the passage supports the claim, \
             B = it does not."
        );
        if let Some((a, b)) = forced_choice_ab(judge, model, &prompt).await {
            let denom = a + b;
            let support = if denom > 0.0 { a / denom } else { 0.0 };
            if support > max_support {
                max_support = support;
            }
            checked += 1;
            // Early exit: a clearly-supporting passage settles it.
            if max_support >= 0.95 {
                break;
            }
        }
    }
    if checked == 0 {
        return None;
    }
    let vp = 1.0 - max_support;
    eprintln!(
        "    [gv] claim={:?} chunks_checked={checked} max_support={max_support:.3} violation_prob={vp:.3}",
        claim.chars().take(90).collect::<String>()
    );
    Some(vp)
}

/// One forced-choice A/B logprob pass. Returns `(p_A, p_B)`.
async fn forced_choice_ab(judge: &dyn InferenceProvider, model: &str, prompt: &str) -> Option<(f64, f64)> {
    let req = CompletionRequest {
        prompt: prompt.to_string(),
        system_message: Some("You are a careful classifier. Answer with a single letter.".into()),
        preferred_speed: Speed::Medium,
        max_tokens: Some(1),
        structured_output: Some(serde_json::json!({
            "type": "string", "enum": ["A", "B"], "x_forced_choice": true
        })),
        think_budget: Some(0),
        enable_thinking: Some(false),
        model_id: Some(model.to_string()),
        ..Default::default()
    };
    match judge.complete(&req).await {
        Ok(resp) => {
            let m: std::collections::HashMap<String, f64> = serde_json::from_str(resp.text.trim()).ok()?;
            Some((m.get("A").copied().unwrap_or(0.0), m.get("B").copied().unwrap_or(0.0)))
        }
        Err(e) => {
            eprintln!("    [judge] {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_think_removes_reasoning_blocks() {
        assert_eq!(strip_think("<think>plan</think>The answer"), "The answer");
        assert_eq!(strip_think("bare answer"), "bare answer");
        assert_eq!(strip_think("<think>unterminated"), "");
    }
}
