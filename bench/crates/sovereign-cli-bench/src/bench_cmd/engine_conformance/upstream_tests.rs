// SPDX-License-Identifier: AGPL-3.0-or-later
//! When a projection row may blame the prompt or the generation instead of
//! itself: only after the values differ, and only for what a generation can
//! move. Split from tests.rs, which stays under its size ceiling.

use super::check::{CellVerdict, Check};
use super::record::{Facets, Output, Usage};
use super::tests::{is_differs, pair};

#[test]
fn unobserved_tokens_let_the_answer_text_excuse_counts_but_not_absences() {
    // Equal prompts, no greedy tokens: the remote target's shape.
    let f = |text: &str, usage: Option<(u64, u64)>, finish: Option<&str>| Facets {
        prompt_ids: Some(vec![1, 2]),
        output: Some(Output {
            text: text.into(),
            usage: usage.map(|(prompt, completion)| Usage { prompt, completion }),
            finish: finish.map(str::to_string),
            ..Default::default()
        }),
        ..Default::default()
    };
    let excused = |v: &CellVerdict| matches!(v, CellVerdict::CouldNotJudge(why) if why.contains("answer text differs"));
    let (u, n) = (Some((2, 5)), Some((2, 9)));
    assert!(excused(&pair(
        &Check::Usage,
        f("a", u, None),
        f("b", n, None)
    )));
    assert!(is_differs(&pair(
        &Check::Usage,
        f("a", u, None),
        f("a", n, None)
    )));
    assert!(
        is_differs(&pair(&Check::Usage, f("a", u, None), f("b", None, None))),
        "usage one side never reports is a gap, whatever was generated"
    );
    assert!(
        is_differs(&pair(
            &Check::Usage,
            f("a", u, None),
            f("b", Some((0, 5)), None)
        )),
        "a prompt count does not depend on what was generated"
    );
    let (stop, length) = (Some("stop"), Some("length"));
    assert!(excused(&pair(
        &Check::Finish,
        f("a", None, stop),
        f("b", None, length)
    )));
    assert!(is_differs(&pair(
        &Check::Finish,
        f("a", None, stop),
        f("a", None, length)
    )));
    assert!(
        is_differs(&pair(&Check::Text, f("a", u, stop), f("b", u, stop))),
        "the text cannot excuse a check on the text"
    );
    let tokens = |mut x: Facets, t: Vec<i64>| {
        x.greedy_tokens = Some(t);
        x
    };
    assert!(
        is_differs(&pair(
            &Check::Usage,
            tokens(f("a", u, None), vec![7]),
            tokens(f("b", n, None), vec![7])
        )),
        "observed equal tokens outrank the text stand-in"
    );
}

#[test]
fn equal_values_pass_whatever_the_prompts_or_generations_did() {
    let usage = |ids: Vec<i64>, text: &str| Facets {
        prompt_ids: Some(ids),
        output: Some(Output {
            text: text.into(),
            usage: Some(Usage {
                prompt: 0,
                completion: 0,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(
        pair(&Check::Usage, usage(vec![1], "a"), usage(vec![2], "b")),
        CellVerdict::Passed,
        "the excuse is asked only after the values differ"
    );
}

#[test]
fn a_reasoning_difference_excuses_a_count_when_the_answers_agree() {
    let f = |reasoning: &str, completion: u64| Facets {
        prompt_ids: Some(vec![1, 2]),
        output: Some(Output {
            text: String::new(),
            reasoning: Some(reasoning.into()),
            usage: Some(Usage {
                prompt: 280,
                completion,
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(matches!(
        pair(&Check::Usage, f("long thought", 95), f("short", 70)),
        CellVerdict::CouldNotJudge(why) if why.contains("reasoning differs")
    ));
    assert!(is_differs(&pair(&Check::Usage, f("same", 95), f("same", 70))));
}
