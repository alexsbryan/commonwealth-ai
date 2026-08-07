// SPDX-License-Identifier: AGPL-3.0-or-later
//! Streaming gate pipeline — Phase A SCAFFOLD (spec:
//! docs/specs/STREAMING_GATE_PIPELINE.md).
//!
//! Verify completed sentences of the held draft on the FAST slot (4B) AS the
//! 35B streams later tokens, so audit #1 overlaps synthesis instead of running
//! after it. The fast slot is idle during the draft (the draft owns the 35B), so
//! the per-sentence checks add ~no wall-clock — that is the whole point; a Slow
//! (35B) check would contend with the draft and defeat the overlap.
//!
//! Behind SOVEREIGN_GATE_PIPELINE (default OFF). SCAFFOLD boundary: the streamed
//! verdicts are collected and glassbox-logged but NOT yet consumed by the gate.
//! Wiring them into `gate_longform` (to skip re-verification) is the next
//! increment, gated on a fast-slot-verify calibration — the scaffold-quality
//! yes/no check below is deliberately NOT the gate's calibrated forced-choice
//! logit, so it must never gate a release until that calibration lands.

use std::sync::Arc;

use crate::oicp::ShardingPrivacy;
use crate::slot_policy::Workload;
use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};

use super::config::dbg;

/// Sentences shorter than this are structural (headers, fragments) — skip.
const MIN_SENTENCE_CHARS: usize = 20;
const VERIFY_CHUNKS: usize = 8;
const VERIFY_CHUNK_CHARS: usize = 1_200;

/// Per-sentence support verdict: `Some(false)` supported, `Some(true)`
/// unsupported (violation), `None` undecided (fail-open — never counted against
/// the answer).
pub(crate) type SentenceVerdict = Option<bool>;

/// Verifies completed sentences of a held draft concurrently on the fast slot,
/// as the 35B streams later tokens. Feed the growing held text via [`ingest`];
/// [`collect`] awaits every dispatched check.
pub(crate) struct StreamingVerifier {
    inference: Arc<dyn InferenceProvider>,
    chunks: Arc<Vec<String>>,
    posture: ShardingPrivacy,
    /// Count of sentences already dispatched (cursor into the split).
    dispatched: usize,
    handles: Vec<tokio::task::JoinHandle<SentenceVerdict>>,
}

impl StreamingVerifier {
    /// `Some` only on a gated turn with evidence AND the pipeline flag on; `None`
    /// otherwise so the caller pays zero cost on the default path.
    pub(crate) fn maybe_new(
        inference: &Arc<dyn InferenceProvider>,
        gate_on: bool,
        chunks: &[String],
        posture: ShardingPrivacy,
    ) -> Option<Self> {
        if !gate_on || chunks.is_empty() || !super::config::gate_pipeline_enabled() {
            return None;
        }
        Some(Self::build(inference, chunks, posture))
    }

    fn build(
        inference: &Arc<dyn InferenceProvider>,
        chunks: &[String],
        posture: ShardingPrivacy,
    ) -> Self {
        Self {
            inference: inference.clone(),
            chunks: Arc::new(chunks.to_vec()),
            posture,
            dispatched: 0,
            handles: Vec::new(),
        }
    }

    /// Dispatch a fast-slot check for every sentence that is now COMPLETE (all
    /// but the still-streaming last one). Only newly-completed sentences past the
    /// cursor are dispatched, so this is cheap to call on every heartbeat.
    pub(crate) fn ingest(&mut self, held_text: &str) {
        let sentences = super::surgical::split_sentences(held_text);
        let complete = sentences.len().saturating_sub(1); // last may still be growing
        while self.dispatched < complete {
            let idx = self.dispatched;
            self.dispatched += 1;
            self.spawn(sentences[idx].trim().to_string());
        }
    }

    fn spawn(&mut self, sentence: String) {
        if sentence.chars().count() < MIN_SENTENCE_CHARS {
            return; // structural line — nothing to verify
        }
        let inference = self.inference.clone();
        let chunks = self.chunks.clone();
        let posture = self.posture;
        self.handles.push(tokio::spawn(async move {
            verify_sentence(&inference, &chunks, &sentence, posture).await
        }));
    }

    /// Draft complete: dispatch any remaining sentences (incl. the last), then
    /// await every check. Returns `(unsupported_count, verified_count)`.
    pub(crate) async fn collect(mut self, final_text: &str) -> (usize, usize) {
        let sentences = super::surgical::split_sentences(final_text);
        while self.dispatched < sentences.len() {
            let idx = self.dispatched;
            self.dispatched += 1;
            self.spawn(sentences[idx].trim().to_string());
        }
        let (mut unsupported, mut verified) = (0usize, 0usize);
        for h in self.handles {
            match h.await {
                Ok(Some(v)) => {
                    verified += 1;
                    if v {
                        unsupported += 1;
                    }
                }
                Ok(None) => verified += 1, // undecided — fail-open
                Err(_) => {}               // task cancelled/panicked — ignore
            }
        }
        (unsupported, verified)
    }
}

/// Fast-slot supported-check. SCAFFOLD-quality (a yes/no, not the gate's
/// calibrated forced-choice logit) — enough to prove the overlap and measure it;
/// the consume step will use the calibrated fast-slot verdict.
async fn verify_sentence(
    inference: &Arc<dyn InferenceProvider>,
    chunks: &[String],
    sentence: &str,
    posture: ShardingPrivacy,
) -> SentenceVerdict {
    let evidence = chunks
        .iter()
        .take(VERIFY_CHUNKS)
        .map(|c| c.chars().take(VERIFY_CHUNK_CHARS).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n---\n");
    let prompt = format!(
        "PASSAGES (separated by ---):\n\"\"\"\n{evidence}\n\"\"\"\n\n\
         SENTENCE: {sentence}\n\n\
         Do the passages support this sentence? Answer with exactly one word: yes or no."
    );
    let req = CompletionRequest {
        prompt,
        system_message: Some(
            "You judge whether passages support a sentence. Answer yes or no.".to_string(),
        ),
        preferred_speed: Speed::Fast,
        oicp: Some(Workload::Judge.requirements(posture)),
        max_tokens: Some(4),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    match inference.complete(&req).await {
        Ok(resp) => {
            let t = resp.text.trim().to_lowercase();
            if t.starts_with("yes") {
                Some(false)
            } else if t.starts_with("no") {
                Some(true)
            } else {
                None
            }
        }
        Err(e) => {
            dbg(&format!("pipeline verify_sentence failed: {e}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::types::{CompletionResponse, Depth, ProviderCapabilities};
    use futures::Stream;
    use std::pin::Pin;

    // Always answers "no" → every checked sentence reads unsupported.
    struct NoProvider;
    #[async_trait::async_trait]
    impl InferenceProvider for NoProvider {
        async fn complete(&self, _r: &CompletionRequest) -> Result<CompletionResponse> {
            Ok(CompletionResponse {
                text: "no".into(),
                tokens_used: 0,
                prompt_tokens: 0,
                model_id: "test".into(),
                latency_ms: 0,
                oicp_meta: None,
                finish_reason: None,
                completion_tokens: None,
                ..Default::default()
            })
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            unimplemented!()
        }
        async fn embed(&self, _t: &str) -> Result<Vec<f32>> {
            Ok(vec![])
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

    fn posture() -> ShardingPrivacy {
        ShardingPrivacy::LocalOnly
    }

    #[test]
    fn maybe_new_is_none_off_the_gated_path() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(NoProvider);
        // gate_on=false → None regardless of the flag.
        assert!(StreamingVerifier::maybe_new(&inf, false, &["c".into()], posture()).is_none());
        // empty evidence → None.
        assert!(StreamingVerifier::maybe_new(&inf, true, &[], posture()).is_none());
    }

    #[tokio::test]
    async fn ingest_defers_last_sentence_then_collect_verifies_all() {
        let inf: Arc<dyn InferenceProvider> = Arc::new(NoProvider);
        // build() bypasses the env flag so the dispatch/collect logic is testable.
        let mut v = StreamingVerifier::build(&inf, &["some evidence passage".into()], posture());
        // Two complete sentences + an in-progress third.
        v.ingest(
            "Alyosha is the youngest brother here. Ivan wrote an article on courts. Dmitri is",
        );
        assert_eq!(
            v.dispatched, 2,
            "only the two COMPLETE sentences dispatch mid-stream"
        );
        // Draft finishes; the third sentence completes and is checked too.
        let (unsupported, verified) =
            v.collect("Alyosha is the youngest brother here. Ivan wrote an article on courts. Dmitri is reckless and passionate.").await;
        assert_eq!(verified, 3, "all three sentences verified by draft-end");
        assert_eq!(
            unsupported, 3,
            "NoProvider flags every sentence unsupported"
        );
    }
}
