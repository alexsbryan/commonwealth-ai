//! Literary pipeline — the first `Pipeline` implementation.
//!
//! Targets narrative prose (novels, short-story collections) with a
//! chapter-scale unit of composition. Domain scaffold per spec §8.1:
//!
//!   - canonical concern = "canonical concern"
//!   - position = "argument-through-narrative"
//!   - tension = "structural tension"
//!   - absence = "gap"
//!
//! Landing 1 fully implements phase 1 (per-chapter question extraction)
//! including exemplar injection and JSON parsing. Phases 3/5/6/7 are
//! scaffolded with stub compose/parse methods; they land in a later
//! iteration without requiring trait changes.

use super::super::exemplar_bank::{Exemplar, ExemplarKind};
use super::super::trait_def::Pipeline;
use super::super::types::*;
use crate::enrichment::domain::ClusteringConfig;
use crate::error::{Error, Result};

const PHASE1_SYSTEM: &str = include_str!("literary_prompts/phase1_system.md");
const PHASE3_SYSTEM: &str = include_str!("literary_prompts/phase3_system.md");
const PHASE5_SYSTEM: &str = include_str!("literary_prompts/phase5_system.md");
const PHASE6_SYSTEM: &str = include_str!("literary_prompts/phase6_system.md");
const PHASE7_SYSTEM: &str = include_str!("literary_prompts/phase7_system.md");

/// Landing-1 literary pipeline. See module doc for scope.
pub struct LiteraryPipeline {
    vocabulary: Vocabulary,
}

impl LiteraryPipeline {
    pub fn new() -> Self {
        Self {
            vocabulary: Vocabulary {
                canonical_concern_term: "canonical concern".into(),
                position_term: "argument-through-narrative".into(),
                tension_term: "structural tension".into(),
                absence_term: "gap".into(),
                evidence_term: "passage".into(),
            },
        }
    }
}

impl Default for LiteraryPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline for LiteraryPipeline {
    fn id(&self) -> &'static str {
        "literary"
    }

    fn name(&self) -> &'static str {
        "Literary (narrative prose)"
    }

    fn vocabulary(&self) -> &Vocabulary {
        &self.vocabulary
    }

    fn phase1_system(&self) -> &'static str {
        PHASE1_SYSTEM
    }

    fn phase3_system(&self) -> &'static str {
        PHASE3_SYSTEM
    }

    fn phase5_system(&self) -> &'static str {
        PHASE5_SYSTEM
    }

    fn phase6_system(&self) -> &'static str {
        PHASE6_SYSTEM
    }

    fn phase7_system(&self) -> &'static str {
        PHASE7_SYSTEM
    }

    // ── Phase 1 — full implementation ─────────────────────────

    fn compose_phase1(
        &self,
        chapter: &ChapterInput,
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        let mut user = String::new();

        if !exemplars.is_empty() {
            user.push_str("# Reference exemplars\n\n");
            user.push_str("Each block shows how similar chapters should be handled.\n\n");
            for (i, e) in exemplars.iter().enumerate() {
                render_phase1_exemplar(&mut user, i + 1, e);
            }
            user.push_str("---\n\n");
        }

        user.push_str(&format!("# Chapter to analyze\n\n"));
        user.push_str(&format!("**Title:** {}\n", chapter.title));
        if !chapter.metadata.is_empty() {
            // Only surface detector metadata that's meaningful to a
            // reader. We keep it minimal; ordinal/byte offsets are
            // noise in a thematic prompt.
            if let Some(ord) = chapter.metadata.get("ordinal") {
                user.push_str(&format!("**Position:** chapter {ord}\n"));
            }
        }
        user.push('\n');
        user.push_str("**Body:**\n\n");
        user.push_str(&chapter.text);
        user.push_str("\n\n---\n\n");
        user.push_str(
            "Respond with a single JSON object per the schema in the system message.",
        );

        ChatPrompt::new(self.phase1_system(), user)
    }

    fn parse_phase1(&self, response: &str) -> Result<Phase1ChapterResult> {
        let block = extract_json_block(response).ok_or_else(|| {
            Error::Serialization(
                "phase 1 response contained no recognisable JSON object".into(),
            )
        })?;

        #[derive(serde::Deserialize)]
        struct Raw {
            #[serde(default)]
            questions: Vec<String>,
            #[serde(default)]
            reveals: Option<String>,
            #[serde(default)]
            thematic_carriers: Vec<String>,
        }

        let raw: Raw = serde_json::from_str(block).map_err(|e| {
            Error::Serialization(format!("phase 1 response is not valid JSON: {e}"))
        })?;

        if raw.questions.is_empty() {
            return Err(Error::Serialization(
                "phase 1 response is missing the required `questions` field \
                 (must be a non-empty array of strings)"
                    .into(),
            ));
        }
        // Validate that each question is not empty / not whitespace.
        for (i, q) in raw.questions.iter().enumerate() {
            if q.trim().is_empty() {
                return Err(Error::Serialization(format!(
                    "phase 1 response question[{i}] is empty"
                )));
            }
        }

        Ok(Phase1ChapterResult {
            questions: raw.questions,
            reveals: raw.reveals.filter(|s| !s.trim().is_empty()),
            thematic_carriers: raw
                .thematic_carriers
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect(),
        })
    }

    // ── Phase 3 — canonical concern naming ────────────────────

    fn compose_phase3(
        &self,
        cluster: &QuestionCluster,
        chapter_excerpts: &[&ChapterInput],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        let mut user = String::new();
        if !exemplars.is_empty() {
            user.push_str("# Reference exemplars\n\n");
            for (i, e) in exemplars.iter().enumerate() {
                render_generic_exemplar(&mut user, i + 1, e);
            }
            user.push_str("---\n\n");
        }

        user.push_str(&format!("# Question cluster to name (id: {})\n\n", cluster.id));
        user.push_str("The following per-chapter questions have been grouped by similarity:\n\n");
        for (i, q) in cluster.question_refs.iter().enumerate() {
            // Question text isn't in QuestionRef; the runner passes
            // resolved questions as chapter excerpts below. We show
            // the ref list here for provenance.
            user.push_str(&format!(
                "{}. {} (question {} of that chapter)\n",
                i + 1,
                q.chapter_id,
                q.question_index
            ));
        }

        if !chapter_excerpts.is_empty() {
            user.push_str("\n## Chapter excerpts\n\n");
            for ex in chapter_excerpts {
                let snippet: String = ex.text.chars().take(280).collect();
                user.push_str(&format!("**{} — {}:**\n{}…\n\n", ex.chapter_id, ex.title, snippet));
            }
        }

        user.push_str(
            "---\n\nRespond with a single JSON object per the schema in the system message.",
        );

        ChatPrompt::new(self.phase3_system(), user)
    }

    fn parse_phase3(&self, response: &str) -> Result<Phase3ParseResult> {
        let block = extract_json_block(response).ok_or_else(|| {
            Error::Serialization("phase 3 response contained no JSON object".into())
        })?;

        #[derive(serde::Deserialize)]
        struct Raw {
            concern_text: Option<String>,
            #[serde(default)]
            scope: Option<String>,
            #[serde(default)]
            primary_arcs: Vec<String>,
        }
        let raw: Raw = serde_json::from_str(block)
            .map_err(|e| Error::Serialization(format!("phase 3 JSON parse error: {e}")))?;
        let concern_text = raw
            .concern_text
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                Error::Serialization(
                    "phase 3 response is missing the required non-empty `concern_text`".into(),
                )
            })?;
        Ok(Phase3ParseResult {
            concern_text,
            scope: raw.scope.filter(|s| !s.trim().is_empty()),
            primary_arcs: raw
                .primary_arcs
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect(),
        })
    }

    // ── Phase 5 — grounded position extraction ────────────────

    fn compose_phase5(
        &self,
        concern: &CanonicalConcern,
        cluster: &ChunkCluster,
        cluster_chunk_texts: &[(u64, String)],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        let mut user = String::new();
        if !exemplars.is_empty() {
            user.push_str("# Reference exemplars\n\n");
            for (i, e) in exemplars.iter().enumerate() {
                render_generic_exemplar(&mut user, i + 1, e);
            }
            user.push_str("---\n\n");
        }
        user.push_str(&format!("# Canonical concern (id: {})\n\n", concern.id));
        user.push_str(&concern.concern_text);
        user.push_str("\n\n");

        user.push_str(&format!(
            "# Chunk cluster (id: {}, {} passages)\n\n",
            cluster.id,
            cluster_chunk_texts.len()
        ));
        for (id, text) in cluster_chunk_texts {
            let snippet: String = text.chars().take(400).collect();
            user.push_str(&format!("- `chunk_id={id}`: {snippet}\n\n"));
        }
        user.push_str(
            "---\n\nRespond with a single JSON object per the schema. Cite only `chunk_id`s that appear above.",
        );
        ChatPrompt::new(self.phase5_system(), user)
    }

    fn parse_phase5(&self, response: &str) -> Result<Phase5ParseResult> {
        let block = extract_json_block(response).ok_or_else(|| {
            Error::Serialization("phase 5 response contained no JSON object".into())
        })?;

        #[derive(serde::Deserialize)]
        struct RawGrounding {
            chunk_id: Option<u64>,
            #[serde(default)]
            section_id: Option<String>,
            #[serde(default)]
            summary: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct Raw {
            position_text: Option<String>,
            #[serde(default)]
            grounding: Vec<RawGrounding>,
            #[serde(default)]
            extensions: std::collections::HashMap<String, String>,
        }
        let raw: Raw = serde_json::from_str(block)
            .map_err(|e| Error::Serialization(format!("phase 5 JSON parse error: {e}")))?;
        let position_text = raw
            .position_text
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                Error::Serialization(
                    "phase 5 response is missing the required non-empty `position_text`".into(),
                )
            })?;
        if raw.grounding.is_empty() {
            return Err(Error::Serialization(
                "phase 5 response must include at least one `grounding` entry".into(),
            ));
        }
        let grounding: Vec<Grounding> = raw
            .grounding
            .into_iter()
            .filter_map(|g| {
                let id = g.chunk_id?;
                Some(Grounding {
                    chunk_id: id,
                    section_id: g.section_id.unwrap_or_default(),
                    summary: g.summary.unwrap_or_default(),
                })
            })
            .collect();
        if grounding.is_empty() {
            return Err(Error::Serialization(
                "phase 5 response had grounding entries but none carried a valid `chunk_id`"
                    .into(),
            ));
        }

        Ok(Phase5ParseResult {
            position_text,
            grounding,
            extensions: raw.extensions,
        })
    }

    // ── Phase 6 — pairwise tension detection ──────────────────

    fn compose_phase6(
        &self,
        pos_a: &Position,
        pos_b: &Position,
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        let mut user = String::new();
        if !exemplars.is_empty() {
            user.push_str("# Reference exemplars\n\n");
            for (i, e) in exemplars.iter().enumerate() {
                render_generic_exemplar(&mut user, i + 1, e);
            }
            user.push_str("---\n\n");
        }
        user.push_str("# Position A\n\n");
        user.push_str(&format!("id: {}\n", pos_a.id));
        user.push_str(&pos_a.position_text);
        if !pos_a.grounding.is_empty() {
            user.push_str("\nGrounding:\n");
            for g in &pos_a.grounding {
                let snip: String = g.summary.chars().take(160).collect();
                user.push_str(&format!("  · {} — {snip}\n", g.section_id));
            }
        }
        user.push_str("\n\n# Position B\n\n");
        user.push_str(&format!("id: {}\n", pos_b.id));
        user.push_str(&pos_b.position_text);
        if !pos_b.grounding.is_empty() {
            user.push_str("\nGrounding:\n");
            for g in &pos_b.grounding {
                let snip: String = g.summary.chars().take(160).collect();
                user.push_str(&format!("  · {} — {snip}\n", g.section_id));
            }
        }
        user.push_str(
            "\n\n---\n\nRespond with a single JSON object per the schema. It's ok to return `{\"tension\": false}` — most pairs are not in tension.",
        );
        ChatPrompt::new(self.phase6_system(), user)
    }

    fn parse_phase6(&self, response: &str) -> Result<Option<Phase6ParseResult>> {
        let block = extract_json_block(response).ok_or_else(|| {
            Error::Serialization("phase 6 response contained no JSON object".into())
        })?;

        #[derive(serde::Deserialize)]
        struct Raw {
            #[serde(default)]
            tension: Option<bool>,
            #[serde(default)]
            description: Option<String>,
            #[serde(default)]
            specific_disagreement: Option<String>,
            #[serde(default)]
            structural_type: Option<String>,
        }
        let raw: Raw = serde_json::from_str(block)
            .map_err(|e| Error::Serialization(format!("phase 6 JSON parse error: {e}")))?;

        match raw.tension {
            Some(true) => {
                let description = raw
                    .description
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| {
                        Error::Serialization(
                            "phase 6 response says tension=true but has no `description`".into(),
                        )
                    })?;
                Ok(Some(Phase6ParseResult {
                    description,
                    specific_disagreement: raw
                        .specific_disagreement
                        .filter(|s| !s.trim().is_empty()),
                    structural_type: raw.structural_type.filter(|s| !s.trim().is_empty()),
                }))
            }
            Some(false) => Ok(None),
            None => Err(Error::Serialization(
                "phase 6 response is missing required `tension` boolean".into(),
            )),
        }
    }

    // ── Phase 7 — gap detection ───────────────────────────────

    fn compose_phase7(
        &self,
        concerns: &[CanonicalConcern],
        positions: &[Position],
        chapter_titles: &[String],
        exemplars: &[&Exemplar],
    ) -> ChatPrompt {
        let mut user = String::new();
        if !exemplars.is_empty() {
            user.push_str("# Reference exemplars\n\n");
            for (i, e) in exemplars.iter().enumerate() {
                render_generic_exemplar(&mut user, i + 1, e);
            }
            user.push_str("---\n\n");
        }
        user.push_str("# Canonical concerns\n\n");
        for c in concerns {
            user.push_str(&format!("- `{}`: {}\n", c.id, c.concern_text));
        }
        user.push_str("\n# Positions\n\n");
        for p in positions {
            let snip: String = p.position_text.chars().take(220).collect();
            user.push_str(&format!(
                "- `{}` (concern {}): {snip}…\n",
                p.id, p.concern_id
            ));
        }
        user.push_str("\n# Chapter manifest\n\n");
        for (i, t) in chapter_titles.iter().enumerate() {
            user.push_str(&format!("{:>3}. {t}\n", i + 1));
        }
        user.push_str(
            "\n\n---\n\nRespond with a single JSON object per the schema. Return `{\"gaps\": []}` if the atlas is reasonably complete.",
        );
        ChatPrompt::new(self.phase7_system(), user)
    }

    fn parse_phase7(&self, response: &str) -> Result<Vec<Phase7ParseItem>> {
        let block = extract_json_block(response).ok_or_else(|| {
            Error::Serialization("phase 7 response contained no JSON object".into())
        })?;

        #[derive(serde::Deserialize)]
        struct RawGap {
            gap_text: Option<String>,
            #[serde(default)]
            evidence: Option<String>,
            #[serde(default)]
            significance: Option<String>,
        }
        #[derive(serde::Deserialize)]
        struct Raw {
            #[serde(default)]
            gaps: Vec<RawGap>,
        }
        let raw: Raw = serde_json::from_str(block)
            .map_err(|e| Error::Serialization(format!("phase 7 JSON parse error: {e}")))?;
        let mut out = Vec::new();
        for (i, g) in raw.gaps.into_iter().enumerate() {
            let gap_text = g
                .gap_text
                .filter(|s| !s.trim().is_empty())
                .ok_or_else(|| {
                    Error::Serialization(format!("phase 7 gaps[{i}] missing `gap_text`"))
                })?;
            out.push(Phase7ParseItem {
                gap_text,
                evidence: g.evidence.unwrap_or_default(),
                significance: g.significance.unwrap_or_default(),
            });
        }
        Ok(out)
    }

    // ── Clustering configs ────────────────────────────────────

    fn question_clustering_config(&self) -> ClusteringConfig {
        // Literary corpora produce a modest number of questions
        // (hundreds to low thousands for a single novel). Tight
        // clusters are desirable — each canonical concern should
        // unify 3-10 per-chapter questions, not 50.
        ClusteringConfig {
            min_cluster_size: 3,
            epsilon: 0.35,
            label_sample_size: 5,
            max_cluster_points: 0,
            reduced_dims: 0,
        }
    }

    fn chunk_clustering_config(&self) -> ClusteringConfig {
        // Paragraph-level chunks for a novel-length work are in the
        // low thousands. Stick to defaults similar to philosophy.
        ClusteringConfig {
            min_cluster_size: 5,
            epsilon: 0.30,
            label_sample_size: 5,
            max_cluster_points: 0,
            reduced_dims: 0,
        }
    }
}

fn render_generic_exemplar(buf: &mut String, n: usize, e: &Exemplar) {
    buf.push_str(&format!("## Exemplar {n} ({:?})\n\n", e.kind));
    buf.push_str("**Input:**\n```json\n");
    buf.push_str(&serde_json::to_string_pretty(&e.input).unwrap_or_default());
    buf.push_str("\n```\n\n");
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
                buf.push_str("**What the model produced:**\n```json\n");
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
                buf.push_str("**Model output (reject this shape):**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(m).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
    }
    buf.push_str(&format!("**Why:** {}\n\n", e.rationale));
}

fn render_phase1_exemplar(buf: &mut String, n: usize, e: &Exemplar) {
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
                let pretty = serde_json::to_string_pretty(out).unwrap_or_default();
                buf.push_str(&pretty);
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
                buf.push_str("**Corrected output:**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(c).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
        ExemplarKind::Negative => {
            if let Some(m) = e.model_output.as_ref() {
                buf.push_str("**What the model produced (reject this shape):**\n```json\n");
                buf.push_str(&serde_json::to_string_pretty(m).unwrap_or_default());
                buf.push_str("\n```\n\n");
            }
        }
    }
    buf.push_str(&format!("**Why:** {}\n\n", e.rationale));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn chapter(id: &str, title: &str, body: &str) -> ChapterInput {
        let mut meta = HashMap::new();
        meta.insert("ordinal".into(), "1".into());
        ChapterInput {
            chapter_id: id.into(),
            title: title.into(),
            text: body.into(),
            metadata: meta,
            approx_tokens: body.len() / 4,
        }
    }

    fn positive_exemplar() -> Exemplar {
        Exemplar {
            id: "ex_01".into(),
            kind: ExemplarKind::Positive,
            input: serde_json::json!({
                "title": "Part 1, Chapter 1",
                "excerpt": "Happy families are all alike...",
            }),
            output: Some(serde_json::json!({
                "questions": ["What happens when a family's trust breaks?"],
                "reveals": "Microcosm of a larger rupture."
            })),
            model_output: None,
            corrected_output: None,
            rationale: "Names the stakes, not the plot.".into(),
            selector_text: None,
            created_at: String::new(),
        }
    }

    #[test]
    fn identity_is_stable() {
        let p = LiteraryPipeline::new();
        assert_eq!(p.id(), "literary");
        assert_eq!(p.vocabulary().canonical_concern_term, "canonical concern");
    }

    #[test]
    fn phase1_system_is_loaded_from_asset() {
        let p = LiteraryPipeline::new();
        let sys = p.phase1_system();
        assert!(sys.contains("per-chapter question extraction"));
        assert!(sys.contains("strict JSON"));
    }

    #[test]
    fn compose_phase1_includes_chapter_body_and_exemplars() {
        let p = LiteraryPipeline::new();
        let exemplar = positive_exemplar();
        let ex_slice: Vec<&Exemplar> = vec![&exemplar];
        let ch = chapter("sec_0001", "Chapter 1", "Body of the chapter here.");

        let prompt = p.compose_phase1(&ch, &ex_slice);

        assert!(prompt.system.contains("Phase 1"));
        assert!(prompt.user.contains("Body of the chapter"));
        assert!(prompt.user.contains("Reference exemplars"));
        assert!(prompt.user.contains("Names the stakes"));
    }

    #[test]
    fn compose_phase1_without_exemplars_skips_reference_block() {
        let p = LiteraryPipeline::new();
        let ch = chapter("sec_0001", "Chapter 1", "Body.");
        let prompt = p.compose_phase1(&ch, &[]);
        assert!(!prompt.user.contains("Reference exemplars"));
        assert!(prompt.user.contains("Body."));
    }

    #[test]
    fn parse_phase1_accepts_pure_json() {
        let p = LiteraryPipeline::new();
        let raw = r#"{"questions":["q1","q2"],"reveals":"r","thematic_carriers":["A"]}"#;
        let got = p.parse_phase1(raw).unwrap();
        assert_eq!(got.questions, vec!["q1", "q2"]);
        assert_eq!(got.reveals.as_deref(), Some("r"));
        assert_eq!(got.thematic_carriers, vec!["A".to_string()]);
    }

    #[test]
    fn parse_phase1_accepts_fenced_json() {
        let p = LiteraryPipeline::new();
        let raw = "Here is the result:\n\n```json\n{\"questions\":[\"q1\"]}\n```\n";
        let got = p.parse_phase1(raw).unwrap();
        assert_eq!(got.questions, vec!["q1"]);
        assert!(got.reveals.is_none());
        assert!(got.thematic_carriers.is_empty());
    }

    #[test]
    fn parse_phase1_rejects_missing_questions() {
        let p = LiteraryPipeline::new();
        let err = p
            .parse_phase1(r#"{"reveals":"r"}"#)
            .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("questions"), "{msg}");
    }

    #[test]
    fn parse_phase1_rejects_empty_question_strings() {
        let p = LiteraryPipeline::new();
        let err = p
            .parse_phase1(r#"{"questions":["   ", ""]}"#)
            .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("empty"), "{msg}");
    }

    #[test]
    fn parse_phase1_rejects_non_json() {
        let p = LiteraryPipeline::new();
        let err = p.parse_phase1("not json at all").unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("JSON"), "{msg}");
    }

    #[test]
    fn phase1_positive_exemplar_roundtrips_through_parse() {
        // The positive exemplar's `output` field must validate with
        // `parse_phase1` — otherwise the example teaches a shape the
        // parser rejects.
        let p = LiteraryPipeline::new();
        let exemplar = positive_exemplar();
        let target = exemplar.output.as_ref().unwrap();
        let json = serde_json::to_string(target).unwrap();
        let parsed = p.parse_phase1(&json).unwrap();
        assert!(!parsed.questions.is_empty());
    }

    #[test]
    fn parse_phase3_accepts_minimal_concern() {
        let p = LiteraryPipeline::new();
        let got = p
            .parse_phase3(r#"{"concern_text":"Can x survive y?","scope":"novel-wide"}"#)
            .unwrap();
        assert_eq!(got.concern_text, "Can x survive y?");
        assert_eq!(got.scope.as_deref(), Some("novel-wide"));
    }

    #[test]
    fn parse_phase3_rejects_empty_concern() {
        let p = LiteraryPipeline::new();
        assert!(p.parse_phase3(r#"{"concern_text":""}"#).is_err());
        assert!(p.parse_phase3(r#"{}"#).is_err());
    }

    #[test]
    fn parse_phase5_requires_grounding() {
        let p = LiteraryPipeline::new();
        assert!(p
            .parse_phase5(r#"{"position_text":"X","grounding":[]}"#)
            .is_err());
        let got = p
            .parse_phase5(
                r#"{"position_text":"Anna's trajectory...","grounding":[{"chunk_id":1,"section_id":"sec_0001","summary":"s"}]}"#,
            )
            .unwrap();
        assert_eq!(got.grounding.len(), 1);
        assert_eq!(got.grounding[0].chunk_id, 1);
    }

    #[test]
    fn parse_phase6_true_requires_description() {
        let p = LiteraryPipeline::new();
        assert!(p.parse_phase6(r#"{"tension":true}"#).is_err());
        let got = p
            .parse_phase6(r#"{"tension":true,"description":"parallel contrast"}"#)
            .unwrap();
        assert!(got.is_some());
    }

    #[test]
    fn parse_phase6_false_returns_none() {
        let p = LiteraryPipeline::new();
        assert!(p.parse_phase6(r#"{"tension":false}"#).unwrap().is_none());
    }

    #[test]
    fn parse_phase6_missing_tension_errors() {
        let p = LiteraryPipeline::new();
        assert!(p.parse_phase6(r#"{}"#).is_err());
    }

    #[test]
    fn parse_phase7_accepts_empty_gaps_array() {
        let p = LiteraryPipeline::new();
        let gaps = p.parse_phase7(r#"{"gaps":[]}"#).unwrap();
        assert!(gaps.is_empty());
    }

    #[test]
    fn parse_phase7_requires_gap_text() {
        let p = LiteraryPipeline::new();
        assert!(p
            .parse_phase7(r#"{"gaps":[{"evidence":"x","significance":"low"}]}"#)
            .is_err());
        let gaps = p
            .parse_phase7(
                r#"{"gaps":[{"gap_text":"Vronsky's social world after Part 4","evidence":"scant references","significance":"medium"}]}"#,
            )
            .unwrap();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].significance, "medium");
    }

    #[test]
    fn clustering_configs_are_sane() {
        let p = LiteraryPipeline::new();
        let q = p.question_clustering_config();
        assert!(q.min_cluster_size >= 2);
        assert!(q.epsilon > 0.0 && q.epsilon < 1.0);
        let c = p.chunk_clustering_config();
        assert!(c.min_cluster_size >= 2);
        assert!(c.epsilon > 0.0 && c.epsilon < 1.0);
    }
}
