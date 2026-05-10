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

pub mod extract;
pub mod graph;
pub mod patterns;

use std::path::Path;

use crate::enrichment::pipeline::types::ChatCompletionFn;
use crate::error::{Error, Result};
use crate::recipe::Recipe;

pub use extract::{ChunkInput, ExtractedRelationship};
pub use graph::{
    Entity, Evidence, PatternFinding, PatternKind, Relationship,
    ENTITIES_FILENAME, FINDINGS_FILENAME, INVESTIGATION_DIRNAME,
    RELATIONSHIPS_FILENAME,
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
        Error::Recipe(
            "investigation pipeline requires an [enrichment] block".into(),
        )
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
    for chunk in chunks {
        let prompt = extract::compose_extract_prompt(
            chunk,
            &enrichment.entity_types,
            &enrichment.relationship_types,
        );
        let response = (chat)(&prompt).await?;
        let parsed = extract::parse_extract_response(&response)?;
        for rel in parsed {
            all_extractions.push((chunk.chunk_id.to_string(), rel));
        }
    }

    // Phase 2 — Coalesce entities; rewrite relationships to canonical ids.
    let entities_map = extract::group_extracted_entities(&all_extractions);
    let mut entities: Vec<Entity> = entities_map.values().cloned().collect();
    entities.sort_by(|a, b| a.id.cmp(&b.id));

    let mut relationships: Vec<Relationship> =
        Vec::with_capacity(all_extractions.len());
    for (i, (chunk_id, ex)) in all_extractions.iter().enumerate() {
        let from_id = extract::entity_id_for(&ex.from_type, &ex.from_entity);
        let to_id = extract::entity_id_for(&ex.to_type, &ex.to_entity);
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

    // Phase 3 — Run declared pattern detectors.
    let findings = patterns::detect_all(&enrichment.patterns, &entities, &relationships);

    // Persist + return. Persistence is best-effort: a write error
    // is surfaced to the caller, who can decide whether to retry.
    graph::write_outputs(output_dir, &entities, &relationships, &findings)?;
    Ok(InvestigationOutput {
        entities,
        relationships,
        findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::{
        EntityTypeDecl, PatternDecl, RelationshipTypeDecl,
    };
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
                content: "Microsoft invested $13B in OpenAI; OpenAI's largest customer is Microsoft.",
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
        let out =
            run_investigation(&recipe, &chunks, chat, dir.path()).await.unwrap();

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
        let chat: ChatCompletionFn = Arc::new(|_| {
            Box::pin(async { Ok(r#"{"relationships":[]}"#.to_string()) })
        });
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
        let chat: ChatCompletionFn = Arc::new(|_| {
            Box::pin(async { Ok(r#"{"relationships":[]}"#.to_string()) })
        });
        let dir = tempfile::tempdir().unwrap();
        let err = run_investigation(&recipe, &[], chat, dir.path())
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("entity_types"));
    }

    #[tokio::test]
    async fn no_chunks_produces_empty_outputs_on_disk() {
        let recipe = make_recipe();
        let chat: ChatCompletionFn = Arc::new(|_| {
            Box::pin(async { Ok(r#"{"relationships":[]}"#.to_string()) })
        });
        let dir = tempfile::tempdir().unwrap();
        let out = run_investigation(&recipe, &[], chat, dir.path()).await.unwrap();
        assert!(out.entities.is_empty());
        assert!(out.relationships.is_empty());
        assert!(out.findings.is_empty());
        assert!(dir
            .path()
            .join(INVESTIGATION_DIRNAME)
            .join(ENTITIES_FILENAME)
            .exists());
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
