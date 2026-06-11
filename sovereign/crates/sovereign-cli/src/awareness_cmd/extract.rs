// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign awareness extract` — run entity extraction over the
//! current StateStore contents and write atoms.json + edges.json.
//!
//! Bypasses the production CorpusEngine ingest path (which would
//! re-embed every memory and message into LanceDB) — for entity
//! extraction we only need text. Synthesises `StoredChunk` records
//! from `memories` and `conversations` rows, runs
//! `run_entity_extraction` per domain, then patches up the resulting
//! atoms + edges so chunk-id strings round-trip back to
//! `memories.id` / `conversations.id` (the chunk-timestamp resolver
//! reads those tables).
//!
//! The model is chosen via `--mock` / `--dry-run` / default (real),
//! same shape as `chat`. Default real loading is heavy; `--mock` is
//! the inner-loop iteration mode.

use std::collections::HashMap;
use std::sync::Arc;

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, AtomId, AtomsFile, ChunkRef, Entity};
use corpus_engine::enrichment::atlas::edges::{Edge, EdgesFile};
use corpus_engine::enrichment::atlas::writer::{
    read_atlas_atoms, read_atlas_edges, write_atlas, ATLAS_DIRNAME,
};
use corpus_engine::enrichment::domain::Domain;
use corpus_engine::enrichment::domains::conversational::ConversationalDomain;
use corpus_engine::enrichment::domains::personal::PersonalDomain;
use corpus_engine::enrichment::entity_extraction::{run_entity_extraction, EntityExtractionResult};
use corpus_engine::enrichment::EnrichmentProgress;
use corpus_engine::index::StoredChunk;

use sovereign_core::traits::{ConversationStore, MemoryStore};
use sovereign_store::sqlite::SqliteStateStore;

use super::args::{get_flag, has_flag, split_args};
use super::inference::{resolve_inference, InferenceMode};
use super::render::display_path;
use super::store_open::{atlas_dir_for, sovereign_root, state_db_path};

const PERSONAL_VIEW: &str = "personal-knowledge";
const CONVERSATIONAL_VIEW: &str = "conversation-history";
const MAX_CONVERSATION_LIST: usize = 10_000;

pub(super) async fn cmd_extract(args: &[String]) -> i32 {
    let (_pos, flags) = split_args(args);

    let phase = get_flag(&flags, "phase").unwrap_or_else(|| "entity".to_string());
    if !matches!(phase.as_str(), "entity" | "all") {
        eprintln!("awareness extract: --phase must be one of: entity, all (got '{phase}')");
        return 2;
    }
    if phase == "all" {
        eprintln!(
            "awareness extract: --phase all routes through the production CorpusEngine \
             ingest pipeline and is not yet wired up — defer to `sovereign chat` or the \
             daemon to drive ingestion. --phase entity (default) is the supported path."
        );
        return 2;
    }

    let limit: Option<usize> = match get_flag(&flags, "limit") {
        None => None,
        Some(s) => match s.parse::<usize>() {
            Ok(n) if n > 0 => Some(n),
            _ => {
                eprintln!("awareness extract: --limit must be a positive integer (got '{s}')");
                return 2;
            }
        },
    };
    let verbose = has_flag(&flags, "verbose");

    let (inference, mode) = match resolve_inference(&flags).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("awareness extract: {e}");
            return 1;
        }
    };
    println!(
        "awareness extract: inference mode = {}",
        match mode {
            InferenceMode::Real => "real",
            InferenceMode::Mock => "mock",
            InferenceMode::DryRun => "dry-run",
        }
    );

    let root = sovereign_root(&flags);
    let db_path = state_db_path(&root);
    if !db_path.exists() {
        eprintln!(
            "awareness extract: no state db at {} (run `awareness seed` first)",
            display_path(&db_path)
        );
        return 1;
    }
    let store = match SqliteStateStore::open(&db_path) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!(
                "awareness extract: open {} failed: {e}",
                display_path(&db_path)
            );
            return 1;
        }
    };

    // ── Personal corpus ──────────────────────────────────────────
    let memories = match store.get_all_memories().await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("awareness extract: list memories failed: {e}");
            return 1;
        }
    };
    let memories: Vec<_> = match limit {
        Some(n) => memories.into_iter().take(n).collect(),
        None => memories,
    };
    let personal_chunks = build_personal_chunks(&memories);
    println!(
        "awareness extract: personal-knowledge — {} memorie{} → {} chunk{}",
        memories.len(),
        if memories.len() == 1 { "" } else { "s" },
        personal_chunks.len(),
        if personal_chunks.len() == 1 { "" } else { "s" }
    );
    let personal_atlas_dir = atlas_dir_for(&root, PERSONAL_VIEW)
        .join("..")
        .canonicalize()
        .unwrap_or_else(|_| {
            // canonicalize fails when the dir doesn't exist yet; fall
            // back to the corpus dir literally (atlas_dir_for joins
            // `atlas/` at the end).
            atlas_dir_for(&root, PERSONAL_VIEW)
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| atlas_dir_for(&root, PERSONAL_VIEW))
        });
    // Use a clean reconstruction — the canonicalize dance above is
    // fragile when the dir doesn't yet exist. Rebuild from root.
    let personal_corpus_dir = root.join("indexes").join(PERSONAL_VIEW);
    let _ = personal_atlas_dir;

    let personal_summary = if personal_chunks.is_empty() {
        ExtractSummary::empty(PERSONAL_VIEW)
    } else {
        let domain = PersonalDomain;
        let id_map = build_id_map(
            &personal_chunks,
            &memories.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        );
        let result = run_entity_extraction_with_progress(
            &personal_chunks,
            &domain,
            inference.clone(),
            verbose,
        )
        .await;
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                eprintln!("awareness extract: personal extraction failed: {e}");
                return 1;
            }
        };
        let summary = ExtractSummary::from_result(PERSONAL_VIEW, &result);
        if let Err(e) = write_remapped_atlas(&personal_corpus_dir, result, &id_map) {
            eprintln!("awareness extract: write personal atlas failed: {e}");
            return 1;
        }
        summary
    };
    print_summary(&personal_summary);

    // ── Conversational corpus ────────────────────────────────────
    // `list_conversations` returns rows with `messages: Vec::new()`
    // (intentionally — it's a list view). Re-fetch each via
    // `get_conversation` so chunk bodies aren't empty.
    let conv_summaries = match store.list_conversations(MAX_CONVERSATION_LIST, 0).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("awareness extract: list conversations failed: {e}");
            return 1;
        }
    };
    let conv_summaries: Vec<_> = match limit {
        Some(n) => conv_summaries.into_iter().take(n).collect(),
        None => conv_summaries,
    };
    let mut conversations: Vec<sovereign_core::types::Conversation> =
        Vec::with_capacity(conv_summaries.len());
    for summary in &conv_summaries {
        match store.get_conversation(&summary.id).await {
            Ok(full) => conversations.push(full),
            Err(e) => {
                eprintln!(
                    "awareness extract: load conversation {} failed: {e}",
                    summary.id
                );
                return 1;
            }
        }
    }
    let conv_chunks = build_conversational_chunks(&conversations);
    println!();
    println!(
        "awareness extract: conversation-history — {} conversation{} → {} chunk{}",
        conversations.len(),
        if conversations.len() == 1 { "" } else { "s" },
        conv_chunks.len(),
        if conv_chunks.len() == 1 { "" } else { "s" }
    );

    let conv_summary = if conv_chunks.is_empty() {
        ExtractSummary::empty(CONVERSATIONAL_VIEW)
    } else {
        let domain = ConversationalDomain;
        let id_map = build_id_map(
            &conv_chunks,
            &conversations
                .iter()
                .map(|c| c.id.clone())
                .collect::<Vec<_>>(),
        );
        let conv_corpus_dir = root.join("indexes").join(CONVERSATIONAL_VIEW);
        let result =
            run_entity_extraction_with_progress(&conv_chunks, &domain, inference.clone(), verbose)
                .await;
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                eprintln!("awareness extract: conversational extraction failed: {e}");
                return 1;
            }
        };
        let summary = ExtractSummary::from_result(CONVERSATIONAL_VIEW, &result);
        if let Err(e) = write_remapped_atlas(&conv_corpus_dir, result, &id_map) {
            eprintln!("awareness extract: write conversational atlas failed: {e}");
            return 1;
        }
        summary
    };
    print_summary(&conv_summary);

    println!();
    println!(
        "Run `sovereign awareness entities` or `sovereign awareness timeline <name>` to inspect."
    );
    0
}

/// One chunk per memory. `StoredChunk.id` is sequential — we remap
/// to `memory.id` after extraction.
fn build_personal_chunks(memories: &[sovereign_core::types::Memory]) -> Vec<StoredChunk> {
    memories
        .iter()
        .enumerate()
        .map(|(i, m)| StoredChunk {
            id: (i + 1) as u64,
            content: m.content.clone(),
            title: None,
            source_doc_id: None,
        })
        .collect()
}

/// One chunk per conversation. Content is the concatenated message
/// bodies prefixed with role tags so the model has the conversation
/// shape, not just words. Excludes assistant-only conversations
/// (which are rare but possible) by keeping all messages.
fn build_conversational_chunks(
    conversations: &[sovereign_core::types::Conversation],
) -> Vec<StoredChunk> {
    conversations
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let body = c
                .messages
                .iter()
                .map(|m| format!("{}: {}", m.role_str(), m.content))
                .collect::<Vec<_>>()
                .join("\n");
            StoredChunk {
                id: (i + 1) as u64,
                content: body,
                title: c.title.clone(),
                source_doc_id: None,
            }
        })
        .collect()
}

/// Map sequential chunk id strings ("1", "2", …) to the source row
/// ids (memory.id / conversation.id) for post-extraction remapping.
fn build_id_map(chunks: &[StoredChunk], source_ids: &[String]) -> HashMap<String, String> {
    chunks
        .iter()
        .zip(source_ids.iter())
        .map(|(c, sid)| (c.id.to_string(), sid.clone()))
        .collect()
}

async fn run_entity_extraction_with_progress(
    chunks: &[StoredChunk],
    domain: &dyn Domain,
    inference: corpus_engine::InferenceFn,
    verbose: bool,
) -> Result<EntityExtractionResult, String> {
    let report = move |p: EnrichmentProgress| {
        if verbose {
            tracing::debug!(progress = ?p, "entity_extraction: progress");
        }
    };
    run_entity_extraction(chunks, domain, inference, &report)
        .await
        .map_err(|e| e.to_string())
}

/// Replace sequential chunk_ids with the real source row ids in
/// every Entity's `first_appearance` and every Edge's `evidence` +
/// `source`. Ids that don't appear in the map are passed through —
/// the model occasionally emits chunk references the rewrite step
/// already filtered, and we'd rather see them than silently drop.
fn write_remapped_atlas(
    corpus_dir: &std::path::Path,
    result: EntityExtractionResult,
    id_map: &HashMap<String, String>,
) -> Result<(), String> {
    let mut entities: Vec<Entity> = result.entities.clone();
    for e in entities.iter_mut() {
        if let Some(real) = id_map.get(&e.first_appearance.chunk_id) {
            e.first_appearance.chunk_id = real.clone();
        }
    }

    let mut edges: Vec<Edge> = result.edges.clone();
    for edge in edges.iter_mut() {
        // Source AtomId: "chunk-<seq>" → "chunk-<source-id>".
        let s = edge.source.as_str();
        if let Some(stripped) = s.strip_prefix("chunk-") {
            if let Some(real) = id_map.get(stripped) {
                edge.source = AtomId::from_raw(format!("chunk-{}", real));
            }
        }
        // Evidence chunk_id strings.
        for ev in edge.evidence.iter_mut() {
            if let Some(real) = id_map.get(&ev.chunk_id) {
                ev.chunk_id = real.clone();
            }
        }
    }

    let atlas_dir = corpus_dir.join(ATLAS_DIRNAME);
    write_atlas(&atlas_dir, &entities, &[], &edges)
        .map_err(|e| format!("{}: {e}", atlas_dir.display()))?;

    // Sanity: read it back so a corrupted write surfaces here.
    let _ = read_atlas_atoms(&atlas_dir).map_err(|e| format!("read-back atoms.json: {e}"))?;
    let _ = read_atlas_edges(&atlas_dir).map_err(|e| format!("read-back edges.json: {e}"))?;
    Ok(())
}

#[derive(Debug)]
struct ExtractSummary {
    view: &'static str,
    persons: usize,
    organizations: usize,
    initiatives: usize,
    edges: usize,
    failures: usize,
    batches: usize,
    borderline_initiatives: Vec<String>,
}

impl ExtractSummary {
    fn empty(view: &'static str) -> Self {
        Self {
            view,
            persons: 0,
            organizations: 0,
            initiatives: 0,
            edges: 0,
            failures: 0,
            batches: 0,
            borderline_initiatives: Vec::new(),
        }
    }

    fn from_result(view: &'static str, r: &EntityExtractionResult) -> Self {
        use corpus_engine::enrichment::pipeline::atlas::EntityType;
        let mut persons = 0usize;
        let mut orgs = 0usize;
        let mut inits = 0usize;
        let mut borderline: Vec<String> = Vec::new();
        for e in &r.entities {
            match e.entity_type {
                EntityType::Person => persons += 1,
                EntityType::Institution => orgs += 1,
                EntityType::Initiative => {
                    inits += 1;
                    if is_borderline(e) {
                        borderline.push(e.canonical_name.clone());
                    }
                }
                _ => {}
            }
        }
        Self {
            view,
            persons,
            organizations: orgs,
            initiatives: inits,
            edges: r.edges.len(),
            failures: r.failures.len(),
            batches: r.batches_run,
            borderline_initiatives: borderline,
        }
    }
}

fn is_borderline(e: &Entity) -> bool {
    let lower = e.description.to_lowercase();
    let hedges = [
        "talked about",
        "discussed",
        "thinking about",
        "thought about",
        "considered",
        "wondering",
    ];
    hedges.iter().any(|h| lower.contains(h))
}

fn print_summary(s: &ExtractSummary) {
    println!();
    println!("Summary [{}]:", s.view);
    println!(
        "  Entities: {} ({} person, {} organization, {} initiative)",
        s.persons + s.organizations + s.initiatives,
        s.persons,
        s.organizations,
        s.initiatives
    );
    println!("  Involves edges: {}", s.edges);
    println!("  Failures: {}", s.failures);
    println!("  Batches run: {}", s.batches);
    if !s.borderline_initiatives.is_empty() {
        println!(
            "  Borderline initiatives ({}): {}",
            s.borderline_initiatives.len(),
            s.borderline_initiatives.join(", ")
        );
    }
}

// Suppress unused warnings on read_atlas_* (they're used inside
// write_remapped_atlas).
#[allow(dead_code)]
fn _force_use(_a: AtomEnvelope, _af: AtomsFile, _ef: EdgesFile, _cr: ChunkRef) {}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::types::{Conversation, ConversationId, Memory, Message, MessageId, Role};

    fn mem(id: &str, content: &str) -> Memory {
        Memory {
            id: id.into(),
            content: content.into(),
            source: "test".into(),
            confidence: 0.9,
            created_at: 0,
            last_used: 0,
            version: 0,
            deleted_at: None,
            source_conversation_id: None,
            ..Default::default()
        }
    }

    fn conv(id: &str, body: &[(&str, &str)]) -> Conversation {
        let messages: Vec<Message> = body
            .iter()
            .enumerate()
            .map(|(i, (role, content))| Message {
                id: MessageId::from(format!("{id}-m{i}")),
                conversation_id: ConversationId::from(id.to_string()),
                role: match *role {
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    _ => Role::System,
                },
                content: content.to_string(),
                created_at: i as i64,
                metadata: None,
                version: 0,
            })
            .collect();
        Conversation {
            id: id.into(),
            title: None,
            messages,
            created_at: 0,
            updated_at: 0,
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: None,
            searched_sources: None,
        }
    }

    #[test]
    fn build_personal_chunks_assigns_sequential_ids() {
        let memories = vec![mem("a", "alpha"), mem("b", "beta")];
        let chunks = build_personal_chunks(&memories);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].id, 1);
        assert_eq!(chunks[1].id, 2);
        assert_eq!(chunks[0].content, "alpha");
    }

    #[test]
    fn build_conversational_chunks_concatenates_messages() {
        let c = conv("c1", &[("user", "Hello"), ("assistant", "Hi there")]);
        let chunks = build_conversational_chunks(&[c]);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("user: Hello"));
        assert!(chunks[0].content.contains("assistant: Hi there"));
    }

    #[test]
    fn id_map_round_trips_sequential_to_source_id() {
        let memories = vec![mem("memory-a", "x"), mem("memory-b", "y")];
        let chunks = build_personal_chunks(&memories);
        let map = build_id_map(
            &chunks,
            &memories.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        );
        assert_eq!(map.get("1").map(|s| s.as_str()), Some("memory-a"));
        assert_eq!(map.get("2").map(|s| s.as_str()), Some("memory-b"));
    }
}
