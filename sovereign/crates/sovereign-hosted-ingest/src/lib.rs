// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest, composed for svrn: the one composition both distributions hand
//! svrn (phase-b-88). The stock distribution's two binaries hand it to the
//! daemon's `process::run` and to cli-llm's `bin_main_with`
//! (pb-cli-llm-ingest-move-compose); the on-prem distribution hands it to
//! `process::run` with no recipe authoring (phase-b-87). So the stock install
//! and the on-prem one build ingest's engine one way (principle 8).
//!
//! A crate in no package and no leaf, listed in BOTH the `stock` and `onprem`
//! `[[distribution]]` rows of quality/ARCH_LAYERS.toml, so it answers to each
//! row's faces: this file names no sovereign-recipe-author, and on-prem's row
//! is what holds it to that (recipe authoring is the caller's, handed in as
//! `process::RecipeAuthoringCompose`).

/// Ingest's enrichment-config port from ingest's catalog
/// (pb-ingest-dial-tools-close), its atlas port, its engine-free calls
/// (pb-cli-llm-ingest-move-remainder), and the engine built by ingest's face
/// for what svrn hands it (pb-ingest-dial-daemon). `recipe_authoring` is the
/// distribution's: `None` composes ingest without it.
pub fn hosted(
    recipe_authoring: Option<sovereign_daemon::process::RecipeAuthoringCompose>,
) -> sovereign_daemon::process::HostedIngest {
    sovereign_daemon::process::HostedIngest::new(
        std::sync::Arc::new(sovereign_enrichment_catalog::port::CatalogEnrichConfig),
        corpus_engine::face::atlas(),
        corpus_engine::face::recipe_author(),
        sovereign_daemon::process::IngestCalls {
            daemon_chat: Box::new(|base_url, chat_model, embed_model, max_output_tokens| {
                sovereign_enrichment_build::inference_client::DaemonInferenceClient::new(
                    base_url,
                    chat_model,
                    embed_model,
                )
                .map(|c| {
                    c.with_max_output_tokens(max_output_tokens)
                        .into_closures()
                        .1
                })
                .map_err(|e| format!("build daemon client: {e}"))
            }),
            run_folder_tiered: Box::new(|corpus_id, index_path, provider, extractor| {
                Box::pin(async move {
                    corpus_engine::face::run_folder_tiered(
                        &corpus_id,
                        &index_path,
                        provider,
                        extractor,
                    )
                    .await
                    .map_err(|e| e.to_string())
                })
            }),
            gliner_chunk_extractor: Box::new(corpus_engine::face::gliner_chunk_extractor),
            recipe_authoring,
        },
        |host| {
            let face = corpus_engine::face::compose(corpus_engine::face::IngestParts {
                data_dir: host.data_dir,
                provider: host.provider,
                embed_model: host.embed_model,
                node_id: host.node_id,
                chunk_entity_store: host.chunk_entity_store,
                ner: host.ner,
                conv_tiered: host.conv_tiered,
                folder_tiered: host.folder_tiered,
            });
            sovereign_daemon::process::IngestMount {
                // The authoring harness is ingest's too, over the same engine.
                harness: std::sync::Arc::new(sovereign_authoring_harness::EngineHarness::new(
                    std::sync::Arc::clone(&face.engine),
                )),
                port: face.port,
                index: face.index,
                recipe_author: face.recipe_author,
                folder_tiered: face.folder_tiered,
                arm_geometry: face.arm_geometry,
                lazy_stamp: face.lazy_stamp,
            }
        },
    )
}
