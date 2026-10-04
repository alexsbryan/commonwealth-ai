// SPDX-License-Identifier: AGPL-3.0-or-later
//! The declared-ontology arm of entity enumeration: a list question over a
//! corpus that declared its types is answered by a typed query the model
//! writes and code executes, and the cited table enters the evidence pool.
//!
//! Runs only after `atom_enum`'s Stage-1 gate says ENUMERATE — study 1's
//! `full` arm logged that gate at 32/32 K1 list questions and 0/15 K0
//! lookups, three runs — so a lookup pays nothing here. The producer is the
//! feature-fidelity query-layer probe's call, unchanged (`typed_prompt`:
//! primary slot, T 0, thinking off, schema-constrained, one retry when the
//! output does not parse); the executor is `typed::execute`, reached through
//! [`AtlasGraph::typed_answer`]; the table is the brief's listing, injected as
//! one manufactured chunk tagged `source=atom-enum`, which `merge_demand_select`
//! pins the way it pins the overview path's claims.
//!
//! `Err(reason)` hands the turn back to the degree-ranked Stage 2, and the
//! caller names that substitution at info.

use super::super::*;
use crate::atlas_context::typed::{assemble_brief, query_grammar, TypedQuery, QUERY_SYSTEM};
use crate::atlas_context::AtlasGraph;

/// The probe's output cap for one query.
const QUERY_MAX_TOKENS: usize = 400;

/// The seed score every atom-enum injection starts from
/// (`SOVEREIGN_ATOM_ENUM_SCORE`, default 0.04) — one reader for the three
/// paths that inject.
pub(super) fn atom_enum_seed_score() -> f32 {
    std::env::var("SOVEREIGN_ATOM_ENUM_SCORE")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|&s| s > 0.0)
        .unwrap_or(0.04)
}

impl Runtime {
    /// Write, run and render a typed query over the first declared corpus in
    /// scope. See the module doc.
    pub(super) async fn typed_enumeration(
        &self,
        message: &str,
        graphs: &[(String, std::sync::Arc<AtlasGraph>)],
    ) -> std::result::Result<Vec<corpus_index::types::ScoredChunk>, &'static str> {
        let mut declared = graphs.iter().filter(|(_, g)| g.ontology().is_some());
        let (corpus, graph) = declared.next().ok_or("no corpus in scope declared types")?;
        let also = declared.map(|(id, _)| id.as_str()).collect::<Vec<_>>();
        if !also.is_empty() {
            // One ontology per query: the schema's enums are one corpus's
            // declaration. Named, not merged.
            tracing::info!(
                target: "retrieval_audit",
                event = "atom_enum_typed_scope",
                answered = %corpus,
                not_answered = ?also,
                "typed enumeration answers over one declared corpus"
            );
        }
        let policies = graph
            .ontology()
            .ok_or("no corpus in scope declared types")?;
        let grammar = query_grammar(policies);

        let started = std::time::Instant::now();
        let mut parsed: Option<TypedQuery> = None;
        let mut attempts = 0u32;
        // What the last attempt died of: a failed call is not an unparsed reply.
        let mut failure = "the typed query did not parse";
        while parsed.is_none() && attempts < 2 {
            attempts += 1;
            let mut request = CompletionRequest::new(&format!(
                "{}\n\nQuestion: {message}",
                grammar.documentation
            ))
            .with_speed(oicp_types::latency_to_speed(
                oicp_types::LatencyClass::Normal,
            ))
            .with_system(QUERY_SYSTEM);
            request.max_tokens = Some(QUERY_MAX_TOKENS);
            request.temperature = Some(0.0);
            request.think_budget = Some(0);
            request.enable_thinking = Some(false);
            request.structured_output = Some(grammar.schema.clone());
            let raw = match self.inference.complete(&request).await {
                Ok(r) => r.text,
                Err(e) => {
                    tracing::info!(target: "retrieval_audit", event = "atom_enum_typed_call", attempt = attempts, error = %e, "typed query call failed");
                    failure = "the typed query call failed";
                    continue;
                }
            };
            match serde_json::from_str::<TypedQuery>(raw.trim()) {
                Ok(q) => parsed = Some(q),
                Err(e) => {
                    failure = "the typed query did not parse";
                    tracing::info!(
                    target: "retrieval_audit",
                    event = "atom_enum_typed_parse",
                    attempt = attempts,
                    error = %e,
                    raw = %truncate_with_ellipsis(&raw, 400),
                    "typed query did not parse"
                    );
                }
            }
        }
        let query = parsed.ok_or(failure)?;
        let result = graph
            .typed_answer(&query)
            .ok_or("no corpus in scope declared types")?;
        let rows = result.table.as_ref().map_or(0, |t| t.rows.len());
        tracing::info!(
            target: "retrieval_audit",
            event = "atom_enum_typed",
            corpus = %corpus,
            query = %serde_json::to_string(&query).unwrap_or_default(),
            attempts,
            ms = started.elapsed().as_millis() as u64,
            hit = result.hit,
            matched = result.table.as_ref().map_or(0, |t| t.matched),
            rows,
            headline = %result.headline,
            "retrieval_audit: typed enumeration"
        );
        if !result.hit || rows == 0 {
            return Err("the typed query matched nothing in the atlas");
        }
        let brief = assemble_brief(&result);
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("source".to_string(), "atom-enum".to_string());
        metadata.insert("atom_type".to_string(), "typed_table".to_string());
        metadata.insert(
            "typed_query".to_string(),
            serde_json::to_string(&query).unwrap_or_default(),
        );
        Ok(vec![corpus_index::types::ScoredChunk {
            content: format!(
                "{}\n\nComputed from the atlas's declared records by a typed query; each row \
                 cites the chunks that put it in the answer.\n\n{}",
                brief.headline, brief.body
            ),
            title: Some(format!("{corpus} — typed answer")),
            url: None,
            corpus_id: corpus.clone(),
            score: atom_enum_seed_score(),
            metadata,
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
            provenance: corpus_index::index::ChunkProvenance::manufactured("atom_enum_typed_table"),
        }])
    }
}
