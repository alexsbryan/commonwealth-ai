//! Literary atlas pipeline — Step 1 of the v2.1 atlas schema rollout.
//!
//! Extends the `literary` pipeline's Phase 1 to emit a full
//! `SectionExtraction` record (entities / entity-states / relations /
//! relation-states / events / claims / questions) alongside the legacy
//! `questions` field. Phases 3–7 are delegated to the embedded
//! `LiteraryPipeline`; they continue to operate on the legacy
//! questions/concerns/positions flow while the atlas atom graph rides
//! along in each `ExtractedQuestion.section_extraction` slot.
//!
//! The atlas graph is not yet consumed by the downstream phases. When
//! a future landing rewrites Phase 3+ to traverse the atlas directly,
//! this pipeline keeps the same id and the Phase 1 output is already
//! in the right shape — no re-extraction needed.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::super::atlas::{
    ClaimSketch, DiscourseAct, EntitySketch, EntityStateSketch, EntityType, EpistemicStatus,
    EventSketch, QuestionSketch, RelationSketch, RelationStateSketch, SectionExtraction,
    SeedEntities, SeedEntity, SeedStrategy,
};
use super::super::exemplar_bank::{Exemplar, ExemplarKind};
use super::super::trait_def::Pipeline;
use super::super::types::*;
use super::literary::{prepare_phase_json, sanitize_optional_string, LiteraryPipeline};
use crate::engine::CorpusEngine;
use crate::enrichment::atlas::{
    AtlasData, AtlasIngestion, AtlasIngestionConfig, AtlasIngestionRegistry,
};
use crate::enrichment::domain::ClusteringConfig;
use crate::enrichment::pipeline::atlas::EnrichmentDepth;
use crate::error::{Error, Result};
use crate::progress::ProgressCallback;
use crate::types::{EmbedFn, InferenceFn};
use serde::Deserialize;
use tracing::debug;

const PHASE1_ATLAS_SYSTEM: &str =
    include_str!("literary_atlas_prompts/phase1_system.md");

/// Terse Phase 1 preamble used when a default run failed with
/// `PhaseFailureKind::ThinkTruncated`. The asset drops the shape
/// example and prepends a "no reasoning trace" directive so the
/// model emits JSON directly instead of burning its output budget
/// on reflection.
const PHASE1_ATLAS_SYSTEM_TERSE: &str =
    include_str!("literary_atlas_prompts/phase1_system_terse.md");

// Per-facet Phase 3 naming preambles. `compose_phase3_facet`
// selects among these by facet. Each targets the naming convention
// from spec §5.3 — question → thematic concern, claim → position
// family, entity-state → trajectory arc, relation-state →
// relational dynamic, event → narrative thread.
const PHASE3_QUESTION_NAMING: &str =
    include_str!("literary_atlas_prompts/phase3_question_naming.md");
const PHASE3_CLAIM_NAMING: &str =
    include_str!("literary_atlas_prompts/phase3_claim_naming.md");
const PHASE3_ENTITY_STATE_NAMING: &str =
    include_str!("literary_atlas_prompts/phase3_entity_state_trajectory_naming.md");
const PHASE3_RELATION_STATE_NAMING: &str =
    include_str!("literary_atlas_prompts/phase3_relation_state_trajectory_naming.md");
const PHASE3_EVENT_NAMING: &str =
    include_str!("literary_atlas_prompts/phase3_event_thread_naming.md");

const PHASE1A_SEED_SYSTEM: &str =
    include_str!("literary_atlas_prompts/phase1a_seed_system.md");

/// Phase 8 configuration-detection preamble. The LLM reads the
/// atlas summary (not raw text) and emits 0–3 Configuration atoms
/// per spec §2.7, each with an `interpretive_note` articulating
/// alternative readings (the Ricoeur constraint per spec §1.2).
const PHASE8_CONFIGURATION_SYSTEM: &str =
    include_str!("literary_atlas_prompts/phase8_configuration.md");

/// Pipeline id exposed by the registry.
pub const PIPELINE_ID: &str = "literary_atlas";

/// Literary pipeline that extracts the full atlas atom graph in
/// Phase 1. Delegates Phases 3–7 to `LiteraryPipeline` unchanged.
pub struct LiteraryAtlasPipeline {
    inner: LiteraryPipeline,
}

impl LiteraryAtlasPipeline {
    pub fn new() -> Self {
        Self { inner: LiteraryPipeline::new() }
    }
}

impl Default for LiteraryAtlasPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline for LiteraryAtlasPipeline {
    fn id(&self) -> &'static str {
        PIPELINE_ID
    }

    fn name(&self) -> &'static str {
        "Literary — atlas atom graph"
    }

    fn vocabulary(&self) -> &Vocabulary {
        self.inner.vocabulary()
    }

    // ── Phase system preambles ────────────────────────────────
    //
    // Only Phase 1 diverges from `literary`; the rest reuse the same
    // prompt assets. When the atlas-native Phase 3+ lands we swap
    // those in here.

    fn phase1_system(&self) -> &'static str {
        PHASE1_ATLAS_SYSTEM
    }

    fn phase3_system(&self) -> &'static str {
        self.inner.phase3_system()
    }

    fn phase5_system(&self) -> &'static str {
        self.inner.phase5_system()
    }

    fn phase6_system(&self) -> &'static str {
        self.inner.phase6_system()
    }

    fn phase7_system(&self) -> &'static str {
        self.inner.phase7_system()
    }

    // ── Phase 1 — atlas extraction ────────────────────────────

    fn compose_phase1(
        &self,
        chapter: &ChapterInput,
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        // Delegate to the seed-aware variant with no seed so the
        // seed-aware rendering path has a single call site. When a
        // seed is available the runner calls `compose_phase1_with_seed`
        // directly and gets the same body + an extra "known canonical
        // names" block at the top.
        let user = render_phase1_user_body(
            chapter,
            exemplars,
            /*include_exemplars=*/ true,
            /*seed=*/ None,
        );
        ChatPrompt::new(self.phase1_system(), user)
            .with_response_schema(
                "phase1_section_extraction",
                phase1_section_extraction_schema(),
            )
            .with_phase_id("phase1")
    }

    /// Terse Phase 1 variant. Dispatched by the runner when a
    /// default-pass failure is classified as
    /// `PhaseFailureKind::ThinkTruncated`. Swaps the system preamble
    /// and drops the exemplar block to save tokens on a chapter that
    /// already blew past the output budget. Parser is shared with
    /// the default variant.
    fn compose_phase1_terse(
        &self,
        chapter: &ChapterInput,
    ) -> Option<ChatPrompt> {
        let user = render_phase1_user_body(
            chapter,
            /*exemplars=*/ &[],
            /*include_exemplars=*/ false,
            /*seed=*/ None,
        );
        Some(
            ChatPrompt::new(PHASE1_ATLAS_SYSTEM_TERSE, user)
                .with_response_schema(
                    "phase1_section_extraction",
                    phase1_section_extraction_schema(),
                )
                .with_phase_id("phase1_terse"),
        )
    }

    // ── Stage 1a — seed extraction ─────────────────────────────

    fn seed_strategy(&self) -> SeedStrategy {
        SeedStrategy::Llm
    }

    fn compose_seed_prompt(&self, first_section: &ChapterInput) -> Option<ChatPrompt> {
        let mut user = String::new();
        user.push_str("# Opening section\n\n");
        user.push_str(&format!("**Title:** {}\n", first_section.title));
        if let Some(ord) = first_section.metadata.get("ordinal") {
            user.push_str(&format!("**Position:** chapter {ord}\n"));
        }
        user.push_str("\n**Body:**\n\n");
        user.push_str(&first_section.text);
        user.push_str("\n\n---\n\n");
        user.push_str(
            "Respond with a single JSON object per the schema in the system \
             message. Entities only. No prose, no <think> block.",
        );
        Some(ChatPrompt::new(PHASE1A_SEED_SYSTEM, user).with_phase_id("phase1_seed"))
    }

    fn parse_seed_response(&self, response: &str) -> Result<Vec<SeedEntity>> {
        let cleaned = prepare_phase_json(response, "stage 1a (seed)")?;

        #[derive(serde::Deserialize)]
        struct Raw {
            #[serde(default)]
            entries: Vec<Option<RawSeedEntry>>,
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct RawSeedEntry {
            canonical_name: String,
            aliases: Vec<Option<String>>,
            entity_type: Option<EntityType>,
            description: String,
        }

        let raw: Raw = serde_json::from_str(&cleaned).map_err(|e| {
            Error::Serialization(format!("stage 1a (seed) response is not valid JSON: {e}"))
        })?;

        let mut entries: Vec<SeedEntity> = Vec::with_capacity(raw.entries.len());
        for item in raw.entries.into_iter().flatten() {
            let canonical = item.canonical_name.trim().to_string();
            if canonical.is_empty() || is_placeholder_literal(&canonical) {
                continue;
            }
            let description = item.description.trim().to_string();
            let description = if is_placeholder_literal(&description) {
                String::new()
            } else {
                description
            };
            let aliases: Vec<String> = item
                .aliases
                .into_iter()
                .flatten()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty() && !is_placeholder_literal(s))
                .collect();
            entries.push(SeedEntity {
                canonical_name: canonical,
                aliases,
                entity_type: item
                    .entity_type
                    .unwrap_or_else(|| EntityType::Other("unspecified".into())),
                description,
            });
        }
        if entries.is_empty() {
            return Err(Error::Serialization(
                "stage 1a (seed) response contained no valid entity entries — \
                 re-run the seed prompt; if the opening section genuinely has \
                 no named entities, declare SeedStrategy::None on the pipeline \
                 instead"
                    .into(),
            ));
        }
        Ok(entries)
    }

    fn compose_phase1_with_seed(
        &self,
        chapter: &ChapterInput,
        exemplars: &[&Exemplar],
        seed: Option<&SeedEntities>,
    ) -> ChatPrompt {
        let user = render_phase1_user_body(
            chapter,
            exemplars,
            /*include_exemplars=*/ true,
            seed,
        );
        ChatPrompt::new(self.phase1_system(), user)
            .with_response_schema(
                "phase1_section_extraction",
                phase1_section_extraction_schema(),
            )
            .with_phase_id("phase1")
    }

    fn parse_phase1(&self, response: &str) -> Result<Phase1ChapterResult> {
        let cleaned = prepare_phase_json(response, "phase 1 (atlas)")?;

        // Two-step deserialization: parse to `serde_json::Value`
        // first (which silently keeps the last value when the model
        // emits the same key twice, observed on Gemma-31B), then
        // sanitize array fields whose schema declares objects but
        // where the model occasionally drops in a `"//"` comment
        // string or other non-object literal. Only after this
        // cleaning pass do we deserialize into the typed Raw
        // layout. Without this pre-pass a single duplicate field or
        // hallucinated comment string costs the whole section.
        let mut value: serde_json::Value =
            serde_json::from_str(&cleaned).map_err(|e| {
                Error::Serialization(format!(
                    "phase 1 (atlas) response is not valid JSON: {e}"
                ))
            })?;
        sanitize_phase1_object_arrays(&mut value);

        // Deserialize through a lenient Raw layout that tolerates
        // common model-compliance drift — an individual claim missing
        // `epistemic_status`, a lone null in an array, an unknown
        // enum tag — so a single bad claim doesn't throw away the
        // rest of a chapter's extraction. Hard-failing on shape only
        // makes sense when the response as a whole is unusable.
        let raw: RawSectionExtraction = serde_json::from_value(value).map_err(|e| {
            Error::Serialization(format!(
                "phase 1 (atlas) response is not valid JSON: {e}"
            ))
        })?;
        let mut extraction = raw.into_extraction();

        // Reject the common failure mode where the model echoes the
        // schema placeholder for section_id instead of stamping the
        // real one.
        if extraction.section_id.trim().is_empty()
            || is_placeholder_literal(&extraction.section_id)
        {
            // Section id is stamped by the runner from the chapter
            // input anyway — we don't care what the model put here
            // as long as we can see it isn't vacant for debugging.
            extraction.section_id = String::new();
        }

        // Scrub placeholder literals inside string fields. A `"..."`
        // in an `evidence_preview` or `description` slot is schema
        // echo, not an answer.
        scrub_placeholder_strings(&mut extraction);

        // A section with zero atoms from a literary chapter is almost
        // always a parse quality failure — the model skipped the
        // extraction. Surface it as an error so the run file captures
        // the raw response head for post-mortem.
        if extraction.has_no_atoms() {
            return Err(Error::Serialization(
                "phase 1 (atlas) produced no entities, states, relations, \
                 events, claims, or questions — the model did not extract \
                 anything usable. Check the raw response head for schema \
                 echo, truncated output, or a refusal."
                    .into(),
            ));
        }

        // Derive the legacy `questions` / `thematic_carriers` /
        // `setting` / `plot` fields from the atlas extraction so the
        // existing Phase 2/3/4/5 flow still functions against the
        // atlas output. These are back-compat bridges, not the
        // preferred view of the data.
        let questions: Vec<String> = extraction
            .questions_raised
            .iter()
            .map(|q| q.content.trim().to_string())
            .filter(|s| !s.is_empty() && !is_placeholder_literal(s))
            .collect();

        if questions.is_empty() {
            return Err(Error::Serialization(
                "phase 1 (atlas) response has no questions_raised — at \
                 least one thematic question is required so downstream \
                 clustering has something to align on."
                    .into(),
            ));
        }

        let thematic_carriers: Vec<String> = extraction
            .entities_introduced
            .iter()
            .map(|e| e.canonical_name.trim().to_string())
            .chain(
                extraction
                    .entities_developed
                    .iter()
                    .map(|e| e.entity_name.trim().to_string()),
            )
            .filter(|s| !s.is_empty() && !is_placeholder_literal(s))
            .fold(Vec::<String>::new(), |mut acc, name| {
                if !acc.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                    acc.push(name);
                }
                acc
            });

        let plot = first_event_description(&extraction);
        let reveals = first_text_level_claim(&extraction);

        Ok(Phase1ChapterResult {
            questions,
            reveals: sanitize_optional_string(reveals),
            thematic_carriers,
            setting: None,
            plot: sanitize_optional_string(plot),
            section_extraction: Some(extraction),
        })
    }

    // ── Phase 3 — delegate legacy path + atlas facet override ─

    fn compose_phase3(
        &self,
        cluster: &QuestionCluster,
        chapter_excerpts: &[&ChapterInput],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        self.inner
            .compose_phase3(cluster, chapter_excerpts, exemplars)
            .with_phase_id("phase3")
    }

    fn parse_phase3(&self, response: &str) -> Result<Phase3ParseResult> {
        self.inner.parse_phase3(response)
    }

    fn compose_phase3_facet(
        &self,
        cluster: &AtlasCluster,
        facet: Facet,
        excerpts: &[SketchExcerpt],
        exemplars: &[&Exemplar],
    ) -> Option<ChatPrompt> {
        let system = match facet {
            Facet::Question => PHASE3_QUESTION_NAMING,
            Facet::Claim => PHASE3_CLAIM_NAMING,
            Facet::EntityState => PHASE3_ENTITY_STATE_NAMING,
            Facet::RelationState => PHASE3_RELATION_STATE_NAMING,
            Facet::Event => PHASE3_EVENT_NAMING,
        };
        let mut user = String::new();

        if !exemplars.is_empty() {
            user.push_str("# Reference exemplars\n\n");
            for (i, e) in exemplars.iter().enumerate() {
                render_generic_phase3_exemplar(&mut user, i + 1, e);
            }
            user.push_str("---\n\n");
        }

        user.push_str(&format!(
            "# Cluster to name (id: {}, facet: {})\n\n",
            cluster.id,
            facet.as_str()
        ));
        user.push_str(&format!(
            "The following {} sketch(es) were grouped together by embedding \
             similarity and the facet's secondary signal:\n\n",
            excerpts.len()
        ));
        for (i, ex) in excerpts.iter().enumerate() {
            user.push_str(&format!("{}. [{}] {}", i + 1, ex.section_id, ex.content));
            if !ex.anchor.is_empty() {
                user.push_str(&format!("  (anchor: {:?})", ex.anchor));
            }
            user.push('\n');
        }
        user.push_str(
            "\n---\n\nRespond with a single JSON object per the schema in the system message.",
        );

        Some(ChatPrompt::new(system, user).with_phase_id("phase3_facet"))
    }

    fn parse_phase3_facet(
        &self,
        _facet: Facet,
        response: &str,
    ) -> Result<Phase3FacetParseResult> {
        let cleaned = prepare_phase_json(response, "phase 3 (atlas)")?;

        // Accept arbitrary JSON values inside `metadata` because the
        // per-facet prompts legitimately ask for arrays in some slots
        // (e.g. `participants: ["entity_a", "entity_b"]` in the
        // relation_state preamble). Flattening is centralised in
        // `phase3_metadata_value_to_string` so every facet shares the
        // same coercion rules — no facet-specific surprises.
        #[derive(serde::Deserialize)]
        struct Raw {
            label: Option<String>,
            #[serde(default)]
            metadata: Option<std::collections::HashMap<String, serde_json::Value>>,
        }
        let raw: Raw = serde_json::from_str(&cleaned).map_err(|e| {
            Error::Serialization(format!("phase 3 (atlas) JSON parse error: {e}"))
        })?;
        let label = raw
            .label
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !is_placeholder_literal(s))
            .ok_or_else(|| {
                Error::Serialization(
                    "phase 3 (atlas) response missing non-empty `label`".into(),
                )
            })?;
        let metadata: std::collections::HashMap<String, String> = raw
            .metadata
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| {
                let s = phase3_metadata_value_to_string(v)?;
                let s = s.trim().to_string();
                if s.is_empty() || is_placeholder_literal(&s) {
                    None
                } else {
                    Some((k, s))
                }
            })
            .collect();
        Ok(Phase3FacetParseResult { label, metadata })
    }

    // ── Phase 5 — delegate ────────────────────────────────────

    fn compose_phase5(
        &self,
        concern: &CanonicalConcern,
        cluster: &ChunkCluster,
        cluster_chunk_texts: &[(u64, String)],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        self.inner
            .compose_phase5(concern, cluster, cluster_chunk_texts, exemplars)
            .with_phase_id("phase5")
    }

    fn parse_phase5(&self, response: &str) -> Result<Phase5ParseResult> {
        self.inner.parse_phase5(response)
    }

    // ── Phase 6 — delegate ────────────────────────────────────

    fn compose_phase6(
        &self,
        pos_a: &Position,
        pos_b: &Position,
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        self.inner
            .compose_phase6(pos_a, pos_b, exemplars)
            .with_phase_id("phase6")
    }

    fn parse_phase6(&self, response: &str) -> Result<Option<Phase6ParseResult>> {
        self.inner.parse_phase6(response)
    }

    // ── Phase 7 — delegate ────────────────────────────────────

    fn compose_phase7(
        &self,
        concerns: &[CanonicalConcern],
        positions: &[Position],
        chapter_titles: &[String],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        self.inner
            .compose_phase7(concerns, positions, chapter_titles, exemplars)
            .with_phase_id("phase7")
    }

    fn parse_phase7(&self, response: &str) -> Result<Vec<Phase7ParseItem>> {
        self.inner.parse_phase7(response)
    }

    // ── Clustering tuning — delegate ──────────────────────────

    fn question_clustering_config(&self) -> ClusteringConfig {
        self.inner.question_clustering_config()
    }

    fn chunk_clustering_config(&self) -> ClusteringConfig {
        self.inner.chunk_clustering_config()
    }

    // ── Phase 8 (Configuration) — opt in ──────────────────────

    fn runs_configuration_phase(&self) -> bool {
        true
    }

    fn compose_phase8_configuration(
        &self,
        atlas_summary: &crate::enrichment::atlas::analysis::AtlasSummary,
        _exemplars: &[&Exemplar],
    ) -> Option<ChatPrompt> {
        let mut user = String::new();
        user.push_str("Atlas synopsis — structural view of the resolved atoms.\n");
        user.push_str("Use this to identify 0–3 configurations per the system prompt.\n\n");
        user.push_str(&format!("Sections: {}\n\n", atlas_summary.section_count));

        if !atlas_summary.entities.is_empty() {
            user.push_str("## Entities (by salience)\n\n");
            for e in &atlas_summary.entities {
                user.push_str(&format!(
                    "- `{}` **{}** (salience {:.2}) — {}\n",
                    e.id, e.canonical_name, e.salience, e.description
                ));
            }
            user.push('\n');
        }

        if !atlas_summary.relations.is_empty() {
            user.push_str("## Relations\n\n");
            for r in &atlas_summary.relations {
                user.push_str(&format!(
                    "- `{}` **{}** — between {}\n",
                    r.id,
                    r.label,
                    r.participants.join(" × ")
                ));
            }
            user.push('\n');
        }

        if !atlas_summary.trajectories.is_empty() {
            user.push_str("## Character trajectories (state chains in section order)\n\n");
            for t in &atlas_summary.trajectories {
                user.push_str(&format!(
                    "- `{}` **{}** — {}\n",
                    t.entity_id,
                    t.canonical_name,
                    t.state_labels.join(" → ")
                ));
            }
            user.push('\n');
        }

        if !atlas_summary.top_claims.is_empty() {
            user.push_str("## Top claims (by confidence)\n\n");
            for c in &atlas_summary.top_claims {
                let attrib = c
                    .attributed_to
                    .as_deref()
                    .map(|a| format!(" [attributed to **{a}**]"))
                    .unwrap_or_default();
                user.push_str(&format!(
                    "- `{}` ({}){} — {}\n",
                    c.id, c.discourse_act, attrib, c.content
                ));
            }
            user.push('\n');
        }

        if !atlas_summary.open_questions.is_empty() {
            user.push_str("## Open questions (unresolved by any claim)\n\n");
            for q in &atlas_summary.open_questions {
                user.push_str(&format!("- `{}` — {}\n", q.id, q.content));
            }
            user.push('\n');
        }

        if !atlas_summary.key_events.is_empty() {
            user.push_str("## Key events\n\n");
            for ev in &atlas_summary.key_events {
                user.push_str(&format!(
                    "- `{}` — {} (participants: {})\n",
                    ev.id,
                    ev.description,
                    ev.participants.join(", ")
                ));
            }
            user.push('\n');
        }

        user.push_str("\nReturn 0–3 configurations as strict JSON per the system prompt.");

        Some(ChatPrompt::new(PHASE8_CONFIGURATION_SYSTEM, user).with_phase_id("phase8_configuration"))
    }

    fn parse_phase8_configuration(
        &self,
        response: &str,
    ) -> Result<Vec<crate::enrichment::atlas::analysis::Phase8ParseItem>> {
        let cleaned =
            super::literary::prepare_phase_json(response, "phase 8 (configuration)")?;

        #[derive(serde::Deserialize)]
        struct RawOutput {
            #[serde(default)]
            configurations: Vec<crate::enrichment::atlas::analysis::Phase8ParseItem>,
        }

        let raw: RawOutput = serde_json::from_str(&cleaned).map_err(|e| {
            crate::error::Error::Serialization(format!(
                "phase 8 (configuration) response is not valid JSON: {e}"
            ))
        })?;
        Ok(raw.configurations)
    }
}

// ── Helpers ──────────────────────────────────────────────────

/// Coerce a Phase 3 metadata value into a flat string for the
/// `HashMap<String, String>` that downstream consumers expect.
///
/// Some per-facet preambles ask the model to emit arrays (e.g.
/// `participants: ["entity_a", "entity_b"]` for relation-state
/// trajectories). The downstream metadata bag is flat strings, so we
/// flatten arrays by joining string elements with ", ". Other shapes
/// are preserved by stringifying — better than dropping the slot
/// entirely. Returns `None` only for explicit nulls so the parser's
/// .filter_map drops them, matching the prior `Option<String>` shape.
fn phase3_metadata_value_to_string(v: serde_json::Value) -> Option<String> {
    use serde_json::Value;
    match v {
        Value::Null => None,
        Value::String(s) => Some(s),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(items) => {
            let parts: Vec<String> = items
                .into_iter()
                .filter_map(phase3_metadata_value_to_string)
                .filter(|s| !s.trim().is_empty())
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(", "))
            }
        }
        Value::Object(map) => serde_json::to_string(&Value::Object(map)).ok(),
    }
}

/// Filter non-object items out of Phase 1 array fields whose schema
/// declares structs. Observed on Gemma-31B running sep-al-farabi
/// sec_0003: the model interleaved `"//"` comment-strings into
/// `entities_introduced` between actual entity objects, breaking
/// deserialization. Walks the seven known sketch arrays in the
/// section extraction and drops anything that isn't an Object or
/// Null. Idempotent — strings/numbers/booleans never legitimately
/// appear in these slots.
fn sanitize_phase1_object_arrays(value: &mut serde_json::Value) {
    use serde_json::Value;
    let Value::Object(top) = value else { return };
    const OBJECT_ARRAY_FIELDS: &[&str] = &[
        "entities_introduced",
        "entities_developed",
        "relations_introduced",
        "relations_developed",
        "events",
        "claims",
        "questions_raised",
    ];
    for key in OBJECT_ARRAY_FIELDS {
        if let Some(Value::Array(items)) = top.get_mut(*key) {
            items.retain(|item| matches!(item, Value::Object(_) | Value::Null));
        }
    }
}

/// Pick the first event description from the extraction to fill the
/// legacy `plot` field. The atlas has richer event records; this is a
/// one-sentence back-compat summary.
/// Compose the user-message body for Phase 1. Shared by the
/// default, seed-aware, and terse variants so the body stays
/// identical across all three — only the system preamble +
/// whether exemplars + whether a seed block are included differ.
///
/// When `seed` is `Some`, a "Known canonical names in this corpus"
/// block is rendered at the top of the user message. Chapter-level
/// map calls use these to resolve pronouns and alias variants to
/// stable forms, which is the whole point of Stage 1a.
pub(super) fn render_phase1_user_body(
    chapter: &ChapterInput,
    exemplars: &[&Exemplar],
    include_exemplars: bool,
    seed: Option<&SeedEntities>,
) -> String {
    let mut user = String::new();

    if let Some(seed) = seed {
        if !seed.entries.is_empty() {
            user.push_str("# Known canonical names in this corpus\n\n");
            user.push_str(
                "When a character, place, or other entity below appears in the \
                 chapter under any form (full name, patronymic, nickname, \
                 pronoun with clear antecedent), use the CANONICAL NAME from \
                 this list — not whatever form the chapter happened to use \
                 and not a translated/transliterated variant. If a name from \
                 the text is not in this list, treat it as a new entity and \
                 choose a canonical form of your own.\n\n",
            );
            for entry in &seed.entries {
                let aliases = if entry.aliases.is_empty() {
                    String::new()
                } else {
                    format!(" (aliases: {})", entry.aliases.join(", "))
                };
                let description = if entry.description.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", entry.description)
                };
                user.push_str(&format!(
                    "- **{}** [{}]{}{}\n",
                    entry.canonical_name,
                    entry.entity_type.as_str_repr(),
                    aliases,
                    description,
                ));
            }
            user.push_str("\n---\n\n");
        }
    }

    if include_exemplars && !exemplars.is_empty() {
        user.push_str("# Reference exemplars\n\n");
        user.push_str(
            "Each block shows the shape of a well-formed atlas \
             extraction. Produce your own analysis of the chapter \
             below; do NOT copy any exemplar's content.\n\n",
        );
        for (i, e) in exemplars.iter().enumerate() {
            render_atlas_exemplar(&mut user, i + 1, e);
        }
        user.push_str("---\n\n");
    }

    user.push_str("# Chapter to analyse\n\n");
    user.push_str(&format!("**Section id:** {}\n", chapter.chapter_id));
    user.push_str(&format!("**Title:** {}\n", chapter.title));
    if let Some(ord) = chapter.metadata.get("ordinal") {
        user.push_str(&format!("**Position:** chapter {ord}\n"));
    }
    user.push_str("\n**Body:**\n\n");
    user.push_str(&chapter.text);
    user.push_str("\n\n---\n\n");
    user.push_str(&format!(
        "Use `section_id = \"{}\"` in your response. Respond with \
         a single JSON object per the schema in the system message.",
        chapter.chapter_id
    ));

    user
}

fn first_event_description(extraction: &SectionExtraction) -> Option<String> {
    extraction
        .events
        .first()
        .map(|e| e.description.trim().to_string())
        .filter(|s| !s.is_empty() && !is_placeholder_literal(s))
}

/// Pick the first unattributed (text-level) claim to fill the legacy
/// `reveals` field — that's typically the structural argument of the
/// section. Falls back to the first attributed claim if no text-level
/// claim is present.
fn first_text_level_claim(extraction: &SectionExtraction) -> Option<String> {
    extraction
        .claims
        .iter()
        .find(|c| c.attributed_to.is_none())
        .or_else(|| extraction.claims.first())
        .map(|c| c.content.trim().to_string())
        .filter(|s| !s.is_empty() && !is_placeholder_literal(s))
}

/// Wipe placeholder-literal strings from a `SectionExtraction` so a
/// `"..."` that slipped past the prompt doesn't pollute downstream.
/// Strings that become empty after scrubbing stay empty — the skip-on-
/// empty serde attributes ensure they won't serialise.
fn scrub_placeholder_strings(e: &mut SectionExtraction) {
    fn scrub(s: &mut String) {
        if is_placeholder_literal(s) {
            s.clear();
        }
    }
    for entity in &mut e.entities_introduced {
        scrub(&mut entity.canonical_name);
        entity.aliases.retain(|a| !is_placeholder_literal(a));
        scrub(&mut entity.description);
        scrub(&mut entity.anchor);
    }
    for state in &mut e.entities_developed {
        scrub(&mut state.entity_name);
        scrub(&mut state.label);
        scrub(&mut state.anchor);
    }
    for relation in &mut e.relations_introduced {
        relation.participants.retain(|p| !is_placeholder_literal(p));
        scrub(&mut relation.label);
        scrub(&mut relation.anchor);
    }
    for state in &mut e.relations_developed {
        state.participants.retain(|p| !is_placeholder_literal(p));
        scrub(&mut state.label);
        scrub(&mut state.anchor);
    }
    for event in &mut e.events {
        scrub(&mut event.description);
        event.participants.retain(|p| !is_placeholder_literal(p));
        scrub(&mut event.anchor);
    }
    for claim in &mut e.claims {
        scrub(&mut claim.content);
        if let Some(a) = claim.attributed_to.as_mut() {
            scrub(a);
        }
        claim.attributed_to = claim.attributed_to.take().filter(|s| !s.is_empty());
        scrub(&mut claim.anchor);
    }
    for q in &mut e.questions_raised {
        scrub(&mut q.content);
        scrub(&mut q.anchor);
    }
}

// ── Lenient deserialisation layer ────────────────────────────
//
// Models drift on schema compliance: a claim drops a required field,
// an alias list has a stray null, a relation arrives with one
// participant. Hard-failing on any of these loses a whole chapter's
// extraction to save one malformed atom. The `Raw*` structs mirror
// the Phase 1 sketch shapes but accept optional required fields and
// drop nulls inside arrays, logging each drop so the prompt-compliance
// signal shows up in tracing without failing the run.
//
// Classification enums that moved to Phase 5 (state_type, event_type,
// relation_type, scope, question_type) are NOT present in these Raw
// structs — models are instructed to omit them, and the sketches on
// disk don't carry them.

fn vec_of_some<T>(v: Vec<Option<T>>) -> Vec<T> {
    v.into_iter().flatten().collect()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSectionExtraction {
    section_id: String,
    entities_introduced: Vec<Option<RawEntitySketch>>,
    entities_developed: Vec<Option<RawEntityStateSketch>>,
    relations_introduced: Vec<Option<RawRelationSketch>>,
    relations_developed: Vec<Option<RawRelationStateSketch>>,
    events: Vec<Option<RawEventSketch>>,
    claims: Vec<Option<RawClaimSketch>>,
    questions_raised: Vec<Option<RawQuestionSketch>>,
}

impl RawSectionExtraction {
    fn into_extraction(self) -> SectionExtraction {
        SectionExtraction {
            section_id: self.section_id,
            // Pin depth at `Extracted` — the atlas pipeline is by
            // definition the extraction-first ingestion strategy.
            // A structure-first strategy would build its
            // `SectionExtraction` records with
            // `EnrichmentDepth::Structural` instead.
            enrichment_depth: crate::enrichment::pipeline::atlas::EnrichmentDepth::Extracted,
            entities_introduced: vec_of_some(self.entities_introduced)
                .into_iter()
                .filter_map(RawEntitySketch::into_sketch)
                .collect(),
            entities_developed: vec_of_some(self.entities_developed)
                .into_iter()
                .filter_map(RawEntityStateSketch::into_sketch)
                .collect(),
            relations_introduced: vec_of_some(self.relations_introduced)
                .into_iter()
                .filter_map(RawRelationSketch::into_sketch)
                .collect(),
            relations_developed: vec_of_some(self.relations_developed)
                .into_iter()
                .filter_map(RawRelationStateSketch::into_sketch)
                .collect(),
            events: vec_of_some(self.events)
                .into_iter()
                .filter_map(RawEventSketch::into_sketch)
                .collect(),
            claims: vec_of_some(self.claims)
                .into_iter()
                .filter_map(RawClaimSketch::into_sketch)
                .collect(),
            questions_raised: vec_of_some(self.questions_raised)
                .into_iter()
                .filter_map(RawQuestionSketch::into_sketch)
                .collect(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEntitySketch {
    canonical_name: String,
    aliases: Vec<Option<String>>,
    entity_type: Option<EntityType>,
    description: String,
    anchor: String,
}

impl RawEntitySketch {
    fn into_sketch(self) -> Option<EntitySketch> {
        let name = self.canonical_name.trim().to_string();
        if name.is_empty() {
            debug!("literary_atlas: dropping entity sketch — canonical_name missing");
            return None;
        }
        Some(EntitySketch {
            canonical_name: name,
            aliases: vec_of_some(self.aliases),
            entity_type: self.entity_type.unwrap_or_else(|| {
                debug!("literary_atlas: defaulting entity_type to Other(\"unspecified\")");
                EntityType::Other("unspecified".into())
            }),
            description: self.description,
            anchor: self.anchor,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEntityStateSketch {
    entity_name: String,
    label: String,
    anchor: String,
}

impl RawEntityStateSketch {
    fn into_sketch(self) -> Option<EntityStateSketch> {
        let entity = self.entity_name.trim().to_string();
        let label = self.label.trim().to_string();
        if entity.is_empty() || label.is_empty() {
            debug!(
                "literary_atlas: dropping entity state sketch — entity_name={:?} label={:?}",
                entity, label
            );
            return None;
        }
        Some(EntityStateSketch {
            entity_name: entity,
            label,
            anchor: self.anchor,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRelationSketch {
    participants: Vec<Option<String>>,
    label: String,
    anchor: String,
}

impl RawRelationSketch {
    fn into_sketch(self) -> Option<RelationSketch> {
        let participants = vec_of_some(self.participants);
        let label = self.label.trim().to_string();
        if participants.len() < 2 || label.is_empty() {
            debug!(
                "literary_atlas: dropping relation sketch — participants={} label={:?}",
                participants.len(),
                label
            );
            return None;
        }
        Some(RelationSketch {
            participants,
            label,
            anchor: self.anchor,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawRelationStateSketch {
    participants: Vec<Option<String>>,
    label: String,
    anchor: String,
}

impl RawRelationStateSketch {
    fn into_sketch(self) -> Option<RelationStateSketch> {
        let participants = vec_of_some(self.participants);
        let label = self.label.trim().to_string();
        if participants.len() < 2 || label.is_empty() {
            debug!(
                "literary_atlas: dropping relation state sketch — participants={} label={:?}",
                participants.len(),
                label
            );
            return None;
        }
        Some(RelationStateSketch {
            participants,
            label,
            anchor: self.anchor,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawEventSketch {
    description: String,
    participants: Vec<Option<String>>,
    anchor: String,
}

impl RawEventSketch {
    fn into_sketch(self) -> Option<EventSketch> {
        let description = self.description.trim().to_string();
        if description.is_empty() {
            debug!("literary_atlas: dropping event sketch — description missing");
            return None;
        }
        Some(EventSketch {
            description,
            participants: vec_of_some(self.participants),
            anchor: self.anchor,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawClaimSketch {
    content: String,
    discourse_act: Option<DiscourseAct>,
    epistemic_status: Option<EpistemicStatus>,
    // Tolerant: the prompt asks for a single string, but Qwopus-27B
    // and other big-model variants sometimes emit a co-author array
    // (`["Author A", "Author B"]`). Phase 1 should not lose the
    // claim over a stylistic drift in attribution shape — flatten
    // arrays via `phase3_metadata_value_to_string` so the same
    // adapter that hardened Phase 3 metadata also works here.
    attributed_to: Option<serde_json::Value>,
    anchor: String,
}

impl RawClaimSketch {
    fn into_sketch(self) -> Option<ClaimSketch> {
        let content = self.content.trim().to_string();
        if content.is_empty() {
            debug!("literary_atlas: dropping claim sketch — content missing");
            return None;
        }
        // `discourse_act` is the field we refuse to default — it carries
        // the information the atlas uses to calibrate downstream
        // language ("argued" vs "enacted" vs "implied"). Dropping
        // claims without it preserves that invariant while keeping the
        // rest of the chapter.
        let Some(discourse_act) = self.discourse_act else {
            debug!("literary_atlas: dropping claim sketch — discourse_act missing");
            return None;
        };
        // `epistemic_status` has a sensible narrative-prose default —
        // the text commits unless it signals otherwise. Defaulting is
        // preferable to losing the claim.
        let epistemic_status = self
            .epistemic_status
            .unwrap_or(EpistemicStatus::Confident);
        let attributed_to = self
            .attributed_to
            .and_then(phase3_metadata_value_to_string)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Some(ClaimSketch {
            content,
            discourse_act,
            epistemic_status,
            attributed_to,
            anchor: self.anchor,
        })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawQuestionSketch {
    content: String,
    anchor: String,
}

impl RawQuestionSketch {
    fn into_sketch(self) -> Option<QuestionSketch> {
        let content = self.content.trim().to_string();
        if content.is_empty() {
            return None;
        }
        Some(QuestionSketch {
            content,
            anchor: self.anchor,
        })
    }
}

/// Render a Phase 3 facet-naming exemplar. The naming prompt
/// expects a small, shape-focused example — we surface the input
/// selector text + the target label/metadata. Keeps the naming
/// budget lean compared to the Phase 1 exemplar renderer.
pub(super) fn render_generic_phase3_exemplar(buf: &mut String, n: usize, e: &Exemplar) {
    buf.push_str(&format!("## Exemplar {n} ({:?})\n\n", e.kind));
    if let Some(input_text) = e.input.get("cluster_text").and_then(|v| v.as_str()) {
        buf.push_str(&format!("**Cluster snapshot:**\n{input_text}\n\n"));
    } else if let Some(selector) = e.selector_text.as_deref() {
        buf.push_str(&format!("**Cluster snapshot:**\n{selector}\n\n"));
    }
    match e.kind {
        ExemplarKind::Positive => {
            if let Some(out) = e.output.as_ref() {
                buf.push_str("**Target label:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(out).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
        ExemplarKind::Corrected => {
            if let Some(m) = e.model_output.as_ref() {
                buf.push_str("**What the model produced:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(m).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
            let correction = e.corrected_output.as_ref().or(e.output.as_ref());
            if let Some(c) = correction {
                buf.push_str("**Corrected label:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(c).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
        ExemplarKind::Negative => {
            if let Some(m) = e.model_output.as_ref() {
                buf.push_str("**Reject this label:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(m).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
    }
    buf.push_str(&format!("**Why:** {}\n\n", e.rationale));
}

/// Render one atlas exemplar into the user-message buffer. Mirrors
/// `render_phase1_exemplar` in `literary.rs` but targets the atlas
/// input shape (chapter text + expected SectionExtraction).
fn render_atlas_exemplar(buf: &mut String, n: usize, e: &Exemplar) {
    buf.push_str(&format!("## Exemplar {n} ({:?})\n\n", e.kind));
    if let Some(title) = e.input.get("title").and_then(|v| v.as_str()) {
        buf.push_str(&format!("**Chapter:** {title}\n"));
    }
    if let Some(excerpt) = e.input.get("excerpt").and_then(|v| v.as_str()) {
        buf.push_str(&format!("**Excerpt:** {excerpt}\n\n"));
    }
    match e.kind {
        ExemplarKind::Positive => {
            if let Some(out) = e.output.as_ref() {
                buf.push_str("**Target output:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(out).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
        ExemplarKind::Corrected => {
            if let Some(m) = e.model_output.as_ref() {
                buf.push_str("**Model produced:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(m).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
            let correction = e.corrected_output.as_ref().or(e.output.as_ref());
            if let Some(c) = correction {
                buf.push_str("**Corrected output:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(c).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
        ExemplarKind::Negative => {
            if let Some(m) = e.model_output.as_ref() {
                buf.push_str("**Reject this shape:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(m).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
    }
    buf.push_str(&format!("**Why:** {}\n\n", e.rationale));
}

// ── Phase 1 JSON Schema (for grammar-constrained generation) ─
//
// Lenient JSON Schema that mirrors `RawSectionExtraction`. Used by
// `phase1_section_extraction_schema()` so the daemon can pass it to
// `LlamaSampler::llguidance` and force the model to emit valid JSON.
// Strictness goal: eliminate the "invalid JSON syntax" failure mode
// (missing commas, unclosed brackets, duplicate keys) that recurs on
// long Phase 1 prompts. We do NOT enumerate enum strings — the
// `string_enum_with_other!` machinery already absorbs unknown values
// into `Other(String)`. We do NOT require most fields — the existing
// `Raw*::into_sketch()` drops sketches whose required fields are
// missing, so the parser stays the source of truth on completeness.
// Only `section_id` and `questions_raised` are required at the top
// level (mirroring the existing parser checks at parse_phase1).
const PHASE1_SECTION_EXTRACTION_SCHEMA: &str = r##"{
  "type": "object",
  "additionalProperties": true,
  "properties": {
    "section_id": { "type": "string" },
    "entities_introduced": {
      "type": "array",
      "items": { "$ref": "#/$defs/entity_sketch" }
    },
    "entities_developed": {
      "type": "array",
      "items": { "$ref": "#/$defs/entity_state_sketch" }
    },
    "relations_introduced": {
      "type": "array",
      "items": { "$ref": "#/$defs/relation_sketch" }
    },
    "relations_developed": {
      "type": "array",
      "items": { "$ref": "#/$defs/relation_state_sketch" }
    },
    "events": {
      "type": "array",
      "items": { "$ref": "#/$defs/event_sketch" }
    },
    "claims": {
      "type": "array",
      "items": { "$ref": "#/$defs/claim_sketch" }
    },
    "questions_raised": {
      "type": "array",
      "items": { "$ref": "#/$defs/question_sketch" }
    }
  },
  "required": ["section_id", "questions_raised"],
  "$defs": {
    "entity_sketch": {
      "type": "object",
      "additionalProperties": true,
      "properties": {
        "canonical_name": { "type": "string" },
        "aliases": { "type": "array", "items": { "type": "string" } },
        "entity_type": { "type": "string" },
        "description": { "type": "string" },
        "anchor": { "type": "string" }
      },
      "required": ["canonical_name"]
    },
    "entity_state_sketch": {
      "type": "object",
      "additionalProperties": true,
      "properties": {
        "entity_name": { "type": "string" },
        "label": { "type": "string" },
        "anchor": { "type": "string" }
      },
      "required": ["entity_name", "label"]
    },
    "relation_sketch": {
      "type": "object",
      "additionalProperties": true,
      "properties": {
        "participants": { "type": "array", "items": { "type": "string" } },
        "label": { "type": "string" },
        "anchor": { "type": "string" }
      },
      "required": ["participants", "label"]
    },
    "relation_state_sketch": {
      "type": "object",
      "additionalProperties": true,
      "properties": {
        "participants": { "type": "array", "items": { "type": "string" } },
        "label": { "type": "string" },
        "anchor": { "type": "string" }
      },
      "required": ["participants", "label"]
    },
    "event_sketch": {
      "type": "object",
      "additionalProperties": true,
      "properties": {
        "description": { "type": "string" },
        "participants": { "type": "array", "items": { "type": "string" } },
        "anchor": { "type": "string" }
      },
      "required": ["description"]
    },
    "claim_sketch": {
      "type": "object",
      "additionalProperties": true,
      "properties": {
        "content": { "type": "string" },
        "discourse_act": { "type": "string" },
        "epistemic_status": { "type": "string" },
        "attributed_to": {
          "anyOf": [
            { "type": "string" },
            { "type": "array", "items": { "type": "string" } },
            { "type": "null" }
          ]
        },
        "anchor": { "type": "string" }
      },
      "required": ["content", "discourse_act"]
    },
    "question_sketch": {
      "type": "object",
      "additionalProperties": true,
      "properties": {
        "content": { "type": "string" },
        "anchor": { "type": "string" }
      },
      "required": ["content"]
    }
  }
}"##;

/// Return the Phase 1 section-extraction JSON Schema as a parsed
/// `serde_json::Value`. Callers thread this through
/// `ChatPrompt::with_response_schema()` so the daemon's
/// grammar-constrained sampler forces the model into valid JSON.
///
/// The schema lives as a const string to avoid a `schemars` dep; the
/// const is unit-tested for parse-validity below so drift caught at
/// compile + test time, not at first runtime use.
pub fn phase1_section_extraction_schema() -> serde_json::Value {
    serde_json::from_str(PHASE1_SECTION_EXTRACTION_SCHEMA)
        .expect("PHASE1_SECTION_EXTRACTION_SCHEMA must be valid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample_chapter() -> ChapterInput {
        ChapterInput {
            chapter_id: "sec_0001".into(),
            title: "The Elder's Counsel".into(),
            text: "Zosima laid his hand upon Alyosha's head.".into(),
            metadata: HashMap::new(),
            approx_tokens: 10,
        }
    }

    #[test]
    fn seed_strategy_is_llm() {
        let p = LiteraryAtlasPipeline::new();
        assert_eq!(p.seed_strategy(), SeedStrategy::Llm);
    }

    #[test]
    fn compose_seed_prompt_uses_seed_asset_and_first_section_body() {
        let p = LiteraryAtlasPipeline::new();
        let prompt = p
            .compose_seed_prompt(&sample_chapter())
            .expect("Llm strategy returns Some");
        // System preamble identifies itself as the seed pass.
        assert!(prompt.system.contains("seed entity extraction"));
        assert!(prompt.system.contains("seed entity list"));
        // User body carries the chapter text verbatim.
        assert!(prompt.user.contains("The Elder's Counsel"));
        assert!(prompt.user.contains("Zosima laid his hand"));
    }

    #[test]
    fn parse_seed_response_extracts_typed_entries() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "entries": [
            {
              "canonical_name": "Fyodor Pavlovich Karamazov",
              "aliases": ["Fyodor", "the father"],
              "entity_type": "person",
              "description": "Patriarch of the Karamazov household."
            },
            {
              "canonical_name": "Alyosha",
              "aliases": ["Alexei Fyodorovich", "Alyoshka"],
              "entity_type": "person",
              "description": "Youngest brother; novice at the monastery."
            }
          ]
        }"#;
        let entries = p.parse_seed_response(response).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].canonical_name, "Fyodor Pavlovich Karamazov");
        assert_eq!(entries[0].aliases.len(), 2);
        assert_eq!(entries[0].entity_type, EntityType::Person);
        assert_eq!(
            entries[1].description,
            "Youngest brother; novice at the monastery."
        );
    }

    #[test]
    fn parse_seed_response_strips_placeholder_literals_and_nulls() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "entries": [
            {
              "canonical_name": "Zossima",
              "aliases": [null, "Father Zossima", "..."],
              "entity_type": "person",
              "description": "..."
            },
            {
              "canonical_name": "...",
              "description": "placeholder"
            }
          ]
        }"#;
        let entries = p.parse_seed_response(response).unwrap();
        assert_eq!(entries.len(), 1, "placeholder canonical_name drops entry");
        assert_eq!(entries[0].canonical_name, "Zossima");
        assert_eq!(entries[0].aliases, vec!["Father Zossima".to_string()]);
        assert!(entries[0].description.is_empty());
    }

    #[test]
    fn parse_seed_response_errors_on_empty_entries() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{"entries": []}"#;
        let err = p.parse_seed_response(response).unwrap_err();
        assert!(format!("{err}").contains("no valid entity entries"));
    }

    #[test]
    fn parse_seed_response_strips_think_block() {
        let p = LiteraryAtlasPipeline::new();
        let response = "<think>considering the chapter's characters</think>\n\
            {\"entries\":[{\"canonical_name\":\"Fyodor\",\"entity_type\":\"person\",\"description\":\"x\"}]}";
        let entries = p.parse_seed_response(response).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].canonical_name, "Fyodor");
    }

    #[test]
    fn compose_phase1_with_seed_renders_canonical_names_block() {
        let p = LiteraryAtlasPipeline::new();
        let seed = SeedEntities {
            schema_version: SeedEntities::SCHEMA_VERSION,
            corpus_id: "brothers_karamazov".into(),
            origin: super::super::super::atlas::SeedOrigin::Llm,
            entries: vec![SeedEntity {
                canonical_name: "Alyosha".into(),
                aliases: vec!["Alexei Fyodorovich".into()],
                entity_type: EntityType::Person,
                description: "Youngest Karamazov brother.".into(),
            }],
            written_at: "t".into(),
        };
        let prompt = p.compose_phase1_with_seed(&sample_chapter(), &[], Some(&seed));
        assert!(prompt.user.contains("Known canonical names"));
        assert!(prompt.user.contains("**Alyosha**"));
        assert!(prompt.user.contains("Alexei Fyodorovich"));
        assert!(prompt.user.contains("Youngest Karamazov brother"));
        // Chapter body still present below the seed block.
        assert!(prompt.user.contains("The Elder's Counsel"));
    }

    #[test]
    fn compose_phase1_with_seed_none_matches_legacy_compose_phase1() {
        let p = LiteraryAtlasPipeline::new();
        let default_prompt = p.compose_phase1(&sample_chapter(), &[]);
        let seed_none_prompt = p.compose_phase1_with_seed(&sample_chapter(), &[], None);
        assert_eq!(default_prompt.user, seed_none_prompt.user);
        assert_eq!(default_prompt.system, seed_none_prompt.system);
    }

    #[test]
    fn default_pipeline_has_no_seed_strategy() {
        use crate::enrichment::pipeline::pipelines::literary::LiteraryPipeline;
        let p = LiteraryPipeline::new();
        assert_eq!(p.seed_strategy(), SeedStrategy::None);
        // compose_seed_prompt returns None — the pipeline doesn't
        // know how to produce a seed.
        assert!(p.compose_seed_prompt(&sample_chapter()).is_none());
    }

    #[test]
    fn pipeline_id_is_literary_atlas() {
        let p = LiteraryAtlasPipeline::new();
        assert_eq!(p.id(), "literary_atlas");
    }

    #[test]
    fn compose_phase1_terse_uses_terse_preamble_and_omits_exemplars() {
        // Terse variant swaps the system preamble AND drops the
        // exemplar block, since the whole point is to save tokens
        // on a chapter that already blew past the budget.
        let p = LiteraryAtlasPipeline::new();
        let prompt = p.compose_phase1_terse(&sample_chapter()).expect("literary_atlas always returns Some");
        // Pin the terse-specific directive from the asset.
        assert!(
            prompt.system.contains("Do NOT show your reasoning"),
            "expected terse directive in system preamble"
        );
        // Default preamble's shape example should be gone — terse
        // asset drops it to save tokens.
        assert!(!prompt.system.contains("EXAMPLE_ONLY_REPLACE_ME"));
        // User body still carries the chapter id + title so the
        // model has something to ground on.
        assert!(prompt.user.contains("sec_0001"));
        assert!(prompt.user.contains("The Elder's Counsel"));
        // No exemplar header — even if an exemplar bank existed,
        // the terse path wouldn't thread it through.
        assert!(!prompt.user.contains("# Reference exemplars"));
    }

    #[test]
    fn compose_phase1_terse_is_shorter_than_default_variant() {
        // Sanity: the whole reason this variant exists is to use
        // fewer tokens in the prompt so more are available for the
        // JSON answer. Pin that it's strictly smaller than the
        // default at an identical chapter input.
        let p = LiteraryAtlasPipeline::new();
        let default_prompt = p.compose_phase1(&sample_chapter(), &[]);
        let terse_prompt = p.compose_phase1_terse(&sample_chapter()).expect("literary_atlas always returns Some");
        let default_total = default_prompt.system.len() + default_prompt.user.len();
        let terse_total = terse_prompt.system.len() + terse_prompt.user.len();
        assert!(
            terse_total < default_total,
            "terse prompt should be smaller than default: terse={terse_total}, default={default_total}"
        );
    }

    #[test]
    fn compose_phase1_mentions_section_id_and_title() {
        let p = LiteraryAtlasPipeline::new();
        let prompt = p.compose_phase1(&sample_chapter(), &[]);
        assert!(prompt.user.contains("sec_0001"));
        assert!(prompt.user.contains("The Elder's Counsel"));
        // System preamble is the atlas one, not the legacy literary one.
        assert!(prompt.system.contains("atlas extraction"));
    }

    #[test]
    fn parse_phase1_roundtrips_slim_extraction() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [{
            "canonical_name": "Alyosha",
            "entity_type": "person",
            "description": "Youngest Karamazov, novice at the monastery.",
            "anchor": "Alyosha knelt at Zosima's feet"
          }],
          "entities_developed": [{
            "entity_name": "Alyosha",
            "label": "Unshaken faith meeting mortality",
            "anchor": "without Zosima in it"
          }],
          "events": [{
            "description": "Zosima instructs Alyosha to leave the monastery.",
            "participants": ["Zosima", "Alyosha"],
            "anchor": "go out into the world"
          }],
          "claims": [{
            "content": "Active love costs more than dreamt love.",
            "discourse_act": "argue",
            "epistemic_status": "confident",
            "attributed_to": "Zosima",
            "anchor": "love in dreams is greedy"
          }],
          "questions_raised": [{
            "content": "Can a faith formed in the cell survive the world outside?",
            "anchor": "faith in the cell"
          }]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();

        // Legacy fields are derived for back-compat with v1 Phase 2/3.
        assert_eq!(parsed.questions.len(), 1);
        assert!(parsed.questions[0].contains("faith"));
        // Plot = first event description.
        assert!(parsed
            .plot
            .as_deref()
            .unwrap()
            .contains("leave the monastery"));
        // Thematic carriers deduplicated across entity sketches.
        assert_eq!(parsed.thematic_carriers, vec!["Alyosha".to_string()]);
        // Atlas structure preserved intact.
        let extraction = parsed.section_extraction.expect("should carry atlas");
        assert_eq!(extraction.entities_introduced.len(), 1);
        assert_eq!(
            extraction.entities_introduced[0].entity_type,
            EntityType::Person
        );
        assert_eq!(extraction.claims[0].discourse_act, DiscourseAct::Argue);
        assert_eq!(
            extraction.claims[0].epistemic_status,
            EpistemicStatus::Confident
        );
        // Anchors preserved.
        assert_eq!(
            extraction.claims[0].anchor,
            "love in dreams is greedy"
        );
    }

    #[test]
    fn parse_phase1_rejects_empty_extraction() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{"section_id": "sec_0001"}"#;
        let err = p.parse_phase1(response).unwrap_err();
        assert!(
            format!("{err}").contains("did not extract"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_phase1_rejects_extraction_with_no_questions() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [{
            "canonical_name": "Alyosha",
            "entity_type": "person"
          }]
        }"#;
        let err = p.parse_phase1(response).unwrap_err();
        assert!(
            format!("{err}").contains("questions"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_phase1_tolerates_unknown_entity_type_tag() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [{
            "canonical_name": "Grace",
            "entity_type": "deity",
            "description": "A personified force in the text"
          }],
          "questions_raised": [{"content": "Is grace earned or given?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let extraction = parsed.section_extraction.unwrap();
        assert_eq!(
            extraction.entities_introduced[0].entity_type,
            EntityType::Other("deity".into())
        );
    }

    #[test]
    fn parse_phase1_strips_think_block_before_parsing() {
        let p = LiteraryAtlasPipeline::new();
        let response = "<think>reasoning about what to extract…</think>\n\
            {\"section_id\":\"sec_0001\",\
             \"claims\":[{\"content\":\"c\",\"discourse_act\":\"enact\",\
                          \"epistemic_status\":\"confident\"}],\
             \"questions_raised\":[{\"content\":\"q?\"}]}";
        let parsed = p.parse_phase1(response).unwrap();
        assert_eq!(parsed.questions, vec!["q?".to_string()]);
    }

    #[test]
    fn parse_phase1_drops_malformed_claims_keeps_rest_of_extraction() {
        // A claim missing discourse_act is malformed — we drop it.
        // The other claim plus questions + entity survive.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [{
            "canonical_name": "Alyosha",
            "entity_type": "person"
          }],
          "claims": [
            {"content": "no discourse act here"},
            {"content": "a proper claim", "discourse_act": "argue", "epistemic_status": "confident"}
          ],
          "questions_raised": [{"content": "Why?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let extraction = parsed.section_extraction.unwrap();
        assert_eq!(
            extraction.claims.len(),
            1,
            "malformed claim should be dropped"
        );
        assert_eq!(extraction.claims[0].content, "a proper claim");
        assert_eq!(extraction.entities_introduced.len(), 1);
    }

    #[test]
    fn parse_phase1_defaults_epistemic_status_for_claims() {
        // Narrative prose claims default epistemic_status=Confident
        // when missing; only discourse_act is mandatory.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "claims": [{
            "content": "Passion outside the social order is self-destructive.",
            "discourse_act": "enact"
          }],
          "questions_raised": [{"content": "What becomes of unbound passion?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let c = &parsed.section_extraction.unwrap().claims[0];
        assert_eq!(c.discourse_act, DiscourseAct::Enact);
        assert_eq!(c.epistemic_status, EpistemicStatus::Confident);
    }

    #[test]
    fn phase1_section_extraction_schema_parses_as_valid_json() {
        // Pin the schema-string-vs-JSON-validity contract so a typo
        // in the const fails at unit-test time rather than at first
        // grammar-constrained chat call. The helper itself
        // `expect()`s parse success, so this also asserts the
        // fallback path won't panic in production.
        let schema = phase1_section_extraction_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["section_id"].is_object());
        assert!(schema["properties"]["questions_raised"].is_object());
        // `$defs` carries the per-sketch object schemas — most likely
        // place to fluff a typo when adding a new sketch type.
        let defs = &schema["$defs"];
        for sketch in [
            "entity_sketch",
            "entity_state_sketch",
            "relation_sketch",
            "relation_state_sketch",
            "event_sketch",
            "claim_sketch",
            "question_sketch",
        ] {
            assert!(defs[sketch].is_object(), "missing $defs/{sketch}");
        }
    }

    #[test]
    fn compose_phase1_attaches_response_schema_for_grammar_constraint() {
        // Regression: every Phase 1 prompt path (default, with-seed,
        // terse) must carry the response_schema so the daemon's
        // grammar-constrained sampler engages. Without this the
        // schema is silently dropped and we're back to malformed
        // JSON drift on Gemma-31B / Qwopus-27B.
        let p = LiteraryAtlasPipeline::new();
        let chap = sample_chapter();
        let default_prompt = p.compose_phase1(&chap, &[]);
        assert_eq!(
            default_prompt.response_schema_name.as_deref(),
            Some("phase1_section_extraction")
        );
        assert!(default_prompt.response_schema.is_some());

        let terse_prompt = p.compose_phase1_terse(&chap).expect("terse variant");
        assert!(terse_prompt.response_schema.is_some());

        let seed_prompt = p.compose_phase1_with_seed(&chap, &[], None);
        assert!(seed_prompt.response_schema.is_some());
    }

    #[test]
    fn parse_phase1_keeps_last_value_on_duplicate_keys() {
        // Observed on Gemma-31B running sep-al-farabi sec_0003: the
        // model emitted the same `description` field twice on a
        // single entity, possibly from a self-correction mid-stream.
        // The Value-first parse path silently keeps the last value;
        // we lose the first description but keep the section.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [{
            "canonical_name": "kalâm",
            "entity_type": "concept",
            "description": "First description.",
            "description": "Replacement description.",
            "anchor": "kalam"
          }],
          "claims": [{"content": "X.", "discourse_act": "assert"}],
          "questions_raised": [{"content": "What is kalâm?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let e = &parsed.section_extraction.unwrap().entities_introduced[0];
        assert_eq!(e.description, "Replacement description.");
    }

    #[test]
    fn parse_phase1_filters_comment_strings_from_object_arrays() {
        // Observed on Gemma-31B running sep-al-farabi sec_0003 retry:
        // the model interleaved `"//"` literal strings between entity
        // objects, presumably as commentary. The pre-pass strips
        // those so the typed deserializer sees only valid struct or
        // null entries.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [
            "//",
            {
              "canonical_name": "Al-Fârâbî",
              "entity_type": "person",
              "description": "Philosopher.",
              "anchor": "Al-Farabi"
            },
            "// stray note from the model"
          ],
          "claims": [{"content": "Y.", "discourse_act": "assert"}],
          "questions_raised": [{"content": "Who is Al-Fârâbî?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let entities = &parsed.section_extraction.unwrap().entities_introduced;
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].canonical_name, "Al-Fârâbî");
    }

    #[test]
    fn parse_phase1_flattens_array_attributed_to_for_claims() {
        // Observed on Qwopus3.5-27B running sep-african-sage sec_0002:
        // the model emitted `attributed_to: ["Henry Oruka", "Kwasi
        // Wiredu"]` for a co-attributed claim. The schema asks for a
        // single string, but losing the whole claim over a stylistic
        // drift in attribution shape is too costly. The parser
        // flattens arrays via the same string-coercion adapter that
        // hardens Phase 3 metadata.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "claims": [{
            "content": "African sage philosophy admits both individual and collective authorship.",
            "discourse_act": "argue",
            "attributed_to": ["Henry Oruka", "Kwasi Wiredu"]
          }],
          "questions_raised": [{"content": "Who counts as a sage?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let c = &parsed.section_extraction.unwrap().claims[0];
        assert_eq!(
            c.attributed_to.as_deref(),
            Some("Henry Oruka, Kwasi Wiredu")
        );
    }

    #[test]
    fn parse_phase1_drops_relation_without_two_participants() {
        // Relations inherently involve at least two entities. A one-
        // participant relation is a schema echo or hallucination.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "relations_introduced": [
            {"participants": ["solo"], "label": "lonely bond"},
            {"participants": ["A", "B"], "label": "real bond"}
          ],
          "questions_raised": [{"content": "?"}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let rels = &parsed.section_extraction.unwrap().relations_introduced;
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].label, "real bond");
    }

    #[test]
    fn compose_phase3_facet_selects_right_preamble_per_facet() {
        // Pin the asset-per-facet routing. Each facet carries a
        // distinctive phrase from its Phase 3 prompt that we can
        // match against the returned ChatPrompt.system.
        let p = LiteraryAtlasPipeline::new();
        let cluster = AtlasCluster {
            id: "claim_cl_0001".into(),
            facet: Facet::Claim,
            refs: vec![],
        };
        let excerpts = vec![SketchExcerpt {
            section_id: "sec_0001".into(),
            content: "[enact/confident] love costs".into(),
            anchor: String::new(),
        }];
        let claim_prompt = p
            .compose_phase3_facet(&cluster, Facet::Claim, &excerpts, &[])
            .expect("atlas pipeline supports claim facet naming");
        assert!(claim_prompt.system.contains("position family"));

        let trajectory = AtlasCluster {
            id: "entity_state_cl_0001".into(),
            facet: Facet::EntityState,
            refs: vec![],
        };
        let es_prompt = p
            .compose_phase3_facet(&trajectory, Facet::EntityState, &excerpts, &[])
            .unwrap();
        assert!(es_prompt.system.contains("trajectory arc"));

        let relation = AtlasCluster {
            id: "relation_state_cl_0001".into(),
            facet: Facet::RelationState,
            refs: vec![],
        };
        let rs_prompt = p
            .compose_phase3_facet(&relation, Facet::RelationState, &excerpts, &[])
            .unwrap();
        assert!(rs_prompt.system.contains("relational"));

        let events = AtlasCluster {
            id: "event_cl_0001".into(),
            facet: Facet::Event,
            refs: vec![],
        };
        let ev_prompt = p
            .compose_phase3_facet(&events, Facet::Event, &excerpts, &[])
            .unwrap();
        assert!(ev_prompt.system.contains("narrative thread"));
    }

    #[test]
    fn parse_phase3_facet_roundtrips_label_and_metadata() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "label": "Jane's movement from self-protective observation to acknowledged love.",
          "metadata": {
            "entity_name": "Jane",
            "scope": "novel-wide"
          }
        }"#;
        let parsed = p
            .parse_phase3_facet(Facet::EntityState, response)
            .unwrap();
        assert!(parsed.label.contains("Jane's movement"));
        assert_eq!(parsed.metadata.get("entity_name").unwrap(), "Jane");
    }

    #[test]
    fn parse_phase3_facet_rejects_empty_label() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{"label": ""}"#;
        let err = p.parse_phase3_facet(Facet::Claim, response).unwrap_err();
        assert!(format!("{err}").contains("label"));
    }

    #[test]
    fn parse_phase3_facet_flattens_array_metadata_values() {
        // The relation-state preamble explicitly asks for
        // `participants: ["a", "b"]`. The downstream metadata bag is
        // flat strings, so the parser flattens arrays by joining
        // string elements with ", " — model can be schema-faithful
        // without breaking the parser.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "label": "Frankfurt vs Fischer convergence on PAP rejection.",
          "metadata": {
            "participants": ["Harry Frankfurt", "John Martin Fischer"],
            "dynamic_type": "convergence"
          }
        }"#;
        let parsed = p.parse_phase3_facet(Facet::RelationState, response).unwrap();
        assert!(parsed.label.contains("Frankfurt"));
        assert_eq!(
            parsed.metadata.get("participants").unwrap(),
            "Harry Frankfurt, John Martin Fischer"
        );
        assert_eq!(parsed.metadata.get("dynamic_type").unwrap(), "convergence");
    }

    #[test]
    fn parse_phase3_facet_tolerates_null_metadata_values() {
        // Qwen occasionally emits `{"scope": null}` when unsure —
        // the parser treats null the same as omit.
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "label": "A real label.",
          "metadata": {"scope": null, "entity_name": "Alyosha"}
        }"#;
        let parsed = p.parse_phase3_facet(Facet::EntityState, response).unwrap();
        assert!(parsed.metadata.get("scope").is_none());
        assert_eq!(parsed.metadata.get("entity_name").unwrap(), "Alyosha");
    }

    #[test]
    fn default_pipeline_trait_methods_return_none_for_facet_naming() {
        // v1 LiteraryPipeline inherits the trait default → None.
        // Runners that try atlas naming on v1 pipelines get a
        // clear "unsupported" signal instead of a silent fallback.
        use crate::enrichment::pipeline::pipelines::literary::LiteraryPipeline;
        let p = LiteraryPipeline::new();
        let cluster = AtlasCluster {
            id: "x".into(),
            facet: Facet::Claim,
            refs: vec![],
        };
        let out = p.compose_phase3_facet(&cluster, Facet::Claim, &[], &[]);
        assert!(out.is_none());
    }

    #[test]
    fn parse_phase1_scrubs_placeholder_fields() {
        let p = LiteraryAtlasPipeline::new();
        let response = r#"{
          "section_id": "sec_0001",
          "entities_introduced": [{
            "canonical_name": "Alyosha",
            "entity_type": "person",
            "description": "...",
            "anchor": "…"
          }],
          "questions_raised": [{"content": "A real question."}]
        }"#;
        let parsed = p.parse_phase1(response).unwrap();
        let extraction = parsed.section_extraction.unwrap();
        assert!(extraction.entities_introduced[0].description.is_empty());
        assert!(extraction.entities_introduced[0].anchor.is_empty());
    }
}

// ── AtlasIngestion adapter ───────────────────────────────────
//
// Scaffolded during Step 1 back-fill so a future landing can wire
// the full 8-phase extraction-first flow through the
// `AtlasIngestion` trait without further module reshuffling. Today
// the adapter exists so the trait is exercised and the registry has
// `extraction_first` registered; the actual end-to-end ingestion is
// still driven by the per-phase CLI subcommands
// (`sovereign enrich extract`, `atlas-resolve`, etc.). A later step
// will consolidate the per-phase drivers into a single `ingest()`
// call that returns a populated `AtlasData`.

/// Adapter wrapping the `literary_atlas` extraction pipeline as the
/// canonical `AtlasIngestion` implementation for the
/// `extraction_first` strategy.
pub struct ExtractionFirstAdapter;

impl ExtractionFirstAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ExtractionFirstAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AtlasIngestion for ExtractionFirstAdapter {
    fn id(&self) -> &'static str {
        "extraction_first"
    }

    fn name(&self) -> &'static str {
        "Extraction-first (LLM per-section atlas)"
    }

    fn ingest<'a>(
        &'a self,
        _corpus: Arc<CorpusEngine>,
        _embed_fn: EmbedFn,
        _inference_fn: Option<InferenceFn>,
        _config: AtlasIngestionConfig,
        _progress: Arc<ProgressCallback>,
    ) -> Pin<Box<dyn Future<Output = Result<AtlasData>> + Send + 'a>> {
        // Scaffolded: returns an empty atlas bundle at `Extracted`
        // depth, pending the later landing that chains Phase 1-8
        // into one `ingest()` invocation. The adapter is registered
        // so the registry contract holds; callers that want real
        // atlas output drive the per-phase CLI subcommands today.
        Box::pin(async move {
            Ok(AtlasData {
                atoms: serde_json::json!([]),
                edges: serde_json::json!([]),
                trajectories: serde_json::json!({}),
                manifest: serde_json::json!({}),
                schema_validation: serde_json::json!({
                    "note": "ExtractionFirstAdapter::ingest is scaffolded \
                             for the Open/Closed surface; real atlas \
                             output is produced today by the per-phase \
                             CLI subcommands (extract, atlas-resolve, \
                             name-atlas-clusters, etc.)."
                }),
                dominant_depth: EnrichmentDepth::Extracted,
            })
        })
    }
}

/// Register the extraction-first strategy into an atlas-ingestion
/// registry. Called from
/// `enrichment::atlas::registry::AtlasIngestionRegistry::builtin`
/// so the registry file stays free of strategy-specific imports
/// beyond the trait.
pub fn register_extraction_first(registry: &mut AtlasIngestionRegistry) {
    registry.register("extraction_first", || Arc::new(ExtractionFirstAdapter::new()));
}

#[cfg(test)]
mod adapter_tests {
    use super::*;

    #[test]
    fn extraction_first_adapter_identifies_as_extraction_first() {
        let a = ExtractionFirstAdapter::new();
        assert_eq!(a.id(), "extraction_first");
    }

    #[test]
    fn register_extraction_first_populates_registry() {
        let mut r = AtlasIngestionRegistry::new();
        register_extraction_first(&mut r);
        assert_eq!(r.strategy_ids(), vec!["extraction_first"]);
        assert!(r.get("extraction_first").is_some());
    }

    #[test]
    fn extraction_first_adapter_metadata_is_stable() {
        // Step-1 scaffold: the adapter identifies itself + name.
        // Pins the contract so the registry lookup + status output
        // remain consistent when the full ingest lands.
        let a = ExtractionFirstAdapter::new();
        assert_eq!(a.id(), "extraction_first");
        assert_eq!(a.name(), "Extraction-first (LLM per-section atlas)");
    }
}
