// SPDX-License-Identifier: AGPL-3.0-or-later
//! The recipe shapes svrn generates parse as ingest's `Recipe` (phase-b-48).
//!
//! sovereign-tools writes two kinds of recipe and checks them structurally
//! (`toml::Value` keys and tables): a local corpus's TOML
//! (`local_corpus::config::recipe_toml`) and a knowledge view's document
//! (`knowledge_view::recipes`). What those checks cannot say is that ingest
//! reads the shape; that is this file, one recipe of each shape. The
//! composed path (svrn writing, the stock engine ingesting) is
//! pb-ingest-dial-daemon's watched-folder PROOF.

use corpus_engine::recipe::{AcquirerConfig, ChunkerConfig, Recipe};

/// `recipe_toml`'s shape for a watched folder.
const LOCAL_CORPUS: &str = r#"[corpus]
id = "watched-0123456789abcdef"
name = "Alex's \"Notes\""
description = "Local corpus (watched): Research notes"
license = "local"
mesh_sharing = false
scope = "local"
grantable = true
schema_version = 1

[acquire]
type = "local_file"
path = "/tmp/staged.jsonl"

[extract]
type = "jsonl"
content_field = "content"
title_field = "title"

[chunk]
type = "paragraph"
max_chars = 1500
overlap_chars = 150

[index]
fts = true
vector = true

[retrieval]
personal_scope = true

[display]
category = "watched_folder"
icon = "folder"

[enrichment]
enabled = true
type = "tiered"
"#;

#[test]
fn a_local_corpus_recipe_parses() {
    let recipe = Recipe::from_toml(LOCAL_CORPUS).expect("local corpus recipe parses");
    assert_eq!(recipe.corpus.id, "watched-0123456789abcdef");
    assert_eq!(recipe.corpus.name, r#"Alex's "Notes""#);
    assert_eq!(recipe.corpus.scope.as_deref(), Some("local"));
    assert!(!recipe.corpus.mesh_sharing);
    assert!(recipe.retrieval.personal_scope);
    assert_eq!(
        recipe.display.and_then(|d| d.category).as_deref(),
        Some("watched_folder")
    );
    assert_eq!(recipe.enrichment.unwrap().enrichment_type, "tiered");
}

/// A knowledge view's document shape: the custom `sqlite` acquirer, the
/// `threaded_turns` chunker, an atlas enrichment and a display category.
const KNOWLEDGE_VIEW: &str = r#"[corpus]
id = "conversation-history"
name = "Conversation history"
description = "Recent conversations."
license = "local-only"
mesh_sharing = false
scope = "local"
query_sharing = false
schema_version = 1

[acquire]
type = "custom"
kind = "sqlite"

[acquire.params]
db_path = "/fixture/sovereign.db"
query = "SELECT id, content FROM messages"
content_column = "content"
id_column = "id"
group_column = "conversation_id"
group_separator = "\n\n"

[extract]
type = "jsonl"
content_field = "content"

[chunk]
type = "threaded_turns"

[index]
fts = true
vector = true

[enrichment]
enabled = true
type = "atlas"
domain = "conversational"

[display]
category = "conversation"
icon = "chat-bubble"
"#;

#[test]
fn a_knowledge_view_recipe_parses() {
    let recipe = Recipe::from_toml(KNOWLEDGE_VIEW).expect("knowledge view recipe parses");
    assert_eq!(recipe.corpus.query_sharing, Some(false));
    assert_eq!(recipe.corpus.license, "local-only");
    match &recipe.acquire {
        AcquirerConfig::Custom { kind, params } => {
            assert_eq!(kind, "sqlite");
            assert_eq!(params["group_column"], "conversation_id");
        }
        other => panic!("expected the custom acquirer, got {other:?}"),
    }
    assert!(matches!(recipe.chunk, ChunkerConfig::ThreadedTurns));
    let enrichment = recipe.enrichment.as_ref().unwrap();
    assert_eq!(enrichment.enrichment_type, "atlas");
    assert_eq!(enrichment.domain.as_deref(), Some("conversational"));
    assert_eq!(
        recipe.display.and_then(|d| d.category).as_deref(),
        Some("conversation")
    );
}
