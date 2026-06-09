// SPDX-License-Identifier: AGPL-3.0-or-later
//! Investigation enrichment pipeline.
//!
//! Builds a typed-relationship graph from a corpus by asking an
//! LLM to extract instances of recipe-declared entity and
//! relationship types from each chunk, then runs declared graph
//! patterns (cycles, role-overlap, attribute thresholds) over the
//! resulting graph.
//!
//! Distinct from the existing `Pipeline` trait (which is heavily
//! shaped around the literary-atlas 8-phase flow). The
//! investigation pipeline has a different structure
//! (Extract → Coalesce → DetectPatterns) and a different output
//! shape (typed entities + relationships + findings rather than
//! atom-tagged sketches). Lives as a standalone module rather
//! than fighting the atlas trait.
//!
//! Public entry point: [`run_investigation`]. Callers supply:
//!
//! - the [`Recipe`] (carries the entity / relationship / pattern
//!   schema declarations).
//! - the corpus chunks to extract from.
//! - a [`ChatCompletionFn`] that maps `ChatPrompt` → response
//!   string. Production wraps the daemon's chat slot; tests pass a
//!   deterministic closure returning canned JSON.
//! - the output directory under which `investigation/` will be
//!   created.
//!
//! Returns the in-memory results AND writes the three JSON files
//! to disk, so callers can react to findings synchronously while
//! also persisting the artefact for the audit step.

pub mod aggregate;
pub mod checkpoint;
pub mod extract;
pub mod graph;
pub mod normalize;
pub mod patterns;
pub mod recoalesce;

use std::path::Path;

use crate::enrichment::pipeline::types::ChatCompletionFn;
use crate::error::{Error, Result};
use crate::recipe::Recipe;

pub use extract::{ChunkInput, ExtractedRelationship};
pub use graph::{
    Entity, Evidence, PatternFinding, PatternKind, Relationship, ENTITIES_FILENAME,
    FINDINGS_FILENAME, INVESTIGATION_DIRNAME, RELATIONSHIPS_FILENAME,
};

/// Result of running the investigation pipeline. The same data
/// is also written to JSON under `<output_dir>/investigation/`.
#[derive(Debug, Clone)]
pub struct InvestigationOutput {
    pub entities: Vec<Entity>,
    pub relationships: Vec<Relationship>,
    pub findings: Vec<PatternFinding>,
}

/// Execute the investigation pipeline end-to-end against the
/// supplied chunks. Returns the materialized graph + findings and
/// writes the three JSON files atomically to
/// `<output_dir>/investigation/`.
///
/// The pipeline phases:
///
/// 1. **Extract** — for each chunk, send a schema-driven prompt to
///    the LLM and parse the typed relationships back. Mention-form
///    entities are recorded; canonical resolution happens in
///    coalesce.
/// 2. **Coalesce** — group mentions by `(entity_type, lowercased
///    canonical name)` to produce a single canonical [`Entity`]
///    record per real-world thing, with surface-form variants
///    captured as aliases. Extracted relationships get rewritten
///    to reference canonical entity ids.
/// 3. **DetectPatterns** — run every declared
///    [`PatternDecl`](crate::recipe::PatternDecl) over the
///    materialized graph (in-memory; petgraph for cycle
///    detection).
///
/// The recipe MUST declare `enrichment.enrichment_type =
/// "investigation"`; otherwise this function refuses with
/// `Error::Recipe`. That guard catches misconfigured recipes
/// loudly instead of silently emitting empty outputs.
pub async fn run_investigation<'a>(
    recipe: &Recipe,
    chunks: &[ChunkInput<'a>],
    chat: ChatCompletionFn,
    output_dir: &Path,
) -> Result<InvestigationOutput> {
    let enrichment = recipe.enrichment.as_ref().ok_or_else(|| {
        Error::Recipe("investigation pipeline requires an [enrichment] block".into())
    })?;
    if enrichment.enrichment_type != "investigation" {
        return Err(Error::Recipe(format!(
            "investigation pipeline expected `enrichment.type = \"investigation\"`, got \"{}\"",
            enrichment.enrichment_type
        )));
    }
    if enrichment.entity_types.is_empty() {
        return Err(Error::Recipe(
            "investigation pipeline requires at least one [[enrichment.entity_types]] declaration"
                .into(),
        ));
    }
    if enrichment.relationship_types.is_empty() {
        return Err(Error::Recipe(
            "investigation pipeline requires at least one [[enrichment.relationship_types]] declaration"
                .into(),
        ));
    }

    // Phase 1 — Extract per chunk.
    let mut all_extractions: Vec<(String, ExtractedRelationship)> =
        Vec::with_capacity(chunks.len() * 4);
    let mut all_entities: Vec<(String, extract::ExtractedEntity)> =
        Vec::with_capacity(chunks.len() * 4);

    // Resume from any prior partial run. The append-only checkpoint
    // (`investigation/_phase1_checkpoint.jsonl`) records every chunk whose
    // extraction has settled — so a crash mid-run (e.g. an unrecoverable
    // daemon fault) never discards completed work. We seed the accumulators
    // from the recorded successes and skip every already-processed chunk,
    // turning a multi-hour 35B pass into a restartable one.
    let ckpt_path = checkpoint::checkpoint_path(&output_dir.join(INVESTIGATION_DIRNAME));
    let prior = checkpoint::read_checkpoint(&ckpt_path)?;
    let processed = checkpoint::processed_ids(&prior);
    for (chunk_id, extracted) in checkpoint::collapse_successes(&prior) {
        for ent in extracted.entities {
            all_entities.push((chunk_id.clone(), ent));
        }
        for rel in extracted.relationships {
            all_extractions.push((chunk_id.clone(), rel));
        }
    }
    let resumed = processed.len();
    if resumed > 0 {
        tracing::info!(
            resumed,
            total = chunks.len(),
            "investigation: resuming Phase 1 from checkpoint — skipping already-processed chunks"
        );
    }

    // A chunk's extraction must survive a *transient* daemon fault. Two
    // failure modes are non-fatal and worth a retry, NOT a run abort:
    //   1. inference-call error — e.g. an intermittent `MTP process(verify)
    //      failed` 503 from the daemon's speculative-decode path on a
    //      schema-constrained call. The daemon recovers per-request and the
    //      chunk content is fine, so a re-roll almost always succeeds.
    //   2. unparseable reply — a schema-mask hiccup on noisy OCR; a fresh
    //      sample frequently yields clean JSON.
    // Retry each chunk with exponential backoff and skip it (loudly) only once
    // every attempt is exhausted, so one ~sub-1% glitch can't discard a
    // multi-hour run over hundreds of chunks — and the retries keep the graph
    // COMPLETE rather than silently dropping a node.
    const MAX_CHUNK_ATTEMPTS: u32 = 4;
    let mut skipped = 0usize;
    for chunk in chunks {
        // Already handled in a prior run — its result is seeded above.
        if processed.contains(chunk.chunk_id) {
            continue;
        }
        let prompt = extract::compose_extract_prompt(
            chunk,
            &enrichment.entity_types,
            &enrichment.relationship_types,
        );
        let mut parsed: Option<extract::ExtractedChunk> = None;
        for attempt in 1..=MAX_CHUNK_ATTEMPTS {
            match (chat)(&prompt).await {
                Ok(response) => match extract::parse_extract_response(&response) {
                    Ok(p) => {
                        parsed = Some(p);
                        break;
                    }
                    Err(e) => tracing::warn!(
                        chunk_id = %chunk.chunk_id,
                        attempt,
                        max_attempts = MAX_CHUNK_ATTEMPTS,
                        error = %e,
                        "investigation: extraction reply did not parse — retrying"
                    ),
                },
                Err(e) => tracing::warn!(
                    chunk_id = %chunk.chunk_id,
                    attempt,
                    max_attempts = MAX_CHUNK_ATTEMPTS,
                    error = %e,
                    "investigation: extraction inference call failed — retrying"
                ),
            }
            if attempt < MAX_CHUNK_ATTEMPTS {
                // 0.5s, 1s, 2s — modest next to the ~20s/call cost; lets a
                // per-request daemon glitch clear before the next attempt.
                let backoff_ms = 500u64 << (attempt - 1);
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            }
        }
        let parsed = match parsed {
            Some(p) => p,
            None => {
                skipped += 1;
                tracing::warn!(
                    chunk_id = %chunk.chunk_id,
                    attempts = MAX_CHUNK_ATTEMPTS,
                    "investigation: skipping chunk — all extraction attempts exhausted"
                );
                // Record the skip so a resume doesn't retry it forever.
                checkpoint::append_checkpoint(
                    &ckpt_path,
                    &checkpoint::ChunkCheckpointEntry::Skipped {
                        chunk_id: chunk.chunk_id.to_string(),
                        reason: format!("extraction failed after {MAX_CHUNK_ATTEMPTS} attempts"),
                    },
                )?;
                continue;
            }
        };
        // Checkpoint the success BEFORE folding it into the accumulators, so a
        // crash on the very next chunk still finds this one recorded.
        checkpoint::append_checkpoint(
            &ckpt_path,
            &checkpoint::ChunkCheckpointEntry::Success {
                chunk_id: chunk.chunk_id.to_string(),
                extracted: parsed.clone(),
            },
        )?;
        for ent in parsed.entities {
            all_entities.push((chunk.chunk_id.to_string(), ent));
        }
        for rel in parsed.relationships {
            all_extractions.push((chunk.chunk_id.to_string(), rel));
        }
    }
    if skipped > 0 {
        tracing::warn!(
            skipped,
            resumed,
            total = chunks.len(),
            "investigation: Phase 1 extraction complete WITH skipped chunks (retries exhausted)"
        );
    } else {
        tracing::info!(
            resumed,
            total = chunks.len(),
            "investigation: Phase 1 extraction complete — every chunk parsed"
        );
    }

    // Phase 2 — Coalesce entities; rewrite relationships to canonical ids.
    // The recipe supplies the coalescing vocabulary (aliases, suffixes, …);
    // the Normalizer applies it. Endpoint ids below route through the SAME
    // normalizer so they resolve to the coalesced entity (no dangling edges).
    let normalizer = normalize::Normalizer::from_recipe(recipe);
    let entities_map =
        extract::group_extracted_entities(&normalizer, &all_entities, &all_extractions);
    let mut entities: Vec<Entity> = entities_map.values().cloned().collect();
    entities.sort_by(|a, b| a.id.cmp(&b.id));

    let mut relationships: Vec<Relationship> = Vec::with_capacity(all_extractions.len());
    for (i, (chunk_id, ex)) in all_extractions.iter().enumerate() {
        let from_id = normalizer.entity_id(&ex.from_type, &ex.from_entity);
        let to_id = normalizer.entity_id(&ex.to_type, &ex.to_entity);
        relationships.push(Relationship {
            id: format!("r-{i}"),
            from_entity_id: from_id,
            to_entity_id: to_id,
            relationship_type: ex.relationship_type.clone(),
            attributes: ex.attributes.clone(),
            evidence: Evidence {
                chunk_id: chunk_id.clone(),
                excerpt: ex.verbatim_excerpt.clone(),
            },
            confidence: ex.confidence,
        });
    }

    // Phase 2.5 — Deterministic aggregation. For each declared Threshold
    // pattern whose target attribute is NOT already an edge attribute the
    // LLM emits, treat it as a count aggregation: stamp the distinct
    // edge-count on the target entity so count-based thresholds (e.g.
    // "installations with > N sightings") can fire via the entity-attribute
    // scan. The edge-attribute guard means genuine edge thresholds (e.g.
    // revenue percentage on a `revenue` edge) are left untouched.
    for pattern in &enrichment.patterns {
        if let crate::recipe::PatternDecl::Threshold {
            edge_type,
            attribute,
            ..
        } = pattern
        {
            let attr_on_edges = relationships
                .iter()
                .any(|r| r.relationship_type == *edge_type && r.attributes.contains_key(attribute));
            if !attr_on_edges {
                aggregate::stamp_edge_counts(&mut entities, &relationships, edge_type, attribute);
            }
        }
    }

    // Phase 3 — Run declared pattern detectors.
    let findings = patterns::detect_all(&enrichment.patterns, &entities, &relationships);

    // Persist + return. Persistence is best-effort: a write error
    // is surfaced to the caller, who can decide whether to retry.
    graph::write_outputs(output_dir, &entities, &relationships, &findings)?;

    // Phase 1's results are now folded into the durable graph outputs, so the
    // resume checkpoint has served its purpose. Clear it so a *fresh*
    // re-enrich (e.g. after a recipe/prompt change) starts from zero rather
    // than skipping every chunk against a stale checkpoint. A crash before
    // this point leaves the checkpoint in place for the next run to resume.
    checkpoint::clear_checkpoint(&ckpt_path)?;

    Ok(InvestigationOutput {
        entities,
        relationships,
        findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{EntityTypeDecl, PatternDecl, RelationshipTypeDecl};
    use std::sync::Arc;

    /// A scripted chat closure that returns a canned response
    /// the first time it's invoked, then an empty array for every
    /// subsequent invocation. Lets us drive the pipeline with one
    /// chunk producing relationships and one chunk producing none.
    fn scripted_chat(canned: &'static str) -> ChatCompletionFn {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        Arc::new(move |_prompt| {
            let calls = calls.clone();
            Box::pin(async move {
                let n = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let response = if n == 0 {
                    canned.to_string()
                } else {
                    r#"{"relationships": []}"#.to_string()
                };
                Ok(response)
            })
        })
    }

    fn make_recipe() -> Recipe {
        let toml = r#"
[corpus]
id = "demo"
name = "demo"

[acquire]
type = "bulk_download"
url = "https://example.com/data.zip"

[extract]
type = "html"

[chunk]
type = "paragraph"

[enrichment]
enabled = true
type = "investigation"

[[enrichment.entity_types]]
name = "company"
description = "A corporation"
attributes = ["name", "ticker"]

[[enrichment.relationship_types]]
name = "investment"
description = "A invested in B"
attributes = ["amount_usd", "date"]

[[enrichment.relationship_types]]
name = "revenue"
description = "B's revenue from A"
attributes = ["amount_usd"]

[[enrichment.patterns]]
type = "role_overlap"
name = "invest_in_customer"
description = "A invests in B and B is a customer of A"
[enrichment.patterns.entity_roles]
investor = "investment.from"
customer = "revenue.to"
"#;
        Recipe::from_toml(toml).expect("recipe parses")
    }

    #[tokio::test]
    async fn end_to_end_extracts_and_detects_role_overlap() {
        let recipe = make_recipe();
        let chunks = vec![
            ChunkInput {
                chunk_id: "chunk-0",
                source_title: Some("MSFT 10-K"),
                content:
                    "Microsoft invested $13B in OpenAI; OpenAI's largest customer is Microsoft.",
            },
            ChunkInput {
                chunk_id: "chunk-1",
                source_title: None,
                content: "Distractor chunk with no relationships.",
            },
        ];

        // Canned response: Microsoft invests in OpenAI AND OpenAI's
        // revenue flows to Microsoft. That's exactly the
        // invest_in_customer overlap pattern.
        let canned = r#"
{
    "relationships": [
        {
            "from_entity": "Microsoft",
            "to_entity": "OpenAI",
            "from_type": "company",
            "to_type": "company",
            "type": "investment",
            "attributes": {"amount_usd": 13000000000},
            "verbatim_excerpt": "Microsoft invested $13B in OpenAI",
            "confidence": 0.95
        },
        {
            "from_entity": "OpenAI",
            "to_entity": "Microsoft",
            "from_type": "company",
            "to_type": "company",
            "type": "revenue",
            "attributes": {"amount_usd": 1000000000},
            "verbatim_excerpt": "OpenAI's largest customer is Microsoft.",
            "confidence": 0.92
        }
    ]
}
"#;
        let chat = scripted_chat(canned);
        let dir = tempfile::tempdir().unwrap();
        let out = run_investigation(&recipe, &chunks, chat, dir.path())
            .await
            .unwrap();

        // Two entities (Microsoft, OpenAI), two relationships, one
        // pattern finding.
        assert_eq!(out.entities.len(), 2);
        assert_eq!(out.relationships.len(), 2);
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.findings[0].pattern_name, "invest_in_customer");

        // JSON files are on disk.
        let invest_dir = dir.path().join(INVESTIGATION_DIRNAME);
        assert!(invest_dir.join(ENTITIES_FILENAME).exists());
        assert!(invest_dir.join(RELATIONSHIPS_FILENAME).exists());
        assert!(invest_dir.join(FINDINGS_FILENAME).exists());
    }

    #[tokio::test]
    async fn refuses_when_enrichment_type_is_not_investigation() {
        let mut recipe = make_recipe();
        recipe.enrichment.as_mut().unwrap().enrichment_type = "atlas".into();
        let chat: ChatCompletionFn =
            Arc::new(|_| Box::pin(async { Ok(r#"{"relationships":[]}"#.to_string()) }));
        let dir = tempfile::tempdir().unwrap();
        let err = run_investigation(&recipe, &[], chat, dir.path())
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("expected `enrichment.type"));
    }

    #[tokio::test]
    async fn refuses_when_no_entity_types_declared() {
        let mut recipe = make_recipe();
        recipe.enrichment.as_mut().unwrap().entity_types.clear();
        let chat: ChatCompletionFn =
            Arc::new(|_| Box::pin(async { Ok(r#"{"relationships":[]}"#.to_string()) }));
        let dir = tempfile::tempdir().unwrap();
        let err = run_investigation(&recipe, &[], chat, dir.path())
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("entity_types"));
    }

    #[tokio::test]
    async fn no_chunks_produces_empty_outputs_on_disk() {
        let recipe = make_recipe();
        let chat: ChatCompletionFn =
            Arc::new(|_| Box::pin(async { Ok(r#"{"relationships":[]}"#.to_string()) }));
        let dir = tempfile::tempdir().unwrap();
        let out = run_investigation(&recipe, &[], chat, dir.path())
            .await
            .unwrap();
        assert!(out.entities.is_empty());
        assert!(out.relationships.is_empty());
        assert!(out.findings.is_empty());
        assert!(dir
            .path()
            .join(INVESTIGATION_DIRNAME)
            .join(ENTITIES_FILENAME)
            .exists());
    }

    /// A *transient* inference fault — the daemon's intermittent
    /// `MTP process(verify) failed` 503 on a schema-constrained call — must
    /// NOT abort the run or drop the chunk. The loop retries with backoff and
    /// recovers the extraction. This closure fails the first two attempts,
    /// then returns a valid one-relationship reply; the recovered chunk must
    /// still yield its two entities + one relationship. `start_paused` lets
    /// the backoff sleeps auto-advance so the test stays fast.
    #[tokio::test(start_paused = true)]
    async fn transient_inference_fault_is_retried_not_dropped() {
        let recipe = make_recipe();
        let chunks = vec![ChunkInput {
            chunk_id: "chunk-0",
            source_title: Some("MSFT 10-K"),
            content: "Microsoft invested $13B in OpenAI.",
        }];

        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let chat: ChatCompletionFn = {
            let attempts = attempts.clone();
            Arc::new(move |_prompt| {
                let attempts = attempts.clone();
                Box::pin(async move {
                    let n = attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n < 2 {
                        Err(crate::error::Error::Serialization(
                            "simulated transient daemon fault: MTP process(verify) failed".into(),
                        ))
                    } else {
                        Ok(r#"{
                            "relationships": [
                                {
                                    "from_entity": "Microsoft",
                                    "to_entity": "OpenAI",
                                    "from_type": "company",
                                    "to_type": "company",
                                    "type": "investment",
                                    "attributes": {"amount_usd": 13000000000},
                                    "verbatim_excerpt": "Microsoft invested $13B in OpenAI",
                                    "confidence": 0.95
                                }
                            ]
                        }"#
                        .to_string())
                    }
                })
            })
        };

        let dir = tempfile::tempdir().unwrap();
        let out = run_investigation(&recipe, &chunks, chat, dir.path())
            .await
            .unwrap();

        // Recovered on the 3rd attempt — the chunk is NOT dropped.
        assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(out.entities.len(), 2);
        assert_eq!(out.relationships.len(), 1);
    }

    /// When *every* attempt fails, the chunk is skipped — never fatal. The
    /// run completes and simply yields no extraction for that chunk, so one
    /// permanently-bad chunk can't discard a multi-hour run.
    #[tokio::test(start_paused = true)]
    async fn exhausted_retries_skip_chunk_without_aborting() {
        let recipe = make_recipe();
        let chunks = vec![ChunkInput {
            chunk_id: "chunk-0",
            source_title: None,
            content: "Whatever.",
        }];
        let chat: ChatCompletionFn = Arc::new(|_| {
            Box::pin(async { Err(crate::error::Error::Serialization("always fails".into())) })
        });
        let dir = tempfile::tempdir().unwrap();
        let out = run_investigation(&recipe, &chunks, chat, dir.path())
            .await
            .unwrap();
        assert!(out.entities.is_empty());
        assert!(out.relationships.is_empty());
    }

    /// Resume: a chunk recorded in a prior run's checkpoint must NOT be
    /// re-sent to the LLM, and its extraction must survive into the output.
    /// We seed the checkpoint by hand to simulate a crashed partial run, then
    /// assert (1) only the un-processed chunk hits the chat closure, (2) the
    /// resumed chunk's relationship is in the result, and (3) the checkpoint
    /// is cleared once the run completes.
    #[tokio::test]
    async fn resumes_from_checkpoint_without_recalling_llm() {
        let recipe = make_recipe();
        let dir = tempfile::tempdir().unwrap();

        let ckpt = checkpoint::checkpoint_path(&dir.path().join(INVESTIGATION_DIRNAME));
        checkpoint::append_checkpoint(
            &ckpt,
            &checkpoint::ChunkCheckpointEntry::Success {
                chunk_id: "chunk-0".into(),
                extracted: extract::ExtractedChunk {
                    entities: vec![],
                    relationships: vec![extract::ExtractedRelationship {
                        from_entity: "Microsoft".into(),
                        to_entity: "OpenAI".into(),
                        from_type: "company".into(),
                        to_type: "company".into(),
                        relationship_type: "investment".into(),
                        attributes: Default::default(),
                        verbatim_excerpt: "Microsoft invested in OpenAI".into(),
                        confidence: 0.9,
                    }],
                },
            },
        )
        .unwrap();

        // Counts LLM calls; a resume must skip chunk-0 entirely.
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let chat: ChatCompletionFn = {
            let calls = calls.clone();
            Arc::new(move |_p| {
                let calls = calls.clone();
                Box::pin(async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(r#"{"relationships":[]}"#.to_string())
                })
            })
        };

        let chunks = vec![
            ChunkInput {
                chunk_id: "chunk-0",
                source_title: None,
                content: "already done in a prior run",
            },
            ChunkInput {
                chunk_id: "chunk-1",
                source_title: None,
                content: "fresh chunk",
            },
        ];

        let out = run_investigation(&recipe, &chunks, chat, dir.path())
            .await
            .unwrap();

        // Only chunk-1 hit the LLM — chunk-0 was resumed from the checkpoint.
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        // chunk-0's relationship + its two backfilled endpoints survived.
        assert_eq!(out.relationships.len(), 1);
        assert_eq!(out.entities.len(), 2);
        // Checkpoint cleared on successful completion (fresh re-enrich starts over).
        assert!(!ckpt.exists());
    }

    /// `_` unused-import dance: silences `Phase3PatternsRoundTrip`
    /// style false positives on the `PatternDecl` types when
    /// re-exporting moves around. Imports are real — the recipe
    /// builds a `PatternDecl::RoleOverlap` indirectly through
    /// TOML parsing.
    #[test]
    fn schema_types_are_used() {
        let _ = std::any::type_name::<EntityTypeDecl>();
        let _ = std::any::type_name::<RelationshipTypeDecl>();
        let _ = std::any::type_name::<PatternDecl>();
    }
}
