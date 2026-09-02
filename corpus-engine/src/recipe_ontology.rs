// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `[enrichment.ontology]` block's TOML shapes.
//!
//! Split out of [`crate::recipe`] so the recipe schema's ontology surface —
//! the versioned block, its version-0 prose body, and that body's vocabulary
//! overrides — sits in one file the ontology work can grow without pushing
//! `recipe.rs` past ARCH §3.1's ceiling. Every type here is re-exported from
//! [`crate::recipe`], so a caller's import is unchanged.
//!
//! The block is versioned data, not code: `version` selects an
//! [`OntologyLanguage`](crate::enrichment::ontology::OntologyLanguage) from a
//! registry, and that language — never this module — decides what the rest of
//! the keys mean (ARCH §6.2, §4).

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Custom atlas ontology declared in `[enrichment.ontology]`. The headline
/// "build the ontology for your domain" surface: `guidance` is domain-language
/// instructions for what to extract (entities, relations, events, claims),
/// `[enrichment.ontology]` — a versioned block. `version` (absent = 0) names
/// the declaration language; every other key belongs to that language and is
/// parsed by its `OntologyLanguage` impl into `OntologyPolicies`, which is all
/// the pipeline ever reads. Version 0 is [`OntologyConfig`] (prose); version 1
/// is `OntologyV1` (declared types). Three load-time rules keep this honest:
/// a later version's key in an earlier block is refused naming the version to
/// add; an unknown version is refused naming the highest supported; a
/// version-1 block with no declarations yields version-0 policies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OntologyBlock {
    /// Declaration-language version. Absent means 0 — today's prose block.
    #[serde(default)]
    pub version: u32,
    /// Every other key of the block, interpreted by the language `version`
    /// names. Kept as a table (not a fixed struct) so version N+1 keys are
    /// visible to the load-time rules instead of silently dropped.
    #[serde(flatten)]
    pub body: toml::Table,
}

impl OntologyBlock {
    /// The language this block's `version` selects, or an error naming the
    /// highest version this engine reads (the `check_schema_version` wording).
    pub fn language(&self) -> Result<&'static dyn crate::enrichment::ontology::OntologyLanguage> {
        let registry = crate::enrichment::ontology::OntologyLanguageRegistry::builtin();
        registry.get(self.version).ok_or_else(|| {
            Error::Recipe(format!(
                "[enrichment.ontology] declares version = {} but this engine supports \
                 ontology version <= {}. The recipe was authored against a newer engine; \
                 upgrade `corpus-engine` to load it.",
                self.version,
                registry.max_version()
            ))
        })
    }

    /// Parse the block into policies through its language. `Recipe::from_toml`
    /// has already run this once (eager, so structural errors surface at load);
    /// callers after load may treat `Err` as unreachable but must not hide it.
    pub fn policies(&self) -> Result<crate::enrichment::ontology::OntologyPolicies> {
        self.language()?.parse(&self.body)
    }
}

/// injected into a NEUTRAL atlas Phase-1 prompt by
/// [`crate::enrichment::pipeline::pipelines::configurable_atlas::ConfigurableAtlasPipeline`].
/// The universal atom schema + open `EntityType::Other(..)` labels let a domain
/// expert author the extraction shape in TOML without touching Rust, and the
/// result feeds chat via the same `atoms.json` the prebuilt genre pipelines
/// produce. Precedence: a non-empty `guidance` here beats `pipeline`/`domain`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OntologyConfig {
    /// Domain-language extraction guidance — what entities, relations, events,
    /// and claims matter in THIS corpus's domain, in the domain's own words.
    /// Appended under a "Domain focus" heading to the neutral atlas Phase-1
    /// system prompt. The load-bearing field; an empty `guidance` disables the
    /// custom path (falls back to a prebuilt atlas pipeline).
    #[serde(default)]
    pub guidance: String,

    /// Optional CLI/label vocabulary overrides (what a "concern", "position",
    /// "tension", "absence", and unit of "evidence" are called for this domain).
    /// Omitted fields fall back to generic defaults in the pipeline.
    #[serde(default)]
    pub vocabulary: Option<OntologyVocabulary>,
}

/// Per-domain term overrides for the configurable atlas pipeline's vocabulary.
/// Maps onto the engine's `Vocabulary`; any omitted term uses a generic default.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OntologyVocabulary {
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

#[cfg(test)]
mod tests {
    use crate::recipe::Recipe;

    #[test]
    fn custom_atlas_ontology_parses_and_is_detected() {
        let toml_str = r#"
[corpus]
id = "numis"
name = "numis"

[acquire]
type = "local_file"
path = "/tmp/x.md"

[extract]
type = "markdown"

[chunk]
type = "passthrough"

[enrichment]
enabled = true
type = "atlas"
domain = "medieval-numismatics"

[enrichment.ontology]
guidance = """
Extract coins (mint, ruler, denomination, metal), mints, rulers, hoards.
Relations: minted_by, found_in_hoard, succeeds_ruler.
"""

[enrichment.ontology.vocabulary]
concern_term = "numismatic question"
evidence_term = "passage"
"#;
        let r = Recipe::from_toml(toml_str).expect("recipe must parse");
        let enr = r.enrichment.clone().expect("enrichment parsed");
        assert_eq!(enr.enrichment_type, "atlas");
        let ont = enr.ontology.as_ref().expect("ontology block parsed");
        assert_eq!(ont.version, 0, "no version line means version 0");
        let policies = ont.policies().expect("version-0 block parses");
        assert!(
            policies.prose.guidance.contains("minted_by"),
            "guidance retained"
        );
        let vocab = &policies.prose.terms;
        assert_eq!(vocab.concern_term.as_deref(), Some("numismatic question"));
        assert_eq!(vocab.position_term, None, "omitted term stays None");

        // The accessor signals "use the custom atlas pipeline".
        assert!(r.custom_ontology().is_some());
        assert!(r.produces_enriched_atoms());
    }

    #[test]
    fn custom_ontology_precedence_and_empty_guidance() {
        // Empty/whitespace guidance does NOT trigger the custom path even if the
        // block is present — falls back to pipeline/domain.
        let empty = r#"
[corpus]
id = "c"
name = "c"
[acquire]
type = "local_file"
path = "/tmp/x.md"
[extract]
type = "markdown"
[chunk]
type = "passthrough"
[enrichment]
enabled = true
type = "atlas"
pipeline = "philosophy_atlas"
[enrichment.ontology]
guidance = "   "
"#;
        let r = Recipe::from_toml(empty).expect("parse");
        assert!(
            r.custom_ontology().is_none(),
            "blank guidance must not trigger custom atlas"
        );
        assert_eq!(
            r.enrichment.unwrap().pipeline.as_deref(),
            Some("philosophy_atlas"),
            "falls through to the explicit pipeline pin"
        );

        // No ontology block at all → None.
        let none = r#"
[corpus]
id = "c"
name = "c"
[acquire]
type = "local_file"
path = "/tmp/x.md"
[extract]
type = "markdown"
[chunk]
type = "passthrough"
[enrichment]
enabled = true
type = "atlas"
"#;
        assert!(Recipe::from_toml(none)
            .expect("parse")
            .custom_ontology()
            .is_none());
    }
}
