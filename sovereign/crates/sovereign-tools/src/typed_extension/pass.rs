// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pass A (per-leaf) and Pass B (per-vault-theme) execution.
//!
//! Each pass:
//! 1. Builds a system + user prompt from the leaf summary (Pass A) or
//!    theme summary (Pass B).
//! 2. Calls `inference.complete` with the shared argumentative typed-
//!    extension JSON schema in `structured_output` — llguidance picks
//!    it up at the inference layer and enforces parseable JSON.
//! 3. Parses with `parse_phase1_argumentative`.
//! 4. Wraps the result as a synthetic `SectionExtraction` for the
//!    orchestrator's `resolve_type_extensions` call.
//!
//! Pass B drops mechanisms / positions / evidence from the parsed
//! extension before wrapping — those axes are leaf-local. Pass B's
//! cost-to-recall is positive only on oppositions + concessions
//! (cross-leaf by construction per the spec).

use std::sync::Arc;

use corpus_engine::enrichment::atlas::SourceCitation;
use corpus_engine::enrichment::pipeline::atlas::{
    ArgumentativeExtension, SectionExtraction, TypeExtension,
};
use corpus_engine::enrichment::pipeline::typed_schemas::argumentative::{
    parse_phase1_argumentative, phase1_argumentative_schema, PHASE1_ARGUMENTATIVE_SYSTEM,
};
use corpus_engine::enrichment::pipeline::typed_schemas::render_source_recovery_block;
use serde::Deserialize;
use sovereign_core::conv_tiered::{ConvRaptorNodeRow, VaultThemeRow};
use sovereign_core::traits::InferenceProvider;

use crate::typed_call::{TypedCallError, TypedLlmCall};

use super::synth_section;

/// Minimum summary length for a leaf to be eligible for Pass A.
/// Tiny-bucket nodes (synthetic single-node RAPTOR) carry the chunk
/// title as their "summary"; typed extraction over a title produces
/// no signal and wastes an LLM call.
const MIN_LEAF_SUMMARY_LEN: usize = 40;

/// Maximum number of verbatim source excerpts forwarded into the
/// Pass A user body. RAPTOR builds up to 5 per leaf
/// (`MAX_QUOTE_SPANS_PER_NODE`); we forward all of them so the
/// model's atom-naming pass sees every distinctive source phrasing
/// the summariser stripped.
const PASS_A_MAX_QUOTES: usize = 5;

/// Per-vault-theme cap on member-leaf excerpts surfaced into the
/// Pass B body. Pass B operates on the theme summary alone per spec,
/// but a small budget of source excerpts from the theme's member
/// leaves gives the model verbatim handles for opposition/concession
/// naming the theme summary itself paraphrases away. Same source-
/// recovery rationale as Pass A.
const PASS_B_MAX_MEMBER_EXCERPTS: usize = 6;

/// True when the leaf has enough summary content to be worth running
/// typed extraction over. False for tiny-bucket synthetic nodes.
pub(super) fn leaf_is_extractable(node: &ConvRaptorNodeRow) -> bool {
    node.summary.trim().len() >= MIN_LEAF_SUMMARY_LEN
}

/// Pass A — typed extraction over one RAPTOR leaf summary.
///
/// Returns:
/// - `Ok(Some((section, citation)))` — parsed extension carries at least
///   one atom; the [`SourceCitation`] is the primary-source handle the
///   orchestrator threads through `resolve_type_extensions` so every
///   resulting atom dereferences to the actual source chunk + a verbatim
///   passage preview.
/// - `Ok(None)` — model returned an empty extension (all collections
///   empty); nothing to add to the atlas
/// - `Err(reason)` — every retry budget exhausted without a parseable
///   response; caller logs as a soft failure
pub(super) async fn pass_a_one_leaf(
    corpus_id: &str,
    leaf: &ConvRaptorNodeRow,
    inference: &Arc<dyn InferenceProvider>,
) -> Result<Option<(SectionExtraction, SourceCitation)>, String> {
    let primary_entities = parse_primary_entities(&leaf.primary_entities_json);
    let quote_spans = parse_quote_spans(&leaf.quote_spans_json);
    let quote_texts: Vec<String> = quote_spans.iter().map(|q| q.text.clone()).collect();
    let user = build_pass_a_user_body(&leaf.summary, &primary_entities, &quote_texts);
    let extension = call_argumentative(
        PHASE1_ARGUMENTATIVE_SYSTEM,
        &user,
        inference,
        Some(format!("typed_extension_pass_a:{}", leaf.node_id)),
    )
    .await?;
    if extension.atom_count() == 0 {
        tracing::debug!(
            corpus = corpus_id,
            node_id = %leaf.node_id,
            "typed_extension: pass A returned 0 atoms"
        );
        return Ok(None);
    }
    let citation = citation_from_quote_spans(&leaf.node_id, &quote_spans);
    let section_id = citation.section_id.clone();
    let section = synth_section(section_id, TypeExtension::Argumentative(extension));
    Ok(Some((section, citation)))
}

/// Pass B — typed extraction over one vault_theme summary. Keeps
/// only oppositions + concessions; mechanism / position / evidence on
/// a cross-note theme are too lossy versus Pass A and would risk
/// double-counting an atom whose leaf-level form already extracted.
/// Variant of Pass B that accepts verbatim excerpts collected from
/// the theme's member leaves. Source-recovery rationale matches Pass
/// A: the cross-leaf summary paraphrases away distinctive opposition
/// labels and concession phrasings the model needs to reproduce
/// verbatim for atom names to resolve against the golden. Pass
/// `member_excerpts = &[]` to skip the source-recovery block when
/// the orchestrator can't reach the underlying RAPTOR rows.
pub(super) async fn pass_b_one_theme_with_excerpts(
    corpus_id: &str,
    theme: &VaultThemeRow,
    member_quotes: &[ParsedQuoteSpan],
    inference: &Arc<dyn InferenceProvider>,
) -> Result<Option<(SectionExtraction, SourceCitation)>, String> {
    let member_texts: Vec<String> = member_quotes.iter().map(|q| q.text.clone()).collect();
    let user = build_pass_b_user_body(&theme.summary, &member_texts);
    let extension = call_argumentative(
        PHASE1_ARGUMENTATIVE_SYSTEM,
        &user,
        inference,
        Some(format!("typed_extension_pass_b:{}", theme.theme_id)),
    )
    .await?;
    // Drop everything except oppositions + concessions.
    let trimmed = ArgumentativeExtension {
        positions: Vec::new(),
        mechanisms: Vec::new(),
        evidence_invocations: Vec::new(),
        oppositions: extension.oppositions,
        concessions: extension.concessions,
    };
    if trimmed.atom_count() == 0 {
        tracing::debug!(
            corpus = corpus_id,
            theme_id = %theme.theme_id,
            "typed_extension: pass B returned 0 cross-leaf atoms"
        );
        return Ok(None);
    }
    let citation = citation_from_quote_spans(&format!("theme:{}", theme.theme_id), member_quotes);
    let section_id = citation.section_id.clone();
    let section = synth_section(section_id, TypeExtension::Argumentative(trimmed));
    Ok(Some((section, citation)))
}

/// Drive the LLM call with the shared `TypedLlmCall` helper. Returns
/// the parsed extension OR a string error suitable for the
/// orchestrator's `soft_failures` list. The helper handles budget
/// retry + parse-or-retry + chat-error short-circuit per the typed-
/// call invariant; this wrapper just collapses the structured
/// `TypedCallError<P>` shape into the string the orchestrator wants.
async fn call_argumentative(
    system: &str,
    user: &str,
    inference: &Arc<dyn InferenceProvider>,
    trace_subject: Option<String>,
) -> Result<ArgumentativeExtension, String> {
    let user_owned = user.to_string();
    let mut call = TypedLlmCall::new(system, phase1_argumentative_schema());
    call.trace_subject = trace_subject;
    call.run(
        inference,
        |_budget| async { user_owned.clone() },
        |response_text| parse_phase1_argumentative(response_text).map_err(|e| format!("{e}")),
    )
    .await
    .map(|report| report.value)
    .map_err(|e| match e {
        TypedCallError::Chat { attempt, message } => {
            format!("chat error (attempt {attempt}): {message}")
        }
        TypedCallError::ParseExhausted { attempts, last } => {
            format!("parse error (attempt {attempts}): {last}")
        }
    })
}

fn build_pass_a_user_body(
    summary: &str,
    primary_entities: &[String],
    quote_spans: &[String],
) -> String {
    let mut body = String::new();
    body.push_str("# RAPTOR leaf — argumentative typed extension\n\n");
    body.push_str("**Cluster summary (paraphrase produced by the RAPTOR summariser):**\n\n");
    body.push_str(summary.trim());
    body.push_str("\n\n");

    if !primary_entities.is_empty() {
        body.push_str("**Primary entities active in this cluster (verbatim names):**\n");
        for entity in primary_entities {
            body.push_str("- ");
            body.push_str(entity);
            body.push('\n');
        }
        body.push('\n');
    }

    body.push_str("---\n\n");
    let trimmed: Vec<&str> = quote_spans
        .iter()
        .take(PASS_A_MAX_QUOTES)
        .map(String::as_str)
        .collect();
    body.push_str(&render_source_recovery_block(&trimmed));
    body.push_str("\n\n");
    body.push_str(
        "Return a single JSON object with the typed-extension collections per the \
         schema in the system message. Extract atoms ONLY when the source above \
         directly supports them — do not invent material the cluster does not \
         contain. Omit any collection you cannot populate with real content from \
         this cluster. No prose, no <think> block, no code-fence markers.",
    );
    body
}

fn build_pass_b_user_body(summary: &str, member_excerpts: &[String]) -> String {
    let mut body = String::new();
    body.push_str("# Vault-wide theme — cross-leaf typed extension\n\n");
    body.push_str("**Theme summary (synthesised across multiple notes):**\n\n");
    body.push_str(summary.trim());
    body.push_str("\n\n");

    body.push_str("---\n\n");
    let trimmed: Vec<&str> = member_excerpts
        .iter()
        .take(PASS_B_MAX_MEMBER_EXCERPTS)
        .map(String::as_str)
        .collect();
    body.push_str(&render_source_recovery_block(&trimmed));
    body.push_str("\n\n");
    body.push_str(
        "Return a single JSON object with the typed-extension collections per the \
         schema in the system message. This input is a CROSS-NOTE synthesis — \
         only populate the `oppositions` and `concessions` collections, since \
         these are the axes that span multiple notes. Leave `mechanisms`, \
         `positions`, and `evidence_invocations` empty — the per-leaf pass \
         covers those. Omit even oppositions/concessions when the theme summary \
         does not directly support them. No prose, no <think> block, no \
         code-fence markers.",
    );
    body
}

/// `primary_entities_json` is a JSON array of strings. Tolerate
/// malformed or absent payloads — they're a usability fallback for
/// the prompt, not a correctness gate.
fn parse_primary_entities(json_blob: &str) -> Vec<String> {
    if json_blob.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<String>>(json_blob).unwrap_or_default()
}

/// Local projection of `sovereign_core::types::QuoteSpan`. The
/// conv_raptor_nodes table stores spans as serialised JSON; we need
/// both `text` (for the prompt) AND `chunk_id` (for the
/// primary-source citation handle we attach to every atom in the
/// resulting section).
#[derive(Debug, Clone, Deserialize)]
pub(super) struct ParsedQuoteSpan {
    pub chunk_id: u32,
    pub text: String,
}

/// `quote_spans_json` is a JSON array of `QuoteSpan` objects (see
/// `sovereign_core::types::QuoteSpan`). Parse both `chunk_id` + `text`
/// so the orchestrator can ground every atom in the actual source
/// chunk the verbatim passage came from.
pub(super) fn parse_quote_spans(json_blob: &str) -> Vec<ParsedQuoteSpan> {
    if json_blob.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<ParsedQuoteSpan>>(json_blob)
        .map(|spans| {
            spans
                .into_iter()
                .filter(|s| !s.text.trim().is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Convenience: build a `SourceCitation` from a slice of parsed
/// quote spans. RAPTOR builds spans in cosine-to-centroid order; the
/// first one is the most representative of the cluster, so it makes
/// the right primary source handle for every atom the section
/// produces.
///
/// Thin local wrapper around [`SourceCitation::from_primary`] that
/// projects this module's `ParsedQuoteSpan` shape into the
/// `(chunk_id, &str)` pair the corpus-engine constructor accepts.
pub(super) fn citation_from_quote_spans(
    fallback_id: &str,
    quotes: &[ParsedQuoteSpan],
) -> SourceCitation {
    let primary = quotes.first().map(|q| (q.chunk_id, q.text.as_str()));
    SourceCitation::from_primary(fallback_id, primary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_leaf(node_id: &str, summary: &str, primaries: &[&str]) -> ConvRaptorNodeRow {
        ConvRaptorNodeRow {
            node_id: node_id.into(),
            corpus_id: "test-corpus".into(),
            conv_uuid: "conv-x".into(),
            level: 0,
            summary: summary.into(),
            summary_embedding: Vec::new(),
            centroid_embedding: Vec::new(),
            children_node_ids_json: "[]".into(),
            direct_member_chunk_ids_json: None,
            evidence_chunk_ids_json: "[]".into(),
            quote_spans_json: "[]".into(),
            primary_entities_json: serde_json::to_string(primaries).unwrap(),
            cluster_coherence: 0.9,
            created_at: 0,
        }
    }

    #[test]
    fn leaf_with_short_summary_is_not_extractable() {
        let leaf = mk_leaf("n1", "Convo About React", &[]);
        assert!(!leaf_is_extractable(&leaf));
    }

    #[test]
    fn leaf_with_long_summary_is_extractable() {
        let summary = "The author argues that pharmacy benefit managers extract \
            outsized rents through opaque spread-pricing schemes that cost both \
            payers and patients while delivering little intermediation value.";
        let leaf = mk_leaf("n2", summary, &["PBM", "Spread Pricing"]);
        assert!(leaf_is_extractable(&leaf));
    }

    #[test]
    fn pass_a_user_body_carries_summary_and_entities() {
        let body = build_pass_a_user_body(
            "Spread pricing is a PBM mechanism that buys cheap, bills high.",
            &["spread pricing".into(), "PBM".into()],
            &[],
        );
        assert!(body.contains("Spread pricing"));
        assert!(body.contains("Primary entities"));
        assert!(body.contains("spread pricing"));
        assert!(body.contains("PBM"));
    }

    #[test]
    fn pass_a_user_body_surfaces_verbatim_quote_spans() {
        let body = build_pass_a_user_body(
            "PBMs extract opaque rents through their intermediation role.",
            &["PBM".into()],
            &[
                "The practice known as spread pricing lets PBMs charge payers more than they reimburse pharmacies.".into(),
                "FTC documented $1.4B per year in spread pricing income across the top three PBMs.".into(),
            ],
        );
        assert!(body.contains("Verbatim source excerpts"));
        assert!(body.contains("spread pricing"));
        assert!(body.contains("$1.4B"));
        // The naming-discipline block must reach the prompt.
        assert!(body.contains("Atom-naming discipline"));
        assert!(body.contains("Prefer verbatim phrasings"));
    }

    #[test]
    fn pass_a_user_body_truncates_overly_long_quotes() {
        use corpus_engine::enrichment::pipeline::typed_schemas::SOURCE_RECOVERY_QUOTE_CHAR_CAP;
        let long = "a".repeat(SOURCE_RECOVERY_QUOTE_CHAR_CAP + 50);
        let body = build_pass_a_user_body("summary text here is long enough", &[], &[long]);
        // Body carries an ellipsis token confirming truncation engaged.
        assert!(body.contains('…'));
    }

    #[test]
    fn pass_b_user_body_constrains_axes_and_carries_excerpts() {
        let body = build_pass_b_user_body(
            "Themes around markets-vs-regulation across notes.",
            &["markets vs governments is the durable framing the vault returns to.".into()],
        );
        assert!(body.contains("oppositions"));
        assert!(body.contains("concessions"));
        assert!(body.contains("CROSS-NOTE"));
        assert!(body.contains("Verbatim source excerpts"));
        assert!(body.contains("markets vs governments"));
        assert!(body.contains("Atom-naming discipline"));
    }

    #[test]
    fn pass_b_user_body_works_without_excerpts() {
        // Cross-leaf themes may have no member excerpts surfaced
        // (e.g. when the orchestrator can't reach the underlying
        // RAPTOR rows). The body must still parse and carry the
        // axis-constraint instructions.
        let body = build_pass_b_user_body("Theme summary text without any verbatim excerpts.", &[]);
        assert!(body.contains("CROSS-NOTE"));
        assert!(!body.contains("Verbatim source excerpts"));
    }

    #[test]
    fn primary_entities_tolerates_garbage() {
        assert!(parse_primary_entities("").is_empty());
        assert!(parse_primary_entities("not json").is_empty());
        assert!(parse_primary_entities("null").is_empty());
        assert_eq!(
            parse_primary_entities(r#"["A", "B"]"#),
            vec!["A".to_string(), "B".to_string()]
        );
    }

    #[test]
    fn quote_spans_parse_chunk_id_and_text() {
        let blob = r#"[
            {"chunk_id": 1, "char_start": 0, "char_end": 10, "text": "first quote"},
            {"chunk_id": 2, "char_start": 5, "char_end": 12, "text": "second quote"}
        ]"#;
        let spans = parse_quote_spans(blob);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].chunk_id, 1);
        assert_eq!(spans[0].text, "first quote");
        assert_eq!(spans[1].chunk_id, 2);
    }

    #[test]
    fn quote_spans_tolerate_garbage() {
        assert!(parse_quote_spans("").is_empty());
        assert!(parse_quote_spans("not json").is_empty());
        assert!(parse_quote_spans("[]").is_empty());
        // Empty text fields are filtered.
        assert!(
            parse_quote_spans(r#"[{"chunk_id":1,"char_start":0,"char_end":0,"text":"  "}]"#)
                .is_empty()
        );
    }

    #[test]
    fn citation_with_quotes_points_at_source_chunk() {
        let quotes = vec![
            ParsedQuoteSpan {
                chunk_id: 42,
                text: "Spread pricing lets PBMs charge payers more than they reimburse."
                    .to_string(),
            },
            ParsedQuoteSpan {
                chunk_id: 43,
                text: "second quote".to_string(),
            },
        ];
        let citation = citation_from_quote_spans("raptor:fallback-id", &quotes);
        assert_eq!(citation.section_id, "chunk:42");
        assert_eq!(
            citation.passage_preview.as_deref(),
            Some("Spread pricing lets PBMs charge payers more than they reimburse.")
        );
    }

    #[test]
    fn citation_without_quotes_falls_back_to_fallback_id() {
        let citation = citation_from_quote_spans("raptor:n-leaf-1", &[]);
        assert_eq!(citation.section_id, "raptor:n-leaf-1");
        assert!(citation.passage_preview.is_none());
    }
}
