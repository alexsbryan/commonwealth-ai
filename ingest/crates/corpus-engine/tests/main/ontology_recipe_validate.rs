// SPDX-License-Identifier: AGPL-3.0-or-later
//! `recipe validate` for the ontology block — every rule, one red input each.
//!
//! Split out of `ontology_recipe.rs` when the state-routing and `tension.same`
//! rules pushed that file into arch-gate's 800-1200 approach band (ARCH §3.1).
//! The parsing, version-rule, enum-spelling, template and navigation tests
//! stayed there; this file is the validator's own red inputs.

use corpus_engine::enrichment::ontology::{
    validate_block, MAX_ATTRS_PER_TYPE, MAX_ENUM_VALUES, MAX_TYPES_PER_KIND,
};
use corpus_engine::enrichment::pipeline::atlas::EntityType;
use corpus_engine::testing::validate_recipe_offline;
use corpus_engine::{recipe_templates, Recipe};

use super::ontology_recipe::{maple_house, policies_of, recipe_with_ontology};

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

/// `clock` is one of the two reserved `same` fields and half of the
/// documented default, but the validator only knew `subject` — so writing the
/// default out explicitly was refused as an undeclared attribute.
#[test]
fn validate_same_accepts_the_reserved_clock_field() {
    let body = r#"version = 1
[[enrichment.ontology.types]]
name = "rule"
kind = "claim"
force = "directive"
[enrichment.ontology.tension]
between = ["rule"]
same = ["subject", "clock"]"#;
    let v = validate(body);
    assert!(v.errors.is_empty(), "{:?}", v.errors);
}

/// `same = []` says there is no comparability criterion — the form a corpus
/// uses to seek tensions ACROSS subjects, e.g. two characters in conflict.
/// It names no field, so there is nothing to resolve and nothing to refuse.
#[test]
fn validate_accepts_an_empty_same_as_no_criterion() {
    let body = r#"version = 1
[[enrichment.ontology.types]]
name = "conviction"
kind = "claim"
force = "assertive"
[enrichment.ontology.tension]
between = ["conviction"]
same = []"#;
    let v = validate(body);
    assert!(v.errors.is_empty(), "{:?}", v.errors);
}

/// A state is a condition OF something, and `of` is what routes it to a
/// Phase-1 facet. Without it the type used to load and then do nothing.
#[test]
fn validate_state_without_of_is_refused() {
    let body = r#"version = 1
[[enrichment.ontology.types]]
name = "inner_state"
kind = "state""#;
    let v = validate(body);
    assert_eq!(v.errors.len(), 1, "{:?}", v.errors);
    assert!(v.errors[0].contains("declares no `of`"), "{}", v.errors[0]);
}

/// The `State` atom has no attribute bag, so attributes declared on a state
/// type would be asked of the model and then dropped.
#[test]
fn validate_state_with_attributes_is_refused() {
    let body = r#"version = 1
[[enrichment.ontology.types]]
name = "character"
kind = "entity"
[[enrichment.ontology.types]]
name = "inner_state"
kind = "state"
of = "character"
attributes = [{ name = "intensity", type = "text" }]"#;
    let v = validate(body);
    assert_eq!(v.errors.len(), 1, "{:?}", v.errors);
    assert!(v.errors[0].contains("intensity"), "{}", v.errors[0]);
    assert!(
        v.errors[0].contains("carries no attributes"),
        "{}",
        v.errors[0]
    );
}

/// The two legal shapes, both green: a state of one thing and a state of a
/// pair. `of = "bond"` naming a declared relation is how the second is said.
#[test]
fn validate_accepts_a_state_of_an_entity_and_a_state_of_a_relation() {
    let body = r#"version = 1
[[enrichment.ontology.types]]
name = "character"
kind = "entity"
[[enrichment.ontology.types]]
name = "bond"
kind = "relation"
from = "character"
to = "character"
[[enrichment.ontology.types]]
name = "inner_state"
kind = "state"
of = "character"
[[enrichment.ontology.types]]
name = "bond_state"
kind = "state"
of = "bond""#;
    let v = validate(body);
    assert!(v.errors.is_empty(), "{:?}", v.errors);
}

/// `EntityType` carries an `Other(String)` arm, so a misspelled
/// `seed.entity_types` deserialises into a type no entity has and the row
/// seeds nothing — the only navigation field that could fail this quietly.
#[test]
fn validate_seed_entity_types_must_name_a_real_entity_type() {
    let body = r#"version = 1
[[enrichment.ontology.types]]
name = "character"
kind = "entity"
[enrichment.ontology.navigation.trajectory]
seed = { kinds = ["Entity"], entity_types = ["character", "caracter"] }"#;
    let v = validate(body);
    assert_eq!(v.errors.len(), 1, "{:?}", v.errors);
    assert!(v.errors[0].contains("`caracter`"), "{}", v.errors[0]);
    assert!(
        v.errors[0].contains("navigation.trajectory"),
        "{}",
        v.errors[0]
    );
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
    let v0 = Recipe::from_toml(maple_house()).unwrap();
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

// ── change.document ──────────────────────────────────────────────────────────

const DOCUMENT_FIELDS: &str = r#"version = 1
[[enrichment.ontology.types]]
name = "deal_act"
kind = "claim"
force = "commissive"
[enrichment.ontology.change]
document = { date = "sent", thread = "conversation", id = "msg" }"#;

/// What `svrn recipe validate` prints under "Derived from your
/// declarations:" is `ValidationResult::notes`, line for line.
#[test]
fn validate_prints_which_document_field_becomes_which_stamp() {
    let recipe = Recipe::from_toml(&recipe_with_ontology(DOCUMENT_FIELDS)).unwrap();
    let r = validate_recipe_offline(&recipe);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let line = r
        .notes
        .iter()
        .find(|n| n.starts_with("document fields: "))
        .unwrap_or_else(|| panic!("no document line in {:?}", r.notes));
    assert_eq!(
        line,
        "document fields: document_date ← `sent` (RFC 2822 or ISO 8601, written as ISO 8601), \
         document_thread ← `conversation`, document_id ← `msg` — stamped on each claim from \
         the one document its evidence lands in; a claim in none or several is left \
         unstamped and counted"
    );
    // Printed with no declared types too: the stamps apply to every claim.
    let bare = validate(
        r#"version = 1
[enrichment.ontology.change]
document = { date = "sent" }"#,
    );
    assert!(
        bare.notes
            .iter()
            .any(|n| n.starts_with("document fields: document_date ← `sent`")),
        "{:?}",
        bare.notes
    );
}

#[test]
fn validate_refuses_an_empty_or_colliding_change_document() {
    let e = first_error_containing(
        r#"version = 1
[enrichment.ontology.change]
document = {}"#,
        "names no field",
    );
    assert!(e.contains("date, thread, id"), "{e}");
    first_error_containing(
        r#"version = 1
[enrichment.ontology.change]
document = { thread = " " }"#,
        "change.document.thread is blank",
    );
    let e = first_error_containing(
        r#"version = 1
[[enrichment.ontology.types]]
name = "deal_act"
kind = "claim"
force = "commissive"
attributes = [{ name = "document_date", type = "time" }]
[enrichment.ontology.change]
document = { date = "sent" }"#,
        "Rename the attribute",
    );
    assert!(
        e.contains("`deal_act` declares attribute `document_date`"),
        "{e}"
    );
}

#[test]
fn validate_evidential_fields_are_declared_stamps_measured_somewhere_beside_a_bar() {
    let typ = |evidential: &str, bar: &str| {
        format!(
            r#"version = 1
[[enrichment.ontology.types]]
name = "case"
kind = "entity"
identity_criterion = "the same problem"
identity_evidential = [{evidential}]
{bar}
[enrichment.ontology.change]
document = {{ thread = "thread" }}
"#
        )
    };
    let ok = r#"{ evidence = "document_thread", right = 190, of = 210, measured_on = "uv tune, 257/311" }"#;
    let v = validate(&typ(ok, "identity_bar = 0.5"));
    assert!(v.errors.is_empty(), "{:?}", v.errors);
    assert!(
        v.notes.iter().any(|n| n.starts_with(
            "identity evidence: case ← document_thread (190 of 210, expected precision 0.901"
        )),
        "{:?}",
        v.notes
    );
    // A criterion makes RESOLVE the decider, not the canonical name.
    assert!(
        v.notes
            .iter()
            .any(|n| n.starts_with("identity: case → RESOLVE under its identity_criterion")),
        "{:?}",
        v.notes
    );
    first_error_containing(&typ(ok, ""), "no `identity_bar`");
    first_error_containing(&typ(ok, "identity_bar = 0"), "is not in (0, 1]");
    first_error_containing(
        &typ(
            r#"{ evidence = "author", right = 9, of = 10, measured_on = "x" }"#,
            "identity_bar = 0.5",
        ),
        "which is no source",
    );
    first_error_containing(
        &typ(
            r#"{ evidence = "document_date", right = 9, of = 10, measured_on = "x" }"#,
            "identity_bar = 0.5",
        ),
        "change.document.date",
    );
    first_error_containing(
        &typ(
            r#"{ evidence = "document_thread", right = 13, of = 10, measured_on = "x" }"#,
            "identity_bar = 0.5",
        ),
        "no more right than measured",
    );
    first_error_containing(
        &typ(
            r#"{ evidence = "document_thread", right = 9, of = 10, measured_on = " " }"#,
            "identity_bar = 0.5",
        ),
        "says nothing in `measured_on`",
    );
    let v = validate(&typ(
        r#"{ evidence = "model_choice", right = 4, of = 10, measured_on = "x" }, { evidence = "proposed_answer", right = 8, of = 10, measured_on = "y" }"#,
        "identity_bar = 0.5",
    ));
    assert!(v.errors.is_empty(), "{:?}", v.errors);
    first_error_containing(
        &typ(ok, "identity_bar = 0.5\nidentity_necessary = [\"kind\"]"),
        "necessary attribute `kind`, which is not one of its attributes with `values`",
    );
    let with_kind = typ(ok, "identity_bar = 0.5\nidentity_necessary = [\"kind\"]\nattributes = [{ name = \"kind\", type = \"text\", values = [\"firing\", \"death\"] }]");
    let v = validate(&with_kind);
    assert!(v.errors.is_empty(), "{:?}", v.errors);
}
