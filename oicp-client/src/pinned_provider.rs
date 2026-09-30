// SPDX-License-Identifier: AGPL-3.0-or-later
//! A daemon-backed provider pinned to a named chat model. Moved from
//! sovereign-cli-llm's `probe_cmd::attached` (phase-b pb-cli-llm-bench-move)
//! so bench's judge pin and svrn's probe build it the same way.

use std::sync::Arc;

use sovereign_contracts::traits::InferenceProvider;

use crate::SplitInferenceProvider;

/// Build a daemon-backed provider pinned to an explicit chat model id.
/// Mirrors `chat_cmd::bootstrap`'s provider construction (manifest-aware
/// context window when the daemon serves OICP capabilities, 8192
/// fallback otherwise) — the difference is the caller names the chat
/// model instead of resolving the daemon's default. Used by
/// `--enrich-model` / `--judge-model` to split roles across models.
pub async fn provider_for_model(
    base: &str,
    chat_model: &str,
    embed_model: &str,
) -> Arc<dyn InferenceProvider> {
    let v1 = format!("{base}/v1");
    match crate::fetch_manifest(base, None).await {
        Some(manifest) => Arc::new(SplitInferenceProvider::from_manifest(
            &v1,
            &manifest,
            chat_model.to_string(),
            embed_model.to_string(),
        )),
        None => Arc::new(SplitInferenceProvider::new(
            &v1,
            chat_model.to_string(),
            embed_model.to_string(),
            8192,
            sovereign_contracts::models_manifest::DEFAULT_MANIFEST
                .embed_query_instruction(embed_model),
        )),
    }
}
