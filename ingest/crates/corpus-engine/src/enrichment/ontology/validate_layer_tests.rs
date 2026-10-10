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
name = "move"
kind = "claim"
force = "declaration"
subject = "item"
attributes = [
  { name = "step", type = "text", values = ["open", "shut"] },
  { name = "move_id", type = "text" },
  { name = "at", type = "time" },
  { name = "holder", type = "ref", of = "party" },
]

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

fn validated(recipe: &str) -> super::super::OntologyValidation {
    let recipe = Recipe::from_toml(recipe).expect("the fixture loads");
    validate_block(recipe.ontology_block().expect("an ontology block"))
}

fn warned<'a>(v: &'a super::super::OntologyValidation, needle: &str) -> Vec<&'a String> {
    v.warnings.iter().filter(|w| w.contains(needle)).collect()
}

#[test]
fn an_attribute_nothing_fills_is_warned_with_the_pass_that_would() {
    let v = validated(FIXTURE);
    let item = warned(&v, "ontology type `item`: nothing fills");
    assert_eq!(item.len(), 1, "{:#?}", v.warnings);
    assert!(
        item[0].contains("`code`") && item[0].contains("Point"),
        "{}",
        item[0]
    );
    // Choose asks `side` (the subject's closed field); `owner` and `status`
    // are derived: none is named.
    assert!(
        !item[0].contains("`side`") && !item[0].contains("`owner`"),
        "{}",
        item[0]
    );
    let mv = warned(&v, "ontology type `move`: nothing fills");
    assert_eq!(mv.len(), 1, "{:#?}", v.warnings);
    for f in ["`move_id`", "`at`", "`holder`"] {
        assert!(mv[0].contains(f), "{f} in {}", mv[0]);
    }
    assert!(
        mv[0].contains("Pick") && !mv[0].contains("`step`"),
        "{}",
        mv[0]
    );
    // A sourced type's closed non-identity field: no Choose asks it.
    let party = warned(&v, "ontology type `party`: nothing fills");
    assert!(party.len() == 1 && party[0].contains("`tier`") && !party[0].contains("`domain`"));
    // Every filler is named in the notes, and the subject's.
    let note = v
        .notes
        .iter()
        .find(|n| n.starts_with("fill: move:"))
        .expect("a fill note");
    assert!(
        note.contains("subject `item` ← RESOLVE") && note.contains("step ← Choose on move"),
        "{note}"
    );
}

#[test]
fn a_protocol_keyed_on_an_unfilled_field_is_warned() {
    let v = validated(FIXTURE);
    let identity = warned(&v, "protocol.identity `move.move_id`");
    assert_eq!(identity.len(), 1, "{:#?}", v.warnings);
    assert!(
        identity[0].contains("`item.status` never folds"),
        "{}",
        identity[0]
    );
    assert_eq!(warned(&v, "protocol.effective_time `move.at`").len(), 1);
    // `step` is closed and asked: the rule's qualification is not warned.
    assert!(warned(&v, "qualifies on").is_empty(), "{:#?}", v.warnings);
    // Keyed on the asked closed field instead, the fold is silent.
    let fixed = FIXTURE
        .replace("identity = \"move_id\"", "identity = \"step\"")
        .replace("effective_time = \"at\"\n", "");
    assert!(warned(&validated(&fixed), "derived fold `item_status`").is_empty());
}

#[test]
fn an_identity_rule_keyed_on_an_unfilled_field_is_warned() {
    let v = validated(FIXTURE);
    let id = warned(&v, "ontology type `item`: identity is keyed on");
    assert_eq!(id.len(), 1, "{:#?}", v.warnings);
    assert!(
        id[0].contains("`code` (identity)") && !id[0].contains("`side`"),
        "{}",
        id[0]
    );
    // The sourced party's identity key is read by its source.
    assert!(warned(&v, "ontology type `party`: identity").is_empty());
}

#[test]
fn a_derived_step_through_an_unfilled_ref_is_warned() {
    let v = validated(FIXTURE);
    let step = warned(&v, "derived `owner_of_item` steps through `holder`");
    assert_eq!(step.len(), 1, "{:#?}", v.warnings);
    assert!(step[0].contains("reaches nothing"), "{}", step[0]);
}
