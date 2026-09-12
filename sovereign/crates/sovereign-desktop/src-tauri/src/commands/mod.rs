// SPDX-License-Identifier: AGPL-3.0-or-later
// Façade-level imports: only what the retained Response Types / helpers /
// test module use. Each concern submodule carries its own import block and
// is held to the same standard — the blanket `#![allow(unused_imports)]`
// the auto-split left on every submodule is gone, so rustc names a dead
// `use` the moment a body that needed it is deleted.

use serde::{Deserialize, Serialize};

// ─── Response Types ──────────────────────────────────────────

#[derive(Serialize)]
pub struct MessageResponse {
    pub message_id: String,
    pub role: String,
    pub content: String,
    pub task: Option<TaskSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Serialize)]
pub struct TaskSummary {
    pub id: String,
    pub status: String,
    pub steps_completed: usize,
}

/// sv-surface rung 3 (the rung-4 reading pattern): the conversation LIST
/// entry IS the daemon's wire type — one schema, served over HTTP in attach
/// mode and serialized in-process here. The hand-kept local shape this
/// replaced had already drifted from the wire's `skip_serializing_if` (it
/// emitted `"title": null` where the wire omits the key), which is the
/// exact byte-compat break importing the one type makes impossible.
pub use sovereign_contracts::daemon_wire::ConversationListEntry as ConversationEntry;

/// The CREATE response is likewise the wire type; the `enabled_corpora`
/// echo is `None` on the desktop's own create (it seeds no allow-list) and
/// therefore omitted from the serialized bytes.
pub use sovereign_contracts::daemon_wire::CreateConversationResponse;

// SANCTIONED UNTIL RUNG 6, named so the next reader doesn't "fix" them: the
// two shapes below are the desktop's IN-PROCESS IPC contract, deliberately
// richer than the wire's — the frontend renders `metadata` raw and reads
// `enabled_corpora` here. The wire's `ConversationResponse`/`MessageEntry`
// (sovereign_mesh::turn_http) project metadata into
// provenance/citations/epistemic_state instead. When rung 6 converts the
// desktop to a pure client, the frontend's renderer moves onto the
// projections and these locals die — until then they are the frontend's
// contract, not a twin of the wire.
#[derive(Serialize)]
pub struct ConversationDetail {
    pub id: String,
    pub title: Option<String>,
    pub messages: Vec<MessageEntry>,
    pub created_at: i64,
    pub updated_at: i64,
    /// User-controlled corpus allow-list. `None` = "all installed
    /// corpora" (default); `Some(vec)` = explicit subset. See
    /// `sovereign_contracts::types::Conversation::enabled_corpora`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled_corpora: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct MessageEntry {
    pub id: String,
    pub role: String,
    pub content: String,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// NOT `sovereign_tools_base::web::search::SearchResult` (a WEB result);
/// this is a conversation-search hit.
#[derive(Serialize)]
pub struct SearchResult {
    pub content: String,
    pub conversation_id: String,
}

/// One skill row as the SERVING runtime reports it.
///
/// `Deserialize` as well as `Serialize` since sv-surface D9: this is
/// the type `TurnClient::list_skills` parses the daemon's
/// `/v1/skills` bytes into, so the surface names ONE skill row rather
/// than a wire mirror plus a frontend struct (§10.6). The field names
/// are the route's field names; changing one here without changing
/// `sovereign_mesh::turn_extras_http::SkillWireEntry` breaks the parse
/// loudly at the call site.
#[derive(Serialize, Deserialize)]
pub struct SkillEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub active: bool,
    pub trust_level: String,
}

/// NOT `sovereign_contracts::setup_config::SetupConfig`, which is the
/// `config.toml` structure; this is the setup WIZARD's inbound payload from
/// the frontend and is Deserialize-only.
#[derive(Deserialize)]
pub struct SetupConfig {
    pub model_path: String,
    #[serde(default)]
    pub primary_model_path: Option<String>,
    #[serde(default)]
    pub embed_model_path: Option<String>,
    #[serde(default)]
    pub data_dir: Option<String>,
    #[serde(default)]
    pub active_skills: Vec<String>,
    #[serde(default)]
    pub enabled_tools: Vec<String>,
    #[serde(default)]
    pub search_provider: Option<String>,
    #[serde(default)]
    pub search_api_key: Option<String>,
    #[serde(default)]
    pub selected_tier: Option<String>,
    /// M3 — opt-in for the Recipe Author workspace. `None` from a
    /// wizard step that doesn't surface the toggle preserves the
    /// existing `DesktopConfig.enable_recipe_authoring` value rather
    /// than silently defaulting to `false`.
    #[serde(default)]
    pub enable_recipe_authoring: Option<bool>,
    /// Tier 3 of tool-framework expansion — opt-in for the
    /// `knowledge_lookup` tool's automatic web-escalation path.
    /// Same `None`-preserves-existing semantics as
    /// `enable_recipe_authoring`. See the field of the same name
    /// on `state::DesktopConfig` for behaviour details.
    #[serde(default)]
    pub auto_escalate_to_web: Option<bool>,
}

/// NOT `sovereign_core::deep_research::icd::CorpusEntry` (an estate row:
/// `{corpus_id, kind, chunks_count, searchable, custody}`); this is the
/// catalog-browser card the UI renders.
// sv-surface D9b: `Deserialize` too, so this ONE type is both what the
// frontend receives and how the catalogue row the daemon serves (`GET /internal/corpus/catalog`) is read.
// Deserializing into a private mirror would have minted the twin the
// campaign exists to retire (ARCH §10.6).
#[derive(Serialize, Deserialize)]
pub struct CorpusEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub size_compressed_gb: f64,
    pub size_indexed_gb: f64,
    pub license: String,
    pub tiers: Vec<String>,
    /// "installed", "installing", or "not_installed".
    pub status: String,
    /// Chunk count when installed; null otherwise.
    pub chunks_count: Option<u64>,
    /// True when the recipe enables the epistemic enrichment phase.
    pub enrichment_enabled: bool,
    /// Unix timestamp (seconds) when the index was created. Null unless installed.
    pub indexed_at: Option<u64>,
    /// Embedding model name used when indexing. Null unless installed.
    pub embedding_model: Option<String>,
    /// Embedding vector dimensions. Null unless installed.
    pub embedding_dimensions: Option<usize>,
    /// True when the IVF-PQ vector index is built and semantic search is available.
    /// False means FTS-only search is used (fast but keyword-only).
    pub vector_index_ready: bool,
    /// True when this corpus is installed but its index never finished
    /// building (e.g. an ingest or sync that paused) — it returns ~nothing at
    /// query time, so the runtime skips it and prompts a rebuild. The UI
    /// should badge such a corpus "needs rebuild" rather than present it as
    /// healthy. Mirrors the retrieval readiness gate (`indexes_built`).
    pub needs_rebuild: bool,
    /// URL of the recipe TOML in the public registry. Null for user-added corpora.
    pub registry_url: Option<String>,
    /// Recipe schema version (1 = initial). Used for compatibility checks.
    pub schema_version: Option<u32>,
    /// Parent corpus id when this entry is a layer/satellite (e.g.
    /// `wikipedia-simple` and `wikipedia-newsworthy` carry
    /// `parent_corpus_id = "wikipedia"`). The desktop hides children
    /// from the top-level picker and surfaces them as toggles under
    /// the parent's row. `null` for top-level corpora.
    pub parent_corpus_id: Option<String>,
    /// Catalog presentation tier (`"featured"` / `"preview"` /
    /// `"hidden"`). Sourced from `registry_snapshot.toml`; lets the
    /// desktop curate the picker without growing a parallel allowlist.
    /// `None` defaults to `"preview"` so newly-registered recipes
    /// land under "Coming soon" until promoted by editing the snapshot.
    pub catalog_status: Option<String>,
}

/// One row on the Library shelf — the unified, deduped view of an
/// *installed* corpus the user can ask or explore (Phase 1 UX refactor).
///
/// This is the single source of truth that `notebook_list` assembles by
/// merging three existing surfaces:
///   - `installed_indexes()` — the deduped installed set (id, doc count,
///     freshness, parent),
///   - the `LocalCorpusManager` configs (folder / vault / watched
///     discrimination + the user's chosen display name + scope),
///   - the atlas readers (atoms.json + conv enrichment) — whether the
///     corpus has an explorable map.
///
/// It deliberately carries only the fields the shelf renders; the rich
/// per-surface DTOs (`CorpusEntry`, `LocalCorpusConfig`,
/// `AtlasCorpusSummary`) remain the source for their detail views.
// sv-surface D9b: `Deserialize` too, so this ONE type is both what the
// frontend receives and how the shelf row the daemon serves (`GET /internal/corpus/notebooks`) is read.
// Deserializing into a private mirror would have minted the twin the
// campaign exists to retire (ARCH §10.6).
#[derive(Serialize, Deserialize)]
pub struct NotebookSummary {
    /// Corpus id — the citation handle, structurally unique.
    pub id: String,
    /// Human-facing name. Prefers the user's local-corpus display name,
    /// then the catalog name, then the on-disk index name, then the id.
    pub name: String,
    /// Where this notebook came from, for the shelf icon + grouping:
    /// `"folder"` | `"obsidian"` | `"watched"` | `"catalog"` |
    /// `"installed"` (recipe / CLI / mesh-app / import).
    pub source_kind: String,
    /// Chunk count from the installed index.
    pub doc_count: u64,
    /// True when the corpus has an explorable map on disk — an
    /// `atoms.json` atlas or conv-tiered enrichment. Drives the ✦ badge
    /// and whether the detail view's Explore tab renders the map or the
    /// "Make explorable" CTA.
    pub explorable: bool,
    /// Index build time (Unix seconds) — the freshness signal.
    pub updated_unix: Option<u64>,
    /// `"local"` | `"mesh"` | `"public"`. Local corpora carry their
    /// configured scope; everything else defaults to `"local"`.
    pub scope: String,
    /// Count of open (unadjudicated) conflicts for a governance corpus —
    /// one carrying a `governance_oplog.jsonl`. `None` for an ordinary
    /// corpus (which is what gates the notebook's Conflicts tab off);
    /// `Some(0)` still shows the tab (exports + "all clear" state).
    pub open_conflicts: Option<u32>,
}

/// Detailed health report for a single installed corpus, loaded on demand
/// (avoids opening every LanceDB index on every `list_corpora` call).
// sv-surface D9b: `Deserialize` too, so this ONE type is both what the
// frontend receives and how the health row the daemon serves (`GET /internal/corpus/{corpus}/health`) is read.
// Deserializing into a private mirror would have minted the twin the
// campaign exists to retire (ARCH §10.6).
#[derive(Serialize, Deserialize)]
pub struct CorpusHealthDetail {
    pub corpus_id: String,
    /// Number of extracted claims (0 if no claims table).
    pub claims_count: u64,
    /// Number of stored relationships (0 if no relationships table).
    pub relationships_count: u64,
    /// True if an article_profiles table exists (structured Wikipedia only).
    pub has_article_profiles: bool,
    /// Number of chunks whose enrichment parse failed and can be retried
    /// without re-running inference (0 if no failures file exists).
    pub parse_failure_count: u64,
}

/// Progress payload sent to the frontend during a corpus install.
/// `phase` covers the entire pipeline including enrichment, so the
/// download bar can keep moving through claim and relationship
/// extraction rather than appearing to stall after "indexing".
#[derive(Serialize, Clone, Default)]
pub struct CorpusProgressPayload {
    pub corpus_id: String,
    /// One of: "downloading", "extracting", "chunking", "embedding",
    /// "indexing", "extracting_claims", "finding_relationships",
    /// "extracting_relationships", "complete", "failed".
    pub phase: String,
    pub percent: f32,
    pub chunks_processed: u64,
    /// Total chunks the current phase expects to process (0 when unknown).
    /// Paired with `chunks_per_sec`, this lets the frontend render a
    /// glassbox ETA — "how long is left" — for the dominant embed phase
    /// rather than only a percent. The backend already computes both inside
    /// `IngestProgress::Embedding`; they were previously dropped here.
    #[serde(default)]
    pub chunks_total: u64,
    /// Live embedding throughput (chunks/sec, 0.0 when unknown). ETA =
    /// (chunks_total − chunks_processed) / chunks_per_sec.
    #[serde(default)]
    pub chunks_per_sec: f32,
    /// Optional human-readable status line for the more verbose phases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

// ─── Helpers ─────────────────────────────────────────────────

use std::sync::Arc;

use sovereign_core::time::unix_now as now_epoch;

use crate::state::AppState;

/// The readiness gate for a turn command — the wire form of what
/// `require_runtime!` used to answer, and the reason that macro is gone.
///
/// The two chat commands never wanted a `Runtime` HANDLE: both drive the
/// turn over the socket and their own comments said "readiness gate only".
/// What they wanted was the boolean "is something serving on the client
/// port yet", and `state.runtime.is_some()` answered a different question —
/// in attach mode this process commissions a Runtime over a remote provider
/// and reports `Some` whether or not anything is serving, so the gate said
/// ready while the port was dark. Asking the port is the same repoint
/// `is_backend_ready` made (sv-surface D9b), for the same reason, and it is
/// what frees the eleven-needle boot spine from having to exist in attach
/// at all.
///
/// The substitution is named, not silent (ARCH §18.3): `Ok(false)` (the
/// daemon answered and serves no turns) and a transport failure (nothing
/// answered) are DIFFERENT facts and are traced apart, but both return the
/// one sentence the frontend has always rendered while boot is in flight.
/// Changing that string is a frontend change, not a repoint.
pub(crate) async fn require_backend_ready(state: &Arc<AppState>) -> Result<(), String> {
    const NOT_READY: &str = "Backend is still loading. Please wait.";
    match sovereign_turn_client::TurnClient::new(state.client_base_url())
        .backend_ready()
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => {
            tracing::info!("require_backend_ready: the port answered and serves no turns yet");
            Err(NOT_READY.to_string())
        }
        Err(e) => {
            tracing::info!(
                error = %e,
                "require_backend_ready: nothing answered on the client port yet"
            );
            Err(NOT_READY.to_string())
        }
    }
}

/// The `require_runtime!` shape for commands that need the DATABASE, not
/// the chat Runtime — conversation list/rename/delete, memory tombstones,
/// message search, answer export. Yields an owned `Arc<dyn StateStore>`
/// (the same handle `Runtime::new` is given, see `AppState::store`) and
/// drops the read guard, so no lock is held across the caller's awaits.
///
/// Same not-ready string as `require_runtime!` on purpose: the repoint
/// (daemon-convergence Phase 0) must not change what the frontend renders
/// while bootstrap is in flight.
macro_rules! require_store {
    ($state:expr) => {{
        let guard = $state.store.read().await;
        match guard.as_ref() {
            Some(store) => std::sync::Arc::clone(store),
            None => return Err("Backend is still loading. Please wait.".to_string()),
        }
    }};
}

// ─── Concern submodules (PR5 split of the former 6557-line commands.rs) ───
mod budget;
mod chat;
mod config_setup;
mod contribution;
mod conversation;
mod corpus;
mod corpus_install;
mod diagnostics;
mod document_asset;
mod hardware;
mod lessons;
mod mcp_servers;
mod meshapp;
mod models;
mod reading;
mod recipe_testing;
mod supervisor_ctl;

pub use budget::*;
pub use chat::*;
pub use config_setup::*;
pub use contribution::*;
pub use conversation::*;
pub use corpus::*;
pub use corpus_install::*;
pub use diagnostics::*;
pub use document_asset::*;
pub use hardware::*;
pub use lessons::*;
pub use mcp_servers::*;
pub use meshapp::*;
pub use models::*;
pub use reading::*;
pub use recipe_testing::*;
pub use supervisor_ctl::*;
// Named re-export (the glob only propagates fully-public items):
// setup_flow's first-session-supervision step mirrors the wizard's
// picks into the shared SetupConfig before relaunching.
pub(crate) use config_setup::mirror_to_setup_config;

// ─── Tests ───────────────────────────────────────────────────
