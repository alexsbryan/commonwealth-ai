// SPDX-License-Identifier: AGPL-3.0-or-later
//! What an [`InferenceProvider`] must do to be a correct engine — as code
//! you can run, not prose you have to have read.
//!
//! [`InferenceProvider`]'s contracts live in its doc comments, and most of
//! them are about HONESTY rather than behaviour: report `"unknown"` when
//! you cannot verify an embedding model, return `NotImplemented` rather
//! than a fabricated rerank score, always emit a terminal `Finish` frame
//! so a receiver can tell a finished stream from a truncated one. A new
//! engine that ignores any of them still compiles, still serves, and
//! fails silently and much later — which is this system's characteristic
//! failure (ARCH §18).
//!
//! So the contracts are checked here instead. The split is what makes
//! this cheap:
//!
//! - [`check_sync`] touches no I/O and needs no weights, so it runs
//!   against **any** engine — including one pointed at an endpoint that
//!   is switched off. This is where an engine lies most easily, because
//!   every method involved has a plausible-looking default.
//! - [`check_serving`] drives a real request and needs an engine that can
//!   actually answer. Run it against the in-process reference engine in
//!   CI and against a live engine in a smoke lane.
//!
//! Both return the violations they found rather than panicking, so a
//! caller can report all of them at once — a checker that stops at the
//! first failure turns one run into six.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use sovereign_inference::engine_conformance::check_sync;
//! # fn f(engine: Arc<dyn sovereign_core::traits::InferenceProvider>) {
//! let violations = check_sync(engine.as_ref());
//! assert!(violations.is_empty(), "{violations:#?}");
//! # }
//! ```

use futures::StreamExt;

use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed, StreamFrame};

/// Sample text for the tokenizer check — long enough that any real
/// tokenizer returns a positive count, short enough to cost nothing.
const SAMPLE: &str = "The quick brown fox jumps over the lazy dog. \
                      Correctness is not a property you can add later.";

/// Contracts checkable with no I/O and no weights.
///
/// Safe to run against an engine whose backend is unreachable: nothing
/// here dials anything. Returns one string per violation, empty when the
/// engine conforms.
pub fn check_sync(p: &dyn InferenceProvider) -> Vec<String> {
    let mut v = Vec::new();

    // A zero context window admits no request at all, so every caller's
    // budget arithmetic divides into nothing.
    let caps = p.capabilities();
    if caps.max_context_tokens == 0 {
        v.push(
            "capabilities().max_context_tokens is 0 — no request can ever be admitted. \
             Report the real window, or the largest one this engine will accept."
                .to_string(),
        );
    }

    // Provenance is stamped from these on every streaming response. An
    // empty id is worse than `"unknown"`: it renders as a blank model
    // name rather than as a missing one.
    for speed in [Speed::Fast, Speed::Slow] {
        if p.model_id_for(speed).is_empty() {
            v.push(format!(
                "model_id_for({speed:?}) returned an empty string. Return the model name, \
                 or the trait's `\"unknown\"` default — never an empty id, which surfaces \
                 as a blank attribution instead of a missing one."
            ));
        }
    }
    if p.embed_model_id().is_empty() {
        v.push(
            "embed_model_id() returned an empty string. `\"unknown\"` is the correct answer \
             when the engine cannot verify its embed model — callers treat it as \
             'cannot verify' and refuse to persist. An empty string has no such meaning."
                .to_string(),
        );
    }

    // Prompt budgeting calls this in hot loops; a zero count on real text
    // makes every budget look infinite.
    if p.count_tokens(SAMPLE) == 0 {
        v.push(format!(
            "count_tokens() returned 0 for {} chars of ordinary prose. Compaction and \
             retrieval-bundle truncation both budget against this, so a zero makes every \
             prompt look free.",
            SAMPLE.len()
        ));
    }

    // `None` means "no meaningful local context" and is always allowed.
    // `Some(0)` is a lie in the shape of an answer.
    if let Some(n) = p.effective_context_size() {
        if n == 0 {
            v.push(
                "effective_context_size() returned Some(0). Return None when the engine \
                 owns no local context; Some(0) reads as a real, unusable window."
                    .to_string(),
            );
        }
    }
    if let Some(n) = p.n_ctx_train_for_primary() {
        if n == 0 {
            v.push("n_ctx_train_for_primary() returned Some(0) — return None instead.".to_string());
        }
    }
    // The documented relationship: the trained window is the ceiling the
    // effective one is capped at. Inverted, the Settings UI offers the
    // operator a context bump that silently degrades quality via RoPE.
    if let (Some(eff), Some(train)) = (p.effective_context_size(), p.n_ctx_train_for_primary()) {
        if eff > train {
            v.push(format!(
                "effective_context_size() ({eff}) exceeds n_ctx_train_for_primary() ({train}). \
                 The trained window is the ceiling; a larger effective window means RoPE \
                 scaling is silently in play."
            ));
        }
    }

    // One name, one slot (ARCH §10.6). A duplicate makes `model_id`
    // routing non-deterministic.
    let inventory = p.extras_inventory();
    let mut names: Vec<&str> = inventory.iter().map(|(n, _)| n.as_str()).collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    if names.len() != before {
        v.push(
            "extras_inventory() reported the same slot name twice — a request naming it \
             would route non-deterministically."
                .to_string(),
        );
    }

    v
}

/// Contracts that need an engine which can actually answer a request.
///
/// Drives one tiny completion, one embed batch and one rerank call. Run
/// against the reference engine in CI, and against a live engine in a
/// smoke lane.
pub async fn check_serving(p: &dyn InferenceProvider) -> Vec<String> {
    let mut v = Vec::new();

    // THE invariant this module exists for. `complete_stream_with_finish`
    // must end with exactly one terminal frame, and nothing may follow
    // it — receivers treat a closed channel with no terminal frame as
    // `Cancelled`, so an engine that forgets it turns every completed
    // answer into an apparent truncation.
    let mut req = CompletionRequest::new("Say the single word: ok");
    req.max_tokens = Some(8);
    match p.complete_stream_with_finish(&req).await {
        Ok(stream) => {
            let frames: Vec<StreamFrame> = stream.collect().await;
            match frames.last() {
                None => v.push(
                    "complete_stream_with_finish() yielded no frames at all. A stream must \
                     end with Finish or Error; an empty stream is indistinguishable from a \
                     cancelled one."
                        .to_string(),
                ),
                Some(StreamFrame::Finish { .. }) | Some(StreamFrame::Error(_)) => {}
                Some(other) => v.push(format!(
                    "complete_stream_with_finish() ended on {other:?} instead of a terminal \
                     Finish/Error frame. Receivers read a missing terminal frame as \
                     Cancelled, so this reports every successful answer as truncated."
                )),
            }
            let terminal = frames
                .iter()
                .filter(|f| matches!(f, StreamFrame::Finish { .. } | StreamFrame::Error(_)))
                .count();
            if terminal > 1 {
                v.push(format!(
                    "complete_stream_with_finish() emitted {terminal} terminal frames. \
                     Exactly one must be produced, and it must be last."
                ));
            }
        }
        Err(e) => v.push(format!(
            "complete_stream_with_finish() failed outright: {e}. If this engine cannot \
             stream, the trait's default implementation over complete_stream() is the \
             correct answer — do not override it into an error."
        )),
    }

    // Streaming provenance: whatever id is reported, it must be nameable.
    match p.complete_stream_with_id(&req).await {
        Ok((_, id)) if id.is_empty() => v.push(
            "complete_stream_with_id() returned an empty model id — the response renders \
             with a blank attribution."
                .to_string(),
        ),
        Ok(_) => {}
        Err(e) => v.push(format!("complete_stream_with_id() failed: {e}")),
    }

    // A batch that silently drops or reorders entries corrupts an entire
    // corpus ingest, and the damage is only visible at retrieval time.
    let texts = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
    match p.embed_batch(&texts).await {
        Ok(vectors) if vectors.len() != texts.len() => v.push(format!(
            "embed_batch() returned {} vectors for {} inputs. Callers zip the result back \
             against their chunks positionally, so a length mismatch mis-assigns every \
             embedding after the gap.",
            vectors.len(),
            texts.len()
        )),
        Ok(vectors) => {
            if let Some(i) = vectors.iter().position(|e| e.is_empty()) {
                v.push(format!(
                    "embed_batch() returned an empty vector at index {i} — an all-zero or \
                     absent embedding ranks arbitrarily rather than failing."
                ));
            }
        }
        // An engine without an embed model is expected to error here.
        Err(_) => {}
    }

    // Rerank must refuse rather than fabricate. `NotImplemented` is a
    // correct answer; a wrong-length score vector is not.
    let docs = vec!["first".to_string(), "second".to_string()];
    if let Ok(scores) = p.rerank_batch("query", &docs).await {
        if scores.len() != docs.len() {
            v.push(format!(
                "rerank_batch() returned {} scores for {} docs. Callers map scores back by \
                 position. If this engine has no reranker, return the trait's \
                 NotImplemented default — retrieval falls back to un-reranked fusion.",
                scores.len(),
                docs.len()
            ));
        }
    }

    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use sovereign_core::error::Result;
    use sovereign_core::types::{CompletionResponse, ProviderCapabilities};
    use std::pin::Pin;

    fn echo_response(text: String) -> CompletionResponse {
        CompletionResponse {
            text,
            tokens_used: 1,
            prompt_tokens: 1,
            model_id: "echo".to_string(),
            latency_ms: 0,
            oicp_meta: None,
            finish_reason: None,
            completion_tokens: None,
        }
    }

    /// The reference engine: the smallest thing that satisfies the trait
    /// honestly. Doubles as the worked example an out-of-tree engine
    /// starts from — it implements the four required methods and inherits
    /// every default, which is exactly the intended shape.
    struct EchoEngine;

    #[async_trait]
    impl InferenceProvider for EchoEngine {
        async fn complete(&self, request: &CompletionRequest) -> Result<CompletionResponse> {
            Ok(echo_response(request.prompt.clone()))
        }
        async fn complete_stream(
            &self,
            request: &CompletionRequest,
        ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
            let text = request.prompt.clone();
            Ok(Box::pin(futures::stream::once(async move { Ok(text) })))
        }
        async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3])
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 8192,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: sovereign_core::types::Depth::Shallow,
            }
        }
    }

    /// An engine that implements only the four required methods and takes
    /// every default must PASS. If this ever fails, the defaults on
    /// `InferenceProvider` have stopped being a usable starting point and
    /// the trait — not the engine — is what needs fixing.
    #[test]
    fn the_minimal_reference_engine_passes_the_sync_contract() {
        let violations = check_sync(&EchoEngine);
        assert!(
            violations.is_empty(),
            "a minimal honest engine must conform out of the box: {violations:#?}"
        );
    }

    #[tokio::test]
    async fn the_minimal_reference_engine_passes_the_serving_contract() {
        let violations = check_serving(&EchoEngine).await;
        assert!(violations.is_empty(), "{violations:#?}");
    }

    /// The suite has to be able to FAIL, or it is not a gate (ARCH §18.1
    /// — a check with no failing input you can name). This engine gets
    /// each contract exactly backwards.
    struct DishonestEngine;

    #[async_trait]
    impl InferenceProvider for DishonestEngine {
        async fn complete(&self, _: &CompletionRequest) -> Result<CompletionResponse> {
            Ok(echo_response(String::new()))
        }
        async fn complete_stream(
            &self,
            _: &CompletionRequest,
        ) -> Result<Pin<Box<dyn futures::Stream<Item = Result<String>> + Send>>> {
            Ok(Box::pin(futures::stream::empty()))
        }
        async fn embed(&self, _: &str) -> Result<Vec<f32>> {
            Ok(vec![0.0])
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                // The lie: no window at all.
                max_context_tokens: 0,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: sovereign_core::types::Depth::Shallow,
            }
        }
        fn model_id_for(&self, _: Speed) -> String {
            String::new() // blank attribution
        }
        fn embed_model_id(&self) -> String {
            String::new() // neither a name nor an honest "unknown"
        }
        fn count_tokens(&self, _: &str) -> u32 {
            0 // every prompt looks free
        }
        fn effective_context_size(&self) -> Option<u32> {
            Some(0)
        }
        // Silently drops entries — the corpus-corrupting shape.
        async fn embed_batch(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(vec![vec![0.1]])
        }
        // Fabricates a score rather than refusing.
        async fn rerank_batch(&self, _: &str, _: &[String]) -> Result<Vec<f32>> {
            Ok(vec![1.0])
        }
        // The silent-truncation shape, and the ONLY way to produce it:
        // yields tokens, then just stops. Measured 2026-08-30 — an engine
        // that implements only the four required methods CANNOT violate
        // this contract, because the trait's default
        // `complete_stream_with_finish` chains a synthetic
        // `Finish { Stop }` onto `complete_stream`. So the contract binds
        // exactly the engines that override this method for real finish
        // reasons — `EmbeddedLlamaCpp` today, and any engine that wants
        // an honest `finish_reason` on the OpenAI wire.
        async fn complete_stream_with_finish(
            &self,
            _: &CompletionRequest,
        ) -> Result<Pin<Box<dyn futures::Stream<Item = StreamFrame> + Send>>> {
            Ok(Box::pin(futures::stream::iter(vec![StreamFrame::Token(
                "partial".to_string(),
            )])))
        }
    }

    #[test]
    fn the_sync_contract_catches_every_dishonest_answer() {
        let v = check_sync(&DishonestEngine);
        let joined = v.join("\n");
        for expected in [
            "max_context_tokens is 0",
            "model_id_for",
            "embed_model_id()",
            "count_tokens()",
            "effective_context_size() returned Some(0)",
        ] {
            assert!(
                joined.contains(expected),
                "the suite missed `{expected}`. Found:\n{joined}"
            );
        }
    }

    #[tokio::test]
    async fn the_serving_contract_catches_a_missing_terminal_frame() {
        let v = check_serving(&DishonestEngine).await;
        let joined = v.join("\n");
        assert!(
            joined.contains("terminal Finish/Error frame"),
            "a stream that yields tokens and then simply stops is the silent-truncation \
             bug this check exists for. Found:\n{joined}"
        );
        assert!(
            joined.contains("embed_batch()"),
            "a short embed batch mis-assigns every later embedding. Found:\n{joined}"
        );
        assert!(
            joined.contains("rerank_batch()"),
            "a fabricated rerank score must be caught. Found:\n{joined}"
        );
    }
}
