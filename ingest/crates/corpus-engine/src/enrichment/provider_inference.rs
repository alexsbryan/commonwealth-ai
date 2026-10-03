// SPDX-License-Identifier: AGPL-3.0-or-later
//! Enrichment's inference, dialled through the serving contract's
//! `InferenceProvider` (pb-ingest-dial-tools-local: moved from
//! sovereign-tools' corpus module, which re-exports it at its old path, so
//! the local-corpus port can take a provider rather than an `InferenceFn`).

use std::sync::Arc;

use sovereign_contracts::traits::InferenceProvider;

/// Create a corpus-engine `InferenceFn` from Sovereign's `InferenceProvider`.
/// Used by the optional enrichment pipeline to run claim and relationship
/// extraction prompts.
///
/// Uses `Speed::Fast` (the smaller always-loaded model) with thinking disabled
/// and a capped token budget. Structured JSON extraction doesn't benefit from
/// the 27B primary model or from chain-of-thought reasoning, and running the
/// primary model at ~1 min/chunk makes enrichment impractical on large corpora.
pub fn inference_to_inference_fn(inference: Arc<dyn InferenceProvider>) -> crate::InferenceFn {
    use crate::enrichment::pipeline::ChatPrompt;
    use sovereign_contracts::slot_policy::Workload;

    Arc::new(move |prompt: &ChatPrompt, max_tokens: Option<u32>| {
        let inf = Arc::clone(&inference);
        // Schema (when the caller attaches one) gates llama-cpp's
        // structured-output sampler so the response is forced into a
        // grammar matching the JSON Schema. Phase 1b on business_email
        // sets this to drop the ~54% JSON-parse-failure tail observed
        // on enron-sample-multi-wide; other phases leave it `None` and
        // get the legacy free-form path. Owned clone is required since
        // the future moves the request.
        let structured_output = prompt.response_schema.clone();
        // The caller's single-message prompt rides in `user` (system is
        // empty on this path); the per-call override, when the retry
        // paths pass one, wins over the prompt's own budget.
        let output_budget = max_tokens.or(prompt.max_output_tokens).unwrap_or(4096);
        // SLOT_POLICY §3 EnrichBulk: high-volume corpus claim/relationship
        // extraction where fast-class throughput is existential (the primary
        // model at ~1 min/chunk makes enrichment impractical on large
        // corpora) and quality is bench-validated per recipe.
        let mut request = Workload::EnrichBulk
            .request(prompt.user.clone())
            // POLICY-DEBT(SLOT_POLICY §4.5 EnrichBulk): 4096 > 512 forfeits
            // the batched FastShort claim; kept — dropped 2026-05-29
            // (evening): once grammar-constrained decoding lands via
            // `structured_output`, the cap stops being load-bearing for JSON
            // validity — the schema guarantees valid array close at any token
            // count. Bigger cap (8192) just gives a rambling model more rope:
            // observed 286s batches generating 10358 tokens after grammar lit
            // up, dragging mean latency to 110s/batch. 4096 caps wall clock at
            // ~80s/batch worst case while still fitting most observed valid
            // bodies; over-cap batches end with a smaller-but-valid entity
            // list (acceptable recall hit vs the throughput win).
            .with_output_budget(output_budget);
        request.temperature = prompt.temperature.or(Some(0.1)); // low temperature for consistent JSON output
        request.structured_output = structured_output;
        // POLICY-DEBT(SLOT_POLICY §3 EnrichBulk): Some(0) preserved for P1
        // neutrality (bundle is None); P5 confirms.
        request.think_budget = prompt.thinking_tokens.map(|t| t as usize).or(Some(0)); // suppress thinking — hurts JSON, wastes tokens
        Box::pin(async move {
            let resp = inf
                .complete(&request)
                .await
                .map_err(|e| corpus_index::Error::Embed(format!("inference: {e}")))?;
            Ok(resp.text)
        })
    })
}
