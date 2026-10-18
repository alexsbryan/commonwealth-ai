//! A record's lifecycle as the reader is shown it: the method's replaced and
//! act-under kinds beside the recipe's (`recipe_ontology/lifecycle.rs`), and no
//! prose rule standing in for them.

use crate::enrichment::ontology::TypeIndex;
use crate::recipe_ontology::OntologyBlock;

const DEALS: &str = r#"
[[types]]
name = "deal"
kind = "entity"
identity_criterion = "the same transaction"
attributes = [{ name = "stage", type = "text", values = ["open", "won", "lost"], derived = "deal_stage" }]

[[types]]
name = "stage_update"
kind = "claim"
force = "assertive"
subject = "deal"
description = "where a deal stands"
attributes = [{ name = "stage", type = "text", values = ["open", "won", "lost"] }]

[[folds]]
id = "deal_stage"
by = "protocol"
from = ["^subject"]

[folds.protocol]
identity = "stage"

[[folds.protocol.rules]]
id = "won"
claim_kind = "stage_update"
state = "won"
when = { stage = "won" }
"#;

/// Failing input, named: the window prompt's supersession line ("Write the
/// first's ending even when nothing else in the window speaks of it") was
/// prose with no code behind it, and at 20k halwick-d1's rollover went
/// unwritten (2026-10-18). The reader is shown the two kinds instead, the
/// replaced one with the stage it reads; the line is gone.
#[test]
fn the_reader_is_shown_the_lifecycle_kinds_and_no_supersession_rule() {
    let p = OntologyBlock {
        version: 1,
        body: toml::from_str(DEALS).unwrap(),
    }
    .policies()
    .unwrap();
    let contract = super::Contract::of(&p);
    let index = TypeIndex::from_policies(&p);
    let shown = super::ontology_text(&contract, &p, &index);
    assert!(
        shown.contains("- deal_replaced (a statement about one deal): "),
        "{shown}"
    );
    assert!(
        shown.contains("- deal_act_under (a statement about one deal): "),
        "{shown}"
    );
    let replaced = shown.split("- deal_replaced").nth(1).unwrap();
    assert!(
        replaced.lines().nth(1).unwrap().contains("* stage (text)"),
        "{shown}"
    );
    assert!(!super::PROMPT.contains("rolled into"), "{}", super::PROMPT);
}
