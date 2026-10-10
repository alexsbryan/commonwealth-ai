// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ontology declaration languages — one parser per `[enrichment.ontology]
//! version`, in a registry keyed by the integer.
//!
//! The pipeline never reads the version. It reads
//! [`OntologyPolicies`](super::OntologyPolicies); a version is nothing more
//! than a parser from the block's TOML table to those structs
//! (`ONTOLOGY_PRIMITIVES.md` §0.1). Version 0 is today's prose
//! [`OntologyConfig`] and fills `prose` only. Version 1 adds declared types
//! with attributes and the four corpus-level blocks. A version 2 is one more
//! [`OntologyLanguage`] impl registered in [`OntologyLanguageRegistry::builtin`]
//! plus a `docs/vN.md` section — it touches a version-1 code path only if a
//! policy struct gains a field, and that field carries a default.
//!
//! This file also holds the version-1 TOML types, because
//! `tests/main/recipe_schema.rs` renders every `Deserialize` type in its
//! `SOURCES` list into `ingest/crates/sovereign-recipes/SCHEMA.md` and this file is on that
//! list. The policy structs live in the parent module precisely so they are
//! NOT rendered as recipe surface.
//!
//! Registry shape mirrors `enrichment::domain_registry::DomainRegistry`
//! (ARCH §4: an open set is a registry, an unknown id refuses loudly).

use std::sync::LazyLock;

use serde::Serialize;

use super::OntologyConfig;
use crate::error::{Error, Result};
use crate::recipe_parsing::translate_parse_error;
use understanding_vocab::ontology::decl::{
    Force, OntologyV1, TypeKind, MAX_ENTITIES_PER_SECTION, MIN_ENTITIES_PER_SECTION,
};
use understanding_vocab::ontology::OntologyPolicies;

// ── The trait and its registry ──────────────────────────────────────────────

/// One declaration language: parses the `[enrichment.ontology]` table (minus
/// `version`) into [`OntologyPolicies`]. Implemented once per shipped version.
pub trait OntologyLanguage: Send + Sync {
    /// The integer a recipe writes as `version = N`.
    fn version(&self) -> u32;
    /// Every top-level key this version accepts under `[enrichment.ontology]`.
    /// The load-time rule "a later version's key in an earlier block refuses"
    /// and the `validate` warning for keys no version defines are both
    /// computed from these lists — nothing else re-lists them (§10.6).
    fn keys(&self) -> &'static [&'static str];
    /// Parse the block body. Structural errors (a claim type without `force`,
    /// an unknown enum value) surface here, so `Recipe::from_toml` refuses
    /// them at load rather than at extraction.
    fn parse(&self, body: &toml::Table) -> Result<OntologyPolicies>;
    /// The `SCHEMA.md` section for this version, rendered by the
    /// `recipe_schema_is_fresh` gate after the generated type tables.
    fn schema_doc(&self) -> &'static str;
}

/// Registry of every shipped declaration language, keyed by version.
pub struct OntologyLanguageRegistry {
    /// Sorted ascending by version; `registry_versions_contiguous` pins that
    /// the versions run 0..=max with no gap.
    languages: Vec<Box<dyn OntologyLanguage>>,
}

impl OntologyLanguageRegistry {
    /// Every built-in language. Process-wide singleton; the registry is
    /// immutable after construction.
    pub fn builtin() -> &'static Self {
        static REGISTRY: LazyLock<OntologyLanguageRegistry> = LazyLock::new(|| {
            let mut languages: Vec<Box<dyn OntologyLanguage>> = vec![Box::new(V0), Box::new(V1)];
            languages.sort_by_key(|l| l.version());
            OntologyLanguageRegistry { languages }
        });
        &REGISTRY
    }

    /// The language for `version`, or `None` when this binary does not know
    /// it. Callers turn `None` into an error naming [`Self::max_version`].
    pub fn get(&self, version: u32) -> Option<&dyn OntologyLanguage> {
        self.languages
            .iter()
            .find(|l| l.version() == version)
            .map(|l| l.as_ref())
    }

    /// The highest version this binary reads. The ONE decider for the
    /// "supports ontology version <= N" refusal.
    pub fn max_version(&self) -> u32 {
        self.languages
            .iter()
            .map(|l| l.version())
            .max()
            .unwrap_or(0)
    }

    /// Every registered language, ascending by version.
    pub fn versions(&self) -> impl Iterator<Item = &dyn OntologyLanguage> {
        self.languages.iter().map(|l| l.as_ref())
    }

    /// The lowest version whose key list contains `key`, or `None` when no
    /// version defines it. Drives the load-time rule: a key first defined in
    /// version N inside a block declaring version M < N is a refusal naming
    /// `version = N`, never a silent drop.
    pub fn first_version_defining(&self, key: &str) -> Option<u32> {
        self.languages
            .iter()
            .find(|l| l.keys().contains(&key))
            .map(|l| l.version())
    }

    /// Keys in `body` that NO version defines — typos and stray keys. Sorted.
    /// A `validate` warning, not a load error (community recipes must keep
    /// loading; `deny_unknown_fields` was rejected for that reason).
    pub fn unknown_keys(&self, body: &toml::Table) -> Vec<String> {
        let mut out: Vec<String> = body
            .keys()
            .filter(|k| self.first_version_defining(k).is_none())
            .filter(|k| !V1_RETIRED_KEYS.iter().any(|(retired, _)| retired == k))
            .cloned()
            .collect();
        out.sort();
        out
    }
}

// ── Version 0 — today's prose block ─────────────────────────────────────────

/// Version 0: `guidance` prose plus optional `vocabulary` term overrides —
/// [`OntologyConfig`] exactly as it has always parsed. Fills `prose`; every
/// other policy stays at its default, which is today's behaviour.
struct V0;

const V0_KEYS: &[&str] = &["guidance", "vocabulary"];

impl OntologyLanguage for V0 {
    fn version(&self) -> u32 {
        0
    }

    fn keys(&self) -> &'static [&'static str] {
        V0_KEYS
    }

    fn parse(&self, body: &toml::Table) -> Result<OntologyPolicies> {
        let cfg: OntologyConfig = body.clone().try_into().map_err(translate_parse_error)?;
        Ok(OntologyPolicies::from_prose(
            &cfg.guidance,
            cfg.vocabulary.unwrap_or_default(),
        ))
    }

    fn schema_doc(&self) -> &'static str {
        include_str!("docs/v0.md")
    }
}

// ── Version 1 — declared types on five axes ─────────────────────────────────

/// Version 1: declared types with typed attributes plus the corpus-level
/// blocks. Every key is optional; a version-1 block with none of them yields
/// the same policies as version 0 (`v1_empty_equals_v0_equals_default`).
struct V1;

const V1_KEYS: &[&str] = &[
    "guidance",
    "vocabulary",
    "must_not",
    "types",
    "max_entities_per_section",
    "voices",
    "change",
    "tension",
    "derive",
    "patterns",
    "navigation",
    "paths",
    "sets",
    "folds",
];

/// Keys version 1 once read and no longer does, each with what replaced it.
/// A recipe that still carries one loads, with the key ignored and named on
/// every load (`parse`) and by `recipe validate` (`validate_block`); never an
/// unknown-key typo warning, never a refusal. Order 3 of campaign
/// ontology-layer removes them from this list once no recipe carries them.
pub const V1_RETIRED_KEYS: &[(&str, &str)] = &[
    (
        "document_reading",
        "declared reading is chosen by the declaration: every claim kind with a force and a subject RESOLVE or a metadata source identifies is read document by document",
    ),
    (
        "document_reader",
        "one reader remains, the passes reader (ONTOLOGY_METHOD §Reading)",
    ),
];

/// Keys a version-1 type once read and no longer does, with what replaced
/// each: loaded, ignored and named like `V1_RETIRED_KEYS`.
pub const V1_RETIRED_TYPE_KEYS: &[(&str, &str)] = &[(
    "identity_evidential",
    "what each identity source is worth is estimated on the corpus being read, with no labels (campaign ontology-layer E2, ONTOLOGY_METHOD §Identity); every declared document field, necessary value, the proposed answer and the model's choice is weighed",
)];

/// The retired type keys `body`'s `types` carry: (type name, key, instead).
pub fn retired_type_keys(body: &toml::Table) -> Vec<(String, &'static str, &'static str)> {
    let Some(toml::Value::Array(types)) = body.get("types") else {
        return Vec::new();
    };
    types
        .iter()
        .filter_map(toml::Value::as_table)
        .flat_map(|t| {
            let name = t
                .get("name")
                .and_then(toml::Value::as_str)
                .unwrap_or("<unnamed>");
            V1_RETIRED_TYPE_KEYS
                .iter()
                .filter(|(key, _)| t.contains_key(*key))
                .map(move |&(key, instead)| (name.to_string(), key, instead))
        })
        .collect()
}

/// The retired keys `body` carries, with what replaced each.
pub fn retired_keys(body: &toml::Table) -> Vec<(&'static str, &'static str)> {
    V1_RETIRED_KEYS
        .iter()
        .copied()
        .filter(|(key, _)| body.contains_key(*key))
        .collect()
}

impl OntologyLanguage for V1 {
    fn version(&self) -> u32 {
        1
    }

    fn keys(&self) -> &'static [&'static str] {
        V1_KEYS
    }

    fn parse(&self, body: &toml::Table) -> Result<OntologyPolicies> {
        let mut body = body.clone();
        for (key, instead) in retired_keys(&body) {
            tracing::warn!(key, instead, "ontology: a retired key is ignored");
            body.remove(key);
        }
        for (r#type, key, instead) in retired_type_keys(&body) {
            tracing::warn!(
                r#type,
                key,
                instead,
                "ontology: a retired type key is ignored"
            );
        }
        if let Some(toml::Value::Array(types)) = body.get_mut("types") {
            for t in types.iter_mut().filter_map(toml::Value::as_table_mut) {
                for (key, _) in V1_RETIRED_TYPE_KEYS {
                    t.remove(*key);
                }
            }
        }
        let v1: OntologyV1 = body.try_into().map_err(translate_parse_error)?;
        // Reading is chosen by the declaration (`OntologyPolicies::reads_documents`):
        // every claim kind read, or none. A mix would read some kinds and leave the
        // rest to another reader in the same run, so it refuses, naming the ones the
        // reader cannot read.
        let claims: Vec<_> = v1
            .types
            .iter()
            .filter(|claim| claim.kind == TypeKind::Claim)
            .collect();
        let unsupported: Vec<&str> = claims
            .iter()
            .filter(|claim| !claim.is_document_reading_eligible(&v1.types))
            .map(|claim| claim.name.as_str())
            .collect();
        if !unsupported.is_empty() && unsupported.len() < claims.len() {
            return Err(Error::Recipe(format!(
                "ontology declared reading cannot silently omit unsupported declared claim type(s): {}. Each needs a declared force and a `subject` naming either a source-free entity or event type with an `identity_criterion` or a metadata-sourced entity type with declared identity fields",
                unsupported.join(", ")
            )));
        }
        // The one structural rule this version enforces at parse time. Force
        // is what separates a rule from a finding, and supersession applies
        // to the wrong things without it — so a claim type without it is
        // refused here, not defaulted (§18.3).
        if let Some(t) = v1
            .types
            .iter()
            .find(|t| t.kind == TypeKind::Claim && t.force.is_none())
        {
            return Err(Error::Recipe(format!(
                "ontology type `{}` has kind = \"claim\" but no `force`. Every claim \
                 type names what a source does with it: force = {}.",
                t.name,
                wire_names(&Force::ALL)
            )));
        }
        // The entity cap is a number the grammar enforces on the model, so an
        // out-of-range one is refused here rather than clamped at extraction
        // (§18.3): a clamp would run a corpus under a cap its author never
        // wrote and never hears about.
        if let Some(n) = v1.max_entities_per_section {
            if !(MIN_ENTITIES_PER_SECTION..=MAX_ENTITIES_PER_SECTION).contains(&n) {
                return Err(Error::Recipe(format!(
                    "[enrichment.ontology] max_entities_per_section = {n} is outside \
                     {MIN_ENTITIES_PER_SECTION}..={MAX_ENTITIES_PER_SECTION}. Omit the key \
                     to take the shipped Phase-1 cap."
                )));
            }
        }
        Ok(v1.into_policies())
    }

    fn schema_doc(&self) -> &'static str {
        include_str!("docs/v1.md")
    }
}

#[cfg(test)]
#[path = "language/tests.rs"]
mod tests;

/// The wire spellings of closed enum values, for error messages — read back
/// through serde so the text can never disagree with what the parser accepts.
pub(crate) fn wire_names<T: Serialize>(all: &[T]) -> String {
    all.iter()
        .map(|v| {
            serde_json::to_string(v)
                .unwrap_or_default()
                .trim_matches('"')
                .to_string()
        })
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(" | ")
}
