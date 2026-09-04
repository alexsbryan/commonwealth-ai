// SPDX-License-Identifier: AGPL-3.0-or-later
//! The built-in pipelines' maps, as data (`EPISTEMIC_INDEX.md` §2.1).
//!
//! A built-in pipeline's fixed vocabulary — what a `concept` entity is to the
//! literary genre, that the philosophy genre reconstructs arguments, that the
//! conversation genre never makes the user an entity — is written down in
//! `ontologies/<pipeline>.toml` as the body of a `[enrichment.ontology]`
//! block and parsed by the SAME version-1 language a recipe's block goes
//! through. That is what makes "no private kinds" (§3) structural rather
//! than remembered: a vocabulary the declaration language cannot express
//! fails to parse here, in `builtin_declarations_parse`, before it reaches
//! an atlas.
//!
//! What the TOML does NOT carry, on purpose: vocabulary terms and the
//! configuration flag. Each has one decider on the pipeline
//! (`Pipeline::vocabulary`, `Pipeline::runs_configuration_phase`), and
//! `Pipeline::declared_ontology` fills them from there — a second copy here
//! would be the two-implementations smell (ARCH §10.6).

use std::sync::LazyLock;

use crate::enrichment::atlas::AtlasOntologyFile;
use crate::enrichment::ontology::{OntologyLanguageRegistry, OntologyPolicies};

/// Parse one built-in declaration through the version-1 language. Panics on a
/// malformed file: these are compiled-in assets, checked by
/// `builtin_declarations_parse` before any binary that carries them ships.
fn parse_builtin(pipeline_id: &str, src: &str) -> OntologyPolicies {
    let version = AtlasOntologyFile::BUILTIN_ONTOLOGY_VERSION;
    let table: toml::Table = toml::from_str(src)
        .unwrap_or_else(|e| panic!("built-in ontology for `{pipeline_id}` is not TOML: {e}"));
    let language = OntologyLanguageRegistry::builtin()
        .get(version)
        .unwrap_or_else(|| panic!("ontology language version {version} is not registered"));
    language.parse(&table).unwrap_or_else(|e| {
        panic!("built-in ontology for `{pipeline_id}` does not parse under version {version}: {e}")
    })
}

macro_rules! builtin {
    ($name:ident, $id:literal) => {
        pub(super) static $name: LazyLock<OntologyPolicies> = LazyLock::new(|| {
            parse_builtin($id, include_str!(concat!("ontologies/", $id, ".toml")))
        });
    };
}

builtin!(LITERARY, "literary_atlas");
builtin!(PHILOSOPHY, "philosophy_atlas");
builtin!(CONVERSATION, "conversation_atlas");
builtin!(REFERENTIAL, "referential_atlas");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::ontology::TypeKind;

    /// Every built-in declaration parses under the version-1 language and
    /// declares only entity types whose `name` is an `entity_type` its Phase-1
    /// prompt can emit. Failing input: a `kind = "configuration"` type, or a
    /// name like `theme` that no atom carries as `entity_type`.
    #[test]
    fn builtin_declarations_parse() {
        for (id, p) in [
            ("literary_atlas", &*LITERARY),
            ("philosophy_atlas", &*PHILOSOPHY),
            ("conversation_atlas", &*CONVERSATION),
            ("referential_atlas", &*REFERENTIAL),
        ] {
            assert!(p.has_declarations(), "{id} declares nothing");
            assert!(
                p.prose.guidance.is_empty(),
                "{id}: guidance belongs to a recipe, not a built-in map (it is the custom-path hinge)"
            );
            assert_eq!(
                p.prose.terms,
                Default::default(),
                "{id}: terms come from `vocabulary()`, not the TOML"
            );
            for t in &p.shape.types {
                assert_eq!(
                    t.kind,
                    TypeKind::Entity,
                    "{id}: {} is not an entity type",
                    t.name
                );
            }
        }
    }
}
