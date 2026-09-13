// SPDX-License-Identifier: AGPL-3.0-or-later
//! The input bound at the GLiNER inference seam.
//!
//! **What went wrong.** [`crate::GlinerChunkExtractor::extract_for_conversation`]
//! collected EVERY chunk of a conversation into one `Vec<&str>` and handed
//! it to [`LabeledEntityExtractor::extract_mentions_batch`] in a single
//! call. gline-rs batches natively — one `inference()` over N texts — so
//! the transformer's per-layer attention buffers scale as
//! `N × heads × seq² × 4` bytes and nothing anywhere capped N. A Claude
//! Code transcript imported as a `threaded_turns` corpus puts thousands of
//! chunks in one conversation.
//!
//! Measured 2026-09-12 on pid 47944 (`MallocStackLogging` +
//! `malloc_history`, `vmmap --summary`, a 15 s `sample`): the daemon went
//! 20.2 GB → 79.9 GB in eight minutes with ZERO requests, on stackless
//! power-of-two onnxruntime arena blocks of 1, 2, 2, 8 and 32 GB, with the
//! only busy thread inside `extract_batch` → onnxruntime. Two jetsam
//! SIGTERMs.
//!
//! **Two bounds, and only one of them was the incident.**
//! [`MAX_BATCH_CHUNKS`] is the one that mattered:
//! `corpus-engine/src/chunkers/threaded_turns.rs` already caps a chunk at
//! `MAX_HARD_CHARS` (2,100), so the chunks in that pass were short and
//! numerous, not long. [`MAX_CHUNK_CHARS`] bounds the other axis, which is
//! unguarded on the GLiNER2 path and only SILENTLY truncated on the v1
//! path — see its docs.
//!
//! **Refused, never truncated.** A text over the per-chunk bound is not
//! sent and not shortened: it is counted in [`BoundedInputs::over_cap`],
//! reported by the caller at `warn` with corpus/conversation/chunk id, and
//! carried out of the pass on `ChunkNerOutcome::refused_over_cap` so the
//! enrichment state records it. Silently shipping half a chunk's entities
//! is the substitution ARCH 6 forbids; the pre-existing gline-rs
//! truncation below is exactly that failure, and this module's job is to
//! stop guessing on the caller's behalf.

use sovereign_core::error::Result;

use crate::gliner_ner::EntityMention;
use crate::labeled::LabeledEntityExtractor;

/// Per-text character ceiling. **Derived from the model, not chosen.**
///
/// gline-rs's `Parameters::default()` sets `max_length: Some(512)`
/// (`gline-rs-1.0.1/src/model/params.rs:23` and the `Default` impl at
/// :33), and `sovereign-gliner` passes `Parameters::default()` verbatim at
/// `gliner_ner.rs`'s `GLiNER::<SpanMode>::new` call — a grep for
/// `Parameters::new|with_max_length` across `sovereign-gliner/src` returns
/// nothing (checked 2026-09-12), so 512 is what the v1 path runs.
///
/// The unit is WORDS, not subword tokens: the limit is applied by
/// `RegexSplitter` (`gline-rs-1.0.1/src/text/splitter.rs:38-47`) over the
/// pattern `\w+(?:[-_]\w+)*|\S`, which BREAKS out of the token loop at the
/// limit. That is a **silent truncation** — no error, no report, no way
/// for a caller to learn its text was cut. Everything past word 512 was
/// already being dropped on the floor before this bound existed.
///
/// 512 words × 4 chars/word = 2,048. Four is conservative for English
/// prose (~4.7 letters plus a separator) and about right for the
/// punctuation-dense agent transcripts this path actually sees, where the
/// `|\S` arm makes every bracket and comma its own word. So a text under
/// this ceiling is one gline-rs would not have truncated.
///
/// Interaction worth knowing: `threaded_turns` emits chunks up to
/// `MAX_HARD_CHARS = 2,100` (`corpus-engine/src/chunkers/threaded_turns.rs`),
/// so chunks in the 2,048..2,100 band are refused here. That is intended
/// rather than tolerated — 2,100 chars of transcript is well past 512
/// regex words, so those are precisely the chunks gline-rs was truncating.
pub const MAX_CHUNK_CHARS: usize = 2_048;

/// Per-inference-call chunk ceiling — **the bound that stops the
/// incident.** gline-rs runs one `inference()` per batch, so peak arena is
/// linear in this number; at 16 the attention buffers for a 512-token
/// sequence are hundreds of MB rather than the tens of GB an
/// unbounded-N conversation produced.
///
/// 16 is a batch-size DEFAULT, not a derived quantity, and it is recorded
/// as one in `sovereign/DEFAULTS_LEDGER.md`. It is not tuned: it was
/// chosen as the smallest power of two that still keeps v1's native
/// batching worth having (the trait's looping default is N=1), and the
/// throughput cost of the choice has not been measured.
pub const MAX_BATCH_CHUNKS: usize = 16;

/// A text the bound REFUSED, by position in the caller's slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverCapText {
    /// Index into the `texts` slice the caller passed to
    /// [`BoundedInputs::plan`] — so the caller can name the chunk id.
    pub index: usize,
    /// Length in bytes of the refused text, for the warn line.
    pub chars: usize,
}

/// One caller's slice of texts, split into inference-sized batches with
/// the over-cap ones held back.
///
/// The ONE implementation of the input bound (ARCH 8). Both
/// `GlinerChunkExtractor` entry points — the per-conversation pass and the
/// incremental delta pass — plan through this and drive inference through
/// [`BoundedInputs::extract`]; neither calls
/// [`LabeledEntityExtractor::extract_mentions_batch`] with a raw slice any
/// more.
pub struct BoundedInputs<'a> {
    /// `(original index, text)`, chunked to at most [`MAX_BATCH_CHUNKS`].
    batches: Vec<Vec<(usize, &'a str)>>,
    over_cap: Vec<OverCapText>,
    total: usize,
}

impl<'a> BoundedInputs<'a> {
    /// Apply the bound. Pure — no inference, no logging; the caller owns
    /// the reporting because only the caller knows the corpus, the
    /// conversation and the chunk ids.
    pub fn plan(texts: &[&'a str]) -> Self {
        let mut sendable: Vec<(usize, &'a str)> = Vec::with_capacity(texts.len());
        let mut over_cap = Vec::new();
        for (index, text) in texts.iter().enumerate() {
            if text.len() > MAX_CHUNK_CHARS {
                over_cap.push(OverCapText {
                    index,
                    chars: text.len(),
                });
            } else {
                sendable.push((index, *text));
            }
        }
        Self {
            batches: sendable
                .chunks(MAX_BATCH_CHUNKS)
                .map(<[(usize, &'a str)]>::to_vec)
                .collect(),
            over_cap,
            total: texts.len(),
        }
    }

    /// The texts refused for exceeding [`MAX_CHUNK_CHARS`], in input
    /// order. Empty on the happy path.
    pub fn over_cap(&self) -> &[OverCapText] {
        &self.over_cap
    }

    /// How many inference calls [`extract`](Self::extract) will make.
    /// Exposed for the tracing line and for the tests that pin the bound.
    pub fn batch_count(&self) -> usize {
        self.batches.len()
    }

    /// Drive `extractor` over the planned batches and reassemble the
    /// results into INPUT order, one `Vec` per input text.
    ///
    /// A refused text gets an empty `Vec` — same shape a text with no
    /// entities gets, because the caller's `zip` over its chunks depends
    /// on the one-per-input contract. The refusal is not hidden by that:
    /// it is in [`over_cap`](Self::over_cap), which the caller reports
    /// before calling this.
    pub fn extract(
        &self,
        extractor: &dyn LabeledEntityExtractor,
    ) -> Result<Vec<Vec<EntityMention>>> {
        let mut out: Vec<Vec<EntityMention>> = vec![Vec::new(); self.total];
        for batch in &self.batches {
            let texts: Vec<&str> = batch.iter().map(|(_, t)| *t).collect();
            let mentions = extractor.extract_mentions_batch(&texts)?;
            for ((index, _), m) in batch.iter().zip(mentions) {
                out[*index] = m;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records the shape of everything handed to the inference seam. This
    /// is the assertion surface the guard exists for: what the model
    /// actually receives, not what the caller intended to send.
    #[derive(Default)]
    struct RecordingExtractor {
        /// One entry per `extract_mentions_batch` call: the byte length
        /// of each text in that batch.
        batches: Mutex<Vec<Vec<usize>>>,
    }

    impl LabeledEntityExtractor for RecordingExtractor {
        fn model_id(&self) -> &str {
            "gliner_small-v2.1"
        }
        fn labels(&self) -> Vec<String> {
            vec!["Person".into()]
        }
        fn threshold(&self) -> f32 {
            0.6
        }
        fn extract_mentions(&self, _text: &str) -> Result<Vec<EntityMention>> {
            Ok(Vec::new())
        }
        fn extract_mentions_batch(&self, texts: &[&str]) -> Result<Vec<Vec<EntityMention>>> {
            self.batches
                .lock()
                .unwrap()
                .push(texts.iter().map(|t| t.len()).collect());
            Ok(vec![Vec::new(); texts.len()])
        }
    }

    fn mention(text: &str) -> EntityMention {
        EntityMention {
            text: text.to_string(),
            label: "Person".to_string(),
            char_start: 0,
            char_end: text.len(),
            score: 0.9,
        }
    }

    /// The failing input from the order: 40 chunks of 60,000 characters in
    /// one conversation. Before the bound this was ONE `inference()` call
    /// over 2.4 MB of text; the seam must now see nothing over
    /// `MAX_CHUNK_CHARS` and no batch over `MAX_BATCH_CHUNKS`.
    #[test]
    fn a_forty_by_sixty_thousand_conversation_reaches_the_seam_bounded() {
        let big = "word ".repeat(12_000); // 60,000 bytes
        assert_eq!(big.len(), 60_000);
        let owned: Vec<String> = (0..40).map(|_| big.clone()).collect();
        let texts: Vec<&str> = owned.iter().map(String::as_str).collect();

        let seam = RecordingExtractor::default();
        let plan = BoundedInputs::plan(&texts);
        let out = plan.extract(&seam).unwrap();

        let seen = seam.batches.lock().unwrap();
        for batch in seen.iter() {
            assert!(
                batch.len() <= MAX_BATCH_CHUNKS,
                "a batch of {} exceeds the per-call bound",
                batch.len()
            );
            for len in batch {
                assert!(
                    *len <= MAX_CHUNK_CHARS,
                    "a text of {len} bytes exceeds the per-chunk bound"
                );
            }
        }
        assert_eq!(
            plan.over_cap().len(),
            40,
            "every over-cap chunk is counted, not truncated"
        );
        assert!(
            plan.over_cap().iter().all(|o| o.chars == 60_000),
            "the refusal carries the real length so the warn line is actionable"
        );
        assert_eq!(out.len(), 40, "one result slot per input, refused or not");
        assert!(out.iter().all(Vec::is_empty));
    }

    /// The batch bound alone — the axis that actually caused the incident.
    /// `threaded_turns` chunks are ≤ 2,100 bytes, so the pass that took
    /// the daemon to 79.9 GB was thousands of SHORT texts in one call.
    #[test]
    fn a_thousand_short_chunks_are_split_into_bounded_batches() {
        let owned: Vec<String> = (0..1_000).map(|i| format!("chunk {i} text")).collect();
        let texts: Vec<&str> = owned.iter().map(String::as_str).collect();

        let seam = RecordingExtractor::default();
        let plan = BoundedInputs::plan(&texts);
        plan.extract(&seam).unwrap();

        assert!(plan.over_cap().is_empty(), "short chunks are not refused");
        assert_eq!(plan.batch_count(), 1_000_usize.div_ceil(MAX_BATCH_CHUNKS));
        let seen = seam.batches.lock().unwrap();
        assert_eq!(seen.len(), plan.batch_count());
        assert!(seen.iter().all(|b| b.len() <= MAX_BATCH_CHUNKS));
        assert_eq!(
            seen.iter().map(Vec::len).sum::<usize>(),
            1_000,
            "splitting must not drop a chunk"
        );
    }

    /// The bound must not scramble results. A caller `zip`s its chunk rows
    /// against this output, so a mis-ordered reassembly files one
    /// conversation's entities under another chunk's id — silently.
    #[test]
    fn results_come_back_in_input_order_with_refused_slots_empty() {
        let over = "x".repeat(MAX_CHUNK_CHARS + 1);
        let owned: Vec<String> = (0..40)
            .map(|i| {
                if i % 7 == 3 {
                    over.clone()
                } else {
                    format!("text {i}")
                }
            })
            .collect();
        let texts: Vec<&str> = owned.iter().map(String::as_str).collect();

        /// Returns one mention naming the text it saw, so the output slot
        /// can be checked against the input that produced it.
        struct EchoExtractor;
        impl LabeledEntityExtractor for EchoExtractor {
            fn model_id(&self) -> &str {
                "gliner_small-v2.1"
            }
            fn labels(&self) -> Vec<String> {
                vec!["Person".into()]
            }
            fn threshold(&self) -> f32 {
                0.6
            }
            fn extract_mentions(&self, text: &str) -> Result<Vec<EntityMention>> {
                Ok(vec![mention(text)])
            }
            fn extract_mentions_batch(&self, texts: &[&str]) -> Result<Vec<Vec<EntityMention>>> {
                texts.iter().map(|t| self.extract_mentions(t)).collect()
            }
        }

        let plan = BoundedInputs::plan(&texts);
        let out = plan.extract(&EchoExtractor).unwrap();

        assert_eq!(out.len(), 40);
        for (i, slot) in out.iter().enumerate() {
            if i % 7 == 3 {
                assert!(slot.is_empty(), "slot {i} was refused, so it stays empty");
            } else {
                assert_eq!(slot.len(), 1);
                assert_eq!(
                    slot[0].text,
                    format!("text {i}"),
                    "slot {i} holds another input's mentions — the reassembly is wrong"
                );
            }
        }
        let refused: Vec<usize> = plan.over_cap().iter().map(|o| o.index).collect();
        assert_eq!(refused, vec![3, 10, 17, 24, 31, 38]);
    }

    /// Exactly at the ceiling is sent; one byte over is refused. The
    /// boundary is where an off-by-one turns a bound into a silent
    /// data-loss policy.
    #[test]
    fn the_per_chunk_ceiling_is_inclusive() {
        let at = "x".repeat(MAX_CHUNK_CHARS);
        let over = "x".repeat(MAX_CHUNK_CHARS + 1);
        let plan = BoundedInputs::plan(&[at.as_str(), over.as_str()]);
        assert_eq!(plan.over_cap().len(), 1);
        assert_eq!(plan.over_cap()[0].index, 1);
        assert_eq!(plan.batch_count(), 1);
    }

    /// An empty conversation makes no inference call at all.
    #[test]
    fn no_texts_is_no_batches() {
        let plan = BoundedInputs::plan(&[]);
        assert_eq!(plan.batch_count(), 0);
        assert!(plan.over_cap().is_empty());
        let seam = RecordingExtractor::default();
        assert!(plan.extract(&seam).unwrap().is_empty());
        assert!(seam.batches.lock().unwrap().is_empty());
    }
}
