// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ten ontology-v1 templates (`ONTOLOGY_PRIMITIVES.md` §1) are real
//! recipes: each parses, is an active version-1 ontology, validates
//! error-free with every reference resolved, instantiates, and derives the
//! facets pinned under `tests/fixtures/recipe_templates/<name>.facets.txt`.
//! The goldens re-bless the way the prompt snapshots do:
//!
//! ```text
//! UPDATE_ONTOLOGY_SNAPSHOTS=1 cargo test -p corpus-engine --test main recipe_templates
//! ```

use std::path::PathBuf;

use corpus_engine::testing::validate_recipe_offline;
use corpus_engine::{recipe_templates, Recipe};

/// One template per PRIMITIVES §1 user, in §1 order — the catalog order
/// `list_builtin_names` must return.
const TEN: [&str; 10] = [
    "numismatics",
    "governance",
    "patient-community",
    "contracts",
    "engineering-org",
    "due-diligence",
    "literary",
    "research-notebook",
    "materials-lab",
    "product-support",
];

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/recipe_templates")
}

#[test]
fn recipe_templates_all_ten_parse() {
    assert_eq!(recipe_templates::list_builtin_names(), TEN.to_vec());
    for name in TEN {
        let toml = recipe_templates::load_builtin(name).unwrap();
        let recipe = Recipe::from_toml(toml).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(recipe.ontology_block().unwrap().version, 1, "{name}");
        let p = recipe
            .custom_ontology()
            .unwrap_or_else(|| panic!("{name}: not active"));
        assert!(p.has_declarations(), "{name}");
        // Error-free validation IS the reference check: every `specializes`,
        // `role_of`, `from`, `to`, `of`, `subject` and `ref … of` resolved, and
        // `supersedes`/`between`/`same` named what they must.
        let r = validate_recipe_offline(&recipe);
        assert!(r.errors.is_empty(), "{name}: {:?}", r.errors);
        assert!(r.warnings.is_empty(), "{name}: {:?}", r.warnings);
        assert!(!r.notes.is_empty(), "{name}: derived facets printed");
        assert!(
            toml.contains("version = 1"),
            "{name}: the contract journey asserts this line"
        );

        let instantiated = recipe_templates::instantiate(toml, Some("my-corpus"));
        let inst = Recipe::from_toml(&instantiated).unwrap();
        assert_eq!(inst.corpus.id, "my-corpus");
        assert_eq!(inst.corpus.name, "my-corpus");
        assert!(!instantiated.contains(&format!("id = \"{}\"", recipe_templates::PLACEHOLDER)));
    }
    // The unknown-id error names every template there is (ARCH §4).
    let e = recipe_templates::load_builtin("nope")
        .err()
        .unwrap()
        .to_string();
    for name in TEN {
        assert!(e.contains(name), "{e}");
    }
}

/// What `recipe validate` prints for each template — clock, tension
/// selector, identity default per entity type, question shapes — pinned so a
/// change to the derivation rules is read in a diff, not discovered by an
/// author.
#[test]
fn recipe_templates_derived_facets_match_goldens() {
    for name in TEN {
        let recipe = Recipe::from_toml(recipe_templates::load_builtin(name).unwrap()).unwrap();
        let mut rendered = validate_recipe_offline(&recipe).notes.join("\n");
        rendered.push('\n');
        crate::ontology_prompt_snapshots::assert_golden(
            &fixtures_dir().join(format!("{name}.facets.txt")),
            &rendered,
            "recipe_templates",
        );
    }
}
