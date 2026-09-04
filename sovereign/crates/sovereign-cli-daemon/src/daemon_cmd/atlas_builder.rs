// SPDX-License-Identifier: AGPL-3.0-or-later
//! The daemon's in-process atlas build.
//!
//! `sovereign-tools` declares the seam (`watched::enrich::AtlasBuildRunner`)
//! and cannot depend upward on an implementation of it — hosts are terminal
//! (ARCH_LAYERS). So the host that links the atlas orchestrator installs one,
//! and this is it: `enrich build <corpus> --full` as a library call with the
//! daemon's own inference provider as the Backfill step's embedder, so the
//! build never opens an HTTP session back to the process running it.
//!
//! Carved out of `bootstrap.rs` 2026-09-01. It sits alone because it is the
//! daemon's single point of contact with the orchestrator crate: when that
//! dependency moves, exactly one `use` in this file moves with it.

use std::sync::Arc;

use sovereign_core::traits::InferenceProvider;

/// The daemon's in-process atlas build: `enrich build <corpus> --full` as a
/// library call with the daemon's own inference provider as the Backfill
/// step's embedder (so the build never opens an HTTP session to itself).
/// Implements the seam `sovereign-tools` declares
/// (`watched::enrich::AtlasBuildRunner`) from the one host that links the
/// orchestrator — the tools crate cannot depend upward on it (ARCH_LAYERS:
/// hosts are terminal).
struct InProcessAtlasBuilder {
    provider: Arc<dyn InferenceProvider>,
}

impl sovereign_tools::local_corpus::watched::enrich::AtlasBuildRunner for InProcessAtlasBuilder {
    fn build(
        &self,
        corpus_id: String,
        progress: sovereign_tools::enrich::EnrichProgressFn,
        cancel: sovereign_tools::enrich::CancellationFlag,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = i32> + Send + 'static>> {
        let provider = Arc::clone(&self.provider);
        Box::pin(async move {
            // `from_inputs` rejects only unknown skip ids; with none given it
            // cannot fail, but a wiring error is reported, never swallowed.
            let parsed = match sovereign_enrichment_build::ParsedBuild::from_inputs(
                corpus_id.clone(),
                None,
                &[],
                false,
            ) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(corpus = %corpus_id, error = %e, "in-process atlas build: bad inputs");
                    return 2;
                }
            };
            sovereign_enrichment_build::build_with_progress_with_embedder(
                &parsed,
                Some(progress),
                // The orchestrator takes an `EmbedFn` since ei-5a-build-cut.
                // The daemon still hands it ITS OWN provider — no HTTP session
                // to itself — through the QUERY-side adapter, so the table it
                // seeds shares one vector space with `atlas_navigate_ann`.
                Some(sovereign_core::embed_fn::inference_to_embed_query_fn(
                    provider,
                )),
                Some(cancel),
            )
            .await
        })
    }
}

pub(super) fn in_process_atlas_builder(
    provider: Arc<dyn InferenceProvider>,
) -> Arc<dyn sovereign_tools::local_corpus::watched::enrich::AtlasBuildRunner> {
    Arc::new(InProcessAtlasBuilder { provider })
}
