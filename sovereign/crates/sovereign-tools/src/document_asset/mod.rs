// SPDX-License-Identifier: AGPL-3.0-or-later
//! Document Asset Manager — ingest once, query forever.
//!
//! Handles the full lifecycle of a document asset:
//!
//! 1. **Ingest** — parse, chunk, embed, and build a structural skeleton.
//!    Embedding and skeleton extraction run concurrently. Once embedding
//!    completes, RAG queries are available while the skeleton keeps building.
//!
//! 2. **Route** — classify a user's question into one of four operation
//!    types (RAG, Synthesis, Aggregation, Transformation) using the
//!    skeleton's overview and the question text.
//!
//! 3. **Execute** — run the selected operation and return the response
//!    with source citations.
//!
//! Reuses the existing RAG pipeline (`rag::parse`, `rag::chunk`) for
//! parsing and chunking. Reuses `DocumentOperationTool`'s map-reduce
//! pattern for the synthesis and aggregation executors.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::stream::{self, StreamExt};
use serde::Deserialize;
use sovereign_core::error::{Error, Result};
use sovereign_core::slot_policy::Workload;
use sovereign_core::traits::{EntityExtractor, InferenceProvider, StateStore};
use sovereign_core::types::*;

use crate::rag::chunk::{chunk_text, TextChunk};
use crate::rag::parse::parse_file;
use crate::raptor_atlas::ChunkInput;

/// Concurrency cap for parallel per-batch LLM calls in the T2 entity
/// extraction phase. The mesh load balancer sees this many simultaneous
/// requests and fans them across peers; on single-machine deployments
/// the skeleton batches are Fast-class (EnrichBulk since 2026-07-24) so
/// the fan-out lands on the FastShort continuous-batching companion —
/// 12 keeps its per-sequence KV slices fed without queueing beyond the
/// slot's n_seq. (Historical: 6, tuned for a 2-peer mesh when the
/// batches were Normal-class and a single local slot serialized them.)
const T2_BATCH_CONCURRENCY: usize = 12;

mod artifacts;
mod atoms;
mod execution;
mod manager;
mod motifs;
mod routing;
mod self_reference;
mod skeleton;

// Siblings reach each other through here; the two `pub` lines are the surface
// `lib.rs` re-exports, and the `pub(crate)` block is what conv_tiered_provider
// imports. Everything else is module-internal and stays that way.
use artifacts::*;
// Exactly one artifact builder is reached from outside this module
// (conv_tiered_provider.rs:1265). `build_atlas_artifacts` and its
// checkpointed twin are named only in COMMENTS out there, so re-exporting
// them would claim a surface nothing consumes.
pub(crate) use artifacts::build_raptor_nodes_with_checkpoint;
use atoms::*;
pub use manager::*;
use motifs::*;
pub use self_reference::*;
use skeleton::*;

// ─── Progress types ──────────────────────────────────────────

/// Progress updates emitted during document ingest. The frontend
/// listens to these via Tauri events to drive the progress bar
/// and ingest banner.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum IngestProgress {
    /// File parsed, chunking complete, embedding about to start.
    /// Carries `asset_id` so the UI can route this earliest signal to
    /// the right banner — it's the same id every later event uses.
    Started {
        word_count: usize,
        chunk_count: usize,
        filename: String,
        asset_id: String,
    },
    /// Embedding chunks into the vector store.
    Indexing { done: usize, total: usize },
    /// Embedding complete. RAG queries now available. (T1 done)
    RagAvailable { asset_id: String },
    /// Skeleton enrichment in progress. The same variant fires for
    /// both the T2 (entity extraction) and T3 (RAPTOR + segments +
    /// overview) phases; the `MultiHopReady` event between them
    /// signals the phase boundary.
    BuildingSkeleton { done: usize, total: usize },
    /// T2 done. Entity index + action atoms available; PPR
    /// multi-hop retrieval works while T3 continues.
    MultiHopReady { asset_id: String },
    /// T3 done. All operations available.
    Ready {
        asset_id: String,
        main_entities: usize,
        structural_moments: usize,
    },
    /// Ingest failed.
    Failed { reason: String },
}

/// Progress updates during a query operation. The frontend shows
/// these as loading state text below the input.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum OperationProgress {
    /// Router has decided which operation to use.
    Routing { operation: String },
    /// Retrieving relevant passages from the index.
    Retrieving,
    /// Analysing a specific entity across the document.
    AnalysingEntity { name: String },
    /// Synthesising the final response.
    Synthesising,
}

/// Output of the inference-free preparation step: the persisted
/// `Pending` asset plus the chunked text awaiting embedding.
///
/// The asset id minted in `prepare` is the SAME id `run_ingest` emits
/// every `document:progress` event under. The desktop command returns
/// this id to the UI *before* spawning `run_ingest`, so the banner the
/// UI shows and the events it receives agree on one id. Before this
/// split the command minted one id and `ingest` minted another
/// internally — the banner subscribed to an id that never received a
/// single progress event and sat on "Queued…" for the entire ingest.
pub struct PreparedIngest {
    pub asset: DocumentAsset,
    text_chunks: Vec<TextChunk>,
}

// ─── Parsing helpers ─────────────────────────────────────────

/// Parsed result from one batch of skeleton extraction.
struct SkeletonBatchEntry {
    chunk_index: usize,
    function: SectionFunction,
    /// Entity names paired with their classified kind.
    entity_names_and_kinds: Vec<(String, EntityKind)>,
    moment_description: Option<String>,
}

/// Parse an entity kind string from the LLM response.
fn parse_entity_kind(s: &str) -> EntityKind {
    match s.to_lowercase().as_str() {
        "character" => EntityKind::Character,
        "argument" => EntityKind::Argument,
        "concept" => EntityKind::Concept,
        "claim" => EntityKind::Claim,
        "evidence" => EntityKind::Evidence,
        "theme" => EntityKind::Theme,
        "person" => EntityKind::Person,
        "event" => EntityKind::Event,
        _ => EntityKind::Concept,
    }
}

/// Parse the LLM's JSON response for a skeleton extraction batch.
/// Tolerant of malformed JSON — returns None for unparseable responses
/// rather than failing the whole pipeline.
///
/// Handles two entity formats:
/// - `"key_entities": [{"name": "X", "kind": "Character"}]` (preferred)
/// - `"key_entities": ["X", "Y"]` (fallback — kind defaults to Concept)
fn parse_skeleton_batch(response: &str, batch_start: usize) -> Option<Vec<SkeletonBatchEntry>> {
    let trimmed = response.trim();
    let json_start = trimmed.find('[')?;
    let json_end = trimmed.rfind(']')?;
    if json_end <= json_start {
        return None;
    }
    let json_str = &trimmed[json_start..=json_end];

    let arr: Vec<serde_json::Value> = serde_json::from_str(json_str).ok()?;

    let mut entries = Vec::new();
    for (i, obj) in arr.iter().enumerate() {
        let function_str = obj.get("function")?.as_str()?;
        let function = match function_str.to_lowercase().as_str() {
            "introduces" => SectionFunction::Introduces,
            "develops" => SectionFunction::Develops,
            "complicates" => SectionFunction::Complicates,
            "resolves" => SectionFunction::Resolves,
            "transitions" => SectionFunction::Transitions,
            "evidences" => SectionFunction::Evidences,
            _ => SectionFunction::Develops,
        };

        // Parse entities — handle both object and string formats.
        let entity_names_and_kinds: Vec<(String, EntityKind)> = obj
            .get("key_entities")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        if let Some(obj) = v.as_object() {
                            // {"name": "X", "kind": "Character"}
                            let name = obj.get("name")?.as_str()?.to_string();
                            let kind = obj
                                .get("kind")
                                .and_then(|k| k.as_str())
                                .map(parse_entity_kind)
                                .unwrap_or(EntityKind::Concept);
                            Some((name, kind))
                        } else {
                            v.as_str().map(|s| (s.to_string(), EntityKind::Concept))
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let is_moment = obj
            .get("is_structural_moment")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let moment_description = if is_moment {
            obj.get("moment_description")
                .and_then(|v| v.as_str())
                .map(String::from)
        } else {
            None
        };

        entries.push(SkeletonBatchEntry {
            chunk_index: batch_start + i,
            function,
            entity_names_and_kinds,
            moment_description,
        });
    }

    Some(entries)
}

/// Parse the router's JSON response into a DocumentAssetOperation.
/// Falls back to RAG if the response is unparseable.
fn parse_route_response(response: &str, original_request: &str) -> Result<DocumentAssetOperation> {
    let trimmed = response.trim();
    // Find JSON object in the response.
    let json_start = trimmed.find('{');
    let json_end = trimmed.rfind('}');

    if let (Some(start), Some(end)) = (json_start, json_end) {
        if end > start {
            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&trimmed[start..=end]) {
                let op = obj.get("op").and_then(|v| v.as_str()).unwrap_or("rag");
                return Ok(match op {
                    "synthesis" => DocumentAssetOperation::Synthesis {
                        focus: obj
                            .get("focus")
                            .and_then(|v| v.as_str())
                            .unwrap_or(original_request)
                            .to_string(),
                        entities: obj
                            .get("entities")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    },
                    "aggregation" => DocumentAssetOperation::Aggregation {
                        query: obj
                            .get("query")
                            .and_then(|v| v.as_str())
                            .unwrap_or(original_request)
                            .to_string(),
                    },
                    "transformation" => DocumentAssetOperation::Transformation,
                    "off_topic" => DocumentAssetOperation::OffTopic {
                        reason: obj
                            .get("reason")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unrelated question")
                            .to_string(),
                    },
                    _ => DocumentAssetOperation::Rag {
                        query: obj
                            .get("query")
                            .and_then(|v| v.as_str())
                            .unwrap_or(original_request)
                            .to_string(),
                    },
                });
            }
        }
    }

    // Fallback: unparseable response → treat as RAG.
    Ok(DocumentAssetOperation::Rag {
        query: original_request.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property that makes the GLiNER swap safe: the extractor
    /// contract returns LOWER-CASED names, but what we store must be
    /// cased the way the document casts it — these strings reach the
    /// briefing and the segment-naming prompt.
    #[test]
    fn attribution_recovers_document_casing_from_lowercased_names() {
        let batch = [
            chunk("Mr Verloc walked on. Stevie followed him."),
            chunk("Later, Chief Inspector Heat considered the case."),
        ];
        // Exactly what `EntityExtractor::extract_entities` promises.
        let names = vec![
            "mr verloc".to_string(),
            "stevie".to_string(),
            "chief inspector heat".to_string(),
        ];

        let entries = attribute_entity_names(names, 0, &batch);
        let first: Vec<&str> = entries[0]
            .entity_names_and_kinds
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert!(first.contains(&"Mr Verloc"), "{first:?}");
        assert!(first.contains(&"Stevie"), "{first:?}");
        let second: Vec<&str> = entries[1]
            .entity_names_and_kinds
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(second, vec!["Chief Inspector Heat"], "{second:?}");
    }

    /// A name no chunk contains verbatim still lands somewhere rather
    /// than vanishing from the index.
    #[test]
    fn attribution_falls_back_to_first_chunk_for_unmatched_names() {
        let batch = [chunk("The shop was dim."), chunk("Rain fell.")];
        let entries = attribute_entity_names(vec!["the Professor".to_string()], 7, &batch);
        assert_eq!(entries[0].chunk_index, 7, "batch_start offset preserved");
        assert_eq!(entries[0].entity_names_and_kinds.len(), 1);
        // Unmatched keeps the caller's spelling — there's no document
        // occurrence to recover casing from.
        assert_eq!(entries[0].entity_names_and_kinds[0].0, "the Professor");
        assert!(entries[1].entity_names_and_kinds.is_empty());
    }

    /// The LLM path must keep behaving exactly as before the split.
    #[test]
    fn llm_window_parse_still_attributes_across_chunks() {
        let batch = [
            chunk("Winnie said nothing."),
            chunk("Winnie and Ossipon spoke."),
        ];
        let entries = parse_window_skeleton_batch("Winnie, Ossipon", 0, &batch);
        let names0: Vec<&str> = entries[0]
            .entity_names_and_kinds
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        let names1: Vec<&str> = entries[1]
            .entity_names_and_kinds
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(names0, vec!["Winnie"]);
        assert_eq!(names1, vec!["Winnie", "Ossipon"]);
    }

    #[test]
    fn entity_name_list_tolerates_answer_prefix_and_blank_items() {
        let names = parse_entity_name_list("Answer: Stevie, , Mr Verloc,\n Winnie ");
        assert_eq!(names, vec!["Stevie", "Mr Verloc", "Winnie"]);
    }

    #[test]
    fn parse_route_response_parses_off_topic_with_reason() {
        let resp = r#"{"op": "off_topic", "reason": "different domain"}"#;
        match parse_route_response(resp, "user q").unwrap() {
            DocumentAssetOperation::OffTopic { reason } => {
                assert_eq!(reason, "different domain");
            }
            other => panic!("expected OffTopic, got {other:?}"),
        }
    }

    #[test]
    fn parse_route_response_off_topic_default_reason() {
        let resp = r#"{"op": "off_topic"}"#;
        match parse_route_response(resp, "user q").unwrap() {
            DocumentAssetOperation::OffTopic { reason } => {
                assert_eq!(reason, "unrelated question");
            }
            other => panic!("expected OffTopic, got {other:?}"),
        }
    }

    #[test]
    fn parse_route_response_unknown_op_falls_back_to_rag() {
        let resp = r#"{"op": "nonsense"}"#;
        match parse_route_response(resp, "user q").unwrap() {
            DocumentAssetOperation::Rag { query } => {
                assert_eq!(query, "user q");
            }
            other => panic!("expected Rag, got {other:?}"),
        }
    }

    // ── Self-reference detection ────────────────────────────────

    #[test]
    fn self_reference_matches_common_phrasings() {
        for q in &[
            "Can you summarize this document?",
            "What is this paper about?",
            "summarize the text",
            "What does this say about consciousness?",
            "give me a summary of this",
            "Tell me what the attached is about",
            "what's this about?",
            "Summarize the article please",
        ] {
            assert!(
                detect_self_reference(q),
                "expected self-reference match for {q:?}"
            );
        }
    }

    #[test]
    fn self_reference_rejects_off_topic_questions() {
        for q in &[
            "What is the difference between Theravada and Zen buddhism?",
            "Explain quantum superposition",
            "Who was Erwin Schrödinger?",
            "Give me a recipe for banana bread",
        ] {
            assert!(
                !detect_self_reference(q),
                "expected NO self-reference match for {q:?}"
            );
        }
    }

    #[test]
    fn self_reference_is_case_insensitive() {
        assert!(detect_self_reference("SUMMARIZE THIS DOCUMENT"));
        assert!(detect_self_reference("What Is This Paper About?"));
    }

    // ── Filename grounding ───────────────────────────────────────

    fn test_asset(title: &str, filename: &str) -> DocumentAsset {
        DocumentAsset {
            id: "t".to_string(),
            title: title.to_string(),
            filename: filename.to_string(),
            file_size_mb: 0.0,
            word_count: 0,
            chunk_count: 0,
            document_type: DocumentTypeTag::Unknown,
            ingested_at: chrono::Utc::now(),
            index_id: "t".to_string(),
            skeleton: None,
            state: AssetState::PartiallyReady,
            owner: None,
        }
    }

    #[test]
    fn filename_tokens_drops_stopwords_numbers_short_tokens() {
        let asset = test_asset(
            "11. Erwin Schrodinger   What is Life  1944",
            "11._Erwin_Schrodinger_-_What_is_Life__1944_.pdf",
        );
        let toks = filename_tokens(&asset);
        assert!(toks.contains(&"erwin".to_string()));
        assert!(toks.contains(&"schrodinger".to_string()));
        assert!(toks.contains(&"life".to_string()));
        // stopwords dropped
        assert!(!toks.iter().any(|t| t == "the" || t == "is"));
        // short / digit-only tokens dropped
        assert!(!toks.iter().any(|t| t == "11" || t == "1944"));
        // extensions stripped by the stopword list
        assert!(!toks.contains(&"pdf".to_string()));
    }

    #[test]
    fn mentions_document_matches_author_name() {
        let asset = test_asset(
            "Erwin Schrodinger - What is Life",
            "Erwin_Schrodinger_What_is_Life.pdf",
        );
        let toks = filename_tokens(&asset);
        assert!(mentions_document(
            &toks,
            "What does Schrödinger say about consciousness?"
        ));
        assert!(mentions_document(&toks, "Explain Erwin's thesis"));
    }

    #[test]
    fn mentions_document_rejects_unrelated_question() {
        let asset = test_asset(
            "Erwin Schrodinger - What is Life",
            "Erwin_Schrodinger_What_is_Life.pdf",
        );
        let toks = filename_tokens(&asset);
        assert!(!mentions_document(
            &toks,
            "What is the difference between Theravada and Zen buddhism?"
        ));
        assert!(!mentions_document(&toks, "Who won the 2018 World Cup?"));
    }

    #[test]
    fn mentions_document_unicode_folds_to_ascii() {
        let asset = test_asset("Erwin Schrödinger", "Erwin_Schrodinger.pdf");
        let toks = filename_tokens(&asset);
        // Token is folded when extracted — "schrödinger" → "schrodinger".
        assert!(toks.contains(&"schrodinger".to_string()));
        // Question with the accented form still matches because the query
        // is ASCII-folded at match time.
        assert!(mentions_document(&toks, "What did Schrödinger argue?"));
    }

    #[test]
    fn short_snippet_truncates_at_word_boundary() {
        let content = "The aperiodic crystal described by Schrödinger is a molecular \
                       structure found in chromosomes that differs radically from periodic \
                       crystals studied by physicists. It stores hereditary information.";
        let snip = short_snippet(content, 60);
        assert!(snip.ends_with("..."));
        assert!(!snip.contains("Schrödinger") || snip.len() <= 60 + "...".len() + 10 /* slack */);
        // Snippet ends on a word boundary — no mid-word cut before the ellipsis.
        let pre = snip.trim_end_matches("...");
        assert!(pre.ends_with(|c: char| c.is_ascii_alphanumeric() || c == ','));
    }

    #[test]
    fn short_snippet_returns_input_when_under_max() {
        assert_eq!(short_snippet("hello", 100), "hello");
    }

    #[test]
    fn short_snippet_handles_multibyte_at_boundary() {
        // Regression: Conrad's text uses U+201C / U+201D curly quotes
        // (3 bytes each in UTF-8). Passing `max` such that the byte
        // index lands inside one of those quotes used to panic with
        // "end byte index N is not a char boundary". This is the input
        // shape that book-report bench v1.1 caught on the Tier-4
        // winnie_incurious_motif question.
        let content = "Mr Verloc observed quietly\u{201C}I have no means of action upon the police here.\u{201D} Vladimir replied at length.";
        // Find the byte index of the opening curly quote and ask for a
        // snippet that lands inside it.
        let quote_byte = content
            .find('\u{201C}')
            .expect("test fixture must contain U+201C");
        let inside_quote = quote_byte + 1; // mid-character; would panic on raw slice
        let snip = short_snippet(content, inside_quote);
        // Must not panic AND must end with the ellipsis sentinel.
        assert!(
            snip.ends_with("..."),
            "expected snippet to end with `...`, got: {snip:?}"
        );
    }

    #[test]
    fn mentions_document_word_boundary_avoids_partial_matches() {
        // Token "life" must not match inside "wildlife".
        let asset = test_asset("What is Life", "What_is_Life.pdf");
        let toks = filename_tokens(&asset);
        assert!(toks.contains(&"life".to_string()));
        assert!(!mentions_document(
            &toks,
            "Tell me about the wildlife of Africa"
        ));
        // But "life" as a standalone word still matches.
        assert!(mentions_document(&toks, "Explain the meaning of life"));
    }

    #[test]
    fn parse_route_response_existing_variants_still_work() {
        // Guard against regression in the off_topic branch addition.
        let rag = parse_route_response(r#"{"op":"rag","query":"foo"}"#, "orig").unwrap();
        assert!(matches!(rag, DocumentAssetOperation::Rag { .. }));

        let syn = parse_route_response(
            r#"{"op":"synthesis","focus":"themes","entities":["A","B"]}"#,
            "orig",
        )
        .unwrap();
        match syn {
            DocumentAssetOperation::Synthesis { focus, entities } => {
                assert_eq!(focus, "themes");
                assert_eq!(entities, vec!["A".to_string(), "B".to_string()]);
            }
            other => panic!("expected Synthesis, got {other:?}"),
        }

        let agg = parse_route_response(r#"{"op":"aggregation","query":"x"}"#, "orig").unwrap();
        assert!(matches!(agg, DocumentAssetOperation::Aggregation { .. }));

        let xform = parse_route_response(r#"{"op":"transformation"}"#, "orig").unwrap();
        assert!(matches!(xform, DocumentAssetOperation::Transformation));
    }

    // ── TextTiling boundary detection ───────────────────────────

    /// Build a synthetic embedding sequence: `cluster_count` clusters
    /// of `per_cluster` chunks each, where within-cluster chunks share
    /// a near-identical embedding and between-cluster chunks are
    /// near-orthogonal. The expected break pattern is "false ×
    /// (per_cluster-1), true, false × (per_cluster-1), true, ..."
    fn synthetic_clusters(cluster_count: usize, per_cluster: usize) -> Vec<Vec<f32>> {
        let mut out = Vec::new();
        for c in 0..cluster_count {
            let mut base = vec![0.01; 8];
            base[c % 8] = 1.0;
            for k in 0..per_cluster {
                // Tiny within-cluster jitter so cosine isn't exactly 1.0.
                let mut v = base.clone();
                v[(c + k + 1) % 8] += 0.02;
                out.push(v);
            }
        }
        out
    }

    #[test]
    fn texttiling_fires_breaks_at_cluster_transitions() {
        // 3 clusters × 4 chunks = 12 chunks, 11 gaps. Expected:
        // breaks at gaps 3 and 7 (between cluster 0/1 and 1/2).
        let embeddings = synthetic_clusters(3, 4);
        let breaks = detect_segment_boundaries(&embeddings, 3, 0.5);
        assert_eq!(breaks.len(), 11);

        // The two cluster-transition gaps should be flagged.
        assert!(breaks[3], "expected break at gap 3 (cluster 0→1)");
        assert!(breaks[7], "expected break at gap 7 (cluster 1→2)");

        // Within-cluster gaps should be quiet. We allow one or two
        // false positives in the noisy jitter — the test checks the
        // signal-to-noise floor, not perfection.
        let within_breaks = breaks
            .iter()
            .enumerate()
            .filter(|(i, b)| **b && *i != 3 && *i != 7)
            .count();
        assert!(
            within_breaks <= 1,
            "too many false-positive within-cluster breaks: {within_breaks}"
        );
    }

    #[test]
    fn texttiling_flat_embeddings_emit_no_breaks() {
        // All identical embeddings → similarity is uniformly high,
        // depth signal is flat, no breaks should fire.
        let embeddings = vec![vec![0.5, 0.5, 0.5, 0.5]; 10];
        let breaks = detect_segment_boundaries(&embeddings, 3, 1.0);
        assert_eq!(breaks.len(), 9);
        assert!(
            breaks.iter().all(|b| !b),
            "flat signal should produce no breaks"
        );
    }

    #[test]
    fn texttiling_short_input_returns_empty() {
        assert!(detect_segment_boundaries(&[], 3, 1.0).is_empty());
        assert!(
            detect_segment_boundaries(&[vec![1.0, 0.0]], 3, 1.0).is_empty(),
            "single chunk has no gaps"
        );
    }

    #[test]
    fn texttiling_strict_threshold_silences_marginal_breaks() {
        // Same clusters as before but with a stricter k. The
        // cluster-transition gaps still fire (their depth is much
        // larger than within-cluster noise), but anything marginal
        // gets filtered out.
        let embeddings = synthetic_clusters(3, 4);
        let lax = detect_segment_boundaries(&embeddings, 3, 0.3);
        let strict = detect_segment_boundaries(&embeddings, 3, 1.5);
        let lax_count = lax.iter().filter(|b| **b).count();
        let strict_count = strict.iter().filter(|b| **b).count();
        assert!(
            strict_count <= lax_count,
            "stricter threshold must not produce more breaks (lax {lax_count} vs strict {strict_count})"
        );
    }

    // ── Motif extraction ─────────────────────────────────────────

    fn chunk(content: &str) -> TextChunk {
        TextChunk {
            content: content.to_string(),
            index: 0,
        }
    }

    #[test]
    fn motif_candidates_surface_recurring_word_across_many_chunks() {
        // "incurious" appears in 5 of 20 chunks (25%, well within
        // the 60% topicality ceiling) → must surface.
        // "hat" appears in 1 chunk → must not surface (df < 3 floor).
        // "the" appears in many chunks → must not surface (stoplisted).
        let mut chunks = vec![
            chunk("Winnie was incurious about the matter and went on with her work."),
            chunk("The professor walked alone, carrying his frail explosive device."),
            chunk("Stevie drew his circles, oblivious to the world around him."),
            chunk("Mrs Verloc remained incurious during the long evening at home."),
            chunk("The cab horse was whipped and Stevie's hat fell to the pavement."),
            chunk("Winnie's incurious eyes lighted on the broken figure of her brother."),
            chunk("The narrator notes Mrs Verloc was an incurious person by nature."),
            chunk("An incurious silence settled over the parlour after the explosion."),
        ];
        // Pad with topically-distinct chunks so the document is large
        // enough that "incurious" sits comfortably under the 60% ceiling.
        for i in 0..12 {
            chunks.push(chunk(&format!(
                "Topic {i} unrelated paragraph about unconnected events and \
                 separate matters with totally distinct vocabulary."
            )));
        }
        let cands = extract_motif_candidates(&chunks, 20);
        let terms: Vec<&str> = cands.iter().map(|c| c.term.as_str()).collect();
        assert!(
            terms.contains(&"incurious"),
            "expected 'incurious' in candidates; got {terms:?}"
        );
        assert!(
            !terms.contains(&"hat"),
            "single-chunk term must not surface"
        );
        assert!(!terms.contains(&"the"), "stoplisted word must not surface");
    }

    #[test]
    fn motif_candidates_drop_words_in_too_many_chunks() {
        // A word in >60% of chunks is topical not motivic. Build a
        // doc where "rust" appears in 8/10 chunks — should NOT surface
        // even though it's frequent.
        let mut chunks = Vec::new();
        for _ in 0..8 {
            chunks.push(chunk("This passage talks about rust programming."));
        }
        chunks.push(chunk("This passage talks about Python programming."));
        chunks.push(chunk("This passage talks about Java programming."));
        let cands = extract_motif_candidates(&chunks, 20);
        let terms: Vec<&str> = cands.iter().map(|c| c.term.as_str()).collect();
        assert!(
            !terms.contains(&"rust"),
            "topical term in 80% of chunks must be excluded; got {terms:?}"
        );
    }

    #[test]
    fn motif_candidates_handle_possessives_and_contractions() {
        // "winnie's" should normalize to "winnie" via the 's strip.
        // Need ≥10 chunks total so 3 occurrences stay under the 60% ceiling.
        let mut chunks = vec![
            chunk("Winnie's act was deliberate. Winnie's eyes were closed."),
            chunk("This is about Winnie's life and Winnie's choice."),
            chunk("The novel turns on Winnie's transformation through trauma."),
        ];
        for i in 0..8 {
            chunks.push(chunk(&format!(
                "Topic {i} unrelated paragraph with separate distinct vocabulary."
            )));
        }
        let cands = extract_motif_candidates(&chunks, 20);
        let terms: Vec<&str> = cands.iter().map(|c| c.term.as_str()).collect();
        assert!(
            terms.contains(&"winnie"),
            "possessive strip should normalise 'winnie's' → 'winnie'; got {terms:?}"
        );
        assert!(
            !terms.iter().any(|t| t.ends_with('\'')),
            "no candidate should retain a trailing apostrophe; got {terms:?}"
        );
    }

    #[test]
    fn parse_motif_classification_extracts_array_from_noisy_response() {
        // Model often wraps the JSON in extra text — parser must
        // skip prose and find the [...] span.
        let resp = r#"Sure! Here are the motifs: ["incurious", "circles", "professor"] — let me know if you want more."#;
        let set = parse_motif_classification(resp);
        assert_eq!(set.len(), 3);
        assert!(set.contains("incurious"));
        assert!(set.contains("circles"));
        assert!(set.contains("professor"));
    }

    #[test]
    fn parse_motif_classification_returns_empty_on_no_array() {
        let set = parse_motif_classification("the model refused to comply");
        assert!(set.is_empty());
    }

    #[test]
    fn parse_motif_classification_lowercases() {
        let set = parse_motif_classification(r#"["Incurious", "CIRCLES"]"#);
        assert!(set.contains("incurious"));
        assert!(set.contains("circles"));
    }

    #[test]
    fn cosine_similarity_basic_cases() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
        // Mismatched length → 0.0 sentinel.
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 0.0]), 0.0);
        // Zero magnitude → 0.0 sentinel.
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }
}
