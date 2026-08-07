// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one adapter from Sovereign's `InferenceProvider` to corpus-engine's
//! `EmbedFn`.
//!
//! # Why it lives here
//!
//! It lived in `sovereign_tools::corpus` and had seven callers (desktop,
//! server, cli-llm ×4, cli-dev). When `svrn code index` moved into the shipped
//! `sovereign-cli` (2026-08-06) it needed the same ten lines — and taking
//! `sovereign-tools` for them would have pulled LanceDB, Arrow, Parquet,
//! pdfium and the OCR stack into the end-user binary.
//!
//! Copying it would have made two implementations of one adapter (ARCH §10.6).
//! `sovereign-core` already carries both halves — `corpus-engine` for `EmbedFn`
//! and `sovereign-contracts` for `InferenceProvider` — and every one of those
//! callers already depends on it, so this is the home that costs nobody a new
//! dependency. `sovereign_tools::corpus::inference_to_embed_fn` re-exports it,
//! which is why none of the seven call sites had to change.

use std::sync::Arc;

use crate::traits::InferenceProvider;

/// Create a corpus-engine `EmbedFn` from Sovereign's `InferenceProvider`.
///
/// The error mapping is the load-bearing part: corpus-engine only understands
/// `corpus_engine::Error`, so a provider failure has to arrive as
/// `Error::Embed` or ingestion reports it as something it is not.
pub fn inference_to_embed_fn(inference: Arc<dyn InferenceProvider>) -> corpus_engine::EmbedFn {
    Arc::new(move |text: &str| {
        let inf = Arc::clone(&inference);
        let text = text.to_string();
        Box::pin(async move {
            inf.embed(&text)
                .await
                .map_err(|e| corpus_engine::Error::Embed(e.to_string()))
        })
    })
}
