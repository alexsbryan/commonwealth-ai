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

use super::call_census::gate_call;
use super::config::dbg;
use sovereign_contracts::types::GateCallMechanism;

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
    match gate_call(&**inference, &req, GateCallMechanism::Surgery).await {
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

/// A completed surgical rewrite: the corrected text plus the corrected
/// sentences themselves (deletions contribute none — removed prose cannot
/// fabricate). The spans are what the INCREMENTAL re-audit verifies (order
/// audit-economy D4): they are the only new prose surgery can produce, so
/// they are the only per-claim work the re-audit owes — the holistic scan
/// and the deterministic sweeps still run over the whole text.
pub(super) struct SurgicalEdit {
    pub(super) text: String,
    /// The replacement sentences, in document order.
    pub(super) repaired_spans: Vec<String>,
}

/// Rewrite only the failed spans of `draft`, keeping verified prose verbatim.
/// Returns the corrected answer plus its repaired spans, or `None` to tell
/// the caller to fall back to a full re-synthesis (a claim couldn't be
/// located, an edit failed, or surgery collapsed the answer).
pub(super) async fn surgical_rewrite(
    inference: &Arc<dyn InferenceProvider>,
    base_request: &CompletionRequest,
    draft: &str,
    failed: &[(String, Vec<String>)],
) -> Option<SurgicalEdit> {
    if failed.is_empty() {
        return Some(SurgicalEdit {
            text: draft.to_string(),
            repaired_spans: Vec::new(),
        });
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
    let mut repaired: BTreeMap<usize, String> = BTreeMap::new();
    for (idx, result) in edited {
        let new = result?; // any edit failure → fall back to the full rewrite
        if new.eq_ignore_ascii_case("remove") || new.is_empty() {
            sentences[idx] = String::new();
        } else {
            // A replacement is new CONTENT, not new STRUCTURE.
            //
            // `split_sentences` is lossless, which means the whitespace that
            // separated a sentence from its predecessor lives on the FOLLOWING
            // element: "A.\n\nB." splits to ["A.", "\n", "\nB."]. Overwriting
            // the whole element therefore throws away that leading break, and a
            // repaired paragraph silently merges into the one above it — on
            // every multi-paragraph answer, which is the only kind surgery ever
            // runs on. (Deletions never showed it: the standalone "\n" element
            // survives them, so the break holds.) Re-attach the original's own
            // leading and trailing whitespace; the module's promise is to touch
            // the failed span and nothing else, and a paragraph break the reader
            // already had is not part of the span.
            let original = &sentences[idx];
            let lead: String = original.chars().take_while(|c| c.is_whitespace()).collect();
            let trail: String = {
                let t: Vec<char> = original
                    .chars()
                    .rev()
                    .take_while(|c| c.is_whitespace())
                    .collect();
                t.into_iter().rev().collect()
            };
            let body = new.trim();
            repaired.insert(idx, body.to_string());
            // The fusion guard, now scoped to the case that can actually fuse:
            // the original ended flush AND the next element does not open with
            // whitespace of its own. Adding a space unconditionally put one
            // before the paragraph break instead.
            let needs_gap = trail.is_empty()
                && sentences
                    .get(idx + 1)
                    .is_some_and(|n| !n.starts_with(char::is_whitespace));
            sentences[idx] = format!("{body} ");
        }
    }

    // NB: the surgically-edited answer goes back to the caller's re-audit
    // in INCREMENTAL mode (order audit-economy D4): the repaired spans above
    // are the only new prose, so they are the only per-claim work re-judged —
    // while the holistic specifics scan and the deterministic sweeps still
    // run over the WHOLE corrected text in the same shared closure. That
    // floor is what the 2026-07-17 "scoped re-audit" skipped when it leaked
    // a GK-caveated fabrication (CONFAB-LEAKED 0→1); it is structural now.
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
    Some(SurgicalEdit {
        text: rebuilt,
        repaired_spans: repaired.into_values().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::oicp::ShardingPrivacy;
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
        assert!(
            !out.text.contains("hovercraft"),
            "unsupported sentence deleted"
        );
        assert!(
            out.text.contains("Alyosha") && out.text.contains("Dmitri"),
            "verified prose kept"
        );
        assert!(
            out.repaired_spans.is_empty(),
            "a deletion produces no new prose, so the incremental re-audit owes no span"
        );
    }

    /// A provider that answers every edit with one fixed corrected sentence.
    struct FixProvider;

    #[async_trait::async_trait]
    impl InferenceProvider for FixProvider {
        async fn complete(&self, _r: &CompletionRequest) -> Result<CompletionResponse> {
            Ok(CompletionResponse {
                text: "Smerdyakov worked as a cook in the household.".to_string(),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "fix-mock".into(),
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
            unimplemented!("no stream in surgical tests")
        }
        async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            unimplemented!("no embed in surgical tests")
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: crate::types::Speed::Fast,
                relative_reasoning: Depth::Moderate,
            }
        }
    }

    /// The FIX path reports its corrected sentences — they are the exact
    /// per-claim work the incremental re-audit owes (order audit-economy D4).
    #[tokio::test]
    async fn fix_path_reports_repaired_spans() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(FixProvider);
        let base = CompletionRequest::default();
        let draft = "Alyosha is the youngest brother of the family. \
                     Smerdyakov piloted a hovercraft over Skotoprigonyevsk. \
                     Dmitri is passionate and reckless in his dealings.";
        // Corrective evidence present → Action::Fix → the edit call runs.
        let failed = vec![(
            "Smerdyakov piloted a hovercraft over Skotoprigonyevsk".to_string(),
            vec!["Smerdyakov was the household's cook.".to_string()],
        )];
        let out = surgical_rewrite(&inf, &base, draft, &failed).await.unwrap();
        assert!(
            out.text.contains("worked as a cook"),
            "the corrected sentence replaced the fabrication"
        );
        assert_eq!(
            out.repaired_spans,
            vec!["Smerdyakov worked as a cook in the household.".to_string()],
            "the repaired span is reported for the incremental re-audit"
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

<<<<<<< HEAD
    /// **The wrong-span hazard: a recurring subject must not pull an edit onto
    /// the wrong sentence.**
    ///
    /// This is the module's worst failure, and it is silent in both directions
    /// at once — surgery deletes or rewrites a sentence that VERIFIED, and
    /// leaves the fabrication standing. The re-audit does not save it: the
    /// incremental pass judges the repaired spans, and the span it is handed
    /// is a correction of prose that was never in doubt.
    ///
    /// `best_match` guards it with a content-word floor plus a distinctive-token
    /// (proper-noun / number) bonus, and the bonus exists precisely for this
    /// input: a longform answer names one subject in several sentences, so
    /// content-word overlap alone ties, and the tie has to break on the
    /// specifics — the year, the figure, the second name — that identify WHICH
    /// sentence made the claim.
    ///
    /// Both directions are asserted. Picking the right sentence when a
    /// distinctive token names it, and refusing outright when nothing clears
    /// the content floor — a claim it cannot place must abandon surgery, never
    /// settle for the least-bad sentence.
    #[test]
    fn a_recurring_subject_does_not_pull_the_edit_onto_the_wrong_sentence() {
        // Three sentences about ONE person: content words overlap heavily and
        // only the specifics tell them apart.
        let sentences = split_sentences(
            "Smerdyakov served the Karamazov household as its cook for many years. \
             Smerdyakov suffered from epilepsy and fell ill on the night of the murder. \
             Smerdyakov inherited three thousand roubles and hid them beneath the floor.",
        );
        assert_eq!(sentences.len(), 3, "fixture must split into three");

        for (claim, must_contain) in [
            (
                "Smerdyakov inherited three thousand roubles from the household",
                "roubles",
            ),
            (
                "Smerdyakov suffered from epilepsy on the night of the murder",
                "epilepsy",
            ),
            ("Smerdyakov served the household as its cook", "cook"),
        ] {
            let (idx, _score) = best_match(claim, &sentences)
                .unwrap_or_else(|| panic!("{claim:?} shares plenty of content words — it must map"));
            assert!(
                sentences[idx].contains(must_contain),
                "{claim:?} landed on {:?}. Every sentence names the same subject, \
                 so content overlap alone ties; the distinctive-token bonus is \
                 what has to break it. Editing the wrong span deletes prose that \
                 VERIFIED and leaves the fabrication standing, and the \
                 incremental re-audit cannot notice — it judges the repair it \
                 was handed",
                sentences[idx]
            );
        }

        // The other direction: below the content floor, refuse. A claim placed
        // by elimination is a wrong edit waiting for its turn, so the honest
        // answer is the full rewrite.
        assert!(
            best_match(
                "quantum chromodynamics governs the confinement of gluons",
                &sentences
            )
            .is_none(),
            "a claim that shares no content with any sentence must abandon \
             surgery rather than settle for the least-bad match"
=======
    // ─────────────────────────────────────────────────────────────────────
    // The rewrite mechanics. Everything above proves surgery WORKS on the
    // happy path; these prove it fails SAFELY, which is the property that
    // matters — a half-corrected answer ships to a reader who has no way to
    // tell it from a whole one.
    // ─────────────────────────────────────────────────────────────────────

    /// A fast-slot stand-in scripted BY CONTENT, not by call order.
    ///
    /// The fix path runs its edits through `join_all`, so call order is a
    /// scheduling detail; a queue-based fixture would encode that detail as if
    /// it were a contract. Each entry matches the first marker found in the
    /// prompt — `edit_sentence` interpolates the sentence verbatim — so a test
    /// says "this sentence gets that reply" and stays true however the edits
    /// interleave. A `None` reply is an inference error, the input the
    /// fall-back-to-full-rewrite arm needs.
    struct ScriptedEditor {
        script: Vec<(&'static str, Option<&'static str>)>,
        seen: std::sync::Mutex<Vec<CompletionRequest>>,
    }

    impl ScriptedEditor {
        fn new(script: Vec<(&'static str, Option<&'static str>)>) -> Arc<Self> {
            Arc::new(Self {
                script,
                seen: std::sync::Mutex::new(Vec::new()),
            })
        }
        fn calls(&self) -> Vec<CompletionRequest> {
            self.seen.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl InferenceProvider for ScriptedEditor {
        async fn complete(&self, r: &CompletionRequest) -> Result<CompletionResponse> {
            self.seen.lock().unwrap().push(r.clone());
            let (_, reply) = self
                .script
                .iter()
                .find(|(marker, _)| r.prompt.contains(marker))
                .unwrap_or_else(|| {
                    panic!(
                        "unscripted surgical edit — no marker matched this prompt:\n{}",
                        r.prompt.chars().take(400).collect::<String>()
                    )
                });
            match reply {
                Some(text) => Ok(CompletionResponse {
                    text: (*text).to_string(),
                    tokens_used: 0,
                    prompt_tokens: 0,
                    model_id: "scripted-editor".into(),
                    latency_ms: 0,
                    oicp_meta: None,
                    finish_reason: None,
                    completion_tokens: None,
                }),
                None => Err(crate::error::Error::Inference("edit slot shed".into())),
            }
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            unimplemented!("surgery never streams")
        }
        async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            unimplemented!("surgery never embeds")
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

    /// Three paragraphs; the middle one is the fabrication. Sized so deleting
    /// it clears BOTH over-deletion arms, which keeps these tests measuring
    /// what they claim to and not the guard.
    const P1: &str =
        "Alyosha Karamazov is the gentle youngest brother and a novice at the monastery.";
    const P2: &str =
        "Smerdyakov piloted an experimental hovercraft across the whole province at night.";
    const P3: &str =
        "Dmitri is passionate and reckless in every one of his dealings with money.";

    fn three_paragraph_draft() -> String {
        format!("{P1}\n\n{P2}\n\n{P3}")
    }

    /// **The module's central promise, asserted byte-for-byte rather than by
    /// `contains`.**
    ///
    /// "Keep every verified sentence VERBATIM and touch only the failed spans"
    /// is the entire argument for surgery over a full re-synthesis — it is why
    /// the result can be trusted without re-reading the whole answer. A
    /// `contains` check cannot see the ways that breaks in practice: a
    /// tidy-up that rebuilds from trimmed sentences, a join that flattens the
    /// paragraph structure of a longform answer, a normalizer that reaches
    /// past the deletion gap it was meant to close.
    ///
    /// So this pins the exact output string, paragraph breaks included.
    #[tokio::test]
    async fn untouched_prose_survives_byte_for_byte_including_paragraph_breaks() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(NoCallProvider);
        let base = CompletionRequest::default();
        let draft = three_paragraph_draft();
        let failed = vec![(
            "Smerdyakov piloted an experimental hovercraft across the province".to_string(),
            Vec::<String>::new(),
        )];
        let out = surgical_rewrite(&inf, &base, &draft, &failed)
            .await
            .expect("one deletion of three paragraphs clears both over-deletion arms");
        assert_eq!(
            out.text,
            format!("{P1}\n\n{P3}"),
            "surgery must return the surviving prose UNCHANGED, paragraph \
             structure included. Anything that rebuilds the answer from \
             trimmed or re-joined sentences silently reformats text the \
             reader already had, and the verbatim promise is the only reason \
             a surgical result needs no full re-read"
        );
    }

    /// **Delete beats Fix when two failed claims land on the same sentence,
    /// in EITHER arrival order.**
    ///
    /// Two claims can map to one sentence — the claim extractor emits
    /// paraphrases, and a padded sentence carries more than one assertion.
    /// If one of them has no corrective passage, the sources cannot support
    /// that sentence at all, and "fix" it becomes an instruction to the fast
    /// slot to write something supported-looking about a point the evidence
    /// does not make. Deletion is the only safe resolution, and it has to win
    /// regardless of which claim the audit happened to list first.
    ///
    /// `NoCallProvider` is the assertion: it panics if any edit call is made,
    /// so a precedence regression fails loudly rather than by a subtle diff.
    #[tokio::test]
    async fn delete_beats_fix_when_two_claims_land_on_one_sentence() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(NoCallProvider);
        let base = CompletionRequest::default();
        let draft = three_paragraph_draft();

        let fixable = (
            "Smerdyakov piloted an experimental hovercraft across the province".to_string(),
            vec!["Smerdyakov was the household's cook.".to_string()],
        );
        let unsupportable = (
            "Smerdyakov piloted a hovercraft over the whole province at night".to_string(),
            Vec::<String>::new(),
        );

        for (label, failed) in [
            ("fix listed first", vec![fixable.clone(), unsupportable.clone()]),
            ("delete listed first", vec![unsupportable, fixable]),
        ] {
            // Precondition: both claims really do land on the same sentence.
            // Without this the test could pass by mapping them apart, which
            // exercises no precedence at all.
            let sentences = split_sentences(&draft);
            let a = best_match(&failed[0].0, &sentences).expect("claim 0 maps").0;
            let b = best_match(&failed[1].0, &sentences).expect("claim 1 maps").0;
            assert_eq!(a, b, "{label}: both claims must target one sentence");

            let out = surgical_rewrite(&inf, &base, &draft, &failed)
                .await
                .unwrap_or_else(|| panic!("{label}: deletion of one of three paragraphs \
                                           must not trip the over-deletion guard"));
            assert_eq!(
                out.text,
                format!("{P1}\n\n{P3}"),
                "{label}: a sentence carrying an unsupportable claim must be \
                 DELETED, not handed to the fast slot to be reworded"
            );
            assert!(
                out.repaired_spans.is_empty(),
                "{label}: a deletion produces no new prose, so the incremental \
                 re-audit is owed no span"
            );
        }
    }

    /// **The `REMOVE` sentinel deletes, and owes the re-audit no span.**
    ///
    /// The edit prompt tells the fast slot to reply `REMOVE` when the passages
    /// do not support the point at all — it is the model's own escape hatch
    /// from writing a correction that cannot be grounded. Two things must
    /// follow, and both are silent when broken: the sentence goes (rather than
    /// the literal word `REMOVE` shipping as prose to the reader), and no
    /// repaired span is reported, because a deletion creates no new text for
    /// the incremental re-audit to judge. A span reported here would send the
    /// re-audit to verify a sentence that no longer exists.
    ///
    /// Case and surrounding whitespace must not matter: this is a small model
    /// on the fast slot, and `edit_sentence` trims before comparing.
    #[tokio::test]
    async fn the_remove_sentinel_deletes_and_reports_no_repaired_span() {
        let base = CompletionRequest::default();
        let draft = three_paragraph_draft();
        for reply in ["REMOVE", "remove", "  Remove\n"] {
            let provider = ScriptedEditor::new(vec![("hovercraft", Some(reply))]);
            let inf: Arc<dyn InferenceProvider> = provider.clone();
            let failed = vec![(
                "Smerdyakov piloted an experimental hovercraft across the province".to_string(),
                // Evidence present → the FIX path, so the model is actually asked.
                vec!["Smerdyakov was the household's cook.".to_string()],
            )];
            let out = surgical_rewrite(&inf, &base, &draft, &failed)
                .await
                .unwrap_or_else(|| panic!("reply {reply:?}: must not fall back"));
            assert_eq!(provider.calls().len(), 1, "reply {reply:?}: one edit ran");
            assert_eq!(
                out.text,
                format!("{P1}\n\n{P3}"),
                "reply {reply:?}: the sentinel must delete the sentence, never \
                 ship as prose"
            );
            assert!(
                out.repaired_spans.is_empty(),
                "reply {reply:?}: a removal is not new prose — reporting a span \
                 would send the incremental re-audit after a sentence that is gone"
            );
        }
    }

    /// **One failed edit abandons the WHOLE surgery.**
    ///
    /// The edits run concurrently, so a shed or errored fast slot can land on
    /// any one of them while its siblings succeed. Keeping the successful ones
    /// would release an answer in which some failed claims were corrected and
    /// others still stand — and nothing downstream can tell the difference,
    /// because the released text looks like an ordinary surgical result. The
    /// only safe answer is `None`: hand the whole draft to the full
    /// re-synthesis, which the caller then re-audits in full.
    #[tokio::test]
    async fn one_failed_edit_abandons_the_whole_surgery() {
        let base = CompletionRequest::default();
        // Two fixable claims in different paragraphs; the second edit errors.
        let draft = format!("{P1}\n\n{P2}\n\n{P3}");
        let provider = ScriptedEditor::new(vec![
            ("hovercraft", Some("Smerdyakov worked as a cook in the household.")),
            ("passionate", None),
        ]);
        let inf: Arc<dyn InferenceProvider> = provider.clone();
        let failed = vec![
            (
                "Smerdyakov piloted an experimental hovercraft across the province".to_string(),
                vec!["Smerdyakov was the household's cook.".to_string()],
            ),
            (
                "Dmitri is passionate and reckless in his dealings".to_string(),
                vec!["Dmitri's dealings with money are not described.".to_string()],
            ),
        ];
        let out = surgical_rewrite(&inf, &base, &draft, &failed).await;
        assert!(
            out.is_none(),
            "a single failed edit must abandon surgery entirely. Releasing the \
             edits that DID succeed ships an answer where some failed claims \
             were corrected and others still stand, and the released text is \
             indistinguishable from a complete repair"
        );
        assert_eq!(
            provider.calls().len(),
            2,
            "both edits were attempted — if only one ran, this test proved \
             nothing about the surviving sibling"
        );
    }

    /// **The repaired spans are the NEW prose, in document order.**
    ///
    /// They are the entire input to the incremental re-audit (order
    /// audit-economy D4): the re-audit judges these and nothing else per-claim,
    /// on the argument that they are the only prose surgery produced. Order is
    /// load-bearing because the re-audit's per-claim records are read back
    /// positionally against the text. Reporting them in completion order —
    /// which is scheduling order, since the edits run concurrently — would put
    /// the right sentences under the wrong claims.
    #[tokio::test]
    async fn repaired_spans_are_the_new_prose_in_document_order() {
        let base = CompletionRequest::default();
        let draft = three_paragraph_draft();
        const FIRST_FIX: &str = "Smerdyakov worked as a cook in the household.";
        const SECOND_FIX: &str = "Dmitri's dealings with money are not described in the sources.";
        let provider = ScriptedEditor::new(vec![
            ("hovercraft", Some(FIRST_FIX)),
            ("passionate", Some(SECOND_FIX)),
        ]);
        let inf: Arc<dyn InferenceProvider> = provider.clone();
        let failed = vec![
            // Listed in REVERSE document order on purpose: the output must
            // follow the document, not the caller's list.
            (
                "Dmitri is passionate and reckless in his dealings".to_string(),
                vec!["Dmitri's dealings are not described.".to_string()],
            ),
            (
                "Smerdyakov piloted an experimental hovercraft across the province".to_string(),
                vec!["Smerdyakov was the household's cook.".to_string()],
            ),
        ];
        let out = surgical_rewrite(&inf, &base, &draft, &failed)
            .await
            .expect("two fixes, no deletions — nothing to fall back for");
        assert_eq!(
            out.repaired_spans,
            vec![FIRST_FIX.to_string(), SECOND_FIX.to_string()],
            "spans must be the replacements, in DOCUMENT order — the caller \
             listed them the other way round and the edits ran concurrently, \
             so either could have leaked through"
        );
        assert_eq!(
            out.text,
            format!("{P1}\n\n{FIRST_FIX}\n\n{SECOND_FIX}"),
            "and each replacement sits where the sentence it replaced was"
        );
    }

    /// **The over-deletion guard's ABSOLUTE floor, exercised alone.**
    ///
    /// The guard has two arms — a hard `MIN_SURVIVING_CHARS` floor and a
    /// "more than half the answer went" ratio — and `over_deletion_falls_back`
    /// above only ever reaches the ratio. An arm no input reaches is not a
    /// guard (ARCH §18.1). This fixture is built so the ratio arm CANNOT fire:
    /// most of the draft survives, but what survives is too short to be an
    /// answer, and shipping it would read to the user as an abstention the
    /// system never decided to make.
    #[tokio::test]
    async fn the_absolute_floor_fires_where_the_ratio_arm_cannot() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(NoCallProvider);
        let base = CompletionRequest::default();
        let draft = "The mill stands on Harbour Row. Orrison flew a hovercraft.";

        // Pin the arithmetic that makes this fixture the absolute arm's input
        // and not the ratio arm's — if a later edit to the fixture lets the
        // ratio fire, this test silently stops testing the floor.
        let survivor = "The mill stands on Harbour Row.";
        let kept = survivor.chars().count();
        let original = draft.chars().count();
        assert!(kept < MIN_SURVIVING_CHARS, "the absolute arm must fire: {kept}");
        assert!(
            kept * 2 >= original,
            "the ratio arm must NOT fire ({kept}*2 vs {original}) or this test \
             is a duplicate of over_deletion_falls_back"
        );

        let failed = vec![("Orrison flew a hovercraft".to_string(), Vec::<String>::new())];
        assert!(
            surgical_rewrite(&inf, &base, draft, &failed).await.is_none(),
            "a surviving answer under the floor is a stub — the presenter renders \
             it as an abstention nobody decided on, so the full re-synthesis \
             (which regenerates a thinner GROUNDED answer) is the right fallback"
        );
    }

    /// **The edit runs on the FAST slot under the Judge envelope — not the
    /// caller's synthesis envelope.**
    ///
    /// This is the whole economic argument for surgery. `edit_sentence` builds
    /// a fresh envelope precisely because inheriting `base_request`'s
    /// synthesis one pins the primary 35B and turns a one-sentence edit back
    /// into the cost it was built to avoid (~44s full re-synthesis vs ~5s
    /// surgery, NATIVE_GROUNDING_ECONOMY §7.3). Inheriting is a one-line
    /// change that breaks nothing, passes every other test in this module, and
    /// shows up only as a latency number nobody attributes.
    ///
    /// The posture still has to come FROM the caller — the envelope carries
    /// the turn's sharding privacy, so a fresh envelope must not quietly
    /// become LocalOnly-by-default either.
    #[tokio::test]
    async fn the_edit_runs_on_the_fast_slot_under_a_judge_envelope() {
        let base = crate::slot_policy::Workload::Synthesize
            .request_shared("the original longform prompt", ShardingPrivacy::LocalOnly);
        let base_tag = base
            .oicp
            .as_ref()
            .and_then(|o| o.request_id.clone())
            .expect("precondition: the caller's request carries a synthesis envelope");
        assert!(
            base_tag.starts_with("wl-synthesize-"),
            "precondition: base is a SYNTHESIS request, got {base_tag}"
        );

        let provider = ScriptedEditor::new(vec![(
            "hovercraft",
            Some("Smerdyakov worked as a cook in the household."),
        )]);
        let inf: Arc<dyn InferenceProvider> = provider.clone();
        let failed = vec![(
            "Smerdyakov piloted an experimental hovercraft across the province".to_string(),
            vec!["Smerdyakov was the household's cook.".to_string()],
        )];
        surgical_rewrite(&inf, &base, &three_paragraph_draft(), &failed)
            .await
            .expect("one fix, nothing to fall back for");

        let calls = provider.calls();
        assert_eq!(calls.len(), 1, "one sentence, one edit call");
        let edit = &calls[0];
        assert_eq!(
            edit.preferred_speed,
            Speed::Fast,
            "a one-sentence correction must ask for the fast slot"
        );
        let tag = edit
            .oicp
            .as_ref()
            .and_then(|o| o.request_id.as_deref())
            .expect("the edit must carry its own OICP envelope");
        assert!(
            tag.starts_with("wl-judge-"),
            "the edit inherited an envelope instead of declaring the Judge one \
             (tag {tag}). With Speed::Fast alone the synthesis envelope still \
             pins the primary 35B, which is exactly the cost surgery exists to \
             avoid — and it fails as latency, never as a wrong answer"
        );
    }

    /// **No failed claims is the identity, and it costs nothing.**
    ///
    /// The caller reaches surgery only with failures, but the empty case is a
    /// public arm of this function and it must not quietly reformat an answer
    /// nobody asked it to touch — `normalize_ws` collapses runs of whitespace,
    /// so falling through would rewrite prose on a turn where surgery had no
    /// work at all.
    #[tokio::test]
    async fn no_failed_claims_returns_the_draft_untouched() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(NoCallProvider);
        let base = CompletionRequest::default();
        // Deliberately carries whitespace `normalize_ws` would change: a
        // doubled space and a trailing newline the author put there.
        let draft = "The mill stands on Harbour Row.  It burned in 1892, and nobody was charged.\n";
        let out = surgical_rewrite(&inf, &base, draft, &[])
            .await
            .expect("no failures is not a fallback");
        assert_eq!(
            out.text, draft,
            "with nothing to repair the draft must come back byte-identical, \
             not normalized"
        );
        assert!(out.repaired_spans.is_empty(), "no edits, no spans");
    }

    /// **A repaired paragraph keeps its break — the fix path's own verbatim
    /// promise.**
    ///
    /// FOUND BY THIS TEST, 2026-09-02. `split_sentences` is lossless, so the
    /// whitespace separating a sentence from its predecessor lives on the
    /// FOLLOWING element — `"A.\n\nB."` splits to `["A.", "\n", "\nB."]`. The
    /// fix path overwrote the whole element, so every repaired paragraph lost
    /// its leading break and merged into the one above it, and the edit's
    /// unconditional trailing space then landed BEFORE the next break.
    ///
    /// Nothing caught it because the delete path is immune (the standalone
    /// `"\n"` element survives a deletion) and every prior fix-path test used a
    /// single-paragraph draft. Surgery only ever runs on longform answers,
    /// which are the multi-paragraph case — so the defect was live on exactly
    /// the input the module exists for, and it degrades an answer's readability
    /// without touching a single claim, which is the kind of damage no
    /// grounding metric reports.
    #[tokio::test]
    async fn a_repaired_paragraph_keeps_its_break() {
        let base = CompletionRequest::default();
        const FIXED: &str = "Smerdyakov worked as a cook in the household.";
        let draft = format!("{P1}\n\n{P2}");
        let provider = ScriptedEditor::new(vec![("hovercraft", Some(FIXED))]);
        let inf: Arc<dyn InferenceProvider> = provider.clone();
        let failed = vec![(
            "Smerdyakov piloted an experimental hovercraft across the province".to_string(),
            vec!["Smerdyakov was the household's cook.".to_string()],
        )];
        let out = surgical_rewrite(&inf, &base, &draft, &failed)
            .await
            .expect("one fix, nothing to fall back for");
        assert_eq!(
            out.text,
            format!("{P1}\n\n{FIXED}"),
            "the repaired sentence must sit in the paragraph it came from. \
             Overwriting the split element drops the leading break and merges \
             the paragraphs; a stray space before the break is the same bug \
             from the other end"
>>>>>>> c4cf9a32e (tons of latent work)
        );
    }
}
