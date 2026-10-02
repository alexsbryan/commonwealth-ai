// SPDX-License-Identifier: AGPL-3.0-or-later
//! Built-in `KnowledgeView` recipes.
//!
//! Each builder returns a recipe document (the recipe's serde form, the
//! shape its TOML parses to) that drives ingest's pipeline via the
//! `acquire = { type = "custom" }` escape hatch; ingest parses it, so svrn
//! names no recipe type (pb-ingest-dial-tools-local). A fixture pins every
//! document to the recipe the typed builders produced before
//! (`tests/fixtures/knowledge_view_recipes.json`).
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

use serde_json::{json, Value};

/// A view corpus's identity.
struct ViewCorpus {
    id: &'static str,
    name: &'static str,
    description: &'static str,
}

/// A view's enrichment declaration.
struct ViewEnrichment {
    enrichment_type: &'static str,
    domain: &'static str,
    prompt_version: &'static str,
}

/// The recipe document every view shares: a local-only, never-shared,
/// never-granted SQLite-acquired JSONL corpus with the default index.
/// Every key the typed recipe serialized is written out (none left to a
/// serde default), and no null: the document round-trips through TOML.
fn local_view_recipe(
    corpus: ViewCorpus,
    params: Value,
    chunk: &str,
    enrichment: ViewEnrichment,
    display: Option<Value>,
) -> Value {
    let mut recipe = json!({
        "corpus": {
            "id": corpus.id,
            "name": corpus.name,
            "description": corpus.description,
            "license": "local-only",
            "mesh_sharing": false,
            "scope": "local",
            "query_sharing": false,
            // Structural: KnowledgeView corpora may NEVER be lent to peers,
            // even under a one-off ephemeral grant.
            "grantable": false,
            "size_compressed_gb": 0.0,
            "size_indexed_gb": 0.0,
            "schema_version": 1,
            "kind": "knowledge",
            "on_demand": false
        },
        "acquire": { "type": "custom", "kind": "sqlite", "params": params },
        "extract": { "type": "jsonl", "content_field": "content" },
        "chunk": { "type": chunk },
        "index": {
            "fts": true,
            "vector": true,
            "embedding_model": "qwen3-embedding-0.6b",
            "embedding_dimensions": 0
        },
        "enrichment": {
            "enabled": true,
            "type": enrichment.enrichment_type,
            "domain": enrichment.domain,
            "prompt_version": enrichment.prompt_version,
            "entity_types": [],
            "relationship_types": [],
            "patterns": []
        },
        "filter": [],
        "filter_mode": { "mode": "any" },
        "parameters": {},
        "retrieval": { "dedup_by_source": false, "personal_scope": false }
    });
    if let Some(display) = display {
        recipe["display"] = display;
    }
    recipe
}

/// The `personal-knowledge` view — one document per memory row,
/// enriched with the `personal` domain.
pub fn personal_knowledge_recipe(db_path: &Path) -> Value {
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

    local_view_recipe(
        ViewCorpus {
            id: "personal-knowledge",
            name: "Personal knowledge",
            description: "Enriched perspective on the memories table: \
                          persistent concerns, live tensions, open questions.",
        },
        params,
        "passthrough",
        ViewEnrichment {
            enrichment_type: "field_model",
            domain: "personal",
            prompt_version: "v1",
        },
        // No display.category — KnowledgeView's personal-knowledge
        // view is a digest source, not a corpus the user browses in
        // Atlas View.
        None,
    )
}

/// The `institutional-notes` view — one document per working-note
/// (decision / invariant / todo / postmortem_pointer / uncertainty)
/// from the agent's NoteStore, enriched with the `institutional`
/// domain. Acts as the project's living architectural record:
/// settled stances, live tensions, open questions.
///
/// `db_path` is typically `~/.svrnmesh/notes.db`. The recipe
/// filters out retired notes and the `reflection` kind (which is
/// tool-calibration feedback, not institutional knowledge).
pub fn institutional_notes_recipe(db_path: &Path) -> Value {
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

    local_view_recipe(
        ViewCorpus {
            id: "institutional-notes",
            name: "Institutional knowledge",
            description: "Enriched perspective on the project's working \
                          notes: architectural decisions, invariants, \
                          live tensions, unresolved questions.",
        },
        params,
        "passthrough",
        ViewEnrichment {
            enrichment_type: "field_model",
            domain: "institutional",
            prompt_version: "v1",
        },
        None,
    )
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
pub fn conversation_history_recipe(db_path: &Path, local_only_skill_ids: &[&str]) -> Value {
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

    local_view_recipe(
        ViewCorpus {
            id: "conversation-history",
            name: "Conversation history",
            description: "Enriched perspective on the conversations + \
                          messages tables (180-day window): recurring \
                          topics, unresolved threads, cross-session \
                          connections.",
        },
        params,
        // Pair user + assistant turns into retrieval units. Same
        // chunker the `conversations-anthropic` recipe uses — keeps
        // the two corpora bit-compatible at the chunk layer so the
        // shared `conversation_atlas` pipeline (and the meta-atlas
        // Trace/Rolling bucket downstream) operates on uniform inputs.
        "threaded_turns",
        // v2 atlas enrichment via the `conversational` domain →
        // `conversation_atlas` pipeline (see
        // `ingest/crates/corpus-engine/src/enrichment/pipeline/pipelines/conversation_atlas.rs`).
        // Replaces the v1 `field_model` skeleton; KnowledgeView's
        // splice path now reads the digest from `atlas/atoms.json`
        // via `atlas_digest::render_atlas_digest`.
        ViewEnrichment {
            enrichment_type: "atlas",
            domain: "conversational",
            prompt_version: "v2",
        },
        // Atlas View rail groups every corpus declaring
        // `category = "conversation"` under one "Conversations"
        // header — so this corpus (the user's Sovereign-internal
        // chats) and `conversations-anthropic` (imported Claude
        // chats) appear side by side, regardless of which one they
        // originated from.
        Some(json!({ "category": "conversation", "icon": "chat-bubble" })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // The assertions below read each view document structurally, as the
    // keys and tables it carries. That a document of this shape parses as
    // ingest's `Recipe` is corpus-engine's `a_knowledge_view_recipe_parses`
    // (phase-b-48); the composed path is pb-ingest-dial-daemon's PROOF.

    /// The `[acquire]` of a view document: always the custom `sqlite`
    /// acquirer, so this returns its params.
    fn sqlite_params(recipe: &Value) -> &Value {
        assert_eq!(recipe["acquire"]["type"], "custom");
        assert_eq!(recipe["acquire"]["kind"], "sqlite");
        &recipe["acquire"]["params"]
    }

    /// Every key a document sets agrees with `typed`, the recipe the typed
    /// builders serialized to (defaults filled in).
    fn assert_subtree(doc: &Value, typed: &Value, at: &str) {
        match doc {
            Value::Object(map) => {
                for (k, v) in map {
                    assert_subtree(v, &typed[k.as_str()], &format!("{at}.{k}"));
                }
            }
            _ => assert_eq!(doc, typed, "{at} drifted from the typed recipe"),
        }
    }

    /// Every view document carries — directly and through the TOML file
    /// `ingest_view` materialises — exactly the values the typed builders
    /// produced before they became documents (the fixture was serialized
    /// from those builders at 4849bfb01).
    #[test]
    fn view_recipe_documents_match_the_typed_recipes_they_replaced() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/knowledge_view_recipes.json"
        ))
        .expect("fixture parses");
        let db = PathBuf::from("/fixture/sovereign.db");
        let notes = PathBuf::from("/fixture/notes.db");
        let docs = [
            ("personal", super::personal_knowledge_recipe(&db)),
            ("institutional", super::institutional_notes_recipe(&notes)),
            ("conversation", super::conversation_history_recipe(&db, &[])),
            (
                "conversation_local_only",
                super::conversation_history_recipe(&db, &["inner-work", "o'brien"]),
            ),
        ];
        for (name, doc) in docs {
            assert_subtree(&doc, &fixture[name], name);
            let toml_text = toml::to_string(&doc).expect("document serializes as TOML");
            let via_toml: Value = toml::from_str(&toml_text).expect("TOML parses back");
            assert_eq!(via_toml, doc, "{name}: TOML round-trip drifted");
        }
    }

    #[test]
    fn personal_recipe_is_local_scope() {
        let recipe = personal_knowledge_recipe(&PathBuf::from("/tmp/x.db"));
        assert_eq!(recipe["corpus"]["id"], "personal-knowledge");
        assert_eq!(recipe["corpus"]["scope"], "local");
        assert_eq!(recipe["corpus"]["mesh_sharing"], false);
        assert_eq!(recipe["corpus"]["query_sharing"], false);
        assert_eq!(recipe["enrichment"]["domain"], "personal");
        assert_eq!(recipe["enrichment"]["enabled"], true);
    }

    #[test]
    fn personal_recipe_uses_custom_sqlite_acquirer() {
        let recipe = personal_knowledge_recipe(&PathBuf::from("/tmp/x.db"));
        let params = sqlite_params(&recipe);
        assert_eq!(params["content_column"], "content");
        assert_eq!(params["id_column"], "id");
        assert_eq!(params["version_column"], "version");
    }

    #[test]
    fn conversation_recipe_filters_local_only_skills() {
        let recipe = conversation_history_recipe(
            &PathBuf::from("/tmp/x.db"),
            &["inner-work", "personal-assistant"],
        );
        let params = sqlite_params(&recipe);
        let q = params["query"].as_str().unwrap();
        assert!(q.contains("NOT IN ('inner-work', 'personal-assistant')"));
        assert!(q.contains("180*86400"));
        assert_eq!(params["group_column"], "conversation_id");
        assert_eq!(params["group_separator"], "\n\n");
    }

    #[test]
    fn conversation_recipe_no_filter_when_list_empty() {
        let recipe = conversation_history_recipe(&PathBuf::from("/tmp/x.db"), &[]);
        let q = sqlite_params(&recipe)["query"].as_str().unwrap();
        assert!(!q.contains("NOT IN"));
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
        assert_eq!(
            recipe["chunk"]["type"], "threaded_turns",
            "expected ThreadedTurns chunker post-migration, got {}",
            recipe["chunk"],
        );
        assert!(
            recipe["enrichment"].is_object(),
            "conversation-history must declare enrichment"
        );
        assert_eq!(recipe["enrichment"]["type"], "atlas");
        assert_eq!(recipe["enrichment"]["domain"], "conversational");
        assert!(
            recipe["display"].is_object(),
            "conversation-history must declare [display]"
        );
        assert_eq!(recipe["display"]["category"], "conversation");
    }

    #[test]
    fn institutional_recipe_filters_retired_and_reflections() {
        let recipe = institutional_notes_recipe(&PathBuf::from("/tmp/notes.db"));
        assert_eq!(recipe["corpus"]["id"], "institutional-notes");
        assert_eq!(recipe["corpus"]["scope"], "local");
        let params = sqlite_params(&recipe);
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
        assert_eq!(recipe["enrichment"]["domain"], "institutional");
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
            assert_eq!(r["corpus"]["scope"], "local");
            assert_eq!(r["corpus"]["mesh_sharing"], false);
            assert_eq!(r["corpus"]["query_sharing"], false);
            assert_eq!(r["corpus"]["license"], "local-only");
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
        let query = sqlite_params(&recipe)["query"]
            .as_str()
            .unwrap()
            .to_string();

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
        let q = sqlite_params(&recipe)["query"].as_str().unwrap();
        // Single quotes must be doubled, not closed.
        assert!(q.contains("''); DROP TABLE conversations;--"));
    }
}
