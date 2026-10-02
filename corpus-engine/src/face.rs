// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ingest's face for a distribution that hosts svrn (pb-ingest-dial-daemon,
//! FIVE_PROGRAMS §2c): the one engine a svrn daemon's process holds, built
//! here from the host's inputs and handed back as ingest's ports. svrn links
//! no corpus-engine; the stock binary calls [`compose`] inside the
//! composition svrn hands it (`process::HostedIngest`) and maps the face onto
//! svrn's mount.
//!
//! Moved whole from the daemon's `bootstrap::build_corpus_engine` and
//! `build_folder_tiered_deps`, so there is still one assembly of this
//! engine, not two.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use corpus_engine_atlas_reader::ports::AtlasPort;
use corpus_index::ingest_port::daemon::IngestPort;
use corpus_index::ingest_port::tiered::{
    ChunkEntityExtractorHandle, TieredEnrichmentProvider, TieredProviderHandle,
};
use corpus_index::ingest_port::FolderTieredPort;
use corpus_index::source::IndexSource;
use corpus_index::types::{BatchEmbedFn, EmbedFn};
use sovereign_contracts::daemon_wire::conv_tiered::ChunkEntityStore;
use sovereign_contracts::ner::LabeledEntityExtractor;
use sovereign_contracts::recipe::testing::RecipeAuthorSeams;
use sovereign_contracts::traits::InferenceProvider;

use crate::CorpusEngine;

/// A background chore the host spawns under its own supervision.
pub type Chore = Box<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// What the host hands ingest to build its engine.
pub struct IngestParts {
    /// The data root: the engine's `indexes/` and `recipes/` live under it.
    pub data_dir: PathBuf,
    /// The host's inference: the embed slot, the batch embed and the
    /// enrichment's chat all go through it.
    pub provider: Arc<dyn InferenceProvider>,
    /// The embed model's id, recorded in `_corpus_meta.json`.
    pub embed_model: String,
    /// This node's id, stamped on the partitions it ingests.
    pub node_id: String,
    /// The conv-tiered chunk-entity table the NER adapter writes.
    pub chunk_entity_store: Arc<dyn ChunkEntityStore>,
    /// The served NER kind's handle; `None` leaves tiered ingest on
    /// RAPTOR-derived entities.
    pub ner: Option<Arc<dyn LabeledEntityExtractor>>,
    /// The conv-tiered enrichment provider the engine's tiered runner uses.
    pub conv_tiered: Option<Arc<dyn TieredEnrichmentProvider>>,
    /// The watched-folder driver's own provider over the same store.
    pub folder_tiered: Option<Arc<dyn TieredEnrichmentProvider>>,
}

/// The engine, as the host acts on it.
pub struct IngestFace {
    /// The engine itself, for the few faces other ingest crates build over
    /// it (the authoring harness).
    pub engine: Arc<CorpusEngine>,
    /// Ingest's port.
    pub port: Arc<dyn IngestPort>,
    /// The engine's own cached reader, for a host that reads through it.
    pub index: Arc<dyn IndexSource>,
    /// The watched-folder driver's tiered build; `None` when there is no
    /// folder provider.
    pub folder_tiered: Option<Arc<dyn FolderTieredPort>>,
    /// The recipe-author tools' tester and descriptor.
    pub recipe_author: RecipeAuthorSeams,
    /// Arm the engine's geometry gate (clause ST-8) with the width the
    /// host's embed probe measured.
    pub arm_geometry: Box<dyn Fn(usize) + Send + Sync>,
    /// Stamp legacy canonicals' fingerprints; idempotent, so the host may
    /// restart it.
    pub lazy_stamp: Chore,
}

/// Ingest's atlas port.
pub fn atlas() -> Arc<dyn AtlasPort> {
    Arc::new(crate::IngestAtlas)
}

/// Ingest's recipe-authoring seams, which need no engine.
pub fn recipe_author() -> RecipeAuthorSeams {
    crate::recipe_tester::recipe_author_seams()
}

/// Ingest's per-chunk adapter over a served NER kind, writing `store`: the
/// one [`compose`] wires, for a host that wraps its own (the vault build's
/// meter).
pub fn gliner_chunk_extractor(
    store: Arc<dyn ChunkEntityStore>,
    ner: Arc<dyn LabeledEntityExtractor>,
) -> ChunkEntityExtractorHandle {
    crate::enrichment::chunk_ner::GlinerChunkExtractor::new(store, ner).into_handle()
}

/// Folder tiered enrichment of `corpus_id` at `index_path` through the
/// host's provider and entity extractor; the documents enriched.
pub async fn run_folder_tiered(
    corpus_id: &str,
    index_path: &Path,
    provider: Option<TieredProviderHandle>,
    extractor: Option<ChunkEntityExtractorHandle>,
) -> crate::error::Result<usize> {
    crate::enrichment::tiered::run_folder_tiered_enrichment(
        corpus_id,
        index_path,
        provider.as_ref(),
        extractor.as_ref(),
    )
    .await
    .map(|plan| plan.total_conversations)
}

/// Build the single shared engine (it serves `/mcp` tools AND
/// `corpus_collaborate` ingest). Wires a REAL embed slot through
/// `provider` (a zero-vector stub here once poisoned 4M chunks), the batch
/// variant, the conv-tiered provider, and the shared GLiNER chunk
/// extractor.
pub fn compose(parts: IngestParts) -> IngestFace {
    let IngestParts {
        data_dir,
        provider,
        embed_model,
        node_id,
        chunk_entity_store,
        ner,
        conv_tiered,
        folder_tiered,
    } = parts;
    // The per-chunk adapter over the served NER kind, shared by the engine's
    // tiered runner and the folder driver (one model).
    let chunk_entity_extractor = ner.map(|extractor| {
        crate::enrichment::chunk_ner::GlinerChunkExtractor::new(chunk_entity_store, extractor)
            .into_handle()
    });

    let indexes_dir = data_dir.join("indexes");
    let provider_for_embed = Arc::clone(&provider);
    let embed: EmbedFn = Arc::new(move |text: &str| {
        let p = Arc::clone(&provider_for_embed);
        let text = text.to_string();
        Box::pin(async move {
            p.embed(&text)
                .await
                .map_err(|e| corpus_index::Error::Embed(e.to_string()))
        })
    });
    let provider_for_batch = Arc::clone(&provider);
    let batch_embed: BatchEmbedFn = Arc::new(move |texts: &[String]| {
        let p = Arc::clone(&provider_for_batch);
        let texts = texts.to_vec();
        Box::pin(async move {
            p.embed_batch(&texts)
                .await
                .map_err(|e| corpus_index::Error::Embed(e.to_string()))
        })
    });
    // recipes_dir doubles as the registry's overrides_dir. Locally-published
    // recipes from `svrn recipe publish` land at
    // `~/.svrnmesh/recipes/<id>/recipe.toml` and only resolve when the
    // engine's overrides_dir points there.
    let recipes_dir = data_dir.join("recipes");
    // Recipe enrichment (`[enrichment] enabled = true, type = "atlas"`)
    // requires an InferenceFn — without one, `engine.ingest` logs "no
    // InferenceFn was provided to CorpusEngine — skipping" and silently
    // degrades to chunks-only ingest. Same provider as embed + batch_embed.
    let inference_fn =
        crate::enrichment::provider_inference::inference_to_inference_fn(Arc::clone(&provider));

    let mut engine = CorpusEngine::new(recipes_dir, indexes_dir, embed)
        .with_embedding_model(&embed_model)
        .with_batch_embed_fn(batch_embed)
        .with_inference_fn(inference_fn)
        .with_self_node_id(node_id);
    // Conv-tiered enrichment provider — spec
    // `sovereign/docs/specs/CONV_TIERED_PORT.md`. Absent, the tiered runner
    // falls back to dispatch-plan-only mode.
    if let Some(provider) = conv_tiered {
        engine = engine.with_tiered_provider(provider);
    }
    if let Some(extractor) = chunk_entity_extractor.clone() {
        engine = engine.with_chunk_entity_extractor(extractor);
    }
    let engine = Arc::new(engine);
    tracing::info!(
        target: "corpus_engine::face",
        embed_model = %embed_model,
        folder_tiered = folder_tiered.is_some(),
        "ingest face: engine composed for the host"
    );

    let arm = Arc::clone(&engine);
    let stamp = Arc::clone(&engine);
    IngestFace {
        port: Arc::clone(&engine) as Arc<dyn IngestPort>,
        index: Arc::clone(&engine) as Arc<dyn IndexSource>,
        folder_tiered: folder_tiered.map(|provider| {
            Arc::new(crate::FolderTiered::new(provider, chunk_entity_extractor))
                as Arc<dyn FolderTieredPort>
        }),
        recipe_author: crate::recipe_tester::recipe_author_seams(),
        arm_geometry: Box::new(move |dims| arm.set_expected_embedding_dimensions(dims)),
        // Lazy-stamp canonical fingerprints for installed canonicals that
        // don't yet carry one (legacy ingests pre-dating the canonical-sync
        // surface). One BLAKE3 over the content_hash list per corpus;
        // idempotent. See `CorpusEngine::lazy_stamp_legacy_fingerprints`.
        lazy_stamp: Box::new(move || {
            let engine = Arc::clone(&stamp);
            Box::pin(async move { engine.lazy_stamp_legacy_fingerprints().await })
        }),
        engine,
    }
}
