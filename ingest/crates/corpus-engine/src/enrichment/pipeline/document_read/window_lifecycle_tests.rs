//! A record's lifecycle as the reader is shown it: the method's replaced and
//! act-under kinds beside the recipe's when it opts in
//! (`recipe_ontology/lifecycle.rs`).

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

/// Failing input, named: at 20k halwick-d1's rollover went unwritten and
/// world-s1's nominations were written as leads (2026-10-18). A recipe that
/// opts in shows the reader both kinds, the replaced one with the stage it ends
/// in; one that does not shows neither (lifecycle-prereg.md Result).
#[test]
fn an_opted_in_recipe_shows_the_reader_the_lifecycle_kinds() {
    let shown = |body: &str| {
        let p = OntologyBlock {
            version: 1,
            body: toml::from_str(body).unwrap(),
        }
        .policies()
        .unwrap();
        let contract = super::Contract::of(&p);
        let index = TypeIndex::from_policies(&p);
        super::ontology_text(&contract, &p, &index)
    };
    let opted = shown(&DEALS.replace(
        "identity = \"stage\"\n",
        "identity = \"stage\"\nlifecycle = true\n",
    ));
    assert!(
        opted.contains("- deal_replaced (a statement about one deal): "),
        "{opted}"
    );
    assert!(
        opted.contains("- deal_act_under (a statement about one deal): "),
        "{opted}"
    );
    let replaced = opted.split("- deal_replaced").nth(1).unwrap();
    assert!(
        replaced.lines().nth(1).unwrap().contains("* stage (text)"),
        "{opted}"
    );
    let plain = shown(DEALS);
    assert!(
        !plain.contains("deal_replaced") && !plain.contains("deal_act_under"),
        "{plain}"
    );
}
