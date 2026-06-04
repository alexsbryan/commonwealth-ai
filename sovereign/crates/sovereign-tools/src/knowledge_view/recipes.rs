//! Built-in `KnowledgeView` recipes.
//!
//! Each builder returns a `Recipe` that drives the corpus-engine
//! ingest pipeline via the `AcquirerConfig::Custom` escape hatch.
//! Recipes are constructed in Rust (rather than read from TOML) so
//! they can reference per-install state — the user's SQLite database
//! path, the runtime list of `privacy = "local_only"` skills to filter
//! out of the conversational view — without the recipe loader needing
//! to template them.
//!
//! Both recipes pin `scope = Some("local")` and `mesh_sharing = false`.
//! These fields cannot be overridden by user configuration for these
//! corpora; privacy is structural.

use std::path::Path;

use corpus_engine::recipe::{
    AcquirerConfig, ChunkerConfig, CorpusMeta, EnrichmentConfig, ExtractorConfig, IndexConfig,
    Recipe,
};
use serde_json::json;

/// The `personal-knowledge` view — one document per memory row,
/// enriched with the `personal` domain.
pub fn personal_knowledge_recipe(db_path: &Path) -> Recipe {
    let params = json!({
        "db_path": db_path.display().to_string(),
        "query": "\
            SELECT id, content, last_used AS version \
            FROM memories \
            WHERE deleted_at IS NULL AND confidence > 0.2 \
            ORDER BY last_used DESC\
        ",
        "content_column": "content",
        "id_column": "id",
        "version_column": "version"
    });

    Recipe {
        corpus: CorpusMeta {
            id: "personal-knowledge".into(),
            name: "Personal knowledge".into(),
            description: "Enriched perspective on the memories table: \
                          persistent concerns, live tensions, open questions."
                .into(),
            license: "local-only".into(),
            mesh_sharing: false,
            scope: Some("local".into()),
            query_sharing: Some(false),
            size_compressed_gb: 0.0,
            size_indexed_gb: 0.0,
            schema_version: 1,
            kind: Default::default(),
            on_demand: false,
            parent_corpus_id: None,
            mutable_merge: None,
        },
        acquire: AcquirerConfig::Custom {
            kind: "sqlite".into(),
            params,
        },
        extract: ExtractorConfig::Jsonl {
            content_field: Some("content".into()),
            title_field: None,
            filter: None,
            decompress: None,
        },
        chunk: ChunkerConfig::Passthrough,
        index: IndexConfig::default(),
        enrichment: Some(EnrichmentConfig {
            enabled: true,
            enrichment_type: "field_model".into(),
            domain: Some("personal".into()),
            prompt_version: Some("v1".into()),
            clustering: None,
            alignment: None,
            fault_lines: None,
            entity_types: Vec::new(),
            relationship_types: Vec::new(),
            patterns: Vec::new(),
            reconciliation: None,
        }),
        update: None,
        prebuilt: None,
        catalog: None,
        filters: Vec::new(),
        filter_mode: Default::default(),
        parameters: Default::default(),
        resolved_parameters: Default::default(),
        // No display.category — KnowledgeView's personal-knowledge
        // view is a digest source, not a corpus the user browses in
        // Atlas View.
        display: None,
        retrieval: Default::default(),
    }
}

/// The `institutional-notes` view — one document per working-note
/// (decision / invariant / todo / postmortem_pointer / uncertainty)
/// from the agent's NoteStore, enriched with the `institutional`
/// domain. Acts as the project's living architectural record:
/// settled stances, live tensions, open questions.
///
/// `db_path` is typically `~/.sovereign/notes.db`. The recipe
/// filters out retired notes and the `reflection` kind (which is
/// tool-calibration feedback, not institutional knowledge).
pub fn institutional_notes_recipe(db_path: &Path) -> Recipe {
    let params = json!({
        "db_path": db_path.display().to_string(),
        "query": "\
            SELECT id, kind, content, updated_at AS version \
            FROM notes \
            WHERE retired_at IS NULL \
              AND kind IN ('decision','invariant','postmortem_pointer','todo','uncertainty','redteam_finding') \
            ORDER BY updated_at DESC\
        ",
        "content_column": "content",
        "id_column": "id",
        "version_column": "version",
        // `kind` flows through as chunk metadata so the
        // InstitutionalDomain's `metadata_in` overview filter can
        // restrict skeleton extraction to decisions / invariants /
        // postmortem pointers.
        "metadata_columns": ["kind"]
    });

    Recipe {
        corpus: CorpusMeta {
            id: "institutional-notes".into(),
            name: "Institutional knowledge".into(),
            description: "Enriched perspective on the project's working \
                          notes: architectural decisions, invariants, \
                          live tensions, unresolved questions."
                .into(),
            license: "local-only".into(),
            mesh_sharing: false,
            scope: Some("local".into()),
            query_sharing: Some(false),
            size_compressed_gb: 0.0,
            size_indexed_gb: 0.0,
            schema_version: 1,
            kind: Default::default(),
            on_demand: false,
            parent_corpus_id: None,
            mutable_merge: None,
        },
        acquire: AcquirerConfig::Custom {
            kind: "sqlite".into(),
            params,
        },
        extract: ExtractorConfig::Jsonl {
            content_field: Some("content".into()),
            title_field: None,
            filter: None,
            decompress: None,
        },
        chunk: ChunkerConfig::Passthrough,
        index: IndexConfig::default(),
        enrichment: Some(EnrichmentConfig {
            enabled: true,
            enrichment_type: "field_model".into(),
            domain: Some("institutional".into()),
            prompt_version: Some("v1".into()),
            clustering: None,
            alignment: None,
            fault_lines: None,
            entity_types: Vec::new(),
            relationship_types: Vec::new(),
            patterns: Vec::new(),
            reconciliation: None,
        }),
        update: None,
        prebuilt: None,
        catalog: None,
        filters: Vec::new(),
        filter_mode: Default::default(),
        parameters: Default::default(),
        resolved_parameters: Default::default(),
        // No display.category — institutional-notes is a digest
        // source, not a corpus the user browses in Atlas View.
        display: None,
        retrieval: Default::default(),
    }
}

/// The `conversation-history` view — one document per conversation
/// assembled by the acquirer via group_concat, enriched with the
/// `conversational` domain.
///
/// `local_only_skill_ids` is the list of skill ids whose conversations
/// must be excluded from this corpus (strict privacy separation for
/// v1). The caller resolves this from `SkillRegistry` at Runtime
/// startup so a future skill that declares `privacy = "local_only"`
/// (e.g. a future `health-journal` skill) automatically participates
/// in the guarantee without editing this recipe.
pub fn conversation_history_recipe(db_path: &Path, local_only_skill_ids: &[&str]) -> Recipe {
    let filter_clause = if local_only_skill_ids.is_empty() {
        String::new()
    } else {
        let quoted: Vec<String> = local_only_skill_ids
            .iter()
            .map(|s| format!("'{}'", s.replace('\'', "''")))
            .collect();
        format!(
            " AND (c.skill_id IS NULL OR c.skill_id NOT IN ({}))",
            quoted.join(", ")
        )
    };

    // Per-message content is emitted in the `### [YYYY-MM-DD HH:MM]
    // <role>\n<body>` shape the `threaded_turns` chunker expects.
    // Group-concat with a blank-line separator collapses one row per
    // message into one document per conversation; the chunker then
    // pairs user+assistant turns into retrieval units identical in
    // shape to the units produced from the Anthropic-export ingest
    // path, so the conversation_atlas pipeline runs against
    // bit-compatible inputs from either source.
    let query = format!(
        "SELECT \
            c.id   AS conversation_id, \
            c.updated_at AS version, \
            ( \
                '### [' || strftime('%Y-%m-%d %H:%M', m.created_at, 'unixepoch') || '] ' \
                || m.role || char(10) \
                || m.content \
            ) AS content \
         FROM conversations c \
         JOIN messages m ON m.conversation_id = c.id \
         WHERE c.deleted_at IS NULL \
           AND c.updated_at > (strftime('%s','now') - 180*86400){filter_clause} \
         ORDER BY c.updated_at DESC, m.created_at ASC"
    );

    let params = json!({
        "db_path": db_path.display().to_string(),
        "query": query,
        "content_column": "content",
        "id_column": "conversation_id",
        "version_column": "version",
        "group_column": "conversation_id",
        "group_separator": "\n\n"
    });

    Recipe {
        corpus: CorpusMeta {
            id: "conversation-history".into(),
            name: "Conversation history".into(),
            description: "Enriched perspective on the conversations + \
                          messages tables (180-day window): recurring \
                          topics, unresolved threads, cross-session \
                          connections."
                .into(),
            license: "local-only".into(),
            mesh_sharing: false,
            scope: Some("local".into()),
            query_sharing: Some(false),
            size_compressed_gb: 0.0,
            size_indexed_gb: 0.0,
            schema_version: 1,
            kind: Default::default(),
            on_demand: false,
            parent_corpus_id: None,
            mutable_merge: None,
        },
        acquire: AcquirerConfig::Custom {
            kind: "sqlite".into(),
            params,
        },
        extract: ExtractorConfig::Jsonl {
            content_field: Some("content".into()),
            title_field: None,
            filter: None,
            decompress: None,
        },
        // Pair user + assistant turns into retrieval units. Same
        // chunker the `conversations-anthropic` recipe uses — keeps
        // the two corpora bit-compatible at the chunk layer so the
        // shared `conversation_atlas` pipeline (and the meta-atlas
        // Trace/Rolling bucket downstream) operates on uniform inputs.
        chunk: ChunkerConfig::ThreadedTurns,
        index: IndexConfig::default(),
        // v2 atlas enrichment via the `conversational` domain →
        // `conversation_atlas` pipeline (see
        // `corpus-engine/src/enrichment/pipeline/pipelines/conversation_atlas.rs`).
        // Replaces the v1 `field_model` skeleton; KnowledgeView's
        // splice path now reads the digest from `atlas/atoms.json`
        // via `atlas_digest::render_atlas_digest`.
        enrichment: Some(EnrichmentConfig {
            enabled: true,
            enrichment_type: "atlas".into(),
            domain: Some("conversational".into()),
            prompt_version: Some("v2".into()),
            clustering: None,
            alignment: None,
            fault_lines: None,
            entity_types: Vec::new(),
            relationship_types: Vec::new(),
            patterns: Vec::new(),
            reconciliation: None,
        }),
        update: None,
        prebuilt: None,
        catalog: None,
        filters: Vec::new(),
        filter_mode: Default::default(),
        parameters: Default::default(),
        resolved_parameters: Default::default(),
        // Atlas View rail groups every corpus declaring
        // `category = "conversation"` under one "Conversations"
        // header — so this corpus (the user's Sovereign-internal
        // chats) and `conversations-anthropic` (imported Claude
        // chats) appear side by side, regardless of which one they
        // originated from.
        display: Some(corpus_engine::DisplayMeta {
            category: Some("conversation".into()),
            icon: Some("chat-bubble".into()),
        }),
        retrieval: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn personal_recipe_is_local_scope() {
        let recipe = personal_knowledge_recipe(&PathBuf::from("/tmp/x.db"));
        assert_eq!(recipe.corpus.id, "personal-knowledge");
        assert_eq!(recipe.corpus.scope.as_deref(), Some("local"));
        assert!(!recipe.corpus.mesh_sharing);
        assert_eq!(recipe.corpus.query_sharing, Some(false));
        let enrichment = recipe.enrichment.unwrap();
        assert_eq!(enrichment.domain.as_deref(), Some("personal"));
        assert!(enrichment.enabled);
    }

    #[test]
    fn personal_recipe_uses_custom_sqlite_acquirer() {
        let recipe = personal_knowledge_recipe(&PathBuf::from("/tmp/x.db"));
        match recipe.acquire {
            AcquirerConfig::Custom { kind, params } => {
                assert_eq!(kind, "sqlite");
                assert_eq!(params["content_column"], "content");
                assert_eq!(params["id_column"], "id");
                assert_eq!(params["version_column"], "version");
            }
            other => panic!("expected Custom acquirer, got {other:?}"),
        }
    }

    #[test]
    fn conversation_recipe_filters_local_only_skills() {
        let recipe = conversation_history_recipe(
            &PathBuf::from("/tmp/x.db"),
            &["inner-work", "personal-assistant"],
        );
        match recipe.acquire {
            AcquirerConfig::Custom { kind, params } => {
                assert_eq!(kind, "sqlite");
                let q = params["query"].as_str().unwrap();
                assert!(q.contains("NOT IN ('inner-work', 'personal-assistant')"));
                assert!(q.contains("180*86400"));
                assert_eq!(params["group_column"], "conversation_id");
                assert_eq!(params["group_separator"], "\n\n");
            }
            other => panic!("expected Custom acquirer, got {other:?}"),
        }
    }

    #[test]
    fn conversation_recipe_no_filter_when_list_empty() {
        let recipe = conversation_history_recipe(&PathBuf::from("/tmp/x.db"), &[]);
        match recipe.acquire {
            AcquirerConfig::Custom { params, .. } => {
                let q = params["query"].as_str().unwrap();
                assert!(!q.contains("NOT IN"));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn conversation_recipe_uses_threaded_turns_chunker_post_v2_migration() {
        // Conversation-history migrated from v1 paragraph chunker +
        // `field_model` enrichment to v2 `threaded_turns` chunker +
        // `atlas` enrichment alongside the conversation-imports
        // landing (§4.14c). The chunker rename is load-bearing —
        // it's what makes the user's Sovereign-internal chats
        // produce atom shapes byte-compatible with imported Claude
        // chats.
        let recipe = conversation_history_recipe(&PathBuf::from("/tmp/x.db"), &[]);
        assert!(
            matches!(recipe.chunk, ChunkerConfig::ThreadedTurns),
            "expected ThreadedTurns chunker post-migration, got {:?}",
            recipe.chunk,
        );
        let enrichment = recipe
            .enrichment
            .as_ref()
            .expect("conversation-history must declare enrichment");
        assert_eq!(enrichment.enrichment_type, "atlas");
        assert_eq!(enrichment.domain.as_deref(), Some("conversational"));
        let display = recipe
            .display
            .as_ref()
            .expect("conversation-history must declare [display]");
        assert_eq!(display.category.as_deref(), Some("conversation"));
    }

    #[test]
    fn institutional_recipe_filters_retired_and_reflections() {
        let recipe = institutional_notes_recipe(&PathBuf::from("/tmp/notes.db"));
        assert_eq!(recipe.corpus.id, "institutional-notes");
        assert_eq!(recipe.corpus.scope.as_deref(), Some("local"));
        match recipe.acquire {
            AcquirerConfig::Custom { kind, params } => {
                assert_eq!(kind, "sqlite");
                let q = params["query"].as_str().unwrap();
                assert!(q.contains("retired_at IS NULL"));
                assert!(q.contains("kind IN"));
                assert!(q.contains("'decision'"));
                assert!(q.contains("'invariant'"));
                assert!(
                    !q.contains("'reflection'"),
                    "reflections are tool-calibration feedback, not institutional knowledge"
                );
                // metadata_columns must include `kind` so the
                // InstitutionalDomain's metadata_in filter can run.
                let cols = params["metadata_columns"].as_array().unwrap();
                assert!(cols.iter().any(|v| v.as_str() == Some("kind")));
            }
            other => panic!("expected Custom acquirer, got {other:?}"),
        }
        let enrichment = recipe.enrichment.unwrap();
        assert_eq!(enrichment.domain.as_deref(), Some("institutional"));
    }

    // Pins the §7.2 structural privacy invariant for all three
    // KnowledgeView recipes. ARCH_PRINCIPLES.md §7.2 cites this test
    // by name — keep the name stable.
    #[test]
    fn knowledge_view_recipes_are_structurally_local() {
        let p = personal_knowledge_recipe(&PathBuf::from("/a"));
        let c = conversation_history_recipe(&PathBuf::from("/b"), &[]);
        let i = institutional_notes_recipe(&PathBuf::from("/c"));
        for r in [p, c, i] {
            assert_eq!(r.corpus.scope.as_deref(), Some("local"));
            assert!(!r.corpus.mesh_sharing);
            assert_eq!(r.corpus.query_sharing, Some(false));
            assert_eq!(r.corpus.license, "local-only");
        }
    }

    #[test]
    fn conversation_recipe_180_day_window_filters_old_messages() {
        // Covers §11 "180-day window applied correctly". We execute
        // the recipe's actual SQL against a DB containing a recent
        // conversation and one 200 days old. Only the recent one
        // must come through.
        use rusqlite::Connection;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("windowed.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversations (
                id TEXT PRIMARY KEY,
                updated_at INTEGER NOT NULL,
                deleted_at INTEGER,
                skill_id TEXT
            );
            CREATE TABLE messages (
                id TEXT PRIMARY KEY,
                conversation_id TEXT NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );",
        )
        .unwrap();

        let now: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let old = now - 200 * 86400;
        conn.execute(
            "INSERT INTO conversations VALUES ('c-recent', ?1, NULL, NULL)",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO conversations VALUES ('c-ancient', ?1, NULL, NULL)",
            rusqlite::params![old],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages VALUES ('m-recent', 'c-recent', 'user', 'within window', ?1)",
            rusqlite::params![now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages VALUES ('m-ancient', 'c-ancient', 'user', 'too old to appear', ?1)",
            rusqlite::params![old],
        )
        .unwrap();

        let recipe = conversation_history_recipe(&db_path, &[]);
        let query = match recipe.acquire {
            AcquirerConfig::Custom { params, .. } => params["query"].as_str().unwrap().to_string(),
            _ => unreachable!(),
        };

        let mut stmt = conn.prepare(&query).unwrap();
        let rows: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>("content"))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        let combined = rows.join("|");
        assert!(
            combined.contains("within window"),
            "recent conversation must pass the 180-day filter: {combined}"
        );
        assert!(
            !combined.contains("too old"),
            "200-day-old conversation must be filtered out: {combined}"
        );
    }

    #[test]
    fn sql_injection_in_skill_id_is_escaped() {
        let nasty = "evil'); DROP TABLE conversations;--";
        let recipe = conversation_history_recipe(&PathBuf::from("/x"), &[nasty]);
        match recipe.acquire {
            AcquirerConfig::Custom { params, .. } => {
                let q = params["query"].as_str().unwrap();
                // Single quotes must be doubled, not closed.
                assert!(q.contains("''); DROP TABLE conversations;--"));
            }
            _ => unreachable!(),
        }
    }
}
