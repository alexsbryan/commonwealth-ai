// SPDX-License-Identifier: AGPL-3.0-or-later
//! GLiNER2 backend — schema-driven extraction on **bare `ort`**, no
//! `gline-rs`.
//!
//! Productionised from `examples/gliner2_probe.rs` (SP1, findings at
//! `research/enrichment-spikes/findings/SP1_gliner2.md`). Roadmap item
//! P2.1; the in-house bare-`ort` precedent is the PaddleOCR detector
//! (`sovereign-tools` `local_corpus/ocr/paddle/detect.rs`).
//!
//! **Why a second backend rather than a new model behind the old one.**
//! The GLiNER2 export is a monolithic encoder+span-head graph with a
//! different input contract — `input_ids` / `attention_mask` /
//! `text_positions` / `schema_positions` / `span_idx` → `span_scores`
//! — and a declarative schema string
//! (`( [P] task ( [E] field … ) ) [SEP_TEXT] words`). `gline-rs`
//! implements v1's contract only, so this cannot be a model swap;
//! `GlinerExtractor::new` refuses a V2 model id by generation.
//!
//! **Measured against v1** (2026-08-02, M2 Max, quiet box, 50 real
//! chunks, three runs per arm, each isolated in its own process):
//!
//! | | v1 (gline-rs) | GLiNER2 (this module) |
//! |---|---|---|
//! | chunks/s | 2.77–2.87 | **7.16–7.23** (2.52×) |
//! | max RSS | 11.53–11.96 GB | **2.39–2.44 GB** |
//!
//! Both directions matter: it is faster *and* ~4.8× lighter. SP1's
//! original "6–7 GB incremental, the blocker for desktop residency" was
//! an artifact of subtracting two different workloads — see the
//! correction block in the findings doc.
//!
//! **What this module does NOT do.** Tuple-linked relations. The
//! export's `span_scores` head yields typed spans *per field*, not
//! linked `(author, work)` pairs, so relation extraction stays on the
//! LLM judgment path per SP1's partial result. The schema API here
//! carries the field group because typed slots are genuinely useful on
//! their own, not because pairing is solved.

use std::sync::Mutex;

use ort::session::Session;
use ort::value::Tensor;
use regex::Regex;
use sovereign_core::error::{Error, Result};
use tokenizers::Tokenizer;

use crate::gliner_ner::{
    model_spec, resolve_model_paths, role_marker_regex, strip_role_markers, EntityMention,
    GlinerGeneration, DEFAULT_LABELS, GLINER2_MODEL_ID,
};

/// Score cutoff for GLiNER2.
///
/// Deliberately **not** v1's `DEFAULT_THRESHOLD` (0.6). This is the
/// export README's own default, and the two heads are not calibrated to
/// the same scale — v1's 0.6 was tuned against gline-rs softmax scores
/// (validation showed 0.5 admitted "joy"/"desire" noise there). Carrying
/// v1's number over would be borrowing a tuning result across models
/// that never shared a scale.
pub const GLINER2_DEFAULT_THRESHOLD: f32 = 0.5;

/// Maximum span width, in words, the export's `span_idx` grid encodes.
/// Fixed by the graph — `span_scores` is `(1, fields, words, 8)`.
const MAX_SPAN_WIDTH: usize = 8;

/// One word of the input, with byte offsets back into the original text.
struct Word {
    lower: String,
    start: usize,
    end: usize,
}

/// `WhitespaceTokenSplitter` from the export's README.
///
/// Run over the ORIGINAL text with `(?i)` rather than over
/// `to_lowercase()`: lowercasing can change byte lengths (e.g. `İ`), and
/// the offsets recorded here are used to slice the original string.
fn split_words(text: &str) -> Vec<Word> {
    // Compiling per call is measurable at 7 chunks/s; hold it static.
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:https?://[^\s]+|www\.[^\s]+)|[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}|@[a-z0-9_]+|\w+(?:[-_]\w+)*|\S",
        )
        .expect("GLiNER2 word-splitter regex is a compile-time constant")
    });
    re.find_iter(text)
        .map(|m| Word {
            lower: m.as_str().to_lowercase(),
            start: m.start(),
            end: m.end(),
        })
        .collect()
}

/// One extracted span.
#[derive(Debug, Clone, PartialEq)]
pub struct Gliner2Hit {
    /// Schema field this span filled (e.g. `person`, or `author` for a
    /// relation-style field group).
    pub label: String,
    /// Span text, sliced from the ORIGINAL (un-lowercased) input.
    pub text: String,
    pub score: f32,
    /// Inclusive word-index range — what overlap suppression works on.
    pub word_start: usize,
    pub word_end: usize,
    /// Byte offsets into the input passed to `extract`.
    pub char_start: usize,
    pub char_end: usize,
}

/// Greedy span non-maximum suppression, highest score first.
///
/// **This is the production gap the probe left open.** The raw head
/// scores every (start, width) pair independently, so a single mention
/// surfaces several times at different widths — SP1 measured 8.5
/// mentions/chunk against v1's 3.2 and attributed the gap partly to
/// "duplicate overlapping spans (no NMS in the probe)". Without this,
/// entity counts are inflated and `chunk_entities` rows multiply.
///
/// Flat, not nested: a kept span suppresses ANY span it overlaps,
/// regardless of label. That matches v1's `SpanMode` behaviour, so the
/// two backends produce comparable mention sets. Nested NER would need a
/// per-label pass and a different downstream contract.
fn suppress_overlapping_spans(mut hits: Vec<Gliner2Hit>) -> Vec<Gliner2Hit> {
    // Descending score; ties broken by earlier start then longer span so
    // the result is deterministic (f32 has no total order — a bare
    // `partial_cmp` unwrap would also panic on NaN).
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.word_start.cmp(&b.word_start))
            .then(b.word_end.cmp(&a.word_end))
    });

    let mut kept: Vec<Gliner2Hit> = Vec::with_capacity(hits.len());
    for hit in hits {
        let overlaps = kept
            .iter()
            .any(|k| hit.word_start <= k.word_end && k.word_start <= hit.word_end);
        if !overlaps {
            kept.push(hit);
        }
    }
    // Restore reading order — callers render these next to the text.
    kept.sort_by_key(|h| (h.word_start, h.word_end));
    kept
}

/// Schema-driven GLiNER2 extractor.
///
/// One loaded ONNX session behind a `Mutex` — `ort`'s `run` needs
/// `&mut`, and the trait surface this satisfies is `&self`.
pub struct Gliner2Extractor {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    model_id: String,
    /// Lower-cased field names handed to the schema, paired with the
    /// canonical casing to report back (the export preprocesses to
    /// lowercase; `chunk_entities` rows want `Person`, not `person`).
    labels: Vec<(String, String)>,
    threshold: f32,
    role_marker_re: Regex,
}

impl Gliner2Extractor {
    /// Load the default GLiNER2 export with v1's 5-label tag set, so
    /// mention sets stay comparable across backends.
    pub fn new_default() -> Result<Self> {
        Self::new(GLINER2_MODEL_ID, DEFAULT_LABELS, GLINER2_DEFAULT_THRESHOLD)
    }

    /// Custom construction. `labels` are schema field names; casing is
    /// preserved for output and lowercased for the model.
    pub fn new(model_id: &str, labels: &[&str], threshold: f32) -> Result<Self> {
        let spec = model_spec(model_id);
        if spec.generation != GlinerGeneration::V2 {
            // The mirror of `GlinerExtractor::new`'s guard. A v1 graph
            // here would fail on a missing `text_positions` input, which
            // reads like a corrupt model rather than the wrong backend.
            return Err(Error::Storage(format!(
                "{model_id} is a GLiNER v1-generation model; Gliner2Extractor \
                 drives the bare-ort GLiNER2 contract and cannot run it. Use \
                 GlinerExtractor for v1 models."
            )));
        }

        let (tokenizer_path, model_path) = resolve_model_paths(model_id)?;
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| Error::Storage(format!("GLiNER2 tokenizer {tokenizer_path:?}: {e}")))?;
        let session = Session::builder()
            .map_err(|e| Error::Storage(format!("ort Session::builder: {e}")))?
            .commit_from_file(&model_path)
            .map_err(|e| Error::Storage(format!("ort commit_from_file {model_path:?}: {e}")))?;

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            model_id: model_id.to_string(),
            labels: labels
                .iter()
                .map(|l| (l.to_lowercase(), (*l).to_string()))
                .collect(),
            threshold,
            role_marker_re: role_marker_regex(),
        })
    }

    /// The model id this extractor loaded.
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Extract the default label set from `text`, role markers stripped
    /// and overlapping spans suppressed.
    pub fn extract(&self, text: &str) -> Result<Vec<Gliner2Hit>> {
        let processed = strip_role_markers(text, &self.role_marker_re);
        let fields: Vec<&str> = self.labels.iter().map(|(lower, _)| lower.as_str()).collect();
        let hits = self.forward("entities", &fields, &processed)?;
        let mut hits = suppress_overlapping_spans(hits);
        // Report canonical label casing.
        for hit in &mut hits {
            if let Some((_, canonical)) = self.labels.iter().find(|(lower, _)| *lower == hit.label)
            {
                hit.label = canonical.clone();
            }
        }
        Ok(hits)
    }

    /// Run one field group as a schema — the typed-slot surface
    /// (`task` = the group name, `fields` = its slots). Returns raw
    /// slot fills; pairing them into tuples is NOT done here (SP1: the
    /// export exposes typed spans per field, not linked tuples).
    pub fn extract_fields(&self, task: &str, fields: &[&str], text: &str) -> Result<Vec<Gliner2Hit>> {
        let processed = strip_role_markers(text, &self.role_marker_re);
        let hits = self.forward(task, fields, &processed)?;
        Ok(suppress_overlapping_spans(hits))
    }

    /// One GLiNER2 forward pass.
    /// Schema: `( [P] <task> ( [E] f1 [E] f2 … ) ) [SEP_TEXT] <words>`.
    fn forward(&self, task: &str, fields: &[&str], text: &str) -> Result<Vec<Gliner2Hit>> {
        let words = split_words(text);
        let num_words = words.len();
        if num_words == 0 || fields.is_empty() {
            return Ok(Vec::new());
        }

        let mut schema_tokens: Vec<String> = vec!["(".into(), "[P]".into()];
        schema_tokens.extend(task.split_whitespace().map(String::from));
        schema_tokens.push("(".into());
        for f in fields {
            schema_tokens.push("[E]".into());
            schema_tokens.extend(f.split_whitespace().map(String::from));
        }
        schema_tokens.push(")".into());
        schema_tokens.push(")".into());
        let num_schema_words = schema_tokens.len() + 1; // +1 for [SEP_TEXT]

        let mut full: Vec<&str> = schema_tokens.iter().map(|s| s.as_str()).collect();
        full.push("[SEP_TEXT]");
        for w in &words {
            full.push(w.lower.as_str());
        }

        let encoding = self
            .tokenizer
            .encode(full, false)
            .map_err(|e| Error::Storage(format!("GLiNER2 tokenize: {e}")))?;
        let token_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let word_ids = encoding.get_word_ids();
        let seq_len = token_ids.len();

        let first_tok = |target: u32| word_ids.iter().position(|&w| w == Some(target));
        let mut text_positions = Vec::with_capacity(num_words);
        for wi in 0..num_words {
            let pos = first_tok((num_schema_words + wi) as u32).ok_or_else(|| {
                Error::Storage(format!("GLiNER2: word {wi} missing from token mapping"))
            })?;
            text_positions.push(pos as i64);
        }
        let mut schema_positions = Vec::new();
        for (i, tok) in schema_tokens.iter().enumerate() {
            if tok == "[P]" || tok == "[E]" {
                let pos = first_tok(i as u32).ok_or_else(|| {
                    Error::Storage(format!("GLiNER2: schema token {i} not mapped"))
                })?;
                schema_positions.push(pos as i64);
            }
        }
        let num_schema_pos = schema_positions.len();

        let mut spans = Vec::with_capacity(num_words * MAX_SPAN_WIDTH * 2);
        for start in 0..num_words {
            for width in 1..=MAX_SPAN_WIDTH {
                if start + width <= num_words {
                    spans.push(start as i64);
                    spans.push((start + width - 1) as i64);
                } else {
                    spans.push(0);
                    spans.push(0);
                }
            }
        }

        // Tensors are built OUTSIDE `ort::inputs![]` on purpose: inside the
        // macro, `?` must yield an `ort::Error`, so mapping into this
        // crate's error type has to happen first.
        let tensor = |what: &'static str, r: ort::Result<Tensor<i64>>| {
            r.map_err(move |e| Error::Storage(format!("GLiNER2 {what}: {e}")))
        };
        let input_ids = tensor(
            "input_ids",
            Tensor::from_array((vec![1i64, seq_len as i64], token_ids)),
        )?;
        let attention_mask = tensor(
            "attention_mask",
            Tensor::from_array((vec![1i64, seq_len as i64], vec![1i64; seq_len])),
        )?;
        let text_positions = tensor(
            "text_positions",
            Tensor::from_array((vec![num_words as i64], text_positions)),
        )?;
        let schema_positions = tensor(
            "schema_positions",
            Tensor::from_array((vec![num_schema_pos as i64], schema_positions)),
        )?;
        let span_idx = tensor(
            "span_idx",
            Tensor::from_array((
                vec![1i64, (num_words * MAX_SPAN_WIDTH) as i64, 2i64],
                spans,
            )),
        )?;

        // `Session::run` takes `&self` on rc.9, so the guard needs no
        // `mut`. The `Mutex` stays for parity with the v1 extractor: it
        // serialises inference, which keeps peak arena allocation bounded
        // under concurrent callers. Revisit only with a measurement.
        let session = self
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let inputs = ort::inputs![
            "input_ids" => input_ids,
            "attention_mask" => attention_mask,
            "text_positions" => text_positions,
            "schema_positions" => schema_positions,
            "span_idx" => span_idx,
        ]
        .map_err(|e| Error::Storage(format!("GLiNER2 inputs: {e}")))?;
        let outputs = session
            .run(inputs)
            .map_err(|e| Error::Storage(format!("GLiNER2 session.run: {e}")))?;

        let view = outputs["span_scores"]
            .try_extract_tensor::<f32>()
            .map_err(|e| Error::Storage(format!("GLiNER2 span_scores: {e}")))?;
        let shape = view.shape().to_vec(); // (1, fields, words, MAX_SPAN_WIDTH)
        let num_fields = shape[1];

        let mut hits = Vec::new();
        for fi in 0..num_fields {
            for start in 0..num_words {
                for w in 0..MAX_SPAN_WIDTH {
                    let score = view[[0, fi, start, w]];
                    if score < self.threshold {
                        continue;
                    }
                    let end = start + w;
                    if end >= num_words {
                        continue;
                    }
                    let (char_start, char_end) = (words[start].start, words[end].end);
                    hits.push(Gliner2Hit {
                        label: fields.get(fi).unwrap_or(&"?").to_string(),
                        text: text[char_start..char_end].to_string(),
                        score,
                        word_start: start,
                        word_end: end,
                        char_start,
                        char_end,
                    });
                }
            }
        }
        Ok(hits)
    }
}

/// GLiNER2 on the ingest seam.
///
/// No `extract_mentions_batch` override: this export is one graph call
/// per text (the schema is baked into the prompt encoding), so the
/// trait's looping default IS the batch path. That is what the 7.29
/// chunks/s figure was measured against, so nothing is forfeited.
impl crate::labeled::LabeledEntityExtractor for Gliner2Extractor {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Canonical casing, not the lowercased schema field names the
    /// model is actually fed — the audit trail should read `Person`,
    /// matching the `chunk_entities.label` values it explains.
    fn labels(&self) -> Vec<String> {
        self.labels
            .iter()
            .map(|(_, canonical)| canonical.clone())
            .collect()
    }

    fn threshold(&self) -> f32 {
        self.threshold
    }

    fn extract_mentions(&self, text: &str) -> Result<Vec<EntityMention>> {
        let hits = self.extract(text)?;
        // Span-NMS has already suppressed the nested-span nest; this
        // second pass collapses the *repeat* mentions NMS keeps by
        // design (the same name at two places in the chunk), applying
        // the identical rule v1 applies — one row per (text, label).
        Ok(crate::labeled::dedupe_strongest(
            hits.into_iter()
                .map(|h| EntityMention {
                    text: crate::gliner_ner::normalize_mention_text(&h.text),
                    label: h.label,
                    char_start: h.char_start,
                    char_end: h.char_end,
                    score: h.score,
                })
                .collect(),
        ))
    }
}

impl sovereign_core::traits::EntityExtractor for Gliner2Extractor {
    fn extract_entities(&self, text: &str) -> Vec<String> {
        match self.extract(text) {
            Ok(hits) => {
                let mut out: Vec<String> = hits
                    .into_iter()
                    .map(|h| h.text.to_lowercase())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                out.retain(|s| !s.trim().is_empty());
                out
            }
            Err(e) => {
                // Same degradation contract as the v1 extractor: entity-aware
                // retrieval falls back to cosine+MMR rather than failing the
                // query. Logged, never silent.
                tracing::warn!(
                    error = %e,
                    model_id = %self.model_id,
                    "GLiNER2 extract_entities failed; returning no entities"
                );
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(label: &str, score: f32, word_start: usize, word_end: usize) -> Gliner2Hit {
        Gliner2Hit {
            label: label.into(),
            text: format!("w{word_start}..{word_end}"),
            score,
            word_start,
            word_end,
            char_start: word_start,
            char_end: word_end,
        }
    }

    /// The defect NMS exists to fix: the head scores every (start, width)
    /// pair, so one mention arrives as a nest of overlapping spans.
    #[test]
    fn overlapping_spans_collapse_to_the_highest_scoring_one() {
        let kept = suppress_overlapping_spans(vec![
            hit("person", 0.91, 3, 3),
            hit("person", 0.99, 3, 4),
            hit("person", 0.88, 4, 4),
        ]);
        assert_eq!(kept.len(), 1, "kept = {kept:?}");
        assert_eq!((kept[0].word_start, kept[0].word_end), (3, 4));
        assert!((kept[0].score - 0.99).abs() < f32::EPSILON);
    }

    #[test]
    fn distinct_mentions_at_different_positions_both_survive() {
        let kept = suppress_overlapping_spans(vec![
            hit("person", 0.99, 0, 0),
            hit("person", 0.97, 7, 8),
        ]);
        assert_eq!(kept.len(), 2, "kept = {kept:?}");
        // Reading order, not score order — callers render these inline.
        assert_eq!(kept[0].word_start, 0);
        assert_eq!(kept[1].word_start, 7);
    }

    /// Flat NER: suppression is across labels, matching v1's `SpanMode`,
    /// so the two backends yield comparable mention sets.
    #[test]
    fn suppression_is_flat_across_labels() {
        let kept = suppress_overlapping_spans(vec![
            hit("person", 0.95, 2, 3),
            hit("organization", 0.80, 3, 4),
        ]);
        assert_eq!(kept.len(), 1, "kept = {kept:?}");
        assert_eq!(kept[0].label, "person");
    }

    #[test]
    fn suppression_is_deterministic_when_scores_tie() {
        let a = suppress_overlapping_spans(vec![hit("x", 0.9, 1, 2), hit("y", 0.9, 2, 3)]);
        let b = suppress_overlapping_spans(vec![hit("y", 0.9, 2, 3), hit("x", 0.9, 1, 2)]);
        assert_eq!(a, b, "tie-break must not depend on input order");
    }

    #[test]
    fn empty_input_suppresses_to_empty() {
        assert!(suppress_overlapping_spans(Vec::new()).is_empty());
    }

    #[test]
    fn word_splitter_records_offsets_into_the_original_text() {
        let text = "Alain Locke wrote about Dewey.";
        let words = split_words(text);
        let joined: Vec<&str> = words.iter().map(|w| &text[w.start..w.end]).collect();
        assert_eq!(
            joined,
            vec!["Alain", "Locke", "wrote", "about", "Dewey", "."]
        );
        assert_eq!(words[0].lower, "alain");
    }

    /// Guard mirror of `GlinerExtractor`'s: a v1 id must be refused
    /// before the bare-ort session is built, or it fails as a missing
    /// `text_positions` input that reads like a corrupt model.
    #[test]
    fn refuses_a_v1_model_id_by_generation() {
        let err = Gliner2Extractor::new(
            crate::gliner_ner::DEFAULT_MODEL_ID,
            DEFAULT_LABELS,
            GLINER2_DEFAULT_THRESHOLD,
        )
        .err()
        .expect("v1 model id must be refused");
        let msg = format!("{err}");
        assert!(msg.contains("v1-generation"), "msg = {msg}");
    }
}
