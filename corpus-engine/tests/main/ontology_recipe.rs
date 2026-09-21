// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ontology declaration v1, P1: the recipe block parses into policies through
//! the language registry; the three version rules refuse loudly; `validate`
//! covers the block; the templates are real recipes.
//!
//! Every refusal has a named red input here (ARCH §18.1). The I1 byte pin
//! lives beside the composer (`configurable_atlas.rs`).

use corpus_engine::enrichment::atlas::atoms::AtomType;
use corpus_engine::enrichment::ontology::{
    validate_block, AttrFamily, Deontic, Force, OntologyLanguageRegistry, OntologyPolicies,
    OntologyTypeDecl, SupersessionClock, TypeKind,
};
use corpus_engine::recipe::{EntityTypeDecl, RelationshipTypeDecl};
use corpus_engine::testing::validate_recipe_offline;
use corpus_engine::{recipe_templates, Recipe};

/// The maple-house recipe as vendored by `build.rs` (no repo-relative path).
pub(crate) const MAPLE_HOUSE: &str =
    include_str!(concat!(env!("OUT_DIR"), "/recipes/maple-house/recipe.toml"));

/// A minimal loadable recipe wrapped around an `[enrichment.ontology]` body.
pub(crate) fn recipe_with_ontology(body: &str) -> String {
    format!(
        r#"
[corpus]
id = "ont-test"
name = "Ontology test"

[acquire]
type = "local_file"
path = "/tmp/x.md"

[extract]
type = "markdown"

[chunk]
type = "paragraph"

[enrichment]
enabled = true
type = "atlas"

[enrichment.ontology]
{body}
"#
    )
}

pub(crate) fn policies_of(body: &str) -> OntologyPolicies {
    Recipe::from_toml(&recipe_with_ontology(body))
        .expect("recipe loads")
        .ontology_block()
        .expect("block present")
        .policies()
        .expect("block parses")
}

pub(crate) fn load_err(body: &str) -> String {
    Recipe::from_toml(&recipe_with_ontology(body))
        .err()
        .expect("recipe must be refused")
        .to_string()
}

// ── Parsing ─────────────────────────────────────────────────────────────────

#[test]
fn v0_parse_fills_prose_only() {
    let p = policies_of(
        r#"guidance = "Rules of a house."
[enrichment.ontology.vocabulary]
position_term = "rule""#,
    );
    assert_eq!(p.prose.guidance, "Rules of a house.");
    assert_eq!(p.prose.terms.position_term.as_deref(), Some("rule"));
    let mut prose_only = OntologyPolicies::default();
    prose_only.prose = p.prose.clone();
    assert_eq!(p, prose_only, "version 0 touches prose and nothing else");
    assert!(p.is_active());
    assert!(!p.has_declarations());
}

#[test]
fn v1_parse_numismatics_example() {
    let toml = recipe_templates::load_builtin("numismatics").expect("template");
    let recipe = Recipe::from_toml(toml).expect("numismatics template loads");
    let p = recipe.custom_ontology().expect("active custom ontology");
    assert_eq!(recipe.ontology_block().unwrap().version, 1);

    let kinds: Vec<(&str, TypeKind)> = p
        .shape
        .types
        .iter()
        .map(|t| (t.name.as_str(), t.kind))
        .collect();
    assert!(kinds.contains(&("coin", TypeKind::Entity)));
    assert!(kinds.contains(&("sceatta", TypeKind::Entity)));
    assert!(kinds.contains(&("ruler", TypeKind::Entity)));
    assert!(kinds.contains(&("attribution", TypeKind::Claim)));

    let coin = p.type_decl("coin").unwrap();
    let fam = |n: &str| &coin.attributes.iter().find(|a| a.name == n).unwrap().family;
    assert!(matches!(fam("ruler"), AttrFamily::Ref { of } if of == "ruler"));
    assert!(matches!(fam("metal"), AttrFamily::Text { values } if values.len() == 4));
    assert!(matches!(fam("weight"), AttrFamily::Quantity { unit: Some(u) } if u == "g"));
    assert!(matches!(fam("struck"), AttrFamily::Time { range: true }));
    assert!(matches!(fam("denomination"), AttrFamily::Text { values } if values.is_empty()));

    assert_eq!(
        p.type_decl("sceatta").unwrap().specializes.as_deref(),
        Some("coin")
    );
    assert_eq!(
        p.type_decl("ruler").unwrap().role_of.as_deref(),
        Some("person")
    );

    let attribution = p.claim_types().next().expect("one claim type");
    assert_eq!(attribution.name, "attribution");
    assert_eq!(attribution.force, Some(Force::Assertive));
    assert_eq!(attribution.subject.as_deref(), Some("coin"));
    assert_eq!(attribution.grades.len(), 4);
    assert_eq!(
        p.derivation.tension.between,
        vec!["attribution".to_string()]
    );
    assert_eq!(p.change.clock, SupersessionClock::DocumentDate);
}

#[test]
fn v1_empty_equals_v0_equals_default() {
    let v1_empty = policies_of("version = 1");
    let v0_empty = policies_of("");
    assert_eq!(v1_empty, v0_empty);
    assert_eq!(v1_empty, OntologyPolicies::default());
    assert!(v1_empty.is_empty());
    assert!(
        !v1_empty.is_active(),
        "an empty block does not select the custom path"
    );
}

/// The maple migrate path: the same prose + vocabulary under `version = 1`
/// yields exactly the version-0 policies.
#[test]
fn v1_with_guidance_equals_v0() {
    let v0 = Recipe::from_toml(MAPLE_HOUSE)
        .unwrap()
        .custom_ontology()
        .unwrap();
    let migrated = Recipe::migrate_ontology_version(MAPLE_HOUSE, 1)
        .expect("migrates")
        .expect("was version 0");
    assert_eq!(
        migrated.lines().count(),
        MAPLE_HOUSE.lines().count() + 1,
        "the migration adds one line and nothing else"
    );
    let v1 = Recipe::from_toml(&migrated)
        .unwrap()
        .custom_ontology()
        .unwrap();
    assert_eq!(v0, v1);
    assert!(
        Recipe::migrate_ontology_version(&migrated, 1)
            .unwrap()
            .is_none(),
        "already at version 1 → nothing to do"
    );
}

// ── The three version rules ─────────────────────────────────────────────────

#[test]
fn unknown_version_error_names_max() {
    let max = OntologyLanguageRegistry::builtin().max_version();
    let e = load_err(&format!("version = {}", max + 1));
    assert!(e.contains(&format!("version = {}", max + 1)), "{e}");
    assert!(e.contains(&format!("ontology version <= {max}")), "{e}");
}

#[test]
fn v1_key_without_version_error_names_fix() {
    let e = load_err(
        r#"guidance = "x"
[[enrichment.ontology.types]]
name = "coin"
kind = "entity""#,
    );
    assert!(e.contains("`types`"), "names the offending key: {e}");
    assert!(
        e.contains("version 1"),
        "names the version it belongs to: {e}"
    );
    assert!(e.contains("version = 1"), "names the line to add: {e}");
    assert!(
        e.contains("recipe migrate --ontology-version 1"),
        "names the command: {e}"
    );
}

#[test]
fn v1_claim_without_force_is_refused() {
    let e = load_err(
        r#"version = 1
[[enrichment.ontology.types]]
name = "finding"
kind = "claim""#,
    );
    assert!(e.contains("`finding`"), "{e}");
    assert!(e.contains("no `force`"), "{e}");
    for f in ["assertive", "directive", "declaration", "commissive"] {
        assert!(e.contains(f), "lists every force: {e}");
    }
}

#[test]
fn v1_unknown_kind_is_refused() {
    let e = load_err(
        r#"version = 1
[[enrichment.ontology.types]]
name = "coin"
kind = "thing""#,
    );
    assert!(e.contains("thing"), "{e}");
    assert!(e.contains("entity"), "lists the allowed kinds: {e}");
}

#[test]
fn unknown_key_in_any_version_is_a_validate_warning_not_a_load_error() {
    let recipe = Recipe::from_toml(&recipe_with_ontology(
        r#"guidance = "x"
guidnce = "typo""#,
    ))
    .expect("a stray key must not refuse the recipe");
    let v = validate_block(recipe.ontology_block().unwrap());
    assert!(v.errors.is_empty());
    assert_eq!(v.warnings.len(), 1, "{:?}", v.warnings);
    assert!(v.warnings[0].contains("`guidnce`"), "{}", v.warnings[0]);
    assert!(
        v.warnings[0].contains("guidance, vocabulary"),
        "{}",
        v.warnings[0]
    );
}

// ── Enum spellings and round trips ──────────────────────────────────────────

#[test]
fn type_kind_spelling_matches_atom_type_label() {
    for (kind, atom) in [
        (TypeKind::Entity, AtomType::Entity),
        (TypeKind::Relation, AtomType::Relation),
        (TypeKind::Claim, AtomType::Claim),
        (TypeKind::Event, AtomType::Event),
        (TypeKind::State, AtomType::State),
    ] {
        let wire = serde_json::to_string(&kind).unwrap();
        assert_eq!(wire.trim_matches('"'), atom.label(), "{kind:?}");
    }
}

#[test]
fn attr_family_round_trips() {
    let toml = r#"
attributes = [
  { name = "a", type = "text", values = ["x", "y"] },
  { name = "b", type = "quantity", unit = "g" },
  { name = "c", type = "quantity" },
  { name = "d", type = "time", range = true },
  { name = "e", type = "time" },
  { name = "f", type = "ref", of = "coin" },
]
"#;
    #[derive(serde::Deserialize, serde::Serialize, PartialEq, Debug)]
    struct Holder {
        attributes: Vec<corpus_engine::enrichment::ontology::AttrDecl>,
    }
    let h: Holder = toml::from_str(toml).expect("parses");
    let keys: Vec<&str> = h.attributes.iter().map(|a| a.family.key()).collect();
    assert_eq!(
        keys,
        ["text", "quantity", "quantity", "time", "time", "ref"]
    );
    let json = serde_json::to_string(&h).unwrap();
    let back: Holder = serde_json::from_str(&json).unwrap();
    assert_eq!(back, h);
    let back_toml: Holder = toml::from_str(&toml::to_string(&h).unwrap()).unwrap();
    assert_eq!(back_toml, h);
}

#[test]
fn policies_round_trip_json() {
    let p = Recipe::from_toml(recipe_templates::load_builtin("governance").unwrap())
        .unwrap()
        .custom_ontology()
        .unwrap();
    let json = serde_json::to_string_pretty(&p).unwrap();
    let back: OntologyPolicies = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
    // A policies JSON with only `prose` (what an older writer might record)
    // still loads, every other axis default.
    let sparse: OntologyPolicies = serde_json::from_str(r#"{"prose":{"guidance":"g"}}"#).unwrap();
    assert_eq!(
        sparse,
        OntologyPolicies::from_prose("g", Default::default())
    );
}

#[test]
fn investigation_decls_convert() {
    let e = EntityTypeDecl {
        name: "company".into(),
        description: "A corporation".into(),
        attributes: vec!["ticker".into(), "cik".into()],
    };
    let t: OntologyTypeDecl = (&e).into();
    assert_eq!(t.kind, TypeKind::Entity);
    assert_eq!(t.name, "company");
    assert_eq!(t.attributes.len(), 2);
    assert!(t
        .attributes
        .iter()
        .all(|a| matches!(a.family, AttrFamily::Text { .. })));

    let r = RelationshipTypeDecl {
        name: "revenue".into(),
        description: String::new(),
        attributes: vec!["amount_usd".into()],
        directional: true,
    };
    let t: OntologyTypeDecl = (&r).into();
    assert_eq!(t.kind, TypeKind::Relation);
    assert!(t.from.is_none() && t.to.is_none());
}

#[test]
fn registry_versions_contiguous() {
    let reg = OntologyLanguageRegistry::builtin();
    let versions: Vec<u32> = reg.versions().map(|l| l.version()).collect();
    let expected: Vec<u32> = (0..=reg.max_version()).collect();
    assert_eq!(versions, expected);
    assert_eq!(reg.first_version_defining("guidance"), Some(0));
    assert_eq!(reg.first_version_defining("types"), Some(1));
    assert_eq!(reg.first_version_defining("nope"), None);
    for lang in reg.versions() {
        assert!(
            !lang.schema_doc().trim().is_empty(),
            "version {} has a SCHEMA.md section",
            lang.version()
        );
    }
}

// ── Templates (the ten are exercised whole in `recipe_templates.rs`) ────────

#[test]
fn governance_template_labels_reach_the_vocabulary() {
    let p = Recipe::from_toml(recipe_templates::load_builtin("governance").unwrap())
        .unwrap()
        .custom_ontology()
        .unwrap();
    let v = p.vocabulary();
    assert_eq!(v.position_term, "rule");
    assert_eq!(v.tension_term, "conflict");
    let rule = p.claim_types().next().unwrap();
    assert_eq!(
        rule.deontic,
        vec![Deontic::Require, Deontic::Forbid, Deontic::Permit]
    );
    assert_eq!(
        p.change.supersedes.get("rule").map(String::as_str),
        Some("valid")
    );
}

// ── The navigation section (ei-2-map) ───────────────────────────────────────

/// A version-1 block that says nothing about navigation gets the spec's
/// pre-registered table; one that writes a row keeps that row and defaults
/// the rest; and the policies round-trip through JSON — the shape
/// `atlas/ontology.json` records. Failing input: drop `navigation` from
/// `V1_KEYS` (the key becomes a validate warning and the row is lost), or the
/// `#[serde(default = …)]` on a `NavigationPolicy` field.
#[test]
fn v1_navigation_round_trips_toml_and_json_with_defaults() {
    use corpus_engine::enrichment::ontology::{NavigationPolicy, QuestionKind};

    let absent = policies_of("version = 1\n");
    assert_eq!(absent.navigation, NavigationPolicy::default());

    let one_row = policies_of(
        "version = 1\n\
         [enrichment.ontology.navigation.tension]\n\
         seed = { kinds = [\"Claim\"] }\n\
         walk = [\"Tension\"]\n\
         hops = 2\n\
         budget = 8\n",
    );
    let tension = one_row.navigation.walk(QuestionKind::Tension);
    assert_eq!(
        tension.seed.kinds,
        vec![understanding_vocab::atoms::AtomType::Claim]
    );
    assert_eq!(
        tension.walk,
        vec![understanding_vocab::edges::EdgeType::Tension]
    );
    assert_eq!((tension.hops, tension.budget), (2, 8));
    assert_eq!(
        one_row.navigation.walk(QuestionKind::Thematic),
        NavigationPolicy::default().walk(QuestionKind::Thematic),
        "an unwritten row is the default row"
    );

    let json = serde_json::to_string(&one_row).unwrap();
    let back: OntologyPolicies = serde_json::from_str(&json).unwrap();
    assert_eq!(back, one_row);
}

/// An edge kind the atlas does not carry refuses at load, naming the section
/// and a valid spelling (§18.3: refuse, never default). The spec table writes
/// "Opposition"; the on-disk edge is `OpposesIn`, and that is what the error
/// offers. Failing input: `#[serde(other)]` or a string-typed `walk`.
#[test]
fn v1_navigation_unknown_edge_kind_is_refused_naming_the_valid_ones() {
    let err = load_err(
        "version = 1\n\
         [enrichment.ontology.navigation.tension]\n\
         walk = [\"Opposition\"]\n",
    );
    assert!(err.contains("navigation"), "names the section: {err}");
    assert!(err.contains("Opposition"), "names the offender: {err}");
    assert!(err.contains("OpposesIn"), "names a valid kind: {err}");
}
