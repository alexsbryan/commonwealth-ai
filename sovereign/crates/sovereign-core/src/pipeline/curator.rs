//! Curator stage — the Fast slot reduces a ~20-chunk retriever
//! candidate set into a 4–8-chunk [`CuratedPackage`] with a
//! per-section skeleton, a token budget, and a sufficiency verdict.
//! See `prompts::CURATOR_SYSTEM` for the prompt the Fast slot reads.
//!
//! Two entry points:
//! - [`should_curate`] — pure policy decision; bypass for trivial
//!   intents and tiny candidate sets where curation has no room
//!   to add value.
//! - [`curate`] — the Fast-slot dispatch. Builds a structured-output
//!   `CompletionRequest`, runs it through the supplied
//!   [`InferenceProvider`], and validates / repairs the JSON.
//!
//! `curate_request` is exposed `pub` so the curator-unit
//! `voice_eval` harness mode (per the situated-team plan §Iteration
//! loops) can build the same request the runtime would and test
//! against frozen candidate inputs without going through the
//! runtime stack.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::skills::SkillRegister;
use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Intent, RouterClassification, Speed};

use super::prompts::CURATOR_SYSTEM;
use super::stages::{
    CuratedPackage, DraftBudget, RetrievedChunk, SkeletonSection, Sufficiency,
};

/// Decide whether the Curator stage should run for a given turn.
///
/// Returns `false` (bypass) when the turn doesn't benefit from a
/// structured plan — either the intent is trivial enough that one
/// section over all candidates is the correct shape, or the
/// candidate set is small enough that there is no curation to do.
/// Saves a Fast-slot round-trip on the bypass path.
///
/// Bypass is consistent with the plan's "universal scope" posture
/// (every turn flows through the pipeline; bypasses live inside
/// each stage rather than as a routing branch). The runtime still
/// emits `CurationStart` / `CurationComplete` narration on the
/// bypass path so the desktop's chip rendering is uniform.
pub fn should_curate(classification: &RouterClassification, candidates_len: usize) -> bool {
    if candidates_len <= 3 {
        return false;
    }
    !matches!(
        classification.primary.intent,
        Intent::SimpleQuery
            | Intent::MetalingualQuery
            | Intent::ConationQuery
            | Intent::CommissiveQuery
            | Intent::ExpressiveQuery,
    )
}

/// Run the Curator stage. On the bypass path
/// ([`should_curate`] = false) returns
/// [`CuratedPackage::passthrough`] without invoking inference.
/// Otherwise builds a structured-output request and dispatches it
/// against the Fast slot.
///
/// `max_tokens` is the caller's budget for the Drafter's
/// completion — propagated into the curator prompt so the LLM can
/// size its per-section targets, and into the bypass /
/// insufficient packages so downstream sees a complete budget no
/// matter which path was taken.
pub async fn curate(
    provider: Arc<dyn InferenceProvider>,
    classification: &RouterClassification,
    register: SkillRegister,
    user_message: &str,
    candidates: Vec<RetrievedChunk>,
    max_tokens: u32,
) -> Result<CuratedPackage> {
    if !should_curate(classification, candidates.len()) {
        return Ok(CuratedPackage::passthrough(candidates, max_tokens));
    }

    let request = curate_request(classification, register, user_message, &candidates, max_tokens);
    let response = provider.complete(&request).await?;
    parse_curator_response(&response.text, candidates, max_tokens)
}

/// Build the `CompletionRequest` the Curator dispatches. Exposed
/// `pub` so the curator-unit harness can run frozen-input tests
/// against the same request shape the runtime produces.
///
/// `register` is sourced from the active skill (not the router) —
/// `Runtime::resolve_active_skill_register` is the canonical
/// lookup. Keeping it as an explicit parameter rather than
/// re-resolving inside the curator keeps this module skill-system-
/// agnostic, which matters for the curator-unit harness that runs
/// without a `SkillRegistry`.
pub fn curate_request(
    classification: &RouterClassification,
    register: SkillRegister,
    user_message: &str,
    candidates: &[RetrievedChunk],
    max_tokens: u32,
) -> CompletionRequest {
    let candidate_block = format_candidates_for_curator(candidates);
    let prompt = format!(
        "Intent: {intent:?}\n\
         Register: {register:?}\n\
         Confidence: {confidence:.2}\n\
         max_tokens: {max_tokens}\n\
         \n\
         User question:\n{user_message}\n\
         \n\
         Candidates ({n} from the retriever):\n{candidate_block}\n\
         \n\
         Reply with the JSON CuratedPackage matching the supplied schema.",
        intent = classification.primary.intent,
        confidence = classification.primary.confidence,
        n = candidates.len(),
    );

    let mut req = CompletionRequest::new(&prompt).with_speed(Speed::Fast);
    req.system_message = Some(CURATOR_SYSTEM.to_string());
    req.structured_output = Some(curator_schema(candidates.len()));
    req.max_tokens = Some(2048);
    // Curator doesn't need exploratory sampling; tighten the
    // temperature so the JSON stays well-formed and the section
    // counts don't drift. Schema-constrained output already pins
    // structure but a low temperature reduces semantic drift on
    // the field values.
    req.temperature = Some(0.2);
    req
}

/// JSON schema mirror of [`CuratedPackage`]. The chunk-index
/// `maximum` is bound to `n - 1` so the LLM can't reference a
/// chunk index that doesn't exist; on the bypass-degenerate
/// `n == 0` path we skip the maximum constraint and trust the
/// parser to surface an empty kept_chunks.
fn curator_schema(n_candidates: usize) -> serde_json::Value {
    let chunk_index_max = n_candidates.saturating_sub(1);
    let chunk_index_schema = if n_candidates == 0 {
        serde_json::json!({ "type": "integer", "minimum": 0 })
    } else {
        serde_json::json!({
            "type": "integer",
            "minimum": 0,
            "maximum": chunk_index_max,
        })
    };
    serde_json::json!({
        "type": "object",
        "properties": {
            "kept_chunk_indices": {
                "type": "array",
                "items": chunk_index_schema,
                "maxItems": 8,
            },
            "skeleton": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "label": { "type": "string" },
                        "purpose": { "type": "string" },
                        "chunk_refs": {
                            "type": "array",
                            "items": { "type": "integer", "minimum": 0 },
                        },
                        "target_tokens": { "type": "integer", "minimum": 1 },
                    },
                    "required": ["label", "purpose", "chunk_refs", "target_tokens"],
                    "additionalProperties": false,
                },
                "maxItems": 6,
            },
            "sufficiency": {
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["sufficient", "partial", "insufficient"],
                    },
                    "gaps": { "type": "array", "items": { "type": "string" } },
                    "reason": { "type": "string" },
                    "suggested_action": { "type": "string" },
                },
                "required": ["kind"],
                "additionalProperties": false,
            },
            "draft_budget": {
                "type": "object",
                "properties": {
                    "ceiling_tokens": { "type": "integer", "minimum": 1 },
                    "target_tokens": { "type": "integer", "minimum": 1 },
                },
                "required": ["ceiling_tokens", "target_tokens"],
                "additionalProperties": false,
            },
        },
        "required": ["kept_chunk_indices", "skeleton", "sufficiency", "draft_budget"],
        "additionalProperties": false,
    })
}

/// Format the candidate chunks for the Curator prompt. Each chunk
/// gets a stable `[i]` index header — the LLM emits indices into
/// this list in `kept_chunk_indices` and `skeleton[].chunk_refs`,
/// which we resolve against the original candidate vector after
/// parsing. Keeping the LLM's output in index space rather than
/// echoing chunks back keeps the structured-output payload small
/// and removes a round-trip class of "did the model corrupt the
/// chunk text" errors.
fn format_candidates_for_curator(candidates: &[RetrievedChunk]) -> String {
    let mut s = String::with_capacity(candidates.len() * 320);
    for (i, c) in candidates.iter().enumerate() {
        let title = c.title.as_deref().unwrap_or("(untitled)");
        // Truncate per-chunk content so the curator prompt stays
        // bounded even on long candidates. The Curator only needs
        // enough to judge relevance / topicality, not the full
        // passage — that goes to the Drafter via the curated
        // package.
        let preview: String = c.content.chars().take(800).collect();
        s.push_str(&format!(
            "[{i}] corpus={corpus} score={score:.3} title={title}\n{preview}\n---\n",
            corpus = c.corpus_id,
            score = c.score,
        ));
    }
    s
}

/// Wire-shape of the curator's structured-output payload — what
/// the Fast slot literally returns. Distinct from
/// [`CuratedPackage`] because the LLM works in chunk-index space
/// (one `usize` per kept chunk) while the in-memory package
/// resolves those indices into actual [`RetrievedChunk`] values.
#[derive(Debug, Deserialize, Serialize)]
struct CuratorWireResponse {
    kept_chunk_indices: Vec<usize>,
    skeleton: Vec<WireSkeletonSection>,
    sufficiency: WireSufficiency,
    draft_budget: WireDraftBudget,
}

#[derive(Debug, Deserialize, Serialize)]
struct WireSkeletonSection {
    label: String,
    purpose: String,
    chunk_refs: Vec<usize>,
    target_tokens: u32,
}

#[derive(Debug, Deserialize, Serialize)]
struct WireSufficiency {
    kind: String,
    #[serde(default)]
    gaps: Vec<String>,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    suggested_action: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct WireDraftBudget {
    ceiling_tokens: u32,
    target_tokens: u32,
}

/// Parse the curator's JSON response into a [`CuratedPackage`].
/// Resolves index-space `kept_chunk_indices` into actual chunks,
/// drops out-of-range or duplicate indices defensively, and
/// degrades to [`CuratedPackage::insufficient`] when the LLM
/// returns malformed JSON — the runtime treats that as an
/// epistemic-honesty short-circuit rather than a synthesis-time
/// error, consistent with the plan's glass-box stance.
pub fn parse_curator_response(
    text: &str,
    candidates: Vec<RetrievedChunk>,
    max_tokens: u32,
) -> Result<CuratedPackage> {
    let wire: CuratorWireResponse = serde_json::from_str(text.trim())
        .or_else(|_| serde_json::from_str(&extract_json_object(text).unwrap_or_default()))
        .map_err(|e| {
            Error::Inference(format!(
                "curator: malformed JSON response ({e}); raw text was: \
                 {snippet}",
                snippet = text.chars().take(400).collect::<String>(),
            ))
        })?;

    // Insufficient short-circuit: don't bother resolving chunks.
    if wire.sufficiency.kind == "insufficient" {
        let reason = if wire.sufficiency.reason.is_empty() {
            "Curator marked the candidates as insufficient without giving a \
             reason — treating as a no-grounding turn."
                .to_string()
        } else {
            wire.sufficiency.reason
        };
        let action = if wire.sufficiency.suggested_action.is_empty() {
            "answer from general knowledge with that caveat".to_string()
        } else {
            wire.sufficiency.suggested_action
        };
        return Ok(CuratedPackage::insufficient(reason, action, max_tokens));
    }

    // Resolve kept indices against the candidate set, dedup while
    // preserving first-seen order. Out-of-range indices are
    // dropped silently — the schema constrains them but a
    // jail-broken model still gets repaired here.
    let mut seen = Vec::with_capacity(wire.kept_chunk_indices.len());
    let mut kept_chunks: Vec<RetrievedChunk> = Vec::new();
    let mut wire_to_kept: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for &idx in &wire.kept_chunk_indices {
        if idx >= candidates.len() {
            continue;
        }
        if seen.contains(&idx) {
            continue;
        }
        seen.push(idx);
        wire_to_kept.insert(idx, kept_chunks.len());
        kept_chunks.push(candidates[idx].clone());
    }

    // Resolve skeleton chunk_refs against the *kept* chunks, not
    // the original candidates — the LLM emits indices into the
    // candidate space, but the in-memory package's
    // `SkeletonSection.chunk_refs` indexes `kept_chunks` so the
    // Drafter doesn't have to re-check inclusion.
    let skeleton: Vec<SkeletonSection> = wire
        .skeleton
        .into_iter()
        .map(|s| SkeletonSection {
            label: s.label,
            purpose: s.purpose,
            chunk_refs: s
                .chunk_refs
                .into_iter()
                .filter_map(|wire_idx| wire_to_kept.get(&wire_idx).copied())
                .collect(),
            target_tokens: s.target_tokens,
        })
        .collect();

    let sufficiency = match wire.sufficiency.kind.as_str() {
        "partial" => Sufficiency::Partial {
            gaps: wire.sufficiency.gaps,
        },
        // Anything other than "partial" or "insufficient" lands
        // here. The schema enum constrains the wire format to one
        // of three strings; a defensive default for an unknown
        // kind picks the safest interpretation (the model wanted
        // to draft).
        _ => Sufficiency::Sufficient,
    };

    Ok(CuratedPackage {
        kept_chunks,
        skeleton,
        sufficiency,
        draft_budget: DraftBudget {
            ceiling_tokens: wire.draft_budget.ceiling_tokens.min(max_tokens),
            target_tokens: wire.draft_budget.target_tokens.min(max_tokens),
        },
    })
}

/// Best-effort JSON-object extraction for a non-JSON response
/// (e.g. the model wraps the JSON in a fence). Mirrors the
/// `extract_json_object` helper in `voice_eval::judge` — the same
/// shape of soft-fail repair.
fn extract_json_object(text: &str) -> Option<String> {
    if let Some(start) = text.find("```json") {
        let after = &text[start + "```json".len()..];
        if let Some(end) = after.find("```") {
            return Some(after[..end].trim().to_string());
        }
    }
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if end > start {
                return Some(text[start..=end].to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{IntentCandidate, RouterClassification};

    fn classification(intent: Intent) -> RouterClassification {
        RouterClassification {
            primary: IntentCandidate {
                intent,
                confidence: 0.9,
            },
            alternatives: Vec::new(),
            rationale: Some("test".into()),
            coarse_intent: None,
            self_assessment: None,
            timing: None,
        }
    }

    fn chunk(idx: usize) -> RetrievedChunk {
        RetrievedChunk {
            content: format!("chunk {idx} content"),
            title: Some(format!("Title {idx}")),
            url: None,
            corpus_id: "test".into(),
            score: 0.5,
            metadata: Default::default(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    #[test]
    fn should_curate_bypasses_simple_query() {
        assert!(!should_curate(&classification(Intent::SimpleQuery), 20));
    }

    #[test]
    fn should_curate_bypasses_tiny_candidate_set() {
        assert!(!should_curate(
            &classification(Intent::DeepQuery),
            3
        ));
        assert!(should_curate(
            &classification(Intent::DeepQuery),
            4
        ));
    }

    #[test]
    fn should_curate_runs_on_deep_query_with_many_candidates() {
        assert!(should_curate(
            &classification(Intent::DeepQuery),
            20
        ));
        assert!(should_curate(
            &classification(Intent::ComparisonQuery),
            12
        ));
        assert!(should_curate(
            &classification(Intent::KnowledgeQuery),
            10
        ));
    }

    #[test]
    fn passthrough_package_keeps_all_chunks() {
        let cs = vec![chunk(0), chunk(1)];
        let pkg = CuratedPackage::passthrough(cs.clone(), 1024);
        assert_eq!(pkg.kept_chunks.len(), 2);
        assert!(matches!(pkg.sufficiency, Sufficiency::Sufficient));
        assert_eq!(pkg.draft_budget.ceiling_tokens, 1024);
        assert_eq!(pkg.skeleton.len(), 1);
        assert_eq!(pkg.skeleton[0].chunk_refs, vec![0, 1]);
    }

    #[test]
    fn parse_response_resolves_wire_indices_into_kept_space() {
        let candidates = (0..6).map(chunk).collect::<Vec<_>>();
        let wire = serde_json::json!({
            "kept_chunk_indices": [0, 2, 4],
            "skeleton": [
                {
                    "label": "Definitions",
                    "purpose": "Anchor on what each position means.",
                    "chunk_refs": [0, 2],
                    "target_tokens": 200,
                }
            ],
            "sufficiency": { "kind": "sufficient" },
            "draft_budget": { "ceiling_tokens": 1024, "target_tokens": 200 },
        });
        let pkg = parse_curator_response(&wire.to_string(), candidates, 1024).unwrap();
        assert_eq!(pkg.kept_chunks.len(), 3);
        // Wire chunk_refs [0, 2] index the candidate space; in
        // kept space they become [0, 1] (chunk 4 was kept but not
        // referenced by this section).
        assert_eq!(pkg.skeleton[0].chunk_refs, vec![0, 1]);
    }

    #[test]
    fn parse_response_drops_out_of_range_indices() {
        let candidates = (0..3).map(chunk).collect::<Vec<_>>();
        let wire = serde_json::json!({
            "kept_chunk_indices": [0, 99, 1, 0],
            "skeleton": [],
            "sufficiency": { "kind": "sufficient" },
            "draft_budget": { "ceiling_tokens": 512, "target_tokens": 0 },
        });
        let pkg = parse_curator_response(&wire.to_string(), candidates, 512).unwrap();
        // 99 is out of range, 0 is duplicate; expect [0, 1].
        assert_eq!(pkg.kept_chunks.len(), 2);
    }

    #[test]
    fn parse_response_returns_insufficient_short_circuit() {
        let candidates = (0..3).map(chunk).collect::<Vec<_>>();
        let wire = serde_json::json!({
            "kept_chunk_indices": [],
            "skeleton": [],
            "sufficiency": {
                "kind": "insufficient",
                "reason": "Off-domain corpus",
                "suggested_action": "install philosophy",
            },
            "draft_budget": { "ceiling_tokens": 512, "target_tokens": 0 },
        });
        let pkg = parse_curator_response(&wire.to_string(), candidates, 512).unwrap();
        assert!(pkg.kept_chunks.is_empty());
        assert!(pkg.skeleton.is_empty());
        match pkg.sufficiency {
            Sufficiency::Insufficient { reason, suggested_action } => {
                assert_eq!(reason, "Off-domain corpus");
                assert_eq!(suggested_action, "install philosophy");
            }
            _ => panic!("expected Insufficient"),
        }
    }

    #[test]
    fn parse_response_extracts_fenced_json() {
        let candidates = (0..2).map(chunk).collect::<Vec<_>>();
        let raw = format!(
            "Sure, here's the package:\n```json\n{}\n```\nDone.",
            serde_json::json!({
                "kept_chunk_indices": [0],
                "skeleton": [],
                "sufficiency": { "kind": "sufficient" },
                "draft_budget": { "ceiling_tokens": 256, "target_tokens": 100 },
            }),
        );
        let pkg = parse_curator_response(&raw, candidates, 256).unwrap();
        assert_eq!(pkg.kept_chunks.len(), 1);
    }

    #[test]
    fn parse_response_clamps_budget_to_max_tokens() {
        let candidates = (0..2).map(chunk).collect::<Vec<_>>();
        let wire = serde_json::json!({
            "kept_chunk_indices": [0],
            "skeleton": [],
            "sufficiency": { "kind": "sufficient" },
            // The model overshoots the caller's budget; we clamp.
            "draft_budget": { "ceiling_tokens": 99999, "target_tokens": 99999 },
        });
        let pkg = parse_curator_response(&wire.to_string(), candidates, 1024).unwrap();
        assert_eq!(pkg.draft_budget.ceiling_tokens, 1024);
        assert_eq!(pkg.draft_budget.target_tokens, 1024);
    }
}
