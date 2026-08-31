// SPDX-License-Identifier: AGPL-3.0-or-later
//! Falsifier for `quality/TOPOLOGY.md` §10 phase 9, rung 9.2 — hazard 2,
//! "an `Answer` released without a `Judgement`".
//!
//! # The state this makes unrepresentable
//!
//! A gate exit that releases text nobody judged.
//!
//! `kernel_types::Answer` already had no door that does not take a
//! `Judgement` by value, and `kernel-types/tests/ui/answer_without_a_judgement.rs`
//! is a compile-fail test proving it. What was missing was ADOPTION: measured
//! 2026-08-26, `GateOutcome` carried `text: String`, sixteen sites in
//! `grounding/mod.rs` constructed it, and exactly ONE went through
//! `Draft::release` — that one flattening its `Answer` back to a `String` on
//! the very next line. Fifteen exits released prose with no verdict attached,
//! including `judge_failed_open` (the ladder fell open) and
//! `retry_released_unverified` (the gate never re-audited the rewrite), both
//! of which were indistinguishable from a verified pass (ARCH §18.2).
//!
//! `GateOutcome.answer: Answer` closed that by construction. This census
//! guards the second half — that the four named doors stay the ONLY places a
//! gate answer is minted, so a seventeenth exit cannot quietly hand-roll a
//! `Judgement::passed` beside them (ARCH §10.6, one decider).

use std::path::Path;

/// The four doors, plus the dispatch that chooses between them. Every one
/// wraps exactly one `kernel-types` constructor and nothing else decides.
const DOORS: &[&str] = &[
    "fn release_held(",
    "fn release_flawed(",
    "fn abstain(",
    "fn release_unjudged(",
    "fn release_as(",
];

/// Minting calls that may appear ONLY inside a door.
const MINTS: &[&str] = &[
    "Draft::composed(",
    "Answer::abstained(",
    "Judgement::passed(",
    "Judgement::failed(",
    "Judgement::could_not_judge(",
    "Judgement::never_ran(",
];

fn grounding_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime/grounding/mod.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Byte offset ranges covered by the door functions, taken as "from the `fn`
/// line to the next top-level `fn`/`pub` at column 0". Crude on purpose: this
/// is a census, and a door that grew past its neighbour would fail loudly
/// rather than pass quietly.
fn door_spans(src: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for door in DOORS {
        let Some(start) = src.find(door) else {
            panic!(
                "door `{door}` is gone from grounding/mod.rs. If it was renamed, rename it here \
                 too; if it was deleted, a gate exit is minting an answer somewhere else."
            );
        };
        let rest = &src[start + door.len()..];
        let end = rest
            .find("\n}\n")
            .map(|i| start + door.len() + i + 3)
            .unwrap_or(src.len());
        spans.push((start, end));
    }
    spans
}

#[test]
fn only_the_named_doors_mint_a_gate_answer() {
    let src = grounding_source();
    let spans = door_spans(&src);
    let inside = |pos: usize| spans.iter().any(|(a, b)| pos >= *a && pos < *b);

    let mut strays: Vec<String> = Vec::new();
    for mint in MINTS {
        let mut from = 0usize;
        while let Some(rel) = src[from..].find(mint) {
            let pos = from + rel;
            from = pos + mint.len();
            if inside(pos) {
                continue;
            }
            let line = src[..pos].matches('\n').count() + 1;
            strays.push(format!("{mint} at grounding/mod.rs:{line}"));
        }
    }

    assert!(
        strays.is_empty(),
        "a gate answer was minted outside the four named doors. Every exit must go through \
         `release_as` (or one of the doors directly), so that \"what judgement does a turn that \
         ended THIS way carry\" has one answer (ARCH §10.6).\n{strays:#?}"
    );
}

#[test]
fn the_outcome_carries_an_answer_not_a_string() {
    let src = grounding_source();
    // The struct field itself. A revert to `text: String` restores fifteen
    // unjudged exits in one line, and nothing else in the tree would notice.
    assert!(
        src.contains("pub answer: kernel_types::Answer,"),
        "`GateOutcome` no longer carries an `Answer`. Hazard 2 is open again: a `String` field \
         can be assigned from anywhere, and fifteen of the sixteen exits used to do exactly that."
    );
    // Scoped to the struct body: `GateClaim` legitimately carries a
    // `text: String` (the claim as extracted), and a whole-file grep would
    // fail on it — a check that fires on the wrong input is not a check
    // (ARCH §18.1).
    let start = src
        .find("pub(crate) struct GateOutcome {")
        .expect("GateOutcome is declared");
    let body = &src[start..start + src[start..].find("\n}\n").expect("struct closes")];
    assert!(
        !body.contains("pub text: String,"),
        "`GateOutcome.text: String` is back beside the answer — two spellings of what the turn \
         released, which is the divergence the rung removed."
    );
}

const REACHES: [&str; 4] = [
    "GateReach::Held",
    "GateReach::Flawed",
    "GateReach::Declined",
    "GateReach::Unjudged",
];

#[test]
fn every_gate_reach_has_a_door() {
    // The dispatch is exhaustive by the compiler; what a source census adds is
    // that no arm was collapsed into another while keeping its name. Each of
    // the four reaches must appear in the one dispatch.
    let src = grounding_source();
    let start = src
        .find("fn release_as_because(")
        .expect("release_as_because exists — it is the one dispatch");
    let body = &src[start..(start + 1200).min(src.len())];
    for reach in REACHES {
        assert!(
            body.contains(reach),
            "`{reach}` has no arm in `release_as_because` — a turn that ended that way is \
             being released under another verdict's judgement"
        );
    }
}

/// A reach nothing PRODUCES is a state the gate cannot be in — and the check
/// above cannot see it, because an arm can sit in the dispatch forever with no
/// `GateAction` naming it.
///
/// Named failing input (ARCH §18.1), measured rather than imagined: drop this
/// change's six longform constants and the table goes 17 sites -> 11, carrying
/// `Held`, `Declined` and `Unjudged` and **no `Flawed`** — which is exactly
/// the state the tree was in on 2026-08-26 before this test existed. The three
/// longform exits that ARE flawed called `release_flawed` directly and carried
/// a bare string literal on the wire, so the id and the verdict were chosen at
/// two places with nothing holding them together (§10.6). The only thing that
/// noticed was a `variant is never constructed` compiler warning.
#[test]
fn every_gate_reach_is_produced_by_some_action() {
    let src = grounding_source();
    // Only the constant table, so a mention in a doc comment or a match arm
    // cannot satisfy this.
    let actions: Vec<&str> = src
        .match_indices("GateAction::new(")
        .map(|(i, _)| {
            let rest = &src[i..];
            &rest[..rest.find(')').map(|e| e + 1).unwrap_or(rest.len().min(120))]
        })
        .collect();
    assert!(
        actions.len() >= 8,
        "found only {} `GateAction::new(` sites — the scan is broken, not the tree",
        actions.len()
    );
    for reach in REACHES {
        assert!(
            actions.iter().any(|a| a.contains(reach)),
            "no `GateAction` carries `{reach}`, so no exit can reach it. Either an exit is \
             releasing under the wrong verdict, or the variant is scaffolding and should be \
             deleted (ARCH §6 — name what you deleted).\nActions found: {actions:#?}"
        );
    }
}
