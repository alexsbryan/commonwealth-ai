// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn q(id: &str, cat: &str) -> bank::Question {
    bank::Question {
        id: id.to_string(),
        category: cat.to_string(),
        question: format!("q-{id}"),
        expected_facts: Vec::new(),
        expected_sources: Vec::new(),
        notes: String::new(),
        expected_intent: None,
        attribution_mode: "both".to_string(),
    }
}

#[test]
fn sample_stratified_round_robins_categories() {
    // Uneven category sizes; N=4 must take one per category first
    // (appearance order) before doubling up — never 4-from-one-category.
    let qs = vec![
        q("a1", "alpha"),
        q("a2", "alpha"),
        q("a3", "alpha"),
        q("b1", "beta"),
        q("b2", "beta"),
        q("c1", "gamma"),
    ];
    let got = sample_stratified(qs, 4);
    let ids: Vec<&str> = got.iter().map(|q| q.id.as_str()).collect();
    assert_eq!(ids, vec!["a1", "b1", "c1", "a2"]);
    // All three archetypes represented in a 4-of-6 sample.
    let cats: std::collections::HashSet<&str> = got.iter().map(|q| q.category.as_str()).collect();
    assert_eq!(cats.len(), 3);
}

#[test]
fn sample_stratified_is_noop_at_bounds() {
    let mk = || vec![q("a", "x"), q("b", "y")];
    assert_eq!(sample_stratified(mk(), 0).len(), 2, "n=0 → unchanged");
    assert_eq!(sample_stratified(mk(), 5).len(), 2, "n>=len → unchanged");
    assert_eq!(sample_stratified(mk(), 2).len(), 2, "n==len → unchanged");
}

#[test]
fn sample_stratified_deterministic() {
    let mk = || vec![q("a1", "x"), q("b1", "y"), q("a2", "x"), q("b2", "y")];
    let one: Vec<String> = sample_stratified(mk(), 3)
        .iter()
        .map(|q| q.id.clone())
        .collect();
    let two: Vec<String> = sample_stratified(mk(), 3)
        .iter()
        .map(|q| q.id.clone())
        .collect();
    assert_eq!(one, two, "same bank + N must yield the same sample");
    assert_eq!(one, vec!["a1", "b1", "a2"]);
}

fn thr(id: &str, n_turns: usize) -> bank::Thread {
    bank::Thread {
        id: id.to_string(),
        category: "c".to_string(),
        description: String::new(),
        turns: (0..n_turns)
            .map(|i| bank::Turn {
                question: format!("q{i}"),
                expected_facts: Vec::new(),
                expected_sources: Vec::new(),
                notes: String::new(),
            })
            .collect(),
    }
}

#[test]
fn cap_threads_by_turns_bounds_total() {
    // Uneven lengths like the real bank; budget 12 stops before the thread
    // that would overflow it.
    let threads = vec![thr("a", 5), thr("b", 5), thr("c", 12), thr("d", 6)];
    let got = cap_threads_by_turns(threads, 12);
    assert_eq!(
        got.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        vec!["a", "b"], // 5+5=10 ≤ 12; +c(12)=22 > 12 → stop
    );
    assert!(got.iter().map(|t| t.turns.len()).sum::<usize>() <= 12);
}

#[test]
fn cap_threads_by_turns_keeps_oversized_first() {
    // First thread alone exceeds the budget — still run it (never empty).
    let got = cap_threads_by_turns(vec![thr("big", 21), thr("small", 2)], 10);
    assert_eq!(
        got.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        vec!["big"]
    );
}

/// `--closed-book` names an ARM, and an arm that silently ran grounded
/// would file a grounded score as the study's floor — understating every
/// delta measured against it by exactly the thing being measured. All
/// three refusals land before the bank is opened, so this test reaches
/// no daemon and no file (the bank path below does not exist).
#[tokio::test]
async fn closed_book_refuses_without_synth_and_with_prod_pipeline() {
    let argv = |flags: &[&str]| -> Vec<String> {
        ["--bank", "bench/lanes/no-such-bank.toml"]
            .iter()
            .chain(flags.iter())
            .map(|s| s.to_string())
            .collect()
    };
    assert_eq!(cmd_run(&argv(&["--closed-book"])).await, 2, "needs --synth");
    assert_eq!(
        cmd_run(&argv(&["--closed-book", "--synth", "--prod-pipeline"])).await,
        2,
        "--prod-pipeline scores an evidence pool a naked turn never builds"
    );
    assert_eq!(
        cmd_run(&argv(&["--closed-book", "--synth", "--routing-only"])).await,
        2,
        "--routing-only scores a routing decision a naked turn never makes"
    );
}

#[test]
fn cap_threads_by_turns_noop_at_bounds() {
    let mk = || vec![thr("a", 5), thr("b", 6)];
    assert_eq!(cap_threads_by_turns(mk(), 0).len(), 2, "0 → unchanged");
    assert_eq!(
        cap_threads_by_turns(mk(), 11).len(),
        2,
        "==total → unchanged"
    );
    assert_eq!(
        cap_threads_by_turns(mk(), 99).len(),
        2,
        ">total → unchanged"
    );
}
