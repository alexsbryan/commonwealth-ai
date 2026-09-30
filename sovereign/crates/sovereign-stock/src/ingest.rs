// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest, composed for svrn: the one composition both of this distribution's
//! binaries hand svrn (the daemon's `process::run` and cli-llm's
//! `bin_main_with`, pb-cli-llm-ingest-move-compose), so the stock install
//! builds ingest's engine one way.

/// Ingest's enrichment-config port from ingest's catalog
/// (pb-ingest-dial-tools-close), its atlas port, its engine-free calls
/// (pb-cli-llm-ingest-move-remainder), and the engine built by ingest's face
/// for what svrn hands it (pb-ingest-dial-daemon).
pub fn hosted() -> sovereign_daemon::process::HostedIngest {
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
