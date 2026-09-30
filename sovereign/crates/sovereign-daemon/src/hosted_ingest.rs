// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ingest program's faces, composed into this process by a distribution
//! (pb-ingest-dial-tools-close, pb-ingest-dial-daemon; FIVE_PROGRAMS §2c).
//! svrn names no type of ingest's and links no corpus-engine: the
//! distribution builds each port's implementor and hands it here — the
//! enrichment-config port (its implementor is ingest's catalog), the atlas
//! port, and the engine itself, built for [`IngestHost`] and mounted as
//! [`IngestMount`]. svrn alone (no [`HostedIngest`]) reports ingest absent by
//! name where it needs one.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use corpus_engine_atlas_reader::ports::AtlasPort;
use corpus_index::ingest_port::daemon::{IngestPort, RecipeHarnessPort};
use corpus_index::ingest_port::enrich_config::EnrichConfigPort;
use corpus_index::ingest_port::tiered::TieredEnrichmentProvider;
use corpus_index::ingest_port::FolderTieredPort;
use corpus_index::source::IndexSource;

/// What svrn hands ingest to build the engine this process holds.
pub struct IngestHost {
    /// svrn's data root; the engine's `indexes/` and `recipes/` live under it.
    pub data_dir: PathBuf,
    /// svrn's inference: the engine embeds and enriches through it.
    pub provider: Arc<dyn sovereign_contracts::traits::InferenceProvider>,
    /// The embed model's id, derived once by svrn from its config.
    pub embed_model: String,
    /// This node's id.
    pub node_id: String,
    /// svrn's `sovereign.db`, as the chunk-entity table the NER adapter writes.
    pub chunk_entity_store:
        Arc<dyn sovereign_contracts::daemon_wire::conv_tiered::ChunkEntityStore>,
    /// The served NER kind's handle; `None` when none loaded.
    pub ner: Option<Arc<dyn sovereign_contracts::ner::LabeledEntityExtractor>>,
    /// svrn's conv-tiered enrichment provider, for the engine's tiered runner.
    pub conv_tiered: Option<Arc<dyn TieredEnrichmentProvider>>,
    /// svrn's watched-folder provider, for the folder driver's tiered build.
    pub folder_tiered: Option<Arc<dyn TieredEnrichmentProvider>>,
}

/// A host-supervised background chore of ingest's.
pub type IngestChore = Box<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Ingest, composed, as svrn mounts it.
pub struct IngestMount {
    /// Ingest's port: every executing site and read acts through it.
    pub port: Arc<dyn IngestPort>,
    /// The engine's own cached reader; code's chunk index reads through it.
    pub index: Arc<dyn IndexSource>,
    /// The recipe authoring harness over the engine.
    pub harness: Arc<dyn RecipeHarnessPort>,
    /// The recipe-author tools' tester and descriptor.
    pub recipe_author: sovereign_contracts::recipe::testing::RecipeAuthorSeams,
    /// The watched-folder driver's tiered build.
    pub folder_tiered: Option<Arc<dyn FolderTieredPort>>,
    /// Arm the engine's geometry gate with the width svrn's embed probe
    /// measured (clause ST-8).
    pub arm_geometry: Box<dyn Fn(usize) + Send + Sync>,
    /// Stamp legacy canonicals' fingerprints; svrn supervises it.
    pub lazy_stamp: IngestChore,
}

type Compose = Box<dyn Fn(IngestHost) -> IngestMount + Send + Sync>;

/// The distribution's composition of ingest.
pub struct HostedIngest {
    enrich_config: Arc<dyn EnrichConfigPort>,
    atlas: Arc<dyn AtlasPort>,
    compose: Compose,
}

impl HostedIngest {
    /// `enrich_config` reads and writes corpora's enrichment configs;
    /// `atlas` is ingest's atlas port; `compose` builds the engine for
    /// `IngestHost`, once per call: the CLI composes one engine per session
    /// and a metered one per vault build (pb-cli-llm-ingest-move-compose).
    pub fn new(
        enrich_config: Arc<dyn EnrichConfigPort>,
        atlas: Arc<dyn AtlasPort>,
        compose: impl Fn(IngestHost) -> IngestMount + Send + Sync + 'static,
    ) -> Self {
        Self {
            enrich_config,
            atlas,
            compose: Box::new(compose),
        }
    }

    /// The enrichment-config port.
    pub fn enrich_config(&self) -> Arc<dyn EnrichConfigPort> {
        Arc::clone(&self.enrich_config)
    }

    /// The atlas port. svrn's tiered providers take it before the engine
    /// is built.
    pub fn atlas(&self) -> Arc<dyn AtlasPort> {
        Arc::clone(&self.atlas)
    }

    /// Build the engine.
    pub fn compose(&self, host: IngestHost) -> IngestMount {
        (self.compose)(host)
    }
}

/// Why an ingest route refuses on svrn alone. The prefix is the one these
/// routes answered with before ingest left the daemon, so a caller matching
/// it still does; the rest names the program (pb-ingest-dial-daemon).
pub const NO_INGEST: &str = "no corpus engine available on this node: no ingest program is \
                             composed in this process (the stock binary, `svrn daemon`, composes it)";

/// A route ingest serves, answered on svrn alone: 503 naming the program.
async fn absent(uri: axum::http::Uri) -> axum::response::Response {
    use axum::response::IntoResponse;
    tracing::debug!(path = %uri.path(), "ingest routes: no ingest program in this process");
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(serde_json::json!({
            "error": format!(
                "{} reads ingest's corpora, and no ingest program is composed in this \
                 process: run the stock binary (`svrn daemon`)",
                uri.path()
            ),
        })),
    )
        .into_response()
}

/// `/v1/knowledge/landscape_digest` on svrn alone.
pub fn landscape_digest_absent_router() -> axum::Router {
    axum::Router::new().route("/v1/knowledge/landscape_digest", axum::routing::any(absent))
}
