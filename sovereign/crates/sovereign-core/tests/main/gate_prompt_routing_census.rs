// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Every prompt fragment a gate test mock routes on must still appear in a
//! prompt the gate actually builds.**
//!
//! # The failure this makes loud
//!
//! The gate's test mocks are scripted judges: they read `request.prompt`,
//! match a fragment of the production prompt, and reply the way that register
//! would. The match is a REMEMBERED COPY of a literal that lives in production
//! code, and nothing connects the two — so rewording a prompt orphans the
//! branch, the mock falls through to its catch-all string, and every test that
//! drove that register keeps running against a reply the register would never
//! give. No assertion fires. Nothing says the instrument stopped working.
//!
//! It fired twice in one session (2026-09-09), both found by hand:
//!
//! - `value_presence`'s extractor went from "the specific value the ANSWER
//!   gives" to "…the ANSWER puts in front of the reader". `GateMock`'s branch
//!   stopped matching, so the extractor's reply became the fall-through string
//!   — a value absent from every chunk, which the presence veto then refused.
//!   The suite was one prompt reword away from vetoing every gated answer for
//!   a reason no test named.
//! - The specifics scan had said "Compare the ANSWER against the EVIDENCE" and
//!   by then said "…against the passages above"
//!   (`judge::batched`). `GateMock`'s branch had been dead since, and the
//!   guard read the catch-all string as its list of unsupported statements.
//!
//! Two tests in `grounding/tests.rs` already assert their released text does
//! not CONTAIN the catch-all — which is the same bruise, patched one test at a
//! time. This is the ratchet instead: a fragment that no longer matches
//! production fails HERE, naming itself, rather than silently changing what
//! some other test measures (ARCH §18.4 — validate the instrument before the
//! result; §7 — structural, not remembered).
//!
//! # What it does not claim
//!
//! Only that each fragment still occurs. It cannot say the mock's REPLY is
//! still the right shape for that register — that is what the register's own
//! tests are for. A prompt whose wording is unchanged but whose contract moved
//! passes this census, correctly: nothing here is a verdict about behaviour.

use std::path::{Path, PathBuf};

fn grounding_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/runtime/grounding")
}

/// Every `.rs` under `grounding/`, read whole. Walked rather than listed so a
/// prompt moved into a NEW file is still found — a hand-maintained file list
/// is the same remembered-copy footgun one level up.
fn read_tree(dir: &Path, skip_tests: bool, out: &mut Vec<(PathBuf, String)>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            read_tree(&path, skip_tests, out);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // `tests.rs` and `tests/` hold the mocks; they are the SUBJECT here,
        // never the evidence. Counting them as production would let a
        // fragment satisfy the census by matching the mock that copied it.
        let is_tests = name == "tests.rs" || path.components().any(|c| c.as_os_str() == "tests");
        if skip_tests && is_tests {
            continue;
        }
        if !skip_tests && !is_tests {
            continue;
        }
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        out.push((path, body));
    }
}

/// Is this `contains(` call reading the PROMPT?
///
/// The discriminator has to be the receiver, not the literal. A first cut
/// keyed on "looks like prose" swept 35 sites, nearly all of them assertions
/// about released ANSWER text ("Russian ambassador", "[Source: Re: Cornell]")
/// — strings that are supposed to exist only in a fixture. A census that
/// reports what it was not built to see is not a census (§18.1).
///
/// Two spellings reach the prompt in these mocks and both are accepted:
/// `request.prompt.contains(…)` (rustfmt may split it across three lines, so
/// whitespace is collapsed first) and `p.contains(…)` where the mock bound
/// `let p = &request.prompt`.
fn reads_the_prompt(before: &str) -> bool {
    let collapsed: String = before.chars().filter(|c| !c.is_whitespace()).collect();
    if collapsed.ends_with(".prompt") {
        return true;
    }
    // A bare `p` binding — and only a bare one, so `map` or `resp` cannot
    // pass by ending in the same letter.
    let mut chars = collapsed.chars().rev();
    chars.next() == Some('p')
        && !chars
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.')
}

/// Pull every prompt-routing `contains("…")` literal out of the mock sources.
fn routing_fragments(sources: &[(PathBuf, String)]) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for (path, body) in sources {
        let mut cursor = 0usize;
        while let Some(rel) = body[cursor..].find(".contains(\"") {
            let at = cursor + rel;
            let open = at + ".contains(\"".len();
            let Some(rel_end) = body[open..].find('"') else {
                break;
            };
            let lit = &body[open..open + rel_end];
            // Enough preceding source to hold a rustfmt-split receiver.
            let window = &body[at.saturating_sub(120)..at];
            if reads_the_prompt(window) {
                out.push((path.clone(), lit.to_string()));
            }
            cursor = open + rel_end;
        }
    }
    out
}

/// The census. Fails naming the fragment and the file that still carries it.
#[test]
fn every_prompt_fragment_a_mock_routes_on_still_exists_in_a_prompt() {
    let dir = grounding_dir();
    let mut production = Vec::new();
    read_tree(&dir, true, &mut production);
    let mut mocks = Vec::new();
    read_tree(&dir, false, &mut mocks);

    assert!(
        !production.is_empty() && !mocks.is_empty(),
        "the walk found nothing to compare — production {} file(s), mock {} file(s) under {}",
        production.len(),
        mocks.len(),
        dir.display()
    );

    let fragments = routing_fragments(&mocks);
    // FAILING INPUT, named (ARCH §18.1): reword any routed prompt — e.g. drop
    // "word for word" from `citation`'s quote contract — and this list goes
    // non-empty with that fragment on it.
    assert!(
        !fragments.is_empty(),
        "no routing fragments found; the extractor stopped seeing the mocks \
         and this census would pass vacuously"
    );

    let orphans: Vec<String> = fragments
        .iter()
        .filter(|(_, lit)| {
            !production
                .iter()
                .any(|(_, body)| body.contains(lit.as_str()))
        })
        .map(|(path, lit)| {
            let f = path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            format!("  {f}: {lit:?}")
        })
        .collect();

    assert!(
        orphans.is_empty(),
        "{} mock routing fragment(s) match NO prompt this gate builds. The \
         branch is dead: every call it was written to script now falls through \
         to the mock's catch-all, silently. Re-point the branch at the current \
         prompt, or delete it if the register is gone.\n{}",
        orphans.len(),
        orphans.join("\n")
    );
}
