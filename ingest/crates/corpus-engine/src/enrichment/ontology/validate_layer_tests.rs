// SPDX-License-Identifier: AGPL-3.0-or-later
//! `recipe validate` on the ontology layer, one test each, over one
//! domain-free declaration: a sourced `party`, an `item` RESOLVE decides, and
//! a `move` claim about it with a protocol, identity rules and a derived path.

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

/// `by` folds a read field onto its record; a `by` on a type RESOLVE does not
/// decide, on a derived field, or by `first` is refused, each with why.
#[test]
fn a_folded_read_field_is_accepted_and_a_misplaced_by_refuses() {
    let code = r#"{ name = "code", type = "text" },"#;
    let v =
        validated(&fixture().replace(code, r#"{ name = "code", type = "text", by = "agree" },"#));
    assert!(
        !v.errors.iter().any(|e| e.contains("`by")),
        "{:#?}",
        v.errors
    );
    let status = r#"derived = "item_status" },"#;
    for (from, to, says) in [
        (
            r#"{ name = "domain", type = "text" },"#,
            r#"{ name = "domain", type = "text", by = "all" },"#,
            "RESOLVE does not decide `party`",
        ),
        (
            status,
            r#"derived = "item_status", by = "agree" },"#,
            "never both",
        ),
        (
            code,
            r#"{ name = "code", type = "text", by = "first" },"#,
            "agree, all, most",
        ),
    ] {
        let v = validated(&fixture().replace(from, to));
        assert!(
            v.errors.iter().any(|e| e.contains(says)),
            "{says}: {:#?}",
            v.errors
        );
    }
}

/// A protocol's transitions: `by` names a declared path (who may move),
/// `from` and `initial` name states of the derived attribute's closed set,
/// and a document field may be walked backwards (`^from`: the documents a
/// party wrote). Each misnamed one refuses on its own.
#[test]
fn a_transition_names_its_mover_by_path_and_its_states_from_the_closed_set() {
    let when = "when = { step = \"open\" }\n";
    let machine = |by: &str, from: &str, initial: &str| {
        fixture()
            .replace(
                "effective_time = \"at\"\n",
                &format!("effective_time = \"at\"\ninitial = \"{initial}\"\n"),
            )
            .replace(when, &format!("{when}by = \"{by}\"\nfrom = [\"{from}\"]\n"))
            .replace(
                "path = \"batch / tag\"\n",
                "path = \"batch / tag\"\n\n[[enrichment.ontology.paths]]\nid = \"wrote\"\npath = \"document / from / ^from\"\n",
            )
    };
    let v = validated(&machine("wrote", "shut", "open"));
    assert!(
        !v.errors
            .iter()
            .any(|e| e.contains("protocol") || e.contains("backwards")),
        "{:#?}",
        v.errors
    );
    for (recipe, says) in [
        (
            machine("owner_of_item", "shut", "open"),
            "names no declared path",
        ),
        (machine("wrote", "ajar", "open"), "moves from `ajar`"),
        (
            machine("wrote", "shut", "ajar"),
            "`protocol.initial` `ajar`",
        ),
    ] {
        let v = validated(&recipe);
        assert!(
            v.errors.iter().any(|e| e.contains(says)),
            "{says}: {:#?}",
            v.errors
        );
    }
}

/// A declaration the document reader reads makes no record of a type no
/// statement is about, no reference names and no source supplies: `bin` is
/// named. `lot`, which `move` references, is decided by name since
/// 2026-10-20 (`resolve_defaults`), so it is not. A state of `item` is read
/// since 2026-10-15 (its holder is decided), so it is not; nor are `item`,
/// `move` and the sourced `party`. The general extractor reads every kind, so
/// a template it reads warns of nothing (`recipe_templates_all_ten_parse`).
#[test]
fn a_type_the_document_reader_makes_no_record_of_is_warned() {
    let v = validated(&fixture().replace(
        "[[enrichment.ontology.types]]\nname = \"move\"",
        "[[enrichment.ontology.types]]\nname = \"mood\"\nkind = \"state\"\nof = \"item\"\n\n\
         [[enrichment.ontology.types]]\nname = \"bin\"\nkind = \"entity\"\n\n\
         [[enrichment.ontology.types]]\nname = \"move\"",
    ));
    let unread = warned(&v, "makes no record of");
    assert_eq!(unread.len(), 1, "{:#?}", v.warnings);
    assert!(unread[0].contains("`bin` (entity)"), "{}", unread[0]);
    for read in ["`lot`", "`mood`", "`item`", "`move`", "`party`"] {
        assert!(!unread[0].contains(read), "{read}: {}", unread[0]);
    }
}

/// What the document reader leaves inert is warned, each by name: argument
/// reconstruction, a pattern, a voice, a table source and a specializing type
/// with no criterion of its own. The general extractor applies the first three
/// and reads the last two from text, so a template it reads warns of none
/// (`recipe_templates_all_ten_parse`).
#[test]
fn what_the_document_reader_leaves_inert_is_warned_by_name() {
    let v = validated(&format!(
        "{}\n\
         [[enrichment.ontology.types]]\nname = \"near\"\nkind = \"relation\"\nfrom = \"item\"\nto = \"party\"\n\n\
         [[enrichment.ontology.types]]\nname = \"batch_row\"\nkind = \"entity\"\nsource = {{ file = \"batches.csv\" }}\n\n\
         [[enrichment.ontology.types]]\nname = \"gadget\"\nkind = \"entity\"\nspecializes = \"item\"\n\n\
         [[enrichment.ontology.patterns]]\ntype = \"circular_flow\"\nedge_types = [\"near\"]\nmin_entities = 3\n\n\
         [[enrichment.ontology.patterns]]\ntype = \"circular_flow\"\nname = \"loop\"\nedge_types = [\"near\", \"move\"]\nmin_entities = 3\n\n\
         [enrichment.ontology.derive]\narguments = true\n\n\
         [enrichment.ontology.voices]\nnot_entities = [\"the poster\"]\nattributed_to = [\"member\"]\n",
        fixture()
    ));
    let ours = ["near", "batch_row", "gadget", "pattern", "derive", "voices"];
    assert!(
        !v.errors.iter().any(|e| ours.iter().any(|o| e.contains(o))),
        "{:#?}",
        v.errors
    );
    for needle in [
        "`derive.arguments` reconstructs arguments",
        "pattern `loop` walks `move`, which the document reader makes no relation atom of",
        "`voices` (attributed_to)",
        "`batch_row` declares a table source",
        "`gadget` specializes `item` but declares no identity_criterion",
    ] {
        assert_eq!(warned(&v, needle).len(), 1, "{needle}: {:#?}", v.warnings);
    }
    assert!(
        warned(&v, "not_entities").is_empty(),
        "the reader refuses a declared voice as a subject: {:#?}",
        v.warnings
    );
    assert!(
        warned(&v, "`near`").is_empty(),
        "a relation of a decided holder is read, and a pattern over it walks its atoms: {:#?}",
        v.warnings
    );
    assert_eq!(warned(&v, "pattern `").len(), 1, "{:#?}", v.warnings);
}
