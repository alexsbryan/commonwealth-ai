use std::collections::BTreeMap;
use std::sync::Arc;

use super::super::propose::SimilarDocuments;
use super::*;
use crate::InferenceFn;

struct Fixture {
    id: &'static str,
    date: Option<&'static str>,
    thread: &'static str,
    body: &'static str,
    surface: &'static str,
}

/// Two documents share a date, one has none, threads cross dates.
const DOCS: [Fixture; 5] = [
    Fixture {
        id: "d1",
        date: Some("2024-01-01"),
        thread: "7",
        body: "Install fails on Windows.",
        surface: "Install fails",
    },
    Fixture {
        id: "d2",
        date: Some("2024-01-01"),
        thread: "9",
        body: "Lockfile drifts after upgrade.",
        surface: "Lockfile drifts",
    },
    Fixture {
        id: "d3",
        date: Some("2024-01-02"),
        thread: "7",
        body: "labeled bug",
        surface: "labeled bug",
    },
    Fixture {
        id: "d4",
        date: None,
        thread: "9",
        body: "Lockfile drifts again.",
        surface: "Lockfile drifts",
    },
    Fixture {
        id: "d5",
        date: Some("2024-01-03"),
        thread: "4",
        body: "Install fails on Windows, again.",
        surface: "Install fails",
    },
];

fn criterion() -> Criterion {
    Criterion {
        type_name: "case".into(),
        description: String::new(),
        same_when: Some("the same problem".into()),
        keys: vec![],
        evidential: vec![(DocumentStamp::Thread, 0.9)],
        bar: Some(0.5),
        // Below the bar: never asked, so no call is made in any order.
        model_choice: Some(0.3),
        reasoned_choice: None,
        proposed_answer: Some(0.8),
        necessary: vec![],
    }
}

/// Every record with its statements, and every statement's outcome label and
/// record, after resolving the fixtures in `order`.
async fn resolve(order: &[usize], answerer: Answerer<'_>) -> (String, BTreeMap<String, String>) {
    let stamps: Vec<Vec<(DocumentStamp, String)>> = DOCS
        .iter()
        .map(|f| {
            let mut s = vec![(DocumentStamp::Thread, f.thread.to_string())];
            if let Some(d) = f.date {
                s.push((DocumentStamp::Date, d.to_string()));
            }
            s
        })
        .collect();
    let statements: Vec<Vec<Statement>> = DOCS
        .iter()
        .map(|f| {
            let start = f.body.find(f.surface).unwrap();
            vec![Statement {
                id: format!("{}/s0", f.id),
                start,
                end: start + f.surface.len(),
                keys: Default::default(),
            }]
        })
        .collect();
    let mut documents: Vec<(Document<'_>, &[Statement])> = order
        .iter()
        .map(|&k| {
            (
                Document {
                    id: DOCS[k].id,
                    title: None,
                    body: DOCS[k].body,
                    stamps: &stamps[k],
                },
                statements[k].as_slice(),
            )
        })
        .collect();
    let mut resolver = Resolver::with_rule(super::super::ProposalRule::default());
    let mut proposers = Proposers::new(true, SimilarDocuments::new(3, 12, 0.1));
    let mut outcomes = BTreeMap::new();
    resolve_in_clock_order(
        &criterion(),
        &mut documents,
        &mut resolver,
        &mut proposers,
        answerer,
        &mut |_, _, r| {
            for o in &r.outcomes {
                outcomes.insert(
                    o.statement.clone(),
                    format!(
                        "{} {}",
                        o.outcome.label(),
                        o.outcome.record().unwrap_or("-")
                    ),
                );
            }
        },
    )
    .await;
    let records = serde_json::to_string(resolver.records()).unwrap();
    (records, outcomes)
}

/// Every permutation of 0..n.
fn permutations(n: usize) -> Vec<Vec<usize>> {
    if n == 0 {
        return vec![vec![]];
    }
    let mut out = Vec::new();
    for p in permutations(n - 1) {
        for at in 0..=p.len() {
            let mut q = p.clone();
            q.insert(at, n - 1);
            out.push(q);
        }
    }
    out
}

#[test]
fn the_clock_puts_dated_documents_first_and_breaks_ties_by_id() {
    let s = |d: Option<&str>| -> Vec<(DocumentStamp, String)> {
        d.map(|d| vec![(DocumentStamp::Date, d.to_string())])
            .unwrap_or_default()
    };
    let (a, b, c) = (s(Some("2024-01-01")), s(Some("2024-01-01")), s(None));
    let doc = |id, stamps| Document {
        id,
        title: None,
        body: "",
        stamps,
    };
    assert!(clock(&doc("a", &a)) < clock(&doc("b", &b)));
    assert!(clock(&doc("z", &a)) < clock(&doc("a", &c)));
}

#[tokio::test]
async fn any_document_order_gives_the_same_records_and_decisions() {
    // Any call refuses: none is scripted, and none should be made.
    let refuse: InferenceFn = Arc::new(|_, _| {
        Box::pin(async { Err(crate::Error::Extraction("no call expected".into())) })
    });
    for answerer in [Answerer::Select(&refuse), Answerer::Proposed] {
        let orders = permutations(DOCS.len());
        assert_eq!(orders.len(), 120);
        let first = resolve(&orders[0], answerer).await;
        // The run links something and is not refused, or equality is vacuous.
        assert!(
            first.1.values().any(|o| !o.starts_with("opened")),
            "{:?}",
            first.1
        );
        assert!(
            first.1.values().all(|o| !o.starts_with("refused")),
            "{:?}",
            first.1
        );
        for order in &orders[1..] {
            assert_eq!(resolve(order, answerer).await, first, "order {order:?}");
        }
    }
}
