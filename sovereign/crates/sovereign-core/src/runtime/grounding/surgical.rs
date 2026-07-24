// SPDX-License-Identifier: AGPL-3.0-or-later
//! Surgical longform rewrite — correct only the sentences that failed the
//! grounding audit, instead of regenerating the whole answer.
//!
//! `gate_longform`'s default rewrite re-synthesises the ENTIRE answer on the
//! 35B (measured ~44s for a 6k-char draft) to fix a handful of unsupported
//! claims. That is blunt: the great majority of the answer already verified.
//! This module keeps every verified sentence VERBATIM and touches only the
//! failed spans —
//!   * no corrective passage exists  → delete the sentence (deterministic, 0 LLM)
//!   * corrective passages exist      → a small fast-slot (4B) edit of that one
//!                                       sentence (tiny prefill + ~1 sentence gen)
//!
//! The rebuilt answer still runs the caller's re-audit ladder, so the
//! fabrication guarantee is unchanged — this is a pure latency optimisation
//! with a full-rewrite fallback: `surgical_rewrite` returns `None` (→ caller
//! re-synthesises) whenever a failed claim can't be confidently located, an
//! edit call fails, or surgery collapses the answer to a stub.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};

use super::config::dbg;

/// A failed claim must share at least this fraction of its content-words with a
/// sentence to be edited there; below it we abandon surgery for the full
/// rewrite rather than risk cutting the wrong span. Set for the claim-extractor's
/// PARAPHRASES (not verbatim spans) — a genuine match measured 0.44 — with the
/// distinctive-token (proper-noun/number) overlap breaking ties toward the right
/// sentence when a character recurs across many.
const MIN_CLAIM_OVERLAP: f64 = 0.34;
/// Content-word minimum length — skips short function words ("the", "and").
const MIN_WORD_LEN: usize = 4;
/// Below this many chars, a surgically-reduced answer is a stub — fall back.
const MIN_SURVIVING_CHARS: usize = 40;

/// Lossless sentence split: `split_sentences(t).concat() == t`, so we can
/// delete or replace individual sentences and rebuild the rest byte-for-byte.
pub(super) fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        cur.push(c);
        let next = chars.get(i + 1).copied();
        let sentence_end = matches!(c, '.' | '!' | '?') && next.is_none_or(|n| n.is_whitespace());
        let para_break = c == '\n' && next == Some('\n');
        if sentence_end || para_break {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Lowercased alphanumeric tokens of length >= MIN_WORD_LEN.
fn content_words(s: &str) -> HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= MIN_WORD_LEN)
        .map(str::to_string)
        .collect()
}

/// Distinctive tokens of `s`: proper nouns (start uppercase) and numbers,
/// lowercased. These anchor a paraphrased claim to its source sentence.
fn distinctive(s: &str) -> HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= MIN_WORD_LEN)
        .filter(|w| {
            w.chars().next().is_some_and(char::is_uppercase)
                || w.chars().any(|c| c.is_ascii_digit())
        })
        .map(|w| w.to_lowercase())
        .collect()
}

/// The sentence best matching `claim`, gated on real content-word overlap
/// (>= MIN_CLAIM_OVERLAP) and ranked by that overlap plus a distinctive-token
/// (proper-noun/number) bonus so a recurring character maps to the RIGHT
/// sentence. `None` when no sentence clears the content gate → caller falls
/// back to a full rewrite rather than edit the wrong span.
fn best_match(claim: &str, sentences: &[String]) -> Option<(usize, f64)> {
    let cw = content_words(claim);
    if cw.is_empty() {
        return None;
    }
    let dc = distinctive(claim);
    let mut best: Option<(usize, f64)> = None;
    for (i, s) in sentences.iter().enumerate() {
        let sw = content_words(s);
        if sw.is_empty() {
            continue;
        }
        let cover = cw.intersection(&sw).count() as f64 / cw.len() as f64;
        if cover < MIN_CLAIM_OVERLAP {
            continue;
        }
        let dov = if dc.is_empty() {
            0.0
        } else {
            dc.intersection(&distinctive(s)).count() as f64 / dc.len() as f64
        };
        let score = cover + 0.5 * dov;
        if best.is_none_or(|(_, b)| score > b) {
            best = Some((i, score));
        }
    }
    best
}

/// Collapse whitespace artefacts a deletion leaves behind (doubled spaces,
/// 3+ consecutive newlines) and trim the ends.
fn normalize_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut nls = 0usize;
    let mut spaces = 0usize;
    for c in s.chars() {
        match c {
            '\n' => {
                nls += 1;
                spaces = 0;
                if nls <= 2 {
                    out.push(c);
                }
            }
            ' ' | '\t' => {
                spaces += 1;
                nls = 0;
                if spaces <= 1 {
                    out.push(' ');
                }
            }
            _ => {
                nls = 0;
                spaces = 0;
                out.push(c);
            }
        }
    }
    out.trim().to_string()
}

/// Fast-slot single-sentence correction. Returns the corrected sentence (or the
/// literal `REMOVE` sentinel), or `None` on an inference error (→ fallback).
async fn edit_sentence(
    inference: &Arc<dyn InferenceProvider>,
    base_request: &CompletionRequest,
    sentence: &str,
    evidence: &[String],
) -> Option<String> {
    const EV_PER: usize = 2;
    const EV_CHARS: usize = 700;
    let ev = evidence
        .iter()
        .take(EV_PER)
        .map(|p| {
            format!(
                "| {}",
                p.chars()
                    .take(EV_CHARS)
                    .collect::<String>()
                    .replace('\n', "\n| ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "A single sentence from a longer answer failed grounding — the sources do not support it \
         as written.\n\nSENTENCE:\n\"{}\"\n\nWHAT THE SOURCES ACTUALLY SAY:\n{}\n\n\
         Rewrite ONLY this one sentence so it states exactly what the passages support. Do not add \
         any claim the passages do not show. If the passages do not support the point at all, \
         reply with the single word REMOVE. Output only the rewritten sentence (or REMOVE) — no \
         preamble, no quotes.",
        sentence.trim(),
        ev
    );
    let req = CompletionRequest {
        prompt,
        system_message: Some(
            "You correct ONE sentence to match the provided sources. Output only the corrected \
             sentence, or the word REMOVE."
                .to_string(),
        ),
        // The fast slot: this is a tiny edit, not a synthesis — the 35B's cost
        // here is pure waste.
        preferred_speed: Speed::Fast,
        // The gate's Judge envelope (as extract/verify use) — it carries the
        // turn's sharding posture AND, with Speed::Fast, actually routes to the
        // fast slot. Inheriting base_request's SYNTHESIS envelope instead pins
        // the primary/35B and defeats the whole point of a small edit.
        oicp: Some(
            crate::slot_policy::Workload::Judge
                .requirements(crate::slot_policy::posture_of(base_request)),
        ),
        max_tokens: Some(200),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    match inference.complete(&req).await {
        Ok(resp) => Some(resp.text.trim().to_string()),
        Err(e) => {
            dbg(&format!("surgical edit_sentence failed: {e}"));
            None
        }
    }
}

/// Sentence-level correction target.
#[derive(Clone)]
enum Action {
    Delete,
    Fix(Vec<String>),
}

/// Rewrite only the failed spans of `draft`, keeping verified prose verbatim.
/// Returns the corrected answer, or `None` to tell the caller to fall back to a
/// full re-synthesis (a claim couldn't be located, an edit failed, or surgery
/// collapsed the answer).
pub(super) async fn surgical_rewrite(
    inference: &Arc<dyn InferenceProvider>,
    base_request: &CompletionRequest,
    draft: &str,
    failed: &[(String, Vec<String>)],
) -> Option<String> {
    if failed.is_empty() {
        return Some(draft.to_string());
    }
    let mut sentences = split_sentences(draft);
    if sentences.is_empty() {
        return None;
    }
    // Resolve EVERY failed claim to a sentence first; if any is unmappable,
    // abandon surgery (a half-corrected answer is worse than a full rewrite).
    let mut edits: BTreeMap<usize, Action> = BTreeMap::new();
    for (claim, evidence) in failed {
        let idx = match best_match(claim, &sentences) {
            Some((i, _score)) => i,
            None => {
                dbg(&format!(
                    "surgical: claim unmappable → full rewrite: {:?}",
                    claim.chars().take(60).collect::<String>()
                ));
                return None;
            }
        };
        // Delete beats Fix when two claims land on one sentence.
        match edits.get(&idx) {
            Some(Action::Delete) => {}
            _ if evidence.is_empty() => {
                edits.insert(idx, Action::Delete);
            }
            _ => {
                edits.insert(idx, Action::Fix(evidence.clone()));
            }
        }
    }
    dbg(&format!(
        "surgical: {} failed claim(s) → {} sentence edit(s)",
        failed.len(),
        edits.len()
    ));
    // Deletions are free — apply them now. Fixes need the model, so run them
    // CONCURRENTLY: they are independent and the daemon continuous-batches, so N
    // edits cost ~one edit's wall-clock, not N. (Pre-clone the target sentences
    // so the futures don't borrow `sentences` across the await.)
    let mut fix_inputs: Vec<(usize, String, Vec<String>)> = Vec::new();
    for (idx, action) in edits {
        match action {
            Action::Delete => sentences[idx] = String::new(),
            Action::Fix(evidence) => {
                let sentence = sentences[idx].clone();
                fix_inputs.push((idx, sentence, evidence));
            }
        }
    }
    let edited: Vec<(usize, Option<String>)> = futures::future::join_all(fix_inputs.iter().map(
        |(idx, sentence, evidence)| async move {
            (
                *idx,
                edit_sentence(inference, base_request, sentence, evidence).await,
            )
        },
    ))
    .await;
    for (idx, result) in edited {
        let new = result?; // any edit failure → fall back to the full rewrite
        if new.eq_ignore_ascii_case("remove") || new.is_empty() {
            sentences[idx] = String::new();
        } else {
            // keep a trailing space so the following sentence doesn't fuse.
            sentences[idx] = if new.ends_with(char::is_whitespace) {
                new
            } else {
                format!("{new} ")
            };
        }
    }

    // NB: the surgically-edited answer is handed back to the caller's FULL
    // re-audit ladder (`audit(second, true)`), the same one the full-rewrite
    // path runs. An earlier "scoped re-audit" (verify only the changed spans)
    // was faster but leaked a GK-caveated fabrication the holistic scan catches
    // (calibration 2026-07-17, CONFAB-LEAKED 0→1). The holistic re-audit is the
    // safety floor; surgery only changes HOW the corrected text is produced.
    let rebuilt = normalize_ws(&sentences.concat());
    // Over-deletion guard: if surgery stripped more than half the answer, the
    // draft was mostly unsupported — a coherent full re-synthesis (which
    // REGENERATES a thinner grounded answer) beats shipping a collapsed stub
    // that the presenter reads as an abstention. Belt-and-suspenders with the
    // caller's failure-count cap, which normally routes such drafts to the full
    // rewrite before surgery is even attempted.
    let kept = rebuilt.chars().count();
    let original = draft.chars().count().max(1);
    if kept < MIN_SURVIVING_CHARS || kept * 2 < original {
        dbg(&format!(
            "surgical: over-deletion ({kept}/{original} chars survived) — full rewrite"
        ));
        return None;
    }
    Some(rebuilt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::traits::InferenceProvider;
    use crate::types::{CompletionRequest, CompletionResponse, Depth, ProviderCapabilities};
    use futures::Stream;
    use std::pin::Pin;

    #[test]
    fn split_is_lossless() {
        for t in [
            "One sentence only.",
            "First. Second! Third?\n\nNew paragraph here.",
            "No terminal punctuation",
            "Mixed.\nLines\nand. more.",
        ] {
            assert_eq!(split_sentences(t).concat(), t, "roundtrip must be exact");
        }
    }

    #[test]
    fn best_match_locates_the_claim_sentence() {
        let s = split_sentences(
            "Alyosha is the youngest brother. Ivan wrote an article on ecclesiastical courts. \
             Dmitri is passionate and reckless.",
        );
        let (idx, cover) =
            best_match("Ivan wrote an article about ecclesiastical courts", &s).unwrap();
        assert!(cover >= 0.5);
        assert!(s[idx].contains("ecclesiastical"));
    }

    #[test]
    fn normalize_collapses_deletion_gaps() {
        assert_eq!(normalize_ws("a.  b."), "a. b.");
        assert_eq!(normalize_ws("a.\n\n\n\nb."), "a.\n\nb.");
        assert_eq!(normalize_ws("  trim me  "), "trim me");
    }

    // Inference provider that PANICS if called — proves the delete path never
    // touches the model.
    struct NoCallProvider;
    #[async_trait::async_trait]
    impl InferenceProvider for NoCallProvider {
        async fn complete(&self, _r: &CompletionRequest) -> Result<CompletionResponse> {
            panic!("delete-only surgery must not call inference");
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            panic!("no stream in surgery tests");
        }
        async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            panic!("no embed in surgery tests");
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    #[tokio::test]
    async fn delete_path_removes_sentence_without_inference() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(NoCallProvider);
        let base = CompletionRequest::default();
        let draft = "Alyosha is the youngest brother. Smerdyakov piloted a hovercraft over Skotoprigonyevsk. \
                     Dmitri is passionate and reckless.";
        // A failed claim with NO corrective evidence → deterministic deletion.
        let failed = vec![(
            "Smerdyakov piloted a hovercraft over Skotoprigonyevsk".to_string(),
            Vec::<String>::new(),
        )];
        // No fixes (delete-only) → makes no inference calls.
        let out = surgical_rewrite(&inf, &base, draft, &failed).await.unwrap();
        assert!(!out.contains("hovercraft"), "unsupported sentence deleted");
        assert!(
            out.contains("Alyosha") && out.contains("Dmitri"),
            "verified prose kept"
        );
    }

    #[tokio::test]
    async fn unmappable_claim_falls_back() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(NoCallProvider);
        let base = CompletionRequest::default();
        let draft = "Alyosha is the youngest brother. Dmitri is passionate.";
        // Claim shares no content-words with any sentence → None (full rewrite).
        let failed = vec![(
            "quantum chromodynamics governs gluon confinement".to_string(),
            vec!["some corrective passage".to_string()],
        )];
        assert!(surgical_rewrite(&inf, &base, draft, &failed)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn over_deletion_falls_back() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(NoCallProvider);
        let base = CompletionRequest::default();
        // One supported sentence (~78 chars) + one longer unsupported one
        // (~135 chars). Deleting the unsupported one leaves >40 chars (clears the
        // absolute floor) but under half the draft → the ratio guard fires and we
        // fall back to the full rewrite rather than ship the stub.
        let draft = "Alyosha Karamazov is the gentle youngest brother and a novice at the monastery. \
                     Smerdyakov secretly piloted an experimental hovercraft across the province and \
                     later transmitted the plans to a foreign power for profit.";
        let failed = vec![(
            "Smerdyakov secretly piloted an experimental hovercraft across the province"
                .to_string(),
            Vec::<String>::new(),
        )];
        assert!(surgical_rewrite(&inf, &base, draft, &failed)
            .await
            .is_none());
    }
}
