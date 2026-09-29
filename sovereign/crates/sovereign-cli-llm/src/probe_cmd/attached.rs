// SPDX-License-Identifier: AGPL-3.0-or-later
//! The attached-document probe: svrn builds (or reuses) a document asset,
//! meters the build, and answers each question through a minted
//! `DocumentSession` turn (pb-bench-dials-docs).

use std::sync::Arc;

use sovereign_core::traits::InferenceProvider;

use crate::chat_cmd::bootstrap::SplitInferenceProvider;

/// Build a daemon-backed provider pinned to an explicit chat model id.
/// Mirrors `chat_cmd::bootstrap`'s provider construction (manifest-aware
/// context window when the daemon serves OICP capabilities, 8192
/// fallback otherwise) — the difference is the caller names the chat
/// model instead of resolving the daemon's default. Used by
/// `--enrich-model` / `--judge-model` to split roles across models.
pub(crate) async fn provider_for_model(
    base: &str,
    chat_model: &str,
    embed_model: &str,
) -> Arc<dyn InferenceProvider> {
    let v1 = format!("{base}/v1");
    match oicp_client::fetch_manifest(base, None).await {
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
            sovereign_core::models_manifest::DEFAULT_MANIFEST.embed_query_instruction(embed_model),
        )),
    }
}
