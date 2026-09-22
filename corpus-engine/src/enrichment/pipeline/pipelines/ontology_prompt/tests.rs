// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for [`super`], the ontology prompt block.
//!
//! A sibling file, not an inline module: the States block pushed
//! `ontology_prompt.rs` into arch-gate's 800-1200 approach band (ARCH §3.1).
//! `#[path]`, so every name and `use super::*` path is unchanged.

use super::*;

use super::super::ontology_schema::{
    phase1_schema_for, report_added_prompt_size, MAX_ADDED_PROMPT_CHARS,
};
use crate::recipe_templates::numismatics_policies as numismatics;

// ── Phase 6 extras ───────────────────────────────────────

#[test]
fn phase6_extras_empty_when_undeclared() {
    // Invariant I1: a version-0 block (prose only) and a version-1
    // block with no types both add NOTHING to the Phase-6 classifier,
    // so the template renders the bytes it always did.
    assert_eq!(
        render_phase6_extras(&OntologyPolicies::default()),
        "",
        "no declaration, no extras"
    );
    let prose =
        OntologyPolicies::from_prose("Rules about guests and quiet hours.", Default::default());
    assert_eq!(
        render_phase6_extras(&prose),
        "",
        "a version-0 prose block declares no types"
    );
}

#[test]
fn phase6_extras_render_the_relation_section_for_any_declared_corpus() {
    // Numismatics declares types and a `between`, but no
    // `not_conflicts` and no deontic: it gets the `relation` section
    // and neither of the other two.
    let extras = render_phase6_extras(&numismatics());
    assert!(extras.contains("## Relation"), "relation section present");
    assert!(extras.contains("`equivalent`"));
    assert!(
        !extras.contains("## Declared non-conflicts"),
        "numismatics names no non-conflicts"
    );
    assert!(
        !extras.contains("## Deontic reading"),
        "numismatics declares no directive claim type"
    );
    // The term placeholder survives for the caller to substitute.
    assert!(extras.contains("{tension_term}"));
}

#[test]
fn phase6_extras_render_non_conflicts_and_the_deontic_reading_when_declared() {
    let policies = crate::recipe_templates::policies("governance")
        .expect("governance is a shipped ontology template");

    let extras = render_phase6_extras(&policies);
    assert!(extras.contains("## Declared non-conflicts"));
    assert!(
        extras.contains("a rule for visitors versus a rule for members"),
        "the author's own words reach the classifier"
    );
    assert!(extras.contains("## Deontic reading"));
    assert!(
        extras.contains("require not-X"),
        "the interdefinition is what makes two surface forms one rule"
    );
    assert!(extras.contains("## Relation"));
}

#[test]
fn nothing_declared_renders_no_prompt_block() {
    let p = OntologyPolicies::from_prose("Rules of a house.", Default::default());
    assert_eq!(render_declared_types(&p), "");
}

#[test]
fn the_prompt_block_names_every_declared_type_and_its_attributes() {
    let block = render_declared_types(&numismatics());
    assert!(block.starts_with("## Declared types"));
    for name in ["coin", "sceatta", "ruler", "mint", "attribution"] {
        assert!(block.contains(&format!("**{name}**")), "{name} named");
    }
    assert!(block.contains("`entity_type`") && block.contains("`claim_kind`"));
    assert!(block.contains("weight (number in g)"), "unit rendered");
    assert!(block.contains("one of: gold | silver | billon | copper"));
    assert!(block.contains("name of a mint"), "ref target rendered");
    assert!(block.contains("about a coin"), "declared subject rendered");
    assert!(block.contains("grade: die-link | hoard-context"));
    // `sceatta` declares no attributes of its own but inherits coin's.
    assert!(block.contains("a kind of coin"));
}

/// The prose alone did not carry it: the neutral Phase-1 prompt this block
/// is appended to shows a worked JSON example whose `coin` entity has no
/// `attributes` object, and the wessex-hoard build filled 0 of 14. The
/// block has to show the shape, in the author's own keys.
///
/// Falsifier: drop `render_attribute_shape` from `render_declared_types`
/// and the emitted JSON has no example of a filled `attributes` object
/// anywhere in the Phase-1 prompt.
#[test]
fn the_block_shows_where_an_attribute_goes_in_the_json() {
    let block = render_declared_types(&numismatics());
    assert!(block.contains("## Where attributes go"));
    assert!(
        block.contains("\"attributes\": {"),
        "the object is shown as JSON, not only described: {block}"
    );
    assert!(
        block.contains("\"entity_type\": \"coin\""),
        "the sketch is shown whole, in the slot the type actually fills"
    );
    assert!(
        block.contains("never restated as a claim"),
        "the observed competing behaviour is named: the model emitted \
         `weight` as a claim rather than as the coin's attribute"
    );
    for key in [
        "\"metal\": <one of: gold | silver",
        "\"denomination\": <text>",
    ] {
        assert!(block.contains(key), "{key} shown in the author's own keys");
    }
    assert!(
        block.contains("\"weight\": <number in g>"),
        "a quantity is shown as a bare number — the family models get wrong"
    );
    assert!(
        !block.contains("silver") || !block.contains("\"metal\": \"silver\""),
        "the example must not carry a copyable value: a copied value is a \
         fabricated one, and it would read as a filled attribute in the \
         coverage report"
    );
}

/// A ref attribute is the declared ontology's load-bearing relation, and
/// the worked example is the only place its shape is shown filled. Measured
/// on ei7-ans (2026-09-22): `mint` filled from the row (247/362 coins) while
/// `hoard` — section context, named in the heading, never in the row —
/// filled 49/362 with ZERO unresolved-hoard failures (never emitted). Two
/// pins, one for each half of the fix:
///
/// 1. a declaration whose ref-carrying type is NOT first with attributes
///    (the ANS recipe's `hoard` declares text/time attrs ahead of `coin`)
///    still gets the ref-carrying type's example;
/// 2. the ref-context sentence — the only place the prompt says a ref may
///    come from the section heading — renders for a ref declaration and
///    for nothing else.
#[test]
fn a_ref_attribute_earns_the_example_and_names_its_context() {
    let block = render_declared_types(&numismatics());
    assert!(
        block.contains("name of a mint") || block.contains("name of a hoard"),
        "a declared ref renders its target: {block}"
    );
    assert!(
        block.contains("cannot be found from the thing it belongs to"),
        "the ref-context sentence is the heading-context instruction: {block}"
    );

    // The ANS-recipe shape: a `hoard` type declared FIRST with text attrs,
    // ahead of `coin` (refs) — the template itself has no hoard, so the
    // shape is built here the way the ANS recipe declares it.
    let mut ans_shape = numismatics();
    ans_shape.shape.types.insert(
        0,
        OntologyTypeDecl {
            name: "hoard".into(),
            kind: TypeKind::Entity,
            description: "A group of coins buried and found together.".into(),
            attributes: vec![AttrDecl {
                name: "findspot".into(),
                family: AttrFamily::Text { values: vec![] },
                description: String::new(),
            }],
            specializes: None,
            role_of: None,
            ..Default::default()
        },
    );
    let block = render_declared_types(&ans_shape);
    assert!(
        block.contains("\"mint\": <name of a mint>"),
        "the ref-carrying type owns the example even when a text-attr type \
         is declared first: {block}"
    );

    // A declaration with no ref pays nothing for the sentence.
    let mut text_only = numismatics();
    for t in text_only.shape.types.iter_mut() {
        t.attributes
            .retain(|a| !matches!(a.family, AttrFamily::Ref { .. }));
    }
    let block = render_declared_types(&text_only);
    assert!(
        !block.contains("cannot be found from the thing it belongs to"),
        "no declared ref, no ref-context sentence"
    );
}

/// An attribute named in `identity` decides whether two mentions merge, so
/// it cannot read as one of seven interchangeable keys. `catalogue_ref`
/// reached 3 of 14 coins while the corpus stated it on both the catalogue
/// entry and the article, and the merge it exists for fired zero times.
///
/// Falsifier: drop the identity sentence and `catalogue_ref` is named
/// nowhere except in the same list as `denomination`.
#[test]
fn the_identity_keys_are_singled_out() {
    // The SHIPPED template declares no `identity` — the wessex-hoard probe
    // recipe is where P3 added `catalogue_ref` — so the negative case is
    // the fixture as it stands and the positive case declares a key.
    assert!(
        !render_declared_types(&numismatics()).contains("Always fill"),
        "nothing is claimed about identity when the recipe declares no key"
    );

    let mut p = numismatics();
    let coin = p
        .shape
        .types
        .iter_mut()
        .find(|t| t.name == "coin")
        .expect("the template declares a coin");
    coin.attributes.push(AttrDecl {
        name: "catalogue_ref".into(),
        family: AttrFamily::Text { values: vec![] },
        description: String::new(),
    });
    coin.identity = vec!["catalogue_ref".into()];

    let block = render_declared_types(&p);
    assert!(
        block.contains("Always fill `catalogue_ref`"),
        "the declared identity key is named as one: {block}"
    );
}

/// The neutral prompt's claim field list names `attributed_to` and never
/// `subject` — and the build filled `attributed_to` on all 49 claims and
/// `subject` on 1. Named in the prompt, filled; absent from it, empty.
///
/// Falsifier: drop `render_subject_shape` and the word `subject` appears
/// nowhere in the Phase-1 prompt for a corpus that declares one.
#[test]
fn the_block_says_what_a_declared_claim_is_about() {
    let block = render_declared_types(&numismatics());
    assert!(block.contains("## What a claim is about"));
    assert!(
        block.contains("\"subject\": <the canonical_name of the coin>"),
        "the slot is shown, in the declared subject's own type name: {block}"
    );
    assert!(
        block.contains("`attribution` is declared as a claim about a `coin`"),
        "and named from the declaration, not hardcoded"
    );
    assert!(
        block.contains("`subject` is what the claim is about; `attributed_to` is who makes it"),
        "`subject` is separated from `attributed_to`, which the neutral \
         prompt already asks for and the model already fills"
    );

    // A claim type declaring no subject buys none of it.
    let mut p = numismatics();
    for t in &mut p.shape.types {
        t.subject = None;
    }
    assert!(
        !render_declared_types(&p).contains("## What a claim is about"),
        "nothing is said about a link the recipe never declared"
    );
}

/// EVERY worked example in this block shows its `attributes` object.
///
/// The block exists because an example that omits a slot teaches the model
/// to omit it — so an example in the block that omits `attributes` undoes
/// the block. That is not hypothetical: the claim example shipped without
/// one for a single build and took `attribution proposed_date` from 14 of
/// 43 claims to 0 of 41, and every `grade` with it.
///
/// Falsifier: drop `attributes` from either example and the count of
/// `"attributes": {` falls below the count of example blocks.
#[test]
fn no_worked_example_omits_the_attributes_object() {
    let block = render_declared_types(&numismatics());
    let examples =
        block.matches("\"claim_kind\":").count() + block.matches("\"entity_type\":").count();
    assert!(examples >= 2, "both examples render: {block}");
    assert_eq!(
        block.matches("\"attributes\": {").count(),
        examples,
        "every example carries the object it is teaching: {block}"
    );
    assert!(
        block.contains("\"proposed_date\": <date or range>"),
        "the claim example uses the claim type's OWN declared attributes"
    );
    assert!(
        block.contains("\"grade\": <one of the grades above>"),
        "and the reserved key that rides in the same bag"
    );
}

/// The deontic mode is the directive's force, and it was in the schema,
/// absent from the worked example, and filled on 46 of 1,221 obligations
/// (spike 3, 2026-09-19) — the same defect
/// [`no_worked_example_omits_the_attributes_object`] pins for the bag as
/// a whole. So a declared mode is SHOWN in the example and REQUIRED in
/// the bag, and a claim type that declares one earns an example even with
/// no `subject` to hang it on.
///
/// Falsifier: drop the `deontic` push from the example and the prompt
/// stops naming the slot it is asking the model to fill.
#[test]
fn deontic_is_required_and_shown_when_declared() {
    let governance = crate::recipe_templates::policies("governance")
        .expect("governance is a shipped ontology template");

    let block = render_declared_types(&governance);
    assert!(
        block.contains("\"deontic\": \"require\""),
        "the example shows the type's first declared mode: {block}"
    );
    let bag = &phase1_schema_for(&governance)["$defs"]["claim_sketch"]["properties"]["attributes"];
    assert!(
        bag["properties"].get("deontic").is_some(),
        "the schema still offers the slot: {bag}"
    );
    assert_eq!(
        bag["required"],
        serde_json::json!(["deontic"]),
        "and requires it by name, so an opened bag cannot omit it: {bag}"
    );

    // A declared mode with no `subject` still earns the example — before
    // this it got none at all, so the slot was named nowhere.
    let mut no_subject = governance.clone();
    for t in &mut no_subject.shape.types {
        t.subject = None;
    }
    let block = render_declared_types(&no_subject);
    assert!(
        block.contains("## What a claim looks like"),
        "the example renders on the deontic alone: {block}"
    );
    assert!(block.contains("\"deontic\": \"require\""));
    assert!(
        !block.contains("\"subject\":"),
        "and claims no is-about link the recipe never declared: {block}"
    );

    // A recipe declaring no mode pays nothing, in either surface.
    let plain = numismatics();
    assert!(!render_declared_types(&plain).contains("\"deontic\""));
    assert!(
        phase1_schema_for(&plain)["$defs"]["claim_sketch"]["properties"]["attributes"]
            .get("required")
            .is_none()
    );
}

/// A declaration with types but no attributes pays nothing for the block.
#[test]
fn nothing_to_fill_renders_no_shape_section() {
    let mut p = numismatics();
    for t in &mut p.shape.types {
        t.attributes.clear();
    }
    let block = render_declared_types(&p);
    assert!(block.starts_with("## Declared types"), "types still named");
    assert!(
        !block.contains("## Where attributes go"),
        "but no attribute shape: {block}"
    );
}

#[test]
fn voices_and_must_not_render_only_when_declared() {
    let mut p = numismatics();
    assert!(!render_declared_types(&p).contains("## Voices"));
    assert!(!render_declared_types(&p).contains("## Must not"));
    p.assertion.voices.not_entities = vec!["the cataloguer".into()];
    p.assertion.must_not = vec!["price a coin".into()];
    let block = render_declared_types(&p);
    assert!(block.contains("## Voices") && block.contains("the cataloguer"));
    assert!(block.contains("## Must not") && block.contains("- price a coin"));
}

/// The fixture must fit the budget; if it ever does not, the number is
/// the thing to read, not the pass/fail.
///
/// The SHIPPED template is not the worst case a shipped recipe reaches.
/// `sovereign-recipes/wessex-hoard/recipe.toml` declares one more `coin`
/// attribute (`catalogue_ref`) plus an `identity`, and that recipe went
/// over budget on a build while this test stayed green against the
/// template — a gate with no input that could fail it (§18.1). So the
/// probe's shape is measured here too, and the assertion names which of
/// the two blew the budget.
#[test]
fn the_shipped_fixture_fits_the_prompt_budget() {
    let measure = |label: &str, p: &OntologyPolicies| {
        let added =
            report_added_prompt_size(label, &render_declared_types(p), &phase1_schema_for(p));
        assert!(
            added <= MAX_ADDED_PROMPT_CHARS,
            "{label} adds {added} chars, budget {MAX_ADDED_PROMPT_CHARS}"
        );
        added
    };
    let template = measure("numismatics (template)", &numismatics());

    let mut probe = numismatics();
    let coin = probe
        .shape
        .types
        .iter_mut()
        .find(|t| t.name == "coin")
        .expect("the template declares a coin");
    coin.attributes.push(AttrDecl {
        name: "catalogue_ref".into(),
        family: AttrFamily::Text { values: vec![] },
        description: String::new(),
    });
    coin.identity = vec!["catalogue_ref".into()];
    let probe_size = measure("numismatics (wessex-hoard probe)", &probe);

    assert!(
        probe_size > template,
        "the probe is the larger of the two, so it is the one that binds"
    );
}
