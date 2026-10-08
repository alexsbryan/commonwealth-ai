// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::ontology::decl::{AttrDecl, AttrFamily};

fn step(name: &str, inverse: bool) -> PathExpr {
    PathExpr::Step {
        name: name.into(),
        inverse,
    }
}

#[test]
fn a_path_parses_to_steps_groups_and_filters() {
    let p = PathExpr::parse("^subject / document / (from | to | cc) / employer [!ours]").unwrap();
    assert_eq!(
        p,
        PathExpr::Seq(vec![
            step("subject", true),
            step("document", false),
            PathExpr::Alt(vec![
                step("from", false),
                step("to", false),
                step("cc", false)
            ]),
            PathExpr::Filter {
                inner: Box::new(step("employer", false)),
                set: "ours".into(),
                keep: false,
            },
        ])
    );
    assert_eq!(p.sets(), ["ours"]);
    assert_eq!(
        p.describe(),
        "the claims about it → its document → `from`, `to` or `cc` → `employer` not in `ours`"
    );
}

#[test]
fn an_unparenthesized_alternation_and_broken_paths_are_refused() {
    // SPARQL would read this as `(document / from) | to`: never accepted.
    let e = PathExpr::parse("document / from | to").unwrap_err();
    assert!(e.contains("inside parentheses"), "{e}");
    for (bad, says) in [
        ("", "empty"),
        ("document /", "ends where a step"),
        ("(from | to", "not closed"),
        ("employer [ours", "not closed"),
        ("^ (from | to)", "expected a name"),
        ("from . to", "not part of a path"),
    ] {
        let e = PathExpr::parse(bad).unwrap_err();
        assert!(e.contains(says), "{bad:?}: {e}");
    }
}

fn attr(name: &str, derived: Option<&str>) -> AttrDecl {
    AttrDecl {
        name: name.into(),
        family: AttrFamily::Ref {
            of: "company".into(),
        },
        description: String::new(),
        derived: derived.map(str::to_string),
    }
}

fn ty(name: &str, attributes: Vec<AttrDecl>) -> OntologyTypeDecl {
    OntologyTypeDecl {
        name: name.into(),
        attributes,
        ..Default::default()
    }
}

fn policy() -> DerivedPolicy {
    DerivedPolicy {
        paths: vec![PathDecl {
            id: "outside".into(),
            path: "document / (from | to) / employer [!ours]".into(),
        }],
        sets: vec![SetDecl {
            id: "ours".into(),
            of: "company".into(),
            conditions: [("domain".to_string(), Condition::Is("enron.com".into()))].into(),
        }],
        folds: vec![
            FoldDecl {
                id: "party_of_message".into(),
                by: FoldBy::First,
                from: vec!["outside".into()],
                protocol: None,
            },
            FoldDecl {
                id: "party_of_deal".into(),
                by: FoldBy::Most,
                from: vec!["^subject / party".into()],
                protocol: None,
            },
        ],
    }
}

#[test]
fn legacy_fold_wire_omits_the_optional_protocol_declaration() {
    let value = serde_json::to_value(policy()).unwrap();
    assert!(value["folds"][0].get("protocol").is_none());
}

#[test]
fn reads_follows_ids_and_sets_and_order_puts_what_is_read_first() {
    let d = policy();
    assert_eq!(
        d.reads("party_of_message").unwrap(),
        ["domain", "employer", "from", "to"]
            .map(String::from)
            .into()
    );
    assert_eq!(
        d.reads("party_of_deal").unwrap(),
        ["party"].map(String::from).into()
    );
    // Declared deal-first, ordered message-first: the deal reads `party`.
    let types = vec![
        ty("deal", vec![attr("counterparty", Some("party_of_deal"))]),
        ty(
            "stage_update",
            vec![attr("party", Some("party_of_message"))],
        ),
    ];
    let order = d.order(&types).unwrap();
    assert_eq!(
        order,
        [
            ("stage_update", "party", "party_of_message"),
            ("deal", "counterparty", "party_of_deal")
        ]
    );
}

#[test]
fn cycles_and_unknown_ids_are_refused() {
    let mut d = policy();
    d.folds[0].from = vec!["^subject / counterparty".into()];
    let types = vec![
        ty("deal", vec![attr("counterparty", Some("party_of_deal"))]),
        ty(
            "stage_update",
            vec![attr("party", Some("party_of_message"))],
        ),
    ];
    let e = d.order(&types).unwrap_err();
    assert!(
        e.contains("cycle") && e.contains("deal.counterparty"),
        "{e}"
    );

    let mut d = policy();
    d.paths.push(PathDecl {
        id: "loop".into(),
        path: "employer / loop".into(),
    });
    assert!(d.reads("loop").unwrap_err().contains("loop → loop"));
    assert!(d
        .reads("nothing")
        .unwrap_err()
        .contains("no declared path or fold"));
    assert!(d.reads("ours").unwrap_err().contains("is a set"));
}
