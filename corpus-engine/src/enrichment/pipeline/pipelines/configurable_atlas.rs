// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recipe-driven custom atlas — author a domain ontology, feed it to chat.
//!
//! The headline "build the ontology for your specific domain" capability. A
//! domain expert (with the recipe-author agent) describes — in the domain's own
//! language — what entities / relations / events / claims matter
//! (`[enrichment.ontology] guidance = "…"`). That guidance is appended to a
//! NEUTRAL Phase-1 atlas extraction prompt and run through the SAME universal
//! 7-phase atlas machinery as the prebuilt genre pipelines, writing the same
//! `atoms.json` that `runtime/evidence_loop.rs` feeds to chat.
//!
//! ## Realization (why there is no separate `Pipeline` impl here)
//!
//! The atlas pipeline already reads its Phase-1 system prompt through
//! `self.phase1_system()` (see `LiteraryAtlasPipeline::compose_phase1`), and
//! Phases 3–7 are genre-agnostic (they cluster / name / find tensions over
//! whatever atoms Phase 1 produced). So a custom atlas is the SAME pipeline with
//! a different Phase-1 ontology — NOT a parallel re-implementation. Rather than
//! duplicate ~26 trait-method delegations (which would silently diverge the day
//! the atlas pipeline gains a new phase), [`LiteraryAtlasPipeline`] carries an
//! optional [`CustomOntology`]: when present it reports `id() = "custom_atlas"`
//! and swaps the Phase-1 prompt / vocabulary, leaving every downstream phase
//! identical. This module owns the custom-specific data (spec, neutral prompt,
//! assembly); `literary_atlas.rs` owns the field + the ~7 branch points.
//!
//! [`LiteraryAtlasPipeline`]: super::literary_atlas::LiteraryAtlasPipeline

use super::super::types::Vocabulary;
use serde::{Deserialize, Serialize};

/// Pipeline id a recipe-customized atlas pipeline reports from `id()`. Ends in
/// `_atlas` so it passes the build's atlas gate, and is accepted by
/// `enrich init` validation + the desktop bridge allowlist.
pub const PIPELINE_ID: &str = "custom_atlas";

/// Neutral (genre-agnostic) Phase-1 atlas extraction prompt. Unlike the literary
/// base — which frames everything as "reading a literary work" with narrator
/// rules and novel examples — this carries only the universal atom-extraction
/// mechanics (the seven facets + schema + anchoring) and explicitly invites
/// domain-specific `entity_type` labels. The recipe's domain guidance is
/// appended under a "Domain focus" heading by [`CustomOntology::build`].
const NEUTRAL_PHASE1_SYSTEM: &str = include_str!("configurable_atlas_prompts/phase1_system.md");

/// Plain, serializable custom-atlas ontology. Materialized from a recipe's
/// `[enrichment.ontology]` block (`crate::recipe::OntologyConfig`) by
/// `enrich init` and persisted into the enrich `config.json`
/// (`EnrichConfig.ontology`), then mapped to a live pipeline via
/// [`super::literary_atlas::LiteraryAtlasPipeline::with_custom_ontology`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CustomAtlasSpec {
    /// Human-facing domain label (e.g. `"medieval-numismatics"`). Used for the
    /// pipeline's `name()`; cosmetic.
    #[serde(default)]
    pub name: String,
    /// Domain-language extraction guidance — what entities, relations, events,
    /// and claims matter in this corpus's domain. Appended to the neutral
    /// Phase-1 prompt. The load-bearing field.
    pub guidance: String,
    /// Optional per-domain vocabulary terms; omitted terms use generic defaults.
    #[serde(default)]
    pub vocabulary: Option<CustomVocabulary>,
}

/// Optional CLI/label vocabulary overrides for a custom atlas. Maps onto the
/// engine's [`Vocabulary`]; any omitted (or blank) term uses a generic default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CustomVocabulary {
    #[serde(default)]
    pub concern_term: Option<String>,
    #[serde(default)]
    pub position_term: Option<String>,
    #[serde(default)]
    pub tension_term: Option<String>,
    #[serde(default)]
    pub absence_term: Option<String>,
    #[serde(default)]
    pub evidence_term: Option<String>,
}

/// Built custom-ontology overrides held on a `LiteraryAtlasPipeline` running in
/// custom mode. The identity / prompt fields are `&'static str` because the
/// `Pipeline` trait returns `&'static str`; they are leaked ONCE per build
/// process in [`CustomOntology::build`] — the same bounded-leak pattern as
/// `prompts::load_or_baked`. (`pub(super)` so the sibling `literary_atlas`
/// module can read them in its branch points.)
#[derive(Debug)]
pub struct CustomOntology {
    pub(super) name: &'static str,
    pub(super) phase1_system: &'static str,
    /// The raw domain guidance (trimmed), kept separately from
    /// `phase1_system` (which wraps it in the neutral Phase-1 base) so
    /// downstream phases — notably the Phase-6 `tension` classifier —
    /// can compose their own ontology-driven prompts from the same
    /// author-written guidance. Empty when the recipe gave none.
    pub(super) guidance: &'static str,
    pub(super) vocabulary: Vocabulary,
}

impl CustomOntology {
    /// Assemble from a spec: leak the display `name` and the combined Phase-1
    /// system prompt (neutral base + "Domain focus" guidance), and build the
    /// vocabulary (omitted/blank terms → generic defaults).
    pub(super) fn build(spec: &CustomAtlasSpec) -> Self {
        let name: &'static str = Box::leak(
            if spec.name.trim().is_empty() {
                "custom atlas (recipe-defined)".to_string()
            } else {
                format!("custom atlas — {}", spec.name.trim())
            }
            .into_boxed_str(),
        );

        let guidance = spec.guidance.trim();
        let combined = if guidance.is_empty() {
            NEUTRAL_PHASE1_SYSTEM.to_string()
        } else {
            format!("{NEUTRAL_PHASE1_SYSTEM}\n\n## Domain focus\n\n{guidance}")
        };
        let phase1_system: &'static str = Box::leak(combined.into_boxed_str());
        let guidance_leaked: &'static str = Box::leak(guidance.to_string().into_boxed_str());

        Self {
            name,
            phase1_system,
            guidance: guidance_leaked,
            vocabulary: build_vocabulary(spec.vocabulary.as_ref()),
        }
    }
}

/// Build a [`Vocabulary`] from optional per-domain overrides, filling blanks
/// with generic (non-literary) defaults.
fn build_vocabulary(v: Option<&CustomVocabulary>) -> Vocabulary {
    fn term(opt: &Option<String>, default: &str) -> String {
        opt.as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| default.to_string())
    }
    let empty = CustomVocabulary::default();
    let v = v.unwrap_or(&empty);
    Vocabulary {
        canonical_concern_term: term(&v.concern_term, "concern"),
        position_term: term(&v.position_term, "position"),
        tension_term: term(&v.tension_term, "tension"),
        absence_term: term(&v.absence_term, "gap"),
        evidence_term: term(&v.evidence_term, "passage"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_appends_guidance_to_neutral_base_not_literary() {
        let spec = CustomAtlasSpec {
            name: "medieval-numismatics".into(),
            guidance: "Extract coins (mint, ruler, denomination) and hoards.".into(),
            vocabulary: None,
        };
        let ont = CustomOntology::build(&spec);
        // Carries the universal mechanics from the neutral base…
        assert!(ont.phase1_system.contains("seven facets"));
        // …plus the domain focus…
        assert!(ont.phase1_system.contains("## Domain focus"));
        assert!(ont.phase1_system.contains("Extract coins"));
        // …and NOT the literary framing.
        assert!(
            !ont.phase1_system.contains("literary work"),
            "custom atlas must not inherit the literary base prompt"
        );
        assert!(ont.name.contains("medieval-numismatics"));
    }

    #[test]
    fn vocabulary_overrides_apply_and_blanks_default() {
        let spec = CustomAtlasSpec {
            name: String::new(),
            guidance: "x".into(),
            vocabulary: Some(CustomVocabulary {
                concern_term: Some("research question".into()),
                position_term: Some("   ".into()), // blank → default
                ..Default::default()
            }),
        };
        let ont = CustomOntology::build(&spec);
        assert_eq!(ont.vocabulary.canonical_concern_term, "research question");
        assert_eq!(ont.vocabulary.position_term, "position"); // blank fell back
        assert_eq!(ont.vocabulary.evidence_term, "passage"); // omitted fell back
        assert_eq!(ont.name, "custom atlas (recipe-defined)"); // blank name default
    }

    #[test]
    fn empty_guidance_yields_plain_neutral_base() {
        let spec = CustomAtlasSpec {
            name: "d".into(),
            guidance: "   ".into(),
            vocabulary: None,
        };
        let ont = CustomOntology::build(&spec);
        assert!(!ont.phase1_system.contains("## Domain focus"));
        assert!(ont.phase1_system.contains("seven facets"));
    }
}

/// The recipe-driven genre. Every prebuilt genre and this one now reach the
/// atlas machinery through the same trait, so "custom" is not a second mode
/// with its own branch points — it is one more genre (ARCH §10.6).
impl super::genre::AtlasGenre for CustomOntology {
    fn id(&self) -> &'static str {
        PIPELINE_ID
    }

    fn name(&self) -> &'static str {
        self.name
    }

    fn phase1_system(&self) -> &'static str {
        self.phase1_system
    }

    /// The terse RETRY re-extracts under the SAME domain ontology (just a
    /// lighter body), never under the literary terse prompt — a failed chapter
    /// must not come back classified by the wrong frame.
    fn phase1_terse_system(&self) -> &'static str {
        self.phase1_system
    }

    fn vocabulary(&self) -> Option<&super::super::types::Vocabulary> {
        Some(&self.vocabulary)
    }

    /// v1: a recipe ontology skips the literary-framed 1b coverage top-up.
    fn runs_phase1b_coverage(&self) -> bool {
        false
    }

    /// v1: skips the literary seed pass (its seed prompt is literary-specific);
    /// Phase 1 extracts directly under the ontology.
    fn seed_strategy(&self) -> super::super::atlas::SeedStrategy {
        super::super::atlas::SeedStrategy::None
    }

    /// Recipe-ontology corpora (governance rule-sets, policy docs) are
    /// cross-document with uniformly-worded rules. The graph signals miss the
    /// real conflicts (a later decision often resolves with no `attributed_to`,
    /// so entity-overlap cannot reach it) and over-pair claims sharing a broad
    /// scope entity. An embedding top-K net recalls same-topic pairs and lets
    /// the classifier judge conflict-vs-compatible. k/floor measured on the
    /// Maple House governance fixture: planted conflicts sit at cosine
    /// 0.60–0.76, so floor 0.5 keeps them while K bounds the per-claim fan-out.
    fn tension_strategy(&self) -> crate::enrichment::atlas::analysis::TensionStrategy {
        crate::enrichment::atlas::analysis::TensionStrategy::EmbeddingTopK { k: 10, floor: 0.5 }
    }

    /// Judge conflicts in the domain's own terms: fill the ontology-driven
    /// template from the recipe's guidance + tension/position vocabulary.
    fn compose_phase6_classifier(
        &self,
        content: &crate::enrichment::atlas::analysis::CandidateContent,
    ) -> Option<super::super::types::ChatPrompt> {
        let system = super::literary_atlas::custom_phase6_classifier_system(
            self.guidance,
            &self.vocabulary.tension_term,
            &self.vocabulary.position_term,
        );
        Some(
            super::super::types::ChatPrompt::new(
                system,
                super::literary_atlas::render_custom_phase6_classifier_user_body(
                    content,
                    &self.vocabulary.tension_term,
                ),
            )
            .with_response_schema(
                "phase6_classifier_response",
                crate::enrichment::atlas::analysis::phase6_classifier_response_schema(),
            )
            .with_phase_id("phase6_classifier")
            .with_max_output_tokens(256),
        )
    }
}
