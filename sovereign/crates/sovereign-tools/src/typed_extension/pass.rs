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

use corpus_engine::enrichment::pipeline::atlas::{
    ArgumentativeExtension, SectionExtraction, TypeExtension,
};
use corpus_engine::enrichment::pipeline::typed_schemas::argumentative::{
    parse_phase1_argumentative, phase1_argumentative_schema, PHASE1_ARGUMENTATIVE_SYSTEM,
};
use serde::Deserialize;
use sovereign_core::conv_tiered::{ConvRaptorNodeRow, VaultThemeRow};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};

use super::synth_section;

/// Initial decode budget. Mirrors `enrich extract-typed`'s
/// `TYPED_BUDGET_INITIAL` — tight enough to keep wall-clock low on
/// the typical case, generous enough to close the envelope on the
/// rare long output. Retry policy matches: one retry at `TYPED_BUDGET_RETRY`
/// on parse failure, then give up.
const TYPED_BUDGET_INITIAL: usize = 4096;
const TYPED_BUDGET_RETRY: usize = 8192;

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

/// Maximum chars of any one verbatim excerpt forwarded into the
/// prompt. RAPTOR caps spans around full sentences; this is a
/// belt-and-braces truncation in case a future summariser change
/// emits multi-sentence spans.
const PASS_A_QUOTE_CHAR_CAP: usize = 320;

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
    let citation = SourceCitation::from_primary_quote(&leaf.node_id, &quote_spans);
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
    let citation = SourceCitation::from_primary_quote(
        &format!("theme:{}", theme.theme_id),
        member_quotes,
    );
    let section_id = citation.section_id.clone();
    let section = synth_section(section_id, TypeExtension::Argumentative(trimmed));
    Ok(Some((section, citation)))
}

/// Drive the LLM call with the initial budget; on parse failure retry
/// once at the doubled budget. A second parse failure is surfaced as
/// an `Err` for soft-failure accounting.
async fn call_argumentative(
    system: &str,
    user: &str,
    inference: &Arc<dyn InferenceProvider>,
    trace_subject: Option<String>,
) -> Result<ArgumentativeExtension, String> {
    let schema = phase1_argumentative_schema();
    let budgets = [TYPED_BUDGET_INITIAL, TYPED_BUDGET_RETRY];
    let mut last_err: Option<String> = None;
    for (attempt, budget) in budgets.iter().enumerate() {
        let req = CompletionRequest {
            prompt: user.to_string(),
            system_message: Some(system.to_string()),
            preferred_speed: Speed::Slow,
            max_tokens: Some(*budget),
            temperature: Some(0.2),
            structured_output: Some(schema.clone()),
            think_budget: Some(0),
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
            sampling_mode: None,
            assistant_prefix: None,
            cmd_prefix: None,
            url_allowlist: None,
            evidence_id_allowlist: None,
            lark_grammar: None,
        };
        let response = match inference.complete(&req).await {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("chat error (attempt {}): {e}", attempt + 1);
                if let Some(subj) = trace_subject.as_ref() {
                    tracing::warn!(subject = %subj, error = %e, "typed_extension: chat call failed");
                }
                return Err(msg);
            }
        };
        match parse_phase1_argumentative(&response.text) {
            Ok(ext) => {
                if attempt > 0 {
                    if let Some(subj) = trace_subject.as_ref() {
                        tracing::debug!(
                            subject = %subj,
                            attempts = attempt + 1,
                            "typed_extension: parse succeeded on retry"
                        );
                    }
                }
                return Ok(ext);
            }
            Err(e) => {
                last_err = Some(format!("parse error (attempt {}): {e}", attempt + 1));
                // Loop continues to retry with doubled budget.
            }
        }
    }
    Err(last_err
        .unwrap_or_else(|| "typed_extension: both attempts failed with no recorded error".into()))
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

    if !quote_spans.is_empty() {
        body.push_str(
            "**Verbatim source excerpts (sentences pulled from the source chunks underneath this cluster):**\n\n",
        );
        for (i, span) in quote_spans.iter().take(PASS_A_MAX_QUOTES).enumerate() {
            let trimmed = truncate_quote(span);
            body.push_str(&format!("> [{i}] {trimmed}\n"));
        }
        body.push('\n');
    }

    body.push_str("---\n\n");
    body.push_str(SOURCE_RECOVERY_NAMING_DISCIPLINE);
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

    if !member_excerpts.is_empty() {
        body.push_str(
            "**Verbatim source excerpts (sentences pulled from the member notes underneath this theme):**\n\n",
        );
        for (i, span) in member_excerpts.iter().take(PASS_B_MAX_MEMBER_EXCERPTS).enumerate() {
            let trimmed = truncate_quote(span);
            body.push_str(&format!("> [{i}] {trimmed}\n"));
        }
        body.push('\n');
    }

    body.push_str("---\n\n");
    body.push_str(SOURCE_RECOVERY_NAMING_DISCIPLINE);
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

/// Source-recovery atom-naming discipline appended to every Pass A
/// and Pass B user body. Kept out of `PHASE1_ARGUMENTATIVE_SYSTEM`
/// (which is shared with `sovereign enrich extract-typed`, where the
/// model already sees raw section text and doesn't need this push).
///
/// The discipline targets the empirical failure mode observed on
/// 2026-05-24 obsidian-vault bench: 222 typed atoms produced, but
/// the model paraphrased distinctive vault vocabulary ("spread
/// pricing", "tragedy of the commons", "markets vs governments")
/// into idiosyncratic prose ("PBM administrative fee expansion",
/// "buyer-seller matching", "US hyperscaler dominance vs
/// decentralized sovereign infrastructure"). Atoms were technically
/// valid extractions of the paraphrased RAPTOR summary but not
/// resolvable against the source vocabulary — making source
/// recovery and glassbox auditing impossible.
///
/// Modeled on the book_report bench tuning: when atom names are
/// supposed to ground in source phrasing, the prompt must explicitly
/// privilege verbatim excerpts over paraphrase, and must give the
/// model the verbatim excerpts to choose from.
const SOURCE_RECOVERY_NAMING_DISCIPLINE: &str = "**Atom-naming discipline (load-bearing for source recovery):**

1. **Prefer verbatim phrasings from the source excerpts above** when naming
   atoms. The cluster/theme summary is a paraphrase produced by the RAPTOR
   summariser — it strips distinctive vocabulary (e.g. \"spread pricing\"
   may show up only as \"buying drugs cheap and billing payers more\" in the
   summary). The source excerpts hold the verbatim phrasings the
   summariser dropped. When an excerpt names a mechanism, a position, a
   piece of evidence, or an opposition with a distinctive multi-word
   phrase, USE THAT EXACT PHRASE in the atom's name/label.

2. **Do NOT invent prose names.** Names like \"PBM administrative fee
   expansion\" or \"buyer-seller matching\" are paraphrase that lose the
   audit trail. Names like \"spread pricing\", \"tragedy of the commons\",
   \"EUV monopoly\", \"$1.4B FTC PBM spread\" preserve it — a downstream
   reader can grep them against the source. The reader must be able to
   recover what the source said from the atom name alone.

3. **Opposition labels are SHORT.** Two to four words per side. \"markets
   vs regulation\" — NOT \"US hyperscaler dominance vs decentralized
   sovereign infrastructure\". Long verbose labels fail to resolve to the
   source's named contrasts.

4. **Evidence labels lead with the distinctive token** — a dollar figure
   (\"$1.4B FTC PBM spread\"), a named study (\"Ostrom 1990 commons\"), a
   case name (\"Pruitt-Igoe\"), a percentage (\"58% Micron net margin\").
   If the excerpts contain a numeric or proper-noun anchor, that anchor
   becomes the label.

5. **The primary_entities list above carries vault-canonical names —
   prefer them as atom names when the entity is also a mechanism or a
   position the source argues with.** Example: if \"Spread Pricing\" is
   in primary_entities AND the source excerpts use that exact phrase,
   the mechanism atom's name is \"spread pricing\" — not a coinage that
   restates what spread pricing does.

This discipline matters because the bench's atom scorer matches by
name. An atom that captured the right argumentative move but renamed
it doesn't surface as a hit — it surfaces as a miss plus a
fabrication. The system's glassbox premise is that an operator can
trace every atom back to source words; paraphrased names break that
contract structurally.";

fn truncate_quote(span: &str) -> String {
    let trimmed = span.trim();
    if trimmed.chars().count() <= PASS_A_QUOTE_CHAR_CAP {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(PASS_A_QUOTE_CHAR_CAP).collect();
    out.push('…');
    out
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

/// Primary-source handle attached to every section the typed-
/// extension pass produces. Threads through `resolve_type_extensions`
/// (which currently encodes only a section_id as the chunk_id on
/// every atom's `ChunkRef`) and gets used by the orchestrator's
/// post-processing pass to populate `ChunkRef.passage_preview` with
/// the verbatim source sentence.
///
/// This is the load-bearing structure for glassbox source recovery:
/// an atom's `first_appearance.chunk_id` resolves to a real chunk in
/// the corpus's chunks.lance, and its `passage_preview` carries the
/// verbatim sentence the model used to ground the atom.
#[derive(Debug, Clone)]
pub(super) struct SourceCitation {
    /// The `section_id` we put on the synthetic `SectionExtraction`.
    /// `resolve_type_extensions` copies this verbatim into every
    /// atom's `first_appearance.chunk_id` + every edge-emission
    /// `ChunkRef`. Shape: `chunk:<u32>` when a quote_span is
    /// available; falls back to `raptor:<node_id>` or
    /// `theme:<theme_id>` when not (still useful for tracing, just
    /// at coarser grain).
    pub section_id: String,
    /// The verbatim sentence the resolver-emitted ChunkRefs should
    /// carry as their `passage_preview`. `None` when no quote_span
    /// was available — the resolver's `ChunkRef.passage_preview`
    /// stays `None` and the atom degrades to chunk-level grounding
    /// only.
    pub passage_preview: Option<String>,
}

impl SourceCitation {
    /// Build a citation from a leaf's (or theme's member-leaves')
    /// quote_spans. RAPTOR builds the spans in cosine-to-centroid
    /// order; the first one is the most representative of the
    /// cluster, so it makes the right primary source handle for
    /// every atom this section produces.
    pub(super) fn from_primary_quote(
        fallback_id: &str,
        quotes: &[ParsedQuoteSpan],
    ) -> Self {
        if let Some(primary) = quotes.first() {
            Self {
                section_id: format!("chunk:{}", primary.chunk_id),
                passage_preview: Some(primary.text.clone()),
            }
        } else {
            Self {
                section_id: fallback_id.to_string(),
                passage_preview: None,
            }
        }
    }
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
        let long = "a".repeat(PASS_A_QUOTE_CHAR_CAP + 50);
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
        let body =
            build_pass_b_user_body("Theme summary text without any verbatim excerpts.", &[]);
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
        assert!(parse_quote_spans(
            r#"[{"chunk_id":1,"char_start":0,"char_end":0,"text":"  "}]"#
        )
        .is_empty());
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
        let citation = SourceCitation::from_primary_quote("raptor:fallback-id", &quotes);
        assert_eq!(citation.section_id, "chunk:42");
        assert_eq!(
            citation.passage_preview.as_deref(),
            Some("Spread pricing lets PBMs charge payers more than they reimburse.")
        );
    }

    #[test]
    fn citation_without_quotes_falls_back_to_fallback_id() {
        let citation = SourceCitation::from_primary_quote("raptor:n-leaf-1", &[]);
        assert_eq!(citation.section_id, "raptor:n-leaf-1");
        assert!(citation.passage_preview.is_none());
    }
}
