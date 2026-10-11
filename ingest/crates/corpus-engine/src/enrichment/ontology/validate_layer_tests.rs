// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fill analysis's warnings, one test each, over one domain-free
//! declaration: a sourced `party`, an `item` RESOLVE decides, and a `move`
//! claim about it whose protocol, identity and derived path are keyed on
//! fields the passes reader never asks (blind round 2's silent failures,
//! campaign ontology-layer 2026-10-10).

use super::super::validate_block;
use crate::recipe::Recipe;

const FIXTURE: &str = r#"
[corpus]
id = "fill"
name = "fill"
[acquire]
type = "local_file"
path = "/tmp/fill.jsonl"
[extract]
type = "email"
[chunk]
type = "passthrough"
[enrichment]
enabled = true
type = "atlas"
domain = "fill"
[enrichment.ontology]
version = 1

[enrichment.ontology.change]
document = { date = "date", thread = "thread_id", id = "message_id" }

[[enrichment.ontology.types]]
name = "party"
kind = "entity"
attributes = [
  { name = "domain", type = "text" },
  { name = "tier", type = "text", values = ["a", "b"] },
]
identity = ["domain"]
source = { metadata = ["from", "to"], attributes = { domain = "domain" } }

[[enrichment.ontology.types]]
name = "item"
kind = "entity"
identity_criterion = "the same item"
attributes = [
  { name = "code", type = "text" },
  { name = "owner", type = "ref", of = "party", derived = "owner_of_item" },
  { name = "side", type = "text", values = ["left", "right"] },
  { name = "status", type = "text", values = ["open", "shut"], derived = "item_status" },
]
identity = ["code"]
identity_necessary = ["side"]

[[enrichment.ontology.types]]
name = "lot"
kind = "entity"
attributes = [
  { name = "lot_code", type = "text" },
  { name = "tag", type = "ref", of = "party" },
]
identity = ["lot_code"]

[[enrichment.ontology.types]]
name = "move"
kind = "claim"
force = "declaration"
subject = "item"
attributes = [
  { name = "step", type = "text", values = ["open", "shut"] },
  { name = "move_id", type = "text" },
  { name = "at", type = "time" },
  { name = "holder", type = "ref", of = "party" },
  { name = "batch", type = "ref", of = "lot" },
  { name = "grade", type = "text", values = [GRADES] },
]

[[enrichment.ontology.paths]]
id = "lot_tag"
path = "batch / tag"

[[enrichment.ontology.folds]]
id = "owner_of_item"
by = "most"
from = ["^subject / holder"]

[[enrichment.ontology.folds]]
id = "item_status"
by = "protocol"
from = ["^subject"]

[enrichment.ontology.folds.protocol]
identity = "move_id"
effective_time = "at"

[[enrichment.ontology.folds.protocol.rules]]
id = "opened"
claim_kind = "move"
state = "open"
when = { step = "open" }
"#;

/// The fixture, its `grade` a closed set one value wider than a forced choice.
fn fixture() -> String {
    let grades: Vec<String> = (0..=crate::enrichment::atlas::resolve_records::LABELS.len())
        .map(|i| format!("\"g{i}\""))
        .collect();
    FIXTURE.replace("GRADES", &grades.join(", "))
}

fn validated(recipe: &str) -> super::super::OntologyValidation {
    let recipe = Recipe::from_toml(recipe).expect("the fixture loads");
    validate_block(recipe.ontology_block().expect("an ontology block"))
}

fn warned<'a>(v: &'a super::super::OntologyValidation, needle: &str) -> Vec<&'a String> {
    v.warnings.iter().filter(|w| w.contains(needle)).collect()
}

#[test]
fn every_field_a_pass_asks_is_named_with_its_pass_and_the_rest_is_warned() {
    let v = validated(&fixture());
    let note = |t: &str| {
        v.notes
            .iter()
            .find(|n| n.starts_with(&format!("fill: {t}:")))
            .unwrap_or_else(|| panic!("a fill note for {t}: {:#?}", v.notes))
            .clone()
    };
    let mv = note("move");
    for filled in [
        "subject `item` ← RESOLVE",
        "step ← Choose on move",
        "move_id ← Point on move",
        "at ← Point on move",
        "holder ← Pick on move (candidates: `party`'s source records, Mention)",
        "batch ← Pick on move (candidates: Mention)",
    ] {
        assert!(mv.contains(filled), "{filled} in {mv}");
    }
    assert!(
        note("item").contains("code ← Point on each move statement, not the record"),
        "{}",
        note("item")
    );
    // `grade` is wider than a forced choice: warned, with why.
    let wide = warned(&v, "ontology type `move`: nothing fills");
    assert_eq!(wide.len(), 1, "{:#?}", v.warnings);
    assert!(
        wide[0].contains("`grade` (closed with more values") && !wide[0].contains("`holder`"),
        "{}",
        wide[0]
    );
    assert!(warned(&v, "ontology type `item`: nothing fills").is_empty());
    // No read claim kind asks a type no claim kind is about, nor a sourced
    // type's non-identity field.
    let lot = warned(&v, "ontology type `lot`: nothing fills");
    assert!(
        lot.len() == 1 && lot[0].contains("`lot_code`") && lot[0].contains("`tag`"),
        "{lot:#?}"
    );
    let party = warned(&v, "ontology type `party`: nothing fills");
    assert!(party.len() == 1 && party[0].contains("`tier`") && !party[0].contains("`domain`"));
}

#[test]
fn a_protocol_keyed_on_an_unfilled_field_is_warned() {
    // Point fills `move_id` and `at`: the protocol keyed on them is silent.
    let v = validated(&fixture());
    assert!(
        warned(&v, "derived fold `item_status`").is_empty(),
        "{:#?}",
        v.warnings
    );
    // Keyed on a field nothing fills, it is warned.
    let wide = fixture().replace("identity = \"move_id\"", "identity = \"grade\"");
    let v = validated(&wide);
    let identity = warned(&v, "protocol.identity `move.grade`");
    assert_eq!(identity.len(), 1, "{:#?}", v.warnings);
    assert!(
        identity[0].contains("`item.status` never folds"),
        "{}",
        identity[0]
    );
}

#[test]
fn an_identity_rule_keyed_on_an_unfilled_field_is_warned() {
    let v = validated(&fixture());
    let id = warned(&v, "ontology type `lot`: identity is keyed on");
    assert_eq!(id.len(), 1, "{:#?}", v.warnings);
    assert!(id[0].contains("`lot_code` (identity)"), "{}", id[0]);
    // Point fills `item.code`; the sourced party's key is read by its source.
    assert!(warned(&v, "ontology type `item`: identity").is_empty());
    assert!(warned(&v, "ontology type `party`: identity").is_empty());
}

#[test]
fn a_derived_step_through_an_unfilled_ref_is_warned() {
    let v = validated(&fixture());
    let step = warned(&v, "derived `lot_tag` steps through `tag`");
    assert_eq!(step.len(), 1, "{:#?}", v.warnings);
    assert!(step[0].contains("reaches nothing"), "{}", step[0]);
    // `holder` is Picked: the fold stepping through it is not warned.
    assert!(warned(&v, "derived `owner_of_item`").is_empty());
}
