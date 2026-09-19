// SPDX-License-Identifier: AGPL-3.0-or-later
//! The atlas taxonomy — provenance depth and the closed-with-`Other`
//! kind vocabularies (`EntityType`, `StateType`, `RelationType`,
//! `EventType`, `DiscourseAct`, `EpistemicStatus`, `ClaimScope`,
//! `QuestionType`).
//!
//! Lived at the top of `corpus-engine/src/enrichment/pipeline/atlas.rs`
//! (its "vocabulary block") until 2026-09-03; the Phase 1 sketch types
//! that reference these stayed there. Every atom in [`crate::atoms`]
//! names one or more of these, which is why they share the leaf.

use serde::{Deserialize, Serialize};

// ── Open/Closed surface: enrichment depth ─────────────────────
//
// Every atom carries an `enrichment_depth` tag that records which
// ingestion strategy produced it. The brief assembler reads this tag
// and calibrates language accordingly: "Robinson argued…" for
// `Extracted`, "according to her Wikipedia entry…" for `Structural`,
// "Robinson's article characterises…" for `StructuralClassified`.
//
// Today only `Extracted` is produced (literary_atlas pipeline). The
// other variants are wired in now so a future structure-first
// ingestion strategy can land without a schema revision.

/// Provenance depth of an atlas atom. The three values correspond to
/// the three ingestion modes described in the v2.1 spec §5:
///
/// - `Structural` — produced by deterministic parsing of structural
///   signals (infoboxes, section headers, wikilinks). Shallow but
///   broad; no LLM inference, no evidence grounding beyond section
///   boundaries.
/// - `Extracted` — produced by LLM extraction over the section body.
///   Deep and evidence-grounded; the default for authored works.
/// - `StructuralClassified` — `Structural` atoms that a later
///   classification pass has typed and characterised without
///   discovering new atoms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentDepth {
    Structural,
    Extracted,
    StructuralClassified,
}

impl EnrichmentDepth {
    /// Default for per-section extraction records — the
    /// literary/philosophy/journal pipelines all produce `Extracted`
    /// atoms. The structure-first ingestion strategy overrides this
    /// at construction time; it doesn't go through the serde default.
    pub const fn extracted_default() -> Self {
        EnrichmentDepth::Extracted
    }
}

impl Default for EnrichmentDepth {
    fn default() -> Self {
        EnrichmentDepth::extracted_default()
    }
}

/// Serde-level helper used by `#[serde(default = …)]` on
/// `SectionExtraction::enrichment_depth`. Using a function instead of
/// `Default::default` keeps the intent self-documenting at the call
/// site ("v1 caches deserialise as Extracted").
pub fn extracted_default() -> EnrichmentDepth {
    EnrichmentDepth::extracted_default()
}

// ── Enum vocabulary ───────────────────────────────────────────
//
// Each enum has a fixed set of "named" variants plus an `Other(String)`
// fallback. Serialization uses the snake_case name for the known
// variants and the inner string verbatim for `Other`, so a JSON
// response like `{"entity_type": "psychoanalyst"}` round-trips to
// `EntityType::Other("psychoanalyst".into())` without loss.
//
// The `from_str_repr` / `as_str_repr` helpers are the single source of
// truth for the string<->enum mapping; both Serialize and Deserialize
// route through them.

macro_rules! string_enum_with_other {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$variant_meta:meta])* $variant:ident = $lit:literal ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        $vis enum $name {
            $( $(#[$variant_meta])* $variant, )*
            /// Fallback for values the schema has not yet named. The
            /// inner string preserves whatever the model emitted so a
            /// reviewer can see which tags are recurring and decide
            /// whether to promote one to a named variant.
            Other(String),
        }

        impl $name {
            /// Wire names of every named variant, in declaration order —
            /// never `Other`. The one list to read for "the kinds this
            /// enum already emits" (the ontology validator resolves
            /// references against `EntityType::NAMED`, ARCH §10.6).
            pub const NAMED: &[&str] = &[$( $lit, )*];

            pub fn as_str_repr(&self) -> &str {
                match self {
                    $( Self::$variant => $lit, )*
                    Self::Other(s) => s.as_str(),
                }
            }

            pub fn from_str_repr(s: &str) -> Self {
                let t = s.trim();
                // Case-insensitive match on the named variants; any
                // whitespace / hyphen / underscore variation is
                // normalised to the canonical snake_case literal before
                // comparison. This keeps `"Psychological"`,
                // `"psychological"`, and `"Psycho-logical"` all mapping
                // to the same variant.
                let norm = normalise_enum_tag(t);
                $(
                    if norm == $lit {
                        return Self::$variant;
                    }
                )*
                Self::Other(t.to_string())
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str_repr())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Ok(Self::from_str_repr(&s))
            }
        }
    };
}

/// Normalise an enum tag coming off the wire into the canonical
/// snake_case literal. Lowercases, then maps hyphens / whitespace to
/// underscores, then collapses runs.
///
/// `pub(crate)` since 2026-09-01: the ontology parser matches a model's tag
/// against DECLARED type names, and it has to do that by the same rule the
/// named variants get — otherwise `"Coin"` resolves for a generic type and
/// drops for a declared one, which is the reverse of what an author expects.
pub fn normalise_enum_tag(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut last_was_underscore = false;
    for c in lower.chars() {
        let mapped = match c {
            ' ' | '\t' | '-' | '/' => '_',
            c => c,
        };
        if mapped == '_' {
            if !last_was_underscore && !out.is_empty() {
                out.push('_');
                last_was_underscore = true;
            }
        } else {
            out.push(mapped);
            last_was_underscore = false;
        }
    }
    // Trim trailing underscore.
    if out.ends_with('_') {
        out.pop();
    }
    out
}

string_enum_with_other! {
    /// Kind of named thing (§2.1).
    ///
    /// `Initiative` is added for the personal/conversational domains:
    /// a recurring abstract subject the user organises work around
    /// (a project, strategic priority, product launch). The pragmatic
    /// distinction from `Concept` is "active effort toward a future
    /// state" — `Initiative` implies someone is working on it.
    pub enum EntityType {
        Person = "person",
        Concept = "concept",
        Institution = "institution",
        Work = "work",
        Place = "place",
        Initiative = "initiative",
    }
}

string_enum_with_other! {
    /// Kind of condition an entity or relation occupies (§2.2).
    ///
    /// `Relational` is intentionally absent — relation states are
    /// carried by the `Relation` atom's own trajectory (§2.6), not by
    /// a state-type variant on an entity.
    pub enum StateType {
        Psychological = "psychological",
        Epistemic = "epistemic",
        Social = "social",
        Professional = "professional",
    }
}

string_enum_with_other! {
    /// Kind of persistent interaction between entities (§2.6).
    pub enum RelationType {
        Interpersonal = "interpersonal",
        Intellectual = "intellectual",
        Institutional = "institutional",
        Adversarial = "adversarial",
        Collaborative = "collaborative",
        Compositional = "compositional",
    }
}

string_enum_with_other! {
    /// Kind of happening that marks a transition or grounds a claim (§2.3).
    pub enum EventType {
        Action = "action",
        Publication = "publication",
        Encounter = "encounter",
        Realization = "realization",
        ExternalForce = "external_force",
        Decision = "decision",
    }
}

string_enum_with_other! {
    /// What the text *does* with a claim — its illocutionary force (§2.5).
    pub enum DiscourseAct {
        Argue = "argue",
        Assert = "assert",
        Enact = "enact",
        Hypothesize = "hypothesize",
        Warn = "warn",
        Commit = "commit",
        Object = "object",
        Interpret = "interpret",
        Imply = "imply",
    }
}

string_enum_with_other! {
    /// How certain a claim is within the corpus context (§2.5).
    pub enum EpistemicStatus {
        Confident = "confident",
        Tentative = "tentative",
        Contested = "contested",
        Retracted = "retracted",
        Attributed = "attributed",
    }
}

string_enum_with_other! {
    /// How wide a claim's applicability is (§2.5).
    pub enum ClaimScope {
        Universal = "universal",
        Contextual = "contextual",
        Personal = "personal",
        Fictional = "fictional",
    }
}

string_enum_with_other! {
    /// Kind of inquiry a question represents (§2.4).
    pub enum QuestionType {
        Thematic = "thematic",
        Factual = "factual",
        Interpretive = "interpretive",
        Open = "open",
        Rhetorical = "rhetorical",
    }
}
