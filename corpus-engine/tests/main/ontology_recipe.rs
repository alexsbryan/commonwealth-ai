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
    OntologyTypeDecl, SupersessionClock, TypeKind, MAX_ATTRS_PER_TYPE, MAX_ENUM_VALUES,
    MAX_TYPES_PER_KIND,
};
use corpus_engine::enrichment::pipeline::atlas::EntityType;
use corpus_engine::recipe::{EntityTypeDecl, RelationshipTypeDecl};
use corpus_engine::testing::validate_recipe_offline;
use corpus_engine::{recipe_templates, Recipe};

/// The maple-house recipe as vendored by `build.rs` (no repo-relative path).
const MAPLE_HOUSE: &str =
    include_str!(concat!(env!("OUT_DIR"), "/recipes/maple-house/recipe.toml"));

/// A minimal loadable recipe wrapped around an `[enrichment.ontology]` body.
fn recipe_with_ontology(body: &str) -> String {
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

fn policies_of(body: &str) -> OntologyPolicies {
    Recipe::from_toml(&recipe_with_ontology(body))
        .expect("recipe loads")
        .ontology_block()
        .expect("block present")
        .policies()
        .expect("block parses")
}

fn load_err(body: &str) -> String {
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

// ── validate: every rule, one red input each ────────────────────────────────

fn validate(body: &str) -> corpus_engine::enrichment::ontology::OntologyValidation {
    let recipe = Recipe::from_toml(&recipe_with_ontology(body)).expect("loads");
    validate_block(recipe.ontology_block().unwrap())
}

fn first_error_containing(body: &str, needle: &str) -> String {
    let v = validate(body);
    v.errors
        .iter()
        .find(|e| e.contains(needle))
        .cloned()
        .unwrap_or_else(|| panic!("no error containing {needle:?} in {:?}", v.errors))
}

#[test]
fn validate_unresolved_refs_name_the_facet_and_the_declared_set() {
    let body = r#"version = 1
[[enrichment.ontology.types]]
name = "coin"
kind = "entity"
attributes = [{ name = "mint", type = "ref", of = "mint" }]
specializes = "money"
[[enrichment.ontology.types]]
name = "sale"
kind = "event"
participants = { buyer = "merchant" }
[[enrichment.ontology.types]]
name = "attribution"
kind = "claim"
force = "assertive"
subject = "hoard""#;
    for (needle, facet) in [
        ("\"money\"", "specializes"),
        ("\"mint\"", "attributes.mint.of"),
        ("\"merchant\"", "participants.buyer"),
        ("\"hoard\"", "subject"),
    ] {
        let e = first_error_containing(body, needle);
        assert!(e.contains(facet), "{e}");
        assert!(e.contains("declared: attribution, coin, sale"), "{e}");
        assert!(e.contains("base kinds: person, "), "{e}");
    }
}

#[test]
fn validate_base_entity_kinds_resolve_without_declaration() {
    // Every kind the atlas already emits resolves undeclared — the set is
    // the enum's, so a variant added there is accepted here with no edit.
    for base in EntityType::NAMED {
        let body = format!(
            "version = 1\n[[enrichment.ontology.types]]\nname = \"x\"\nkind = \"entity\"\n\
             role_of = \"{base}\"\nattributes = [{{ name = \"at\", type = \"ref\", of = \"{base}\" }}]"
        );
        let v = validate(&body);
        assert!(v.errors.is_empty(), "{base}: {:?}", v.errors);
    }
    // Declaring one of them stays legal (to add attributes).
    let v = validate(
        r#"version = 1
[[enrichment.ontology.types]]
name = "person"
kind = "entity"
attributes = [{ name = "born", type = "time" }]
[[enrichment.ontology.types]]
name = "ruler"
kind = "entity"
role_of = "person""#,
    );
    assert!(v.errors.is_empty(), "{:?}", v.errors);
    // Red input: a name outside both sets still fails, and the message names
    // both sets so the author can see which one to extend.
    let e = first_error_containing(
        r#"version = 1
[[enrichment.ontology.types]]
name = "ruler"
kind = "entity"
role_of = "mint""#,
        "\"mint\"",
    );
    assert!(e.contains("declared: ruler; base kinds: "), "{e}");
}

#[test]
fn pattern_name_defaults_to_type() {
    // §1.6 writes `type = "circular_flow"` and no `name`: the name is the
    // type's wire tag, for every variant; a written name still wins.
    let p = policies_of(
        r#"version = 1
[[enrichment.ontology.types]]
name = "payment"
kind = "event"
[[enrichment.ontology.patterns]]
type = "circular_flow"
edge_types = ["payment"]
min_entities = 3
[[enrichment.ontology.patterns]]
type = "role_overlap"
entity_roles = { payer = "payment.from" }
[[enrichment.ontology.patterns]]
type = "threshold"
edge_type = "payment"
attribute = "amount"
threshold = 0.1
[[enrichment.ontology.patterns]]
type = "custom_sql"
query = "select 1"
[[enrichment.ontology.patterns]]
type = "threshold"
name = "large_payments"
edge_type = "payment"
attribute = "amount"
threshold = 0.5"#,
    );
    assert_eq!(p.derivation.patterns.len(), 5);
    for pat in &p.derivation.patterns[..4] {
        let v = serde_json::to_value(pat).unwrap();
        assert_eq!(v["name"], v["type"], "{v}");
    }
    let named = serde_json::to_value(&p.derivation.patterns[4]).unwrap();
    assert_eq!(named["name"], "large_payments");
}

#[test]
fn validate_same_must_name_subject_or_a_declared_attribute() {
    let body = r#"version = 1
[[enrichment.ontology.types]]
name = "rule"
kind = "claim"
force = "directive"
attributes = [{ name = "valid", type = "time", range = true }]
[enrichment.ontology.tension]
between = ["rule"]
same = ["subject", "valid", "topic"]"#;
    let v = validate(body);
    assert_eq!(v.errors.len(), 1, "{:?}", v.errors);
    assert!(v.errors[0].contains("`topic`"), "{}", v.errors[0]);
    assert!(v.errors[0].contains("attributes: valid"), "{}", v.errors[0]);
}

#[test]
fn validate_supersedes_and_between_must_name_claim_types() {
    let body = r#"version = 1
[[enrichment.ontology.types]]
name = "coin"
kind = "entity"
[[enrichment.ontology.types]]
name = "rule"
kind = "claim"
force = "directive"
[enrichment.ontology.change]
supersedes = { coin = "document_date", rule = "valid" }
[enrichment.ontology.tension]
between = ["coin"]"#;
    let v = validate(body);
    assert!(
        v.errors
            .iter()
            .any(|e| e.contains("change.supersedes names `coin`")),
        "{:?}",
        v.errors
    );
    assert!(
        v.errors
            .iter()
            .any(|e| e.contains("`valid` is neither `document_date`")),
        "{:?}",
        v.errors
    );
    assert!(
        v.errors
            .iter()
            .any(|e| e.contains("tension.between names `coin`")),
        "{:?}",
        v.errors
    );
}

#[test]
fn validate_deontic_only_with_directive() {
    let e = first_error_containing(
        r#"version = 1
[[enrichment.ontology.types]]
name = "finding"
kind = "claim"
force = "assertive"
deontic = ["require"]"#,
        "`finding`",
    );
    assert!(e.contains("not a directive"), "{e}");
    assert!(e.contains("assertive"), "{e}");
}

#[test]
fn validate_reserved_claim_names_rejected() {
    let e = first_error_containing(
        r#"version = 1
[[enrichment.ontology.types]]
name = "same_as"
kind = "claim"
force = "assertive""#,
        "`same_as`",
    );
    assert!(e.contains("reserved"), "{e}");
}

#[test]
fn validate_caps_are_named_with_their_numbers() {
    let mut body = String::from("version = 1\n");
    for i in 0..=MAX_TYPES_PER_KIND {
        body.push_str(&format!(
            "[[enrichment.ontology.types]]\nname = \"e{i}\"\nkind = \"entity\"\n"
        ));
    }
    let attrs: Vec<String> = (0..=MAX_ATTRS_PER_TYPE)
        .map(|i| format!("{{ name = \"a{i}\", type = \"text\" }}"))
        .collect();
    let values: Vec<String> = (0..=MAX_ENUM_VALUES).map(|i| format!("\"v{i}\"")).collect();
    body.push_str(&format!(
        "[[enrichment.ontology.types]]\nname = \"wide\"\nkind = \"event\"\nattributes = [{}, {{ name = \"enum\", type = \"text\", values = [{}] }}]\n",
        attrs.join(", "),
        values.join(", ")
    ));
    let v = validate(&body);
    let has = |s: &str| v.errors.iter().any(|e| e.contains(s));
    assert!(
        has(&format!("at most {MAX_TYPES_PER_KIND} per kind")),
        "{:?}",
        v.errors
    );
    assert!(
        has(&format!("at most {MAX_ATTRS_PER_TYPE}")),
        "{:?}",
        v.errors
    );
    assert!(has(&format!("at most {MAX_ENUM_VALUES}")), "{:?}", v.errors);
}

#[test]
fn validate_both_vocabulary_and_labels_warn() {
    let v = validate(
        r#"version = 1
[enrichment.ontology.vocabulary]
tension_term = "clash"
[[enrichment.ontology.types]]
name = "rule"
kind = "claim"
force = "directive"
[enrichment.ontology.tension]
label = "conflict"
between = ["rule"]"#,
    );
    assert!(v.errors.is_empty(), "{:?}", v.errors);
    assert!(
        v.warnings.iter().any(|w| w.contains("both")),
        "{:?}",
        v.warnings
    );
    // The label wins.
    let p = policies_of(
        r#"version = 1
[enrichment.ontology.vocabulary]
tension_term = "clash"
[enrichment.ontology.tension]
label = "conflict""#,
    );
    assert_eq!(p.vocabulary().tension_term, "conflict");
}

#[test]
fn validate_notes_print_derived_facets() {
    let toml = recipe_templates::load_builtin("numismatics").unwrap();
    let recipe = Recipe::from_toml(toml).unwrap();
    let v = validate_block(recipe.ontology_block().unwrap());
    assert!(v.errors.is_empty(), "{:?}", v.errors);
    let joined = v.notes.join("\n");
    assert!(joined.contains("clock: document_date"), "{joined}");
    assert!(
        joined.contains("tension selector: embedding top-k (k = 10, floor = 0.5) over attribution"),
        "{joined}"
    );
    assert!(
        joined.contains("identity: coin → canonical name (default"),
        "{joined}"
    );
    // `person` is absent on purpose. `ruler` still writes `role_of = "person"`,
    // but that now resolves against the base entity kinds, so the numismatics
    // declaration no longer declares `person` — and only DECLARED types are
    // enumerated: a kind the atlas already emits is not one of the author's
    // nouns, and listing it would promise a facet nobody asked for.
    assert!(
        joined.contains("question shapes: enumerate [coin, sceatta, ruler, mint]"),
        "{joined}"
    );
    assert!(!joined.contains("identity: person"), "{joined}");

    // Identity inherits through `specializes`; a declared key prints its kind.
    let v = validate(
        r#"version = 1
[[enrichment.ontology.types]]
name = "material"
kind = "entity"
identity = ["cas_number"]
attributes = [{ name = "cas_number", type = "text" }]
[[enrichment.ontology.types]]
name = "catalyst"
kind = "entity"
specializes = "material"
[[enrichment.ontology.types]]
name = "person"
kind = "entity"
identity_fallback = ["name", "employer"]"#,
    );
    let joined = v.notes.join("\n");
    assert!(
        joined.contains("identity: material → cas_number (external key, strict merge)"),
        "{joined}"
    );
    assert!(joined.contains("identity: catalyst → cas_number (external key, strict merge) — inherited from `material`"), "{joined}");
    assert!(
        joined.contains("identity: person → name + employer (descriptive keys, judged merge)"),
        "{joined}"
    );

    // A version-0 block derives nothing.
    let v0 = Recipe::from_toml(MAPLE_HOUSE).unwrap();
    assert!(validate_block(v0.ontology_block().unwrap())
        .notes
        .is_empty());
}

#[test]
fn validate_recipe_offline_carries_ontology_results() {
    let recipe = Recipe::from_toml(&recipe_with_ontology(
        r#"version = 1
[[enrichment.ontology.types]]
name = "rule"
kind = "claim"
force = "directive"
subject = "topic""#,
    ))
    .unwrap();
    let r = validate_recipe_offline(&recipe);
    assert!(
        r.errors.iter().any(|e| e.contains("\"topic\"")),
        "{:?}",
        r.errors
    );
    assert!(!r.notes.is_empty());
    assert_eq!(r.source_reachable, None);
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
