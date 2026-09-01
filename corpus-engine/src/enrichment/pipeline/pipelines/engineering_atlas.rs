// SPDX-License-Identifier: AGPL-3.0-or-later
//! Engineering-documentation atlas — Phase 1 emits ONLY claims that
//! mention code artifacts. No entities, no events, no relations,
//! no questions. Designed for drift-detection inputs (ARCH docs,
//! SYSTEM_OVERVIEW, README.md, design.md) where the question that
//! matters is "what does the doc claim, and which code artefact
//! does the claim ground in?".
//!
//! ## Why a new pipeline
//!
//! `literary_atlas` extracts six facets (entities, claims, questions,
//! events, relations, argument reconstructions) per section. Run on
//! a 60-section engineering doc, it produces 30 minutes of LLM calls
//! and ~40 generic questions whose downstream cluster-and-name pass
//! adds no signal. The drift-report renderer reads `claims[]`
//! exclusively. The other five facets are noise the matcher discards.
//!
//! `engineering_atlas` narrows Phase 1 to a single-facet schema —
//! `{claims: [{content, code_anchors, evidence_excerpt}]}` — that's
//! what the drift matcher actually needs. Empirically validated on
//! a 5-section eval (2026-05-11): 0.83-0.88 recall against
//! hand-labeled anchors, 1.00 precision, 17s/section avg on the
//! fast slot. The eval is in `/tmp/grounded_claim_eval.py` and the
//! prompt is the validated form from that loop.
//!
//! ## Composition
//!
//! Wraps [`LiteraryAtlasPipeline`] as `inner` and overrides only the
//! Phase 1 surface (system prompt, compose, parse). Phases 2+ reuse
//! the literary pipeline's logic — they operate on `SectionExtraction`,
//! which `engineering_atlas` produces with only `claims[]` populated;
//! the other facet vecs are empty, so cluster/name/resolve degenerate
//! to no-ops on those facets without erroring.
//!
//! ## Anchor handling
//!
//! Phase 1 emits `code_anchors: Vec<String>` per claim — every code
//! artifact the claim mentions. The pipeline maps `code_anchors[0]`
//! to the existing `ClaimSketch.anchor` field. Multi-anchor cases
//! are recovered by [`super::super::atom_normalizer::BacktickAugmentProcessor`],
//! which runs at the runner level after every parse and harvests
//! backtick-wrapped spans from the claim's `content`. The pipeline
//! intentionally does NOT extend `ClaimSketch` with a new vec field —
//! the existing schema is sufficient when paired with the cross-cutting
//! post-processor.

use serde::Deserialize;

use super::super::atlas::{
    ClaimSketch, DiscourseAct, EnrichmentDepth, EpistemicStatus, SectionExtraction,
};
use super::super::types::{ChapterInput, ChatPrompt, Phase1ChapterResult};
use crate::enrichment::pipeline::Exemplar;
use crate::error::{Error, Result};

pub const PIPELINE_ID: &str = "engineering_atlas";

static PHASE1_SYSTEM: ::std::sync::LazyLock<&'static str> = ::std::sync::LazyLock::new(|| {
    crate::enrichment::pipeline::prompts::load_or_baked(
        "engineering_atlas/phase1_system.md",
        include_str!("engineering_atlas_prompts/phase1_system.md"),
    )
});

/// The engineering genre: technical docs, extracted as a flat claims envelope
/// with code anchors rather than a literary atom graph.
///
/// Until 2026-08-31 this was an `EngineeringAtlasPipeline` wrapper holding 17
/// verbatim delegations to an inner `LiteraryAtlasPipeline`. Phase 1 is the
/// only phase that genuinely differs — it emits a different SHAPE, so it
/// brings its own body, schema and parser; everything below Phase 1 was, and
/// still is, the shared machinery.
#[derive(Debug, Clone, Copy, Default)]
pub struct EngineeringGenre;

impl super::genre::AtlasGenre for EngineeringGenre {
    fn id(&self) -> &'static str {
        PIPELINE_ID
    }

    fn name(&self) -> &'static str {
        "Engineering docs — claims with code anchors"
    }

    fn phase1_system(&self) -> &'static str {
        *PHASE1_SYSTEM
    }

    fn compose_phase1(
        &self,
        chapter: &ChapterInput,
        _exemplars: &[&Exemplar],
        _seed: Option<&super::super::atlas::SeedEntities>,
    ) -> Option<ChatPrompt> {
        let user = render_user_body(chapter);
        Some(
            ChatPrompt::new(*PHASE1_SYSTEM, user)
                .with_response_schema("engineering_claims", phase1_engineering_schema())
                .with_phase_id("phase1"),
        )
    }

    fn compose_phase1_terse(&self, chapter: &ChapterInput) -> Option<ChatPrompt> {
        let user = format!(
            "{}\n\nReminder: respond with one JSON object only. \
             Start with `{{`. No prose, no <think> block.",
            render_user_body(chapter)
        );
        Some(
            ChatPrompt::new(*PHASE1_SYSTEM, user)
                .with_response_schema("engineering_claims", phase1_engineering_schema())
                .with_phase_id("phase1_terse"),
        )
    }

    fn parse_phase1(&self, response: &str) -> Option<Result<Phase1ChapterResult>> {
        Some(parse_engineering_phase1(response))
    }
}

/// Parse the engineering Phase-1 claims envelope. Free, because the genre hook
/// only wraps it — the parse itself is what differs from the shared atlas.
fn parse_engineering_phase1(response: &str) -> Result<Phase1ChapterResult> {
    let cleaned = prepare_response(response)?;
    let raw: RawClaimsEnvelope = serde_json::from_str(&cleaned).map_err(|e| {
        Error::Serialization(format!(
            "phase 1 (engineering atlas) response is not valid JSON: {e} | head: {}",
            cleaned.chars().take(200).collect::<String>()
        ))
    })?;

    let claims: Vec<ClaimSketch> = raw
        .claims
        .into_iter()
        .map(|c| {
            // First anchor (if any) → ClaimSketch.anchor. The
            // backtick_augment post-processor will recover the
            // remaining anchors from the content field.
            let anchor = c.code_anchors.into_iter().next().unwrap_or_default();
            ClaimSketch {
                content: c.content,
                discourse_act: DiscourseAct::Assert,
                epistemic_status: EpistemicStatus::Confident,
                attributed_to: None,
                quotable_excerpt: c.evidence_excerpt,
                anchor,
            }
        })
        .collect();

    Ok(Phase1ChapterResult {
        questions: Vec::new(),
        reveals: None,
        thematic_carriers: Vec::new(),
        setting: None,
        plot: None,
        section_extraction: Some(SectionExtraction {
            section_id: String::new(), // runner stamps this
            enrichment_depth: EnrichmentDepth::Extracted,
            entities_introduced: Vec::new(),
            entities_developed: Vec::new(),
            relations_introduced: Vec::new(),
            relations_developed: Vec::new(),
            events: Vec::new(),
            claims,
            questions_raised: Vec::new(),
            argument_reconstructions: Vec::new(),
            type_extension: None,
            type_extensions: Vec::new(),
        }),
    })
}

/// The engineering atlas pipeline.
pub fn pipeline() -> super::literary_atlas::LiteraryAtlasPipeline {
    super::literary_atlas::LiteraryAtlasPipeline::with_genre(std::sync::Arc::new(EngineeringGenre))
}

/// Render the per-section user body. Just title + text — the engineering
/// extraction prompt is schema-encoded, not exemplar-driven.
fn render_user_body(chapter: &ChapterInput) -> String {
    format!(
        "**Section:** {title}\n**Section id:** {sid}\n\n---\n\n{text}",
        title = chapter.title,
        sid = chapter.chapter_id,
        text = chapter.text,
    )
}

/// JSON Schema (Draft 2020-12 subset the daemon understands) for the
/// Phase 1 envelope. Kept narrow on purpose — every additional field
/// is mask-state the model has to navigate.
fn phase1_engineering_schema() -> serde_json::Value {
    // `x-asciiExtended: true` on every string field blocks the
    // occasional non-Latin token drift observed in drift reports
    // (e.g. CJK characters like "或"/"生成" leaking into claim
    // content). It permits ASCII + 2-byte UTF-8 (Latin Extended,
    // Greek, Cyrillic, Arabic, Hebrew base) so accented names like
    // "Björk" or "café" still pass — only 3+ byte UTF-8 (CJK,
    // Devanagari, Hangul, etc.) is rejected by the grammar mask.
    serde_json::json!({
        "type": "object",
        "required": ["claims"],
        "additionalProperties": false,
        "properties": {
            "claims": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["content", "code_anchors"],
                    "additionalProperties": false,
                    "properties": {
                        "content": {"type": "string", "x-asciiExtended": true},
                        "code_anchors": {
                            "type": "array",
                            "items": {"type": "string", "x-asciiExtended": true}
                        },
                        "evidence_excerpt": {"type": "string", "x-asciiExtended": true}
                    }
                }
            }
        }
    })
}

#[derive(Debug, Deserialize)]
struct RawClaimsEnvelope {
    #[serde(default)]
    claims: Vec<RawClaim>,
}

#[derive(Debug, Deserialize)]
struct RawClaim {
    #[serde(default)]
    content: String,
    #[serde(default)]
    code_anchors: Vec<String>,
    #[serde(default)]
    evidence_excerpt: Option<String>,
}

/// Strip any `<think>` preamble before the JSON envelope. The
/// grammar-constrained sampler shouldn't emit `<think>` blocks when
/// the response_format is enforced, but the model occasionally
/// leads with whitespace; this also handles the no-grammar fallback
/// path. Returns the JSON-bearing tail.
fn prepare_response(response: &str) -> Result<String> {
    let body = if let Some(idx) = response.find("</think>") {
        &response[idx + "</think>".len()..]
    } else {
        response
    };
    let start = body.find('{').ok_or_else(|| {
        Error::Serialization(format!(
            "phase 1 (engineering atlas) response contained no recognisable JSON object | response[head]: {}",
            if body.is_empty() { "<empty response>" } else {
                &body[..body.len().min(200)]
            }
        ))
    })?;
    Ok(body[start..].trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::super::super::trait_def::Pipeline;
    use super::*;

    #[test]
    fn pipeline_id_matches_constant() {
        let p = pipeline();
        assert_eq!(p.id(), PIPELINE_ID);
        assert_eq!(p.id(), "engineering_atlas");
    }

    #[test]
    fn phase1_compose_attaches_response_schema() {
        let p = pipeline();
        let chapter = ChapterInput {
            chapter_id: "sec_0001".to_string(),
            title: "Title".to_string(),
            text: "body".to_string(),
            metadata: Default::default(),
            approx_tokens: 1,
        };
        let prompt = p.compose_phase1(&chapter, &[]);
        // Round-trip via JSON to inspect the schema attachment.
        let json = serde_json::to_string(&prompt).unwrap();
        assert!(json.contains("engineering_claims"), "got: {json}");
        assert!(json.contains("code_anchors"), "got: {json}");
    }

    #[test]
    fn parse_phase1_populates_only_claims_facet() {
        let p = pipeline();
        let resp = r#"{
            "claims": [
                {
                    "content": "Files larger than 1200 LOC must have a §10 roadmap entry.",
                    "code_anchors": ["SYSTEM_OVERVIEW.md §10"],
                    "evidence_excerpt": "Big files without a roadmap entry are bugs."
                }
            ]
        }"#;
        let res = p.parse_phase1(resp).unwrap();
        let sx = res
            .section_extraction
            .expect("section_extraction populated");
        assert_eq!(sx.claims.len(), 1);
        assert_eq!(sx.claims[0].anchor, "SYSTEM_OVERVIEW.md §10");
        assert_eq!(
            sx.claims[0].quotable_excerpt.as_deref(),
            Some("Big files without a roadmap entry are bugs.")
        );
        // Every other facet must be empty — that's the contract
        // engineering_atlas commits to downstream phases.
        assert!(sx.entities_introduced.is_empty());
        assert!(sx.entities_developed.is_empty());
        assert!(sx.questions_raised.is_empty());
        assert!(sx.events.is_empty());
        assert!(sx.relations_introduced.is_empty());
        assert!(sx.relations_developed.is_empty());
        assert!(sx.argument_reconstructions.is_empty());
    }

    #[test]
    fn parse_phase1_tolerates_think_preamble() {
        // Even with grammar constraint the model occasionally lands
        // a think tag if the schema is loose; the parser strips it.
        let p = pipeline();
        let resp = r#"<think>Considering the section...</think>
{"claims": [{"content": "x", "code_anchors": []}]}"#;
        let res = p.parse_phase1(resp).unwrap();
        let sx = res.section_extraction.unwrap();
        assert_eq!(sx.claims.len(), 1);
    }

    #[test]
    fn parse_phase1_no_claims_returns_empty_section() {
        let p = pipeline();
        let res = p.parse_phase1(r#"{"claims": []}"#).unwrap();
        let sx = res.section_extraction.unwrap();
        assert!(sx.claims.is_empty());
        assert!(sx.has_no_atoms());
    }
}
