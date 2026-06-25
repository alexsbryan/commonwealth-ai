// SPDX-License-Identifier: AGPL-3.0-or-later
// Façade-level imports: only what the retained Response Types / helpers /
// test module use. Each concern submodule carries its own import block
// (over-import is fine there — see the submodule preambles).
use std::time::{SystemTime, UNIX_EPOCH};

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

#[derive(Serialize)]
pub struct ConversationEntry {
    pub id: String,
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize)]
pub struct ConversationDetail {
    pub id: String,
    pub title: Option<String>,
    pub messages: Vec<MessageEntry>,
    pub created_at: i64,
    pub updated_at: i64,
    /// User-controlled corpus allow-list. `None` = "all installed
    /// corpora" (default); `Some(vec)` = explicit subset. See
    /// `sovereign_core::types::Conversation::enabled_corpora`.
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

#[derive(Serialize)]
pub struct CreateConversationResponse {
    pub id: String,
    pub created_at: i64,
}

#[derive(Serialize)]
pub struct SearchResult {
    pub content: String,
    pub conversation_id: String,
}

#[derive(Serialize)]
pub struct SkillEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub active: bool,
    pub trust_level: String,
}

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

#[derive(Serialize)]
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
#[derive(Serialize)]
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
}

/// Detailed health report for a single installed corpus, loaded on demand
/// (avoids opening every LanceDB index on every `list_corpora` call).
#[derive(Serialize)]
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
#[derive(Serialize, Clone)]
pub struct CorpusProgressPayload {
    pub corpus_id: String,
    /// One of: "downloading", "extracting", "chunking", "embedding",
    /// "indexing", "extracting_claims", "finding_relationships",
    /// "extracting_relationships", "complete", "failed".
    pub phase: String,
    pub percent: f32,
    pub chunks_processed: u64,
    /// Optional human-readable status line for the more verbose phases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Tier groupings for the desktop knowledge picker. Pure UI metadata —
/// the engine doesn't care about tiers.
fn tiers_for(corpus_id: &str) -> Vec<String> {
    match corpus_id {
        // Wikipedia Core ships in every tier — its scoped 100K + Vital
        // Articles is the baseline general-knowledge corpus.
        "wikipedia" => vec![
            "essential".into(),
            "research".into(),
            "technical".into(),
            "full".into(),
        ],
        // Simple English ships alongside Core in every tier — Layer 0 of
        // the layered Wikipedia stack, ready for chat in 2-3 min.
        "wikipedia-simple" => vec![
            "essential".into(),
            "research".into(),
            "technical".into(),
            "full".into(),
        ],
        "sep" => vec!["research".into(), "full".into()],
        "openalex" => vec!["research".into(), "full".into()],
        "stackexchange" => vec!["technical".into(), "full".into()],
        "gutenberg" => vec!["full".into()],
        "crs_reports" => vec!["research".into(), "full".into()],
        _ => vec!["full".into()],
    }
}

// ─── Helpers ─────────────────────────────────────────────────

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

macro_rules! require_runtime {
    ($state:expr) => {{
        let guard = $state.runtime.read().await;
        if guard.is_none() {
            return Err("Backend is still loading. Please wait.".to_string());
        }
        guard
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
mod document_asset;
mod hardware;
mod models;
mod reading;
mod recipe_testing;
mod meshapp;
mod mcp_servers;

pub use budget::*;
pub use chat::*;
pub use config_setup::*;
pub use contribution::*;
pub use conversation::*;
pub use corpus::*;
pub use corpus_install::*;
pub use document_asset::*;
pub use hardware::*;
pub use models::*;
pub use reading::*;
pub use recipe_testing::*;
pub use meshapp::*;
pub use mcp_servers::*;

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::IngestProgress;

    // ── tiers_for ───────────────────────────────────────────

    #[test]
    fn tiers_for_wikipedia_includes_essential_and_full() {
        let tiers = tiers_for("wikipedia");
        assert!(tiers.contains(&"essential".to_string()));
        assert!(tiers.contains(&"research".to_string()));
        assert!(tiers.contains(&"technical".to_string()));
        assert!(tiers.contains(&"full".to_string()));
    }

    #[test]
    fn tiers_for_sep_is_research_only() {
        let tiers = tiers_for("sep");
        assert!(tiers.contains(&"research".to_string()));
        assert!(tiers.contains(&"full".to_string()));
        // SEP is research-grade and not part of the essential
        // tier — installing it pulls multiple GB.
        assert!(!tiers.contains(&"essential".to_string()));
    }

    #[test]
    fn tiers_for_stackexchange_is_technical() {
        let tiers = tiers_for("stackexchange");
        assert!(tiers.contains(&"technical".to_string()));
        assert!(tiers.contains(&"full".to_string()));
        assert!(!tiers.contains(&"essential".to_string()));
    }

    #[test]
    fn tiers_for_unknown_corpus_falls_back_to_full() {
        let tiers = tiers_for("some_user_corpus");
        assert_eq!(tiers, vec!["full".to_string()]);
    }

    // ── ingest_progress_to_payload ──────────────────────────

    #[test]
    fn payload_for_downloading_carries_percent_and_size_message() {
        let payload = ingest_progress_to_payload(
            "wikipedia",
            &IngestProgress::Downloading {
                percent: 42.5,
                bytes_downloaded: 5_242_880, // 5 MB
                bytes_total: Some(10_485_760),
            },
        );
        assert_eq!(payload.corpus_id, "wikipedia");
        assert_eq!(payload.phase, "downloading");
        assert!((payload.percent - 42.5).abs() < 1e-3);
        // The message should describe the download size in MB so the
        // UI can show "5.0 MB" while progress is below 100%.
        let message = payload
            .message
            .expect("downloading payload should have a message");
        assert!(
            message.contains("MB"),
            "expected MB in message, got '{message}'"
        );
    }

    #[test]
    fn payload_for_embedding_computes_percent_from_total() {
        let payload = ingest_progress_to_payload(
            "sep",
            &IngestProgress::Embedding {
                chunks_embedded: 250,
                total: 1000,
                docs_processed: 10,
                chunks_per_sec: 50.0,
                expected_docs: None,
            },
        );
        assert_eq!(payload.phase, "embedding");
        assert!((payload.percent - 25.0).abs() < 1e-3);
        assert_eq!(payload.chunks_processed, 250);
    }

    #[test]
    fn payload_for_embedding_handles_zero_total() {
        // The pipeline reports `total: 0` early, before it knows the
        // chunk count. The mapping must not divide-by-zero.
        let payload = ingest_progress_to_payload(
            "sep",
            &IngestProgress::Embedding {
                chunks_embedded: 0,
                total: 0,
                docs_processed: 0,
                chunks_per_sec: 0.0,
                expected_docs: None,
            },
        );
        assert_eq!(payload.percent, 0.0);
    }

    #[test]
    fn payload_for_embedding_does_not_overshoot_on_per_section_emit() {
        // Wikipedia JSONL emits one ExtractedDoc per section; for a
        // typical curated set that's ~10× the accepted-article count.
        // Confirm the live-event percent does NOT compute
        // `docs_processed / expected_docs` — that was an earlier
        // (wrong) attempt at filter-aware progress that hit 100%
        // within minutes of an embed run with hours left. Polling-side
        // shard-scan progress is the honest signal; the live-event
        // path falls back to the chunk-total ratio (0 until known).
        let payload = ingest_progress_to_payload(
            "wikipedia",
            &IngestProgress::Embedding {
                chunks_embedded: 339_200,
                total: 0,                // unknown (streaming) → 0% live-event percent
                docs_processed: 592_253, // 11× over the title cap
                chunks_per_sec: 34.0,
                expected_docs: Some(51_222),
            },
        );
        assert_eq!(payload.phase, "embedding");
        assert_eq!(
            payload.percent, 0.0,
            "live-event path must defer to polling shard-scan progress, not lie about completion"
        );
        // The "/ Y articles" context still appears in the message.
        let msg = payload.message.as_deref().unwrap_or_default();
        assert!(msg.contains("articles"), "{msg}");
    }

    #[test]
    fn embed_message_omits_articles_when_no_expected_docs() {
        let m = format_embed_message(339_200, 128_000, 32.0, None);
        assert!(m.contains("128.0k docs"), "{m}");
        assert!(!m.contains("articles"), "{m}");
    }

    #[test]
    fn embed_message_includes_filter_scope_when_known() {
        // Wikipedia Core mid-run: filter expects 51,286 titles, the
        // pipeline has emitted 25,643 sections so far. The display
        // unit ("articles") is approximate but communicates the
        // operator-relevant scale.
        let m = format_embed_message(339_200, 25_643, 32.0, Some(51_286));
        assert!(m.contains("/ 51.3k articles"), "{m}");
        assert!(m.contains("339.2k chunks"), "{m}");
        assert!(
            !m.contains("docs"),
            "should swap in 'articles' wording: {m}"
        );
    }

    #[test]
    fn embed_message_clamps_overshoot_to_expected() {
        // docs_processed > expected (sections-per-article > 1 for
        // wikipedia_jsonl). Clamp the displayed numerator so the
        // ratio reads sensibly instead of "128.0k / 51.3k".
        let m = format_embed_message(339_200, 128_000, 32.0, Some(51_286));
        assert!(m.contains("51.3k / 51.3k articles"), "{m}");
    }

    #[test]
    fn payload_for_complete_marks_full_progress() {
        let payload = ingest_progress_to_payload(
            "sep",
            &IngestProgress::Complete {
                total_chunks: 5000,
                duration_secs: 1234,
            },
        );
        assert_eq!(payload.phase, "complete");
        assert_eq!(payload.percent, 100.0);
        assert_eq!(payload.chunks_processed, 5000);
        let message = payload.message.expect("should include duration");
        assert!(message.contains("1234"));
    }

    /// Cover every variant of `IngestProgress` to catch the case where
    /// a future variant is added to corpus-engine but the desktop's
    /// mapping table is not updated. Without this, a new variant
    /// would silently fall through to whatever default behavior the
    /// match arm produces.
    #[test]
    fn payload_phase_is_set_for_every_progress_variant() {
        let cases = [
            IngestProgress::Downloading {
                percent: 0.0,
                bytes_downloaded: 0,
                bytes_total: None,
            },
            IngestProgress::Extracting {
                documents_processed: 1,
            },
            IngestProgress::Chunking { chunks_created: 1 },
            IngestProgress::Embedding {
                chunks_embedded: 1,
                total: 1,
                docs_processed: 1,
                chunks_per_sec: 1.0,
                expected_docs: None,
            },
            IngestProgress::Indexing {
                chunks_indexed: 1,
                total: 1,
            },
            IngestProgress::Complete {
                total_chunks: 1,
                duration_secs: 1,
            },
        ];
        for case in cases {
            let payload = ingest_progress_to_payload("test", &case);
            assert!(
                !payload.phase.is_empty(),
                "every IngestProgress variant must map to a non-empty phase string"
            );
            assert_eq!(payload.corpus_id, "test");
        }
    }
}
