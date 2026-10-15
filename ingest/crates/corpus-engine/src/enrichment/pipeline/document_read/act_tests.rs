use serde_json::{json, Value};

use super::super::passes::{read, read_fields};
use super::super::tests::{input_chapter, row, scripted};
use super::super::{DocumentReadOutcome, DocumentReadStatus as Status};
use super::*;
use crate::enrichment::atlas::resolve_records::compose_wave;
use crate::enrichment::ontology::OntologyV1;
use crate::enrichment::pipeline::document_read::line_classes::LineClasses;
use std::ops::Range;

/// The passes test declaration with `reported_status` a protocol's move: its
/// `status` qualifies the rules of `case_status_now`. `with_membership` adds
/// a kind no rule names, which Locate still asks line by line.
fn act_policies(with_membership: bool) -> OntologyPolicies {
    let membership = if with_membership {
        "[[types]]\nname = \"membership\"\nkind = \"claim\"\nforce = \"assertive\"\nsubject = \"case\"\n"
    } else {
        ""
    };
    let toml = format!(
        r#"
[[types]]
name = "case"
kind = "entity"
description = "A support case."
identity_criterion = "same issue number"
identity = ["number"]
identity_necessary = ["project"]

[[types.attributes]]
name = "number"
type = "text"

[[types.attributes]]
name = "project"
type = "text"
values = ["uv", "pip"]

[[types.attributes]]
name = "status_now"
type = "text"
values = ["open", "closed"]
derived = "case_status_now"

{membership}
[[types]]
name = "reported_status"
kind = "claim"
force = "assertive"
subject = "case"

[[types.attributes]]
name = "status"
type = "text"
values = ["open", "closed"]
description = "open: still being worked on; closed: done"

[[folds]]
id = "case_status_now"
by = "protocol"
from = ["^subject"]

[folds.protocol]
identity = "status"

[[folds.protocol.rules]]
id = "opened"
claim_kind = "reported_status"
state = "open"
when = {{ status = "open" }}

[[folds.protocol.rules]]
id = "closed"
claim_kind = "reported_status"
state = "closed"
when = {{ status = "closed" }}
"#
    );
    toml::from_str::<OntologyV1>(&toml).unwrap().into_policies()
}

fn about_none() -> Value {
    json!({"A": 0.05, "B": 0.05, "0": 0.9})
}

fn status(open: f64, closed: f64) -> Value {
    json!({"A": open, "B": closed, "0": 1.0 - open - closed})
}

#[test]
fn an_act_is_a_kind_a_protocol_rule_names_about_a_type_resolve_decides() {
    let p = act_policies(true);
    let plan = Plan::of(&p);
    let keys: Vec<(&str, &[String])> = plan
        .kinds
        .iter()
        .map(|k| (k.decl.name.as_str(), k.keys.as_slice()))
        .collect();
    assert_eq!(
        keys,
        [
            ("membership", &[][..]),
            ("reported_status", &["status".to_string()][..])
        ]
    );
    assert_eq!(
        plan.located(),
        [0],
        "Locate asks only the kind no rule names"
    );
    let read = read_fields(&p).unwrap();
    assert!(
        read.contains(&(
            "reported_status".into(),
            "reported_status".into(),
            "status".into(),
            "Act"
        )),
        "{read:?}"
    );
    // The same rules over a subject whose identity a source reads: RESOLVE
    // decides nothing of it, so it is located line by line as before.
    let mut sourced = p.clone();
    let case = sourced
        .shape
        .types
        .iter_mut()
        .find(|t| t.name == "case")
        .unwrap();
    case.source = Some(toml::from_str("metadata = [\"author\"]").unwrap());
    let plan = Plan::of(&sourced);
    assert!(plan.kinds.iter().all(|k| k.keys.is_empty()));
}

#[test]
fn two_wordings_agree_hold_or_say_neither() {
    let key = ClosedAttr {
        name: "status".into(),
        description: String::new(),
        values: vec!["open".into(), "closed".into()],
    };
    let labels = ["A", "B", NONE];
    let dist = |v: Value| -> Result<BTreeMap<String, f64>, String> {
        Ok(serde_json::from_value(v).unwrap())
    };
    assert_eq!(
        reading(
            &key,
            &labels,
            &[dist(status(0.1, 0.8)), dist(status(0.2, 0.6))]
        ),
        Reading::Agreed("closed".into())
    );
    assert_eq!(
        reading(
            &key,
            &labels,
            &[dist(status(0.1, 0.1)), dist(status(0.0, 0.2))]
        ),
        Reading::Neither
    );
    let differ = reading(
        &key,
        &labels,
        &[dist(status(0.1, 0.8)), dist(status(0.1, 0.2))],
    );
    assert!(
        matches!(&differ, Reading::Held(why) if why.contains("closed 0.80") && why.contains("none 0.70")),
        "{differ:?}"
    );
    let refused = reading(&key, &labels, &[dist(status(0.1, 0.8)), Err("cut".into())]);
    assert!(matches!(&refused, Reading::Held(why) if why.starts_with("wording 2 refused")));
}

#[test]
fn the_act_question_names_the_declared_type_and_field_and_both_wordings_make_one_wave() {
    let p = act_policies(false);
    let plan = Plan::of(&p);
    let kind = &plan.kinds[0];
    let FieldPlan::Choose(key) = &kind.fields[0] else {
        panic!("status is a closed field");
    };
    let labels = ["A", "B", NONE];
    let w1 = act_question(("facts\n\n", "view"), kind.subject, key, None, 0, &labels);
    let w2 = act_question(("facts\n\n", "view"), kind.subject, key, None, 1, &labels);
    assert!(w1
        .system
        .starts_with("You read a document and say where a case it is about stands"));
    assert!(w1.system.ends_with("or 0 if it shows no case's status."));
    assert_eq!(w1.system, w2.system);
    assert!(w1.user.ends_with(
        "Kind of thing: case (A support case.)\nAttribute: status (open: still being worked on; \
         closed: done)\n\nWhere does this document show the case it is about standing?\nA open\nB \
         closed\n0 it shows no case's status\nAnswer with its letter."
    ));
    assert!(w2
        .user
        .contains("According to this document, where does the case it concerns stand?"));
    assert!(compose_wave(&[w1, w2]).is_ok());
    let named = Particular {
        line: 4,
        span: 0..7,
        words: "#1401 crash".into(),
    };
    let one = act_question(("", "view"), kind.subject, key, Some(&named), 0, &labels);
    assert!(one
        .user
        .contains("show the case first named or described on line 4 (\"#1401 crash\") standing?"));
}

#[test]
fn sentence_units_join_a_paragraphs_lines_and_split_after_a_stop_before_whitespace() {
    let body =
        "Dear Rosa,\n\nOur price is 2,400\ndollars. The offer of 2.5 weeks stands.\n  Thanks!";
    let lines = super::super::passes::lines(body);
    let found = units(body, &lines, &[true; 4]);
    let got: Vec<(&str, Range<usize>)> = found
        .iter()
        .map(|u| (u.text.as_str(), u.lines.clone()))
        .collect();
    assert_eq!(
        got,
        [
            ("Dear Rosa,", 0..1),
            ("Our price is 2,400 dollars.", 1..3),
            ("The offer of 2.5 weeks stands.", 2..3),
            ("Thanks!", 3..4)
        ]
    );
    // A carried line is a paragraph of its own: no sentence takes it in.
    let body = "We agree.\n> You wrote this\nand more.";
    let lines = super::super::passes::lines(body);
    let found: Vec<Range<usize>> = units(body, &lines, &[true, false, true])
        .into_iter()
        .map(|u| u.lines)
        .collect();
    assert_eq!(found, [0..1, 1..2, 2..3]);
}

#[test]
fn a_citation_marks_its_candidates_and_offers_no_none() {
    let body = "Title\n\n> quoted\n\nIt is closed.";
    let units = units(body, &super::super::passes::lines(body), &[true; 3]);
    let key = ClosedAttr {
        name: "status".into(),
        description: String::new(),
        values: vec!["open".into(), "closed".into()],
    };
    let case = OntologyTypeDecl {
        name: "case".into(),
        ..Default::default()
    };
    let fact = fact(&case, &[(&key, "closed")], None);
    let q = cite_question("", &units, &[0, 2], &fact, &["A", "B"]);
    assert_eq!(
        q.user,
        "Document:\n<<<\n[A] Title\n> quoted\n[B] It is closed.\n>>>\n\nAttribute: status\nFact: the \
         case this document is about has status 'closed'.\n\nWhich marked sentence shows this fact? \
         Answer with its letter."
    );
    assert!(!q.system.contains(" 0 "), "{}", q.system);
    assert_eq!(q.phase_id.as_deref(), Some(CITE_PHASE));
}

async fn read_one(
    p: &OntologyPolicies,
    body: &str,
    answers: Vec<Value>,
) -> (DocumentReadOutcome, Vec<ChatPrompt>) {
    let chapter = input_chapter(&[row(1, "doc-1", body, "{}")]);
    let (infer, seen) = scripted(answers);
    let classes = LineClasses::of(&chapter.source_documents, p);
    let envelope = read(&chapter, p, &classes, &infer).await.unwrap();
    let mut extraction = super::super::parse_response(&envelope, p)
        .unwrap()
        .section_extraction
        .unwrap();
    super::super::validate_and_stamp(&chapter, p, &mut extraction).unwrap();
    let outcome = extraction.document_read.unwrap().documents.remove(0);
    let prompts = seen.lock().unwrap().clone();
    (outcome, prompts)
}

#[tokio::test]
async fn an_act_both_wordings_agree_on_is_cited_and_becomes_a_statement_validation_keeps() {
    let p = act_policies(false);
    let body = "Thread title\n\nThe case   is closed now.\nThanks";
    let (outcome, prompts) = read_one(
        &p,
        body,
        vec![
            about_none(),
            status(0.1, 0.8),
            status(0.2, 0.7),
            // The citation: the second of the three sentences.
            json!({"A": 0.1, "B": 0.8, "C": 0.1}),
            // `number` pointed at: none of the statement's five words.
            json!({"A": 0.02, "B": 0.02, "C": 0.02, "D": 0.02, "E": 0.02, "0": 0.9}),
            // `project`: none of the values.
            json!({"A": 0.2, "B": 0.1, "0": 0.7}),
        ],
    )
    .await;
    let phases: Vec<&str> = prompts
        .iter()
        .map(|p| p.phase_id.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(
        phases,
        [
            "document_passes_about",
            ACT_PHASE,
            ACT_PHASE,
            CITE_PHASE,
            "document_passes_point",
            "document_passes_choose"
        ],
        "every kind is an act: no line is located"
    );
    assert!(prompts[3]
        .user
        .contains("<<<\n[A] Thread title\n[B] The case   is closed now.\n[C] Thanks\n>>>"));
    assert!(prompts[3]
        .user
        .contains("Fact: the case this document is about has status 'closed'."));
    assert_eq!(outcome.status, Status::Read, "{outcome:?}");
    let claim = &outcome.claims[0];
    assert_eq!(
        (
            claim.kind.as_str(),
            claim.evidence.as_str(),
            claim.subject_local_ref.as_str()
        ),
        ("reported_status", "The case   is closed now.", "l2-2")
    );
    assert_eq!(
        claim.fields["status"],
        DocumentReadField::Supported {
            value: json!("closed"),
            evidence: "The case   is closed now.".into(),
            by: Some(SourcePrecision::new("reader_act", Precision::Unmeasured)),
        }
    );
}

#[tokio::test]
async fn wordings_that_differ_are_held_and_nothing_is_cited() {
    let p = act_policies(true);
    let body = "Thread title\nThe case is closed now.";
    let locate_none = json!({"A": 0.05, "0": 0.95});
    let (outcome, prompts) = read_one(
        &p,
        body,
        vec![
            locate_none.clone(),
            locate_none,
            about_none(),
            status(0.1, 0.8),
            status(0.1, 0.3),
        ],
    )
    .await;
    assert_eq!(prompts.len(), 5, "no citation is asked of a held reading");
    assert!(prompts[0].user.contains("A membership\n0 none of them"));
    assert!(
        !prompts[0].user.contains("reported_status"),
        "an act is never located"
    );
    assert_eq!(outcome.status, Status::NothingApplicable);
    assert!(outcome
        .reason
        .as_deref()
        .unwrap()
        .contains("no act was read"));
}

/// A document of one sentence touching an asked line is cited by code: the
/// act's presence was decided, and the citation only points.
#[tokio::test]
async fn a_single_sentence_is_cited_with_no_question() {
    let p = act_policies(false);
    let (outcome, prompts) = read_one(
        &p,
        "Closed by commit 12dd450.",
        vec![
            about_none(),
            status(0.1, 0.8),
            status(0.1, 0.8),
            json!({"A": 0.02, "B": 0.02, "C": 0.02, "D": 0.02, "0": 0.92}),
            json!({"A": 0.2, "B": 0.1, "0": 0.7}),
        ],
    )
    .await;
    assert!(
        prompts
            .iter()
            .all(|q| q.phase_id.as_deref() != Some(CITE_PHASE)),
        "a lone candidate is never asked"
    );
    assert_eq!(outcome.claims[0].evidence, "Closed by commit 12dd450.");
}
