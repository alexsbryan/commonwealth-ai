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
use crate::enrichment::ontology::OntologyPolicies;
use crate::recipe::OntologyVocabulary;
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
/// `[enrichment.ontology]` block by `Recipe::custom_atlas_spec` in
/// `enrich init` and persisted into the enrich `config.json`
/// (`EnrichConfig.ontology`), then mapped to a live pipeline via
/// [`super::literary_atlas::LiteraryAtlasPipeline::with_custom_ontology`].
///
/// Two generations coexist in `config.json`. A spec written before ontology
/// v1 carries `guidance` + `vocabulary` only; one written since also carries
/// `ontology_version` and `policies`. [`Self::policies`] is the one accessor:
/// it returns the recorded policies or synthesizes version-0 policies from
/// the prose, so every reader sees one shape (§10.6).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CustomAtlasSpec {
    /// Human-facing domain label (e.g. `"medieval-numismatics"`). Used for the
    /// pipeline's `name()`; cosmetic.
    #[serde(default)]
    pub name: String,
    /// Domain-language extraction guidance, mirrored from
    /// `policies.prose.guidance` so a reader that predates `policies` still
    /// composes the same Phase-1 prompt.
    #[serde(default)]
    pub guidance: String,
    /// Legacy mirror of `policies.prose.terms`; omitted terms use generic
    /// defaults. Same type as the recipe's `OntologyVocabulary`.
    #[serde(default)]
    pub vocabulary: Option<CustomVocabulary>,
    /// The `[enrichment.ontology] version` the policies were parsed under.
    /// Recorded so a viewer can say which language produced an atlas.
    #[serde(default)]
    pub ontology_version: u32,
    /// The parsed policies — what the pipeline reads. `None` on a spec
    /// written before ontology v1; see [`Self::policies`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policies: Option<OntologyPolicies>,
}

/// The recipe's vocabulary type IS the spec's vocabulary type. It was a
/// field-for-field duplicate until 2026-09-01; the alias keeps every caller
/// compiling while there is one definition.
pub type CustomVocabulary = OntologyVocabulary;

impl CustomAtlasSpec {
    /// The policies this spec selects — recorded when present, otherwise
    /// version-0 policies synthesized from the prose mirror (a legacy
    /// `config.json`). One decider for "what does this corpus's ontology say".
    pub fn policies(&self) -> OntologyPolicies {
        match &self.policies {
            Some(p) => p.clone(),
            None => OntologyPolicies::from_prose(
                &self.guidance,
                self.vocabulary.clone().unwrap_or_default(),
            ),
        }
    }
}

/// The tension selector every recipe-ontology corpus uses. Named once so the
/// genre's `tension_strategy()` and `recipe validate`'s derived-facet note
/// print the same numbers. Recipe-ontology corpora (governance rule-sets,
/// policy docs) are cross-document with uniformly-worded claims: the graph
/// signals miss the real conflicts (a later decision often resolves with no
/// `attributed_to`, so entity-overlap cannot reach it) and over-pair claims
/// sharing a broad scope entity. An embedding top-K net recalls same-topic
/// pairs and lets the classifier judge conflict-vs-compatible. k/floor were
/// measured on the Maple House governance fixture: planted conflicts sit at
/// cosine 0.60–0.76, so floor 0.5 keeps them while K bounds the fan-out.
pub const CUSTOM_TENSION_STRATEGY: crate::enrichment::atlas::analysis::TensionStrategy =
    crate::enrichment::atlas::analysis::TensionStrategy::EmbeddingTopK { k: 10, floor: 0.5 };

/// Built custom-ontology overrides held on a `LiteraryAtlasPipeline` running in
/// custom mode. The identity / prompt fields are `&'static str` because the
/// `Pipeline` trait returns `&'static str`; they are leaked ONCE per build
/// process in [`CustomOntology::from_policies`] — the same bounded-leak pattern
/// as `prompts::load_or_baked`. (`pub(super)` so the sibling `literary_atlas`
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
    /// Assemble from a persisted spec — the `config.json` path. Delegates to
    /// [`Self::from_policies`] through [`CustomAtlasSpec::policies`].
    pub(super) fn build(spec: &CustomAtlasSpec) -> Self {
        Self::from_policies(&spec.name, &spec.policies())
    }

    /// Assemble from policies: leak the display `name` and the combined
    /// Phase-1 system prompt (neutral base + "Domain focus" guidance), and
    /// select the vocabulary. **Invariant I1:** the prompt bytes are exactly
    /// `format!("{NEUTRAL}\n\n## Domain focus\n\n{guidance}")` for any
    /// non-blank guidance and exactly the neutral base otherwise — a
    /// version-1 block with no declarations composes the same bytes as
    /// version 0 (`i1_from_policies_matches_legacy_build_bytes`). P2's
    /// generated schema hangs off `policies.shape` and returns `None` when it
    /// is empty; nothing here reads it.
    pub fn from_policies(name: &str, policies: &OntologyPolicies) -> Self {
        let name: &'static str = Box::leak(
            if name.trim().is_empty() {
                "custom atlas (recipe-defined)".to_string()
            } else {
                format!("custom atlas — {}", name.trim())
            }
            .into_boxed_str(),
        );

        let guidance = policies.prose.guidance.trim();
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
            vocabulary: policies.vocabulary(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipe::Recipe;

    /// The maple-house recipe as vendored by `build.rs` — the governance
    /// fixture whose Phase-1 bytes I1 pins.
    const MAPLE_HOUSE: &str =
        include_str!(concat!(env!("OUT_DIR"), "/recipes/maple-house/recipe.toml"));

    fn spec(name: &str, guidance: &str, vocabulary: Option<CustomVocabulary>) -> CustomAtlasSpec {
        CustomAtlasSpec {
            name: name.into(),
            guidance: guidance.into(),
            vocabulary,
            ..Default::default()
        }
    }

    #[test]
    fn build_appends_guidance_to_neutral_base_not_literary() {
        let ont = CustomOntology::build(&spec(
            "medieval-numismatics",
            "Extract coins (mint, ruler, denomination) and hoards.",
            None,
        ));
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
        let ont = CustomOntology::build(&spec(
            "",
            "x",
            Some(CustomVocabulary {
                concern_term: Some("research question".into()),
                position_term: Some("   ".into()), // blank → default
                ..Default::default()
            }),
        ));
        assert_eq!(ont.vocabulary.canonical_concern_term, "research question");
        assert_eq!(ont.vocabulary.position_term, "position"); // blank fell back
        assert_eq!(ont.vocabulary.evidence_term, "passage"); // omitted fell back
        assert_eq!(ont.name, "custom atlas (recipe-defined)"); // blank name default
    }

    #[test]
    fn empty_guidance_yields_plain_neutral_base() {
        let ont = CustomOntology::build(&spec("d", "   ", None));
        assert!(!ont.phase1_system.contains("## Domain focus"));
        assert!(ont.phase1_system.contains("seven facets"));
    }

    /// I1. Three specs for the same maple-house ontology — a legacy
    /// `config.json` (prose only, no `policies`), the version-0 recipe, and the
    /// same recipe migrated to `version = 1` with nothing else changed — must
    /// compose byte-identical Phase-1 prompts and select the same vocabulary,
    /// and those bytes must be exactly the documented `## Domain focus`
    /// format. Until P2's snapshot module lands this is the pin.
    #[test]
    fn i1_from_policies_matches_legacy_build_bytes() {
        let v0_recipe = Recipe::from_toml(MAPLE_HOUSE).expect("maple-house parses");
        let v0 = v0_recipe
            .custom_atlas_spec()
            .expect("maple-house is a custom ontology");
        assert_eq!(v0.ontology_version, 0);
        assert!(v0.policies.is_some());

        let legacy = CustomAtlasSpec {
            policies: None,
            ontology_version: 0,
            ..v0.clone()
        };

        let migrated = Recipe::migrate_ontology_version(MAPLE_HOUSE, 1)
            .expect("migration yields a loadable recipe")
            .expect("maple-house is version 0, so there is a change");
        let v1 = Recipe::from_toml(&migrated)
            .expect("migrated maple-house parses")
            .custom_atlas_spec()
            .expect("still a custom ontology");
        assert_eq!(v1.ontology_version, 1);

        let a = CustomOntology::build(&legacy);
        let b = CustomOntology::build(&v0);
        let c = CustomOntology::build(&v1);
        assert_eq!(
            a.phase1_system, b.phase1_system,
            "legacy config.json vs v0 recipe"
        );
        assert_eq!(
            b.phase1_system, c.phase1_system,
            "v0 recipe vs version = 1 migration"
        );
        assert_eq!(
            a.phase1_system,
            format!(
                "{NEUTRAL_PHASE1_SYSTEM}\n\n## Domain focus\n\n{}",
                v0.guidance.trim()
            ),
            "the Domain focus bytes are the contract"
        );
        assert_eq!(a.guidance, c.guidance);
        for ont in [&a, &b, &c] {
            assert_eq!(ont.vocabulary.position_term, "rule");
            assert_eq!(ont.vocabulary.tension_term, "conflict");
            assert_eq!(ont.vocabulary.canonical_concern_term, "house question");
            assert_eq!(ont.vocabulary.evidence_term, "passage");
            assert_eq!(ont.vocabulary.absence_term, "gap");
        }
    }

    /// Today's defaults, pinned: the five generic terms, the neutral prompt
    /// when nothing is declared, and the measured tension selector.
    #[test]
    fn defaults_reproduce_today() {
        use super::super::genre::AtlasGenre;
        let ont = CustomOntology::from_policies("d", &OntologyPolicies::default());
        assert_eq!(ont.phase1_system, NEUTRAL_PHASE1_SYSTEM);
        assert_eq!(ont.vocabulary.canonical_concern_term, "concern");
        assert_eq!(ont.vocabulary.position_term, "position");
        assert_eq!(ont.vocabulary.tension_term, "tension");
        assert_eq!(ont.vocabulary.absence_term, "gap");
        assert_eq!(ont.vocabulary.evidence_term, "passage");
        assert!(matches!(
            ont.tension_strategy(),
            crate::enrichment::atlas::analysis::TensionStrategy::EmbeddingTopK { k: 10, floor }
                if (floor - 0.5).abs() < f32::EPSILON
        ));
        let p = OntologyPolicies::default();
        assert!(p.derivation.configurations);
        assert!(!p.derivation.arguments);
        assert!(p.is_empty());
    }

    /// A legacy `config.json` has no `policies`; the accessor synthesizes
    /// version-0 policies from the prose mirror, and a spec that carries
    /// policies round-trips them through JSON unchanged.
    #[test]
    fn custom_atlas_spec_legacy_json_synthesizes_policies() {
        let legacy_json = r#"{"name":"maple","guidance":"Rules of a house.","vocabulary":{"position_term":"rule"}}"#;
        let legacy: CustomAtlasSpec = serde_json::from_str(legacy_json).expect("legacy loads");
        assert!(legacy.policies.is_none());
        assert_eq!(legacy.ontology_version, 0);
        let p = legacy.policies();
        assert_eq!(p.prose.guidance, "Rules of a house.");
        assert_eq!(p.prose.terms.position_term.as_deref(), Some("rule"));
        assert!(!p.has_declarations());

        let with = CustomAtlasSpec {
            policies: Some(p.clone()),
            ontology_version: 1,
            ..legacy.clone()
        };
        let json = serde_json::to_string(&with).expect("serializes");
        let back: CustomAtlasSpec = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(back, with);
        assert_eq!(back.policies(), p);
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

    /// See [`CUSTOM_TENSION_STRATEGY`] for why and for the measured k/floor.
    fn tension_strategy(&self) -> crate::enrichment::atlas::analysis::TensionStrategy {
        CUSTOM_TENSION_STRATEGY
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
