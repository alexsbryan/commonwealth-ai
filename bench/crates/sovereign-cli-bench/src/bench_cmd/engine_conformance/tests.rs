// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every check is shown a passing pair and a failing one, so none of them is
//! a check nobody has watched fail.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::json;

use super::check::{CellVerdict, Check, FailCause};
use super::inventory::{Inventory, Prediction, Row};
use super::record::{CaseRecord, Facets, Outcome, Output, Sampler, ToolCall, Usage};
use super::verdict;

fn rec(target: &str, facets: Facets) -> CaseRecord {
    CaseRecord {
        case_id: "c1".into(),
        target: target.into(),
        rows: vec!["r".into()],
        request: json!({}),
        outcome: Outcome::Ok,
        facets,
    }
}

fn output(text: &str) -> Output {
    Output {
        text: text.into(),
        ..Default::default()
    }
}

fn is_differs(v: &CellVerdict) -> bool {
    matches!(
        v,
        CellVerdict::Failed {
            cause: FailCause::Differs,
            ..
        }
    )
}

/// Judge `check` on a pair built from two facet sets.
fn pair(check: &Check, a: Facets, b: Facets) -> CellVerdict {
    check.judge(Some(&rec("embedded", a)), &rec("llama-server", b))
}

#[test]
fn the_committed_inventory_parses_with_unique_row_ids() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lanes/engine-swap/conformance.toml");
    let inventory = Inventory::load(&path).unwrap();
    let mut ids: Vec<&str> = inventory.rows.iter().map(|r| r.id.as_str()).collect();
    ids.sort_unstable();
    let before = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), before, "a row id appears twice");
}

#[test]
fn a_row_that_names_no_check_is_refused_at_load() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inv.toml");
    std::fs::write(
        &path,
        "[[row]]\nid = \"x\"\njudge = []\npredict = \"pass\"\n",
    )
    .unwrap();
    assert!(Inventory::load(&path)
        .unwrap_err()
        .contains("names no check"));
}

#[test]
fn prompt_ids_pass_when_equal_and_fail_at_the_first_divergence() {
    let f = |ids: Vec<i64>| Facets {
        prompt_ids: Some(ids),
        ..Default::default()
    };
    assert_eq!(
        pair(&Check::PromptIds, f(vec![1, 2, 3]), f(vec![1, 2, 3])),
        CellVerdict::Passed
    );
    let v = pair(&Check::PromptIds, f(vec![1, 2, 3]), f(vec![1, 9, 3]));
    assert!(is_differs(&v));
    assert!(format!("{v:?}").contains("diverge at 1"));
}

#[test]
fn a_facet_missing_on_either_side_cannot_be_judged() {
    let f = Facets {
        prompt_ids: Some(vec![1]),
        ..Default::default()
    };
    let v = pair(&Check::PromptIds, f, Facets::default());
    assert!(matches!(v, CellVerdict::CouldNotJudge(why) if why.contains("llama-server")));
}

#[test]
fn sampler_compares_values_and_order_unless_told_not_to() {
    let s = |temp: f64, order: &[&str]| Facets {
        sampler: Some(Sampler {
            params: BTreeMap::from([("temperature".to_string(), temp)]),
            order: order.iter().map(|s| s.to_string()).collect(),
        }),
        ..Default::default()
    };
    let strict = Check::Sampler { order: true };
    let loose = Check::Sampler { order: false };
    assert_eq!(
        pair(
            &strict,
            s(0.7, &["top_k", "temp"]),
            s(0.7, &["top_k", "temp"])
        ),
        CellVerdict::Passed
    );
    assert!(is_differs(&pair(
        &strict,
        s(0.7, &["top_k"]),
        s(0.71, &["top_k"])
    )));
    assert!(is_differs(&pair(
        &strict,
        s(0.7, &["top_k", "temp"]),
        s(0.7, &["temp", "top_k"])
    )));
    assert_eq!(
        pair(
            &loose,
            s(0.7, &["top_k", "temp"]),
            s(0.7, &["temp", "top_k"])
        ),
        CellVerdict::Passed
    );
    let mut extra = s(0.7, &[]);
    extra
        .sampler
        .as_mut()
        .unwrap()
        .params
        .insert("dry_multiplier".into(), 0.8);
    assert!(
        is_differs(&pair(&loose, s(0.7, &[]), extra)),
        "a parameter only one side sets differs"
    );
}

#[test]
fn grammar_ignores_the_llguidance_header_and_flags_a_dropped_grammar() {
    let g = |text: Option<&str>| Facets {
        grammar: text.map(str::to_string),
        ..Default::default()
    };
    assert_eq!(
        pair(
            &Check::Grammar,
            g(Some("start: \"a\"")),
            g(Some("%llguidance {}\nstart: \"a\""))
        ),
        CellVerdict::Passed
    );
    assert!(is_differs(&pair(
        &Check::Grammar,
        g(Some("start: \"a\"")),
        g(Some("start: \"b\""))
    )));
    assert!(is_differs(&pair(
        &Check::Grammar,
        g(Some("start: \"a\"")),
        g(None)
    )));
    assert_eq!(pair(&Check::Grammar, g(None), g(None)), CellVerdict::Passed);
}

#[test]
fn logprobs_need_the_same_tokens_and_values_within_tolerance() {
    let l = |p: f64| Facets {
        logprobs: Some(vec![vec![(5, -0.1), (7, p)]]),
        ..Default::default()
    };
    let exact = Check::Logprobs { tol: 0.0 };
    assert_eq!(pair(&exact, l(-2.0), l(-2.0)), CellVerdict::Passed);
    assert!(is_differs(&pair(&exact, l(-2.0), l(-2.0 + 1e-12))));
    let swapped = Facets {
        logprobs: Some(vec![vec![(7, -2.0), (5, -0.1)]]),
        ..Default::default()
    };
    assert!(is_differs(&pair(
        &Check::Logprobs { tol: 1.0 },
        l(-2.0),
        swapped
    )));
}

#[test]
fn label_probs_embedding_rerank_and_counts() {
    let lp = |a: f64| Facets {
        label_probs: Some(BTreeMap::from([
            ("A".to_string(), a),
            ("B".to_string(), 1.0 - a),
        ])),
        ..Default::default()
    };
    let lpc = Check::LabelProbs { tol: 1e-3 };
    assert_eq!(pair(&lpc, lp(0.9), lp(0.9005)), CellVerdict::Passed);
    assert!(is_differs(&pair(&lpc, lp(0.9), lp(0.8))));
    let only_a = Facets {
        label_probs: Some(BTreeMap::from([("A".to_string(), 0.9)])),
        ..Default::default()
    };
    assert!(
        is_differs(&pair(&lpc, lp(0.9), only_a)),
        "a label missing on one side is not a zero"
    );

    let e = |v: Vec<f32>| Facets {
        embedding: Some(v),
        ..Default::default()
    };
    let ec = Check::Embedding { min_cos: 0.9995 };
    assert_eq!(
        pair(&ec, e(vec![1.0, 0.0]), e(vec![2.0, 0.0])),
        CellVerdict::Passed
    );
    assert!(is_differs(&pair(&ec, e(vec![1.0, 0.0]), e(vec![1.0, 0.1]))));

    let r = |v: Vec<f32>| Facets {
        rerank_scores: Some(v),
        ..Default::default()
    };
    assert_eq!(
        pair(
            &Check::RerankOrder,
            r(vec![3.0, 1.0, 2.0]),
            r(vec![0.9, 0.1, 0.5])
        ),
        CellVerdict::Passed
    );
    assert!(is_differs(&pair(
        &Check::RerankOrder,
        r(vec![3.0, 1.0, 2.0]),
        r(vec![0.9, 0.5, 0.1])
    )));

    let c = |n: u64| Facets {
        token_count: Some(n),
        ..Default::default()
    };
    assert_eq!(pair(&Check::TokenCount, c(10), c(10)), CellVerdict::Passed);
    assert!(is_differs(&pair(&Check::TokenCount, c(10), c(13))));
}

#[test]
fn projection_checks_compare_what_the_caller_receives() {
    let o = |out: Output| Facets {
        output: Some(out),
        ..Default::default()
    };
    let mut thinking = output("answer");
    thinking.reasoning = Some("because".into());
    assert_eq!(
        pair(&Check::Text, o(thinking.clone()), o(thinking.clone())),
        CellVerdict::Passed
    );
    assert!(is_differs(&pair(
        &Check::Text,
        o(thinking.clone()),
        o(output("answer"))
    )));

    let mut call = output("");
    call.tool_calls = vec![ToolCall {
        name: "run".into(),
        arguments: json!({"cmd": "ls"}),
    }];
    assert_eq!(
        pair(&Check::ToolCalls, o(call.clone()), o(call.clone())),
        CellVerdict::Passed
    );
    assert!(is_differs(&pair(&Check::ToolCalls, o(call), o(output("")))));

    let mut usage = output("x");
    usage.usage = Some(Usage {
        prompt: 10,
        completion: 2,
    });
    usage.finish = Some("stop".into());
    assert_eq!(
        pair(&Check::Usage, o(usage.clone()), o(usage.clone())),
        CellVerdict::Passed
    );
    assert!(is_differs(&pair(
        &Check::Usage,
        o(usage.clone()),
        o(output("x"))
    )));
    assert!(is_differs(&pair(&Check::Finish, o(usage), o(output("x")))));

    let frames = |kinds: &[&str]| {
        let mut out = output("x");
        out.frames = kinds.iter().map(|s| s.to_string()).collect();
        o(out)
    };
    let usage_frames = Check::StreamFrames {
        frames: vec!["usage".into()],
    };
    assert_eq!(
        pair(
            &usage_frames,
            frames(&["token", "usage"]),
            frames(&["usage", "token"])
        ),
        CellVerdict::Passed
    );
    assert!(is_differs(&pair(
        &usage_frames,
        frames(&["token", "usage"]),
        frames(&["token"])
    )));
    assert_eq!(
        pair(&usage_frames, frames(&["token"]), frames(&["token"])),
        CellVerdict::Passed
    );
}

#[test]
fn a_projection_is_not_blamed_for_an_upstream_difference() {
    let f = |ids: Vec<i64>, text: &str| Facets {
        prompt_ids: Some(ids),
        output: Some(output(text)),
        ..Default::default()
    };
    assert!(is_differs(&pair(
        &Check::Text,
        f(vec![1, 2], "a"),
        f(vec![1, 2], "b")
    )));
    assert!(matches!(
        pair(&Check::Text, f(vec![1, 2], "a"), f(vec![1, 3], "b")),
        CellVerdict::CouldNotJudge(why) if why.contains("prompts differ")
    ));
    let frames = Check::StreamFrames {
        frames: vec!["usage".into()],
    };
    let mut with_usage = f(vec![1, 2], "a");
    with_usage.output.as_mut().unwrap().frames = vec!["usage".into()];
    assert!(
        is_differs(&pair(&frames, with_usage, f(vec![9], "a"))),
        "whether a frame arrives does not depend on the prompt"
    );
}

#[test]
fn cost_checks_allow_their_slack_and_no_more() {
    let p = |n: u64| Facets {
        prefill_evaluated: Some(n),
        ..Default::default()
    };
    let pc = Check::Prefill { slack: 16 };
    assert_eq!(pair(&pc, p(100), p(116)), CellVerdict::Passed);
    assert!(is_differs(&pair(&pc, p(100), p(117))));

    let t = |x: f64| Facets {
        tokens_per_s: Some(x),
        ..Default::default()
    };
    let tc = Check::Throughput { ratio: 0.9 };
    assert_eq!(pair(&tc, t(100.0), t(90.0)), CellVerdict::Passed);
    assert!(is_differs(&pair(&tc, t(100.0), t(89.0))));

    let b = |x: u64| Facets {
        resident_bytes: Some(x),
        ..Default::default()
    };
    let bc = Check::ResidentBytes { ratio: 1.05 };
    assert_eq!(pair(&bc, b(100), b(105)), CellVerdict::Passed);
    assert!(is_differs(&pair(&bc, b(100), b(106))));
}

#[test]
fn allowlist_held_reads_the_request_and_the_target_output_only() {
    let mut t = rec(
        "llama-server",
        Facets {
            output: Some(output("see https://a.org/x and [ev-T1-0001].")),
            ..Default::default()
        },
    );
    t.request =
        json!({"url_allowlist": ["https://a.org/x"], "evidence_id_allowlist": ["ev-T1-0001"]});
    assert_eq!(Check::AllowlistHeld.judge(None, &t), CellVerdict::Passed);
    t.request =
        json!({"url_allowlist": ["https://b.org/"], "evidence_id_allowlist": ["ev-T1-0001"]});
    let v = Check::AllowlistHeld.judge(None, &t);
    assert!(is_differs(&v) && format!("{v:?}").contains("https://a.org/x"));
}

#[test]
fn reasoning_within_reads_the_budget_from_the_request() {
    let mut t = rec(
        "llama-server",
        Facets {
            reasoning_tokens: Some(514),
            ..Default::default()
        },
    );
    t.request = json!({"think_budget": 512});
    assert_eq!(
        Check::ReasoningWithin { slack: 2 }.judge(None, &t),
        CellVerdict::Passed
    );
    assert!(is_differs(
        &Check::ReasoningWithin { slack: 0 }.judge(None, &t)
    ));
    t.request = json!({});
    assert!(matches!(
        Check::ReasoningWithin { slack: 0 }.judge(None, &t),
        CellVerdict::CouldNotJudge(_)
    ));
}

#[test]
fn host_checks_compare_reports_against_the_truth_or_the_reference() {
    let mut t = rec("llama-server", Facets::default());
    t.facets
        .host_reported
        .insert("effective_context_size".into(), json!(65536));
    t.facets
        .host_observed
        .insert("effective_context_size".into(), json!(16384));
    let truth = Check::HostTruth {
        keys: vec!["effective_context_size".into()],
    };
    assert!(is_differs(&truth.judge(None, &t)));
    t.facets
        .host_observed
        .insert("effective_context_size".into(), json!(65536));
    assert_eq!(truth.judge(None, &t), CellVerdict::Passed);
    let unknown = Check::HostTruth {
        keys: vec!["n_ctx_train".into()],
    };
    assert!(matches!(
        unknown.judge(None, &t),
        CellVerdict::CouldNotJudge(_)
    ));

    let mut r = rec("embedded", Facets::default());
    r.facets
        .host_reported
        .insert("serving_locus".into(), json!("own-weights"));
    t.facets
        .host_reported
        .insert("serving_locus".into(), json!("forwards-on-box"));
    let equal = Check::HostEqual {
        keys: vec!["serving_locus".into()],
    };
    assert!(is_differs(&equal.judge(Some(&r), &t)));
}

#[test]
fn outcomes_decide_before_any_facet_is_read() {
    let r = rec("embedded", Facets::default());
    let mut t = rec("llama-server", Facets::default());
    t.outcome = Outcome::Refused {
        message: "FIM unavailable".into(),
    };
    assert!(matches!(
        Check::PromptIds.judge(Some(&r), &t),
        CellVerdict::Failed {
            cause: FailCause::Refused,
            ..
        }
    ));
    let mut r2 = r.clone();
    r2.outcome = Outcome::Refused {
        message: "too long".into(),
    };
    assert_eq!(
        Check::PromptIds.judge(Some(&r2), &t),
        CellVerdict::Passed,
        "both refused"
    );
    t.outcome = Outcome::Ok;
    assert!(
        is_differs(&Check::PromptIds.judge(Some(&r2), &t)),
        "served where the reference refused"
    );
    assert!(matches!(
        Check::Scenario { what: "x".into() }.judge(Some(&r), &t),
        CellVerdict::CouldNotJudge(_)
    ));
}

fn row(id: &str, judge: Vec<Check>, predict: Prediction) -> Row {
    Row {
        id: id.into(),
        judge,
        pairs: vec![],
        predict,
    }
}

fn case(id: &str, target: &str, row: &str, count: Option<u64>) -> CaseRecord {
    CaseRecord {
        case_id: id.into(),
        target: target.into(),
        rows: vec![row.into()],
        request: json!({}),
        outcome: Outcome::Ok,
        facets: Facets {
            token_count: count,
            ..Default::default()
        },
    }
}

#[test]
fn roll_up_fails_on_any_case_and_sets_the_verdict_beside_its_prediction() {
    let inventory = Inventory {
        rows: vec![
            row("same", vec![Check::TokenCount], Prediction::Pass),
            row("one-off", vec![Check::TokenCount], Prediction::Silent),
            row(
                "wrongly-predicted",
                vec![Check::TokenCount],
                Prediction::Pass,
            ),
            row("half-seen", vec![Check::TokenCount], Prediction::Pass),
            row("no-cases", vec![Check::TokenCount], Prediction::Pass),
        ],
    };
    let records = vec![
        case("a", "embedded", "same", Some(5)),
        case("a", "llama-server", "same", Some(5)),
        case("b", "embedded", "one-off", Some(5)),
        case("b", "llama-server", "one-off", Some(5)),
        case("c", "embedded", "one-off", Some(5)),
        case("c", "llama-server", "one-off", Some(6)),
        case("d", "embedded", "wrongly-predicted", Some(5)),
        case("d", "llama-server", "wrongly-predicted", Some(6)),
        case("e", "embedded", "half-seen", Some(5)),
        case("e", "llama-server", "half-seen", Some(5)),
        case("f", "embedded", "half-seen", Some(5)),
        case("f", "llama-server", "half-seen", None),
    ];
    let verdicts = verdict::judge(&inventory, &records);
    let get = |id: &str| verdicts.iter().find(|v| v.row == id).unwrap();
    assert_eq!(
        (get("same").verdict, get("same").as_predicted),
        ("passed", Some(true))
    );
    let one_off = get("one-off");
    assert_eq!(
        (
            one_off.verdict,
            one_off.cause,
            one_off.failed,
            one_off.cases
        ),
        ("failed", Some("differs"), 1, 2)
    );
    assert_eq!(
        one_off.as_predicted,
        Some(true),
        "a silent loss is judged as a difference"
    );
    assert_eq!(get("wrongly-predicted").as_predicted, Some(false));
    assert_eq!(
        (get("half-seen").verdict, get("half-seen").as_predicted),
        ("could-not-judge", None)
    );
    assert_eq!(get("no-cases").verdict, "never-ran");
}

#[test]
fn a_row_with_pairs_compares_only_those_targets() {
    let mut r = row("spec", vec![Check::TokenCount], Prediction::Pass);
    r.pairs = vec![("embedded-nospec".into(), "embedded".into())];
    let inventory = Inventory { rows: vec![r] };
    let records = vec![
        case("a", "embedded-nospec", "spec", Some(5)),
        case("a", "embedded", "spec", Some(5)),
        case("a", "llama-server", "spec", Some(9)),
    ];
    let verdicts = verdict::judge(&inventory, &records);
    assert_eq!(verdicts.len(), 1);
    assert_eq!(
        (verdicts[0].reference.as_str(), verdicts[0].verdict),
        ("embedded-nospec", "passed")
    );
}
