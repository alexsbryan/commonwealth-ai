// SPDX-License-Identifier: AGPL-3.0-or-later
//! Falsifier for ledger record **IN-10**: streaming send policy lives in
//! `StreamSink` and nowhere else.
//!
//! # The named failing input (ARCH §18.1)
//!
//! Before noun-convergence rung nc-17, `model_slot.rs` held THREE
//! implementations of "deliver one stream token": the legacy
//! `send_stream_piece`, which was bounded by the inference deadline;
//! `StreamSink::send_piece`, which was a bare `blocking_send`; and two more
//! bare `blocking_send`s inline in `stream_generate_loop`. Only the LEGACY one
//! was hardened, and the legacy path was the one nothing streamed on any more
//! — so every typed chat completion, which is every streaming chat completion,
//! took an unbounded `blocking_send`. A half-open SSE client (browser tab
//! suspended, TCP window closed, connection alive) reads nothing and drops
//! nothing; the send parked forever, the decode loop's deadline check sits at
//! the TOP of the loop and so never ran, and the slot's `Mutex<SlotContext>`
//! stayed held indefinitely. On a daemon serving roughly one concurrent turn
//! that is the whole node.
//!
//! Three deciders for one behaviour is how they drift (ARCH §10.6). The fix
//! folded the policy into one sink. This census is what makes that
//! unforgettable rather than remembered (ARCH §7).
//!
//! # What this pins, and why it is a census rather than a call-site check
//!
//! Record IN-10 carries GR-04's warning: **scan for reachability, not for a
//! literal at a call site.** A test that asserted `send_piece` uses `try_send`
//! would pass while a fourth implementation grew three functions away. So the
//! bar is over the whole production file: the set of functions that can put a
//! token frame on the client channel is exactly `{send_piece}`, and the set
//! that can send ANY frame is exactly `{send_piece, send_finish}` — both
//! `StreamSink` methods. A new sender fails at the moment it is written.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// The only function permitted to construct a token frame for delivery.
const TOKEN_DOOR: &str = "send_piece";

/// The only functions permitted to put ANY frame on the client channel.
/// `send_finish` is the terminal frame and is deliberately `blocking_send`:
/// it is best-effort, and every caller must skip it after a non-`Sent` piece.
const CHANNEL_DOORS: &[&str] = &["send_piece", "send_finish"];

fn slot_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/embedded/model_slot.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The production half of the file, comments stripped, `(line, code)`.
///
/// Both halves matter and both were learned here. `model_slot.rs` carries
/// FOUR interleaved `#[cfg(test)]` modules — the first at line 929, ~4,000
/// lines above `StreamSink` — so this CANNOT truncate at the first one the way
/// the single-module censuses in `sovereign-core` do; it tracks braces and
/// skips each test module in place. And comments are stripped because a
/// sabotage of `daemon_variant_census.rs` on 2026-08-25 passed for exactly one
/// reason: the file's own prose about an invariant contained the literal the
/// check was looking for, and satisfied the check for it.
fn production_lines(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut skip_depth: i32 = -1; // -1 = not inside a test module
    let mut pending_test_mod = false;
    for (i, raw) in src.lines().enumerate() {
        let code = match raw.find("//") {
            Some(idx) => &raw[..idx],
            None => raw,
        };
        let t = code.trim();

        if skip_depth >= 0 {
            skip_depth += braces(code);
            if skip_depth <= 0 {
                skip_depth = -1;
            }
            continue;
        }
        if t.starts_with("#[cfg(test)]") {
            pending_test_mod = true;
            continue;
        }
        if pending_test_mod {
            // `#[cfg(test)] mod x;` is a declaration, not a block.
            if t.starts_with("mod ") && !t.ends_with(';') {
                pending_test_mod = false;
                skip_depth = braces(code).max(1);
                continue;
            }
            pending_test_mod = false;
        }
        out.push((i + 1, code.to_string()));
    }
    out
}

fn braces(code: &str) -> i32 {
    code.chars().filter(|c| *c == '{').count() as i32
        - code.chars().filter(|c| *c == '}').count() as i32
}

/// Walk production lines tracking the most recent `fn` header, and return
/// `(line, enclosing_fn)` for every line matching `needle`.
fn sites(src: &str, needle: &str) -> Vec<(usize, String)> {
    let mut current = String::from("<file scope>");
    let mut hits = Vec::new();
    for (line, code) in production_lines(src) {
        let t = code.trim_start();
        if let Some(rest) = t
            .strip_prefix("pub(crate) fn ")
            .or_else(|| t.strip_prefix("pub(crate) async fn "))
            .or_else(|| t.strip_prefix("pub async fn "))
            .or_else(|| t.strip_prefix("pub fn "))
            .or_else(|| t.strip_prefix("async fn "))
            .or_else(|| t.strip_prefix("fn "))
        {
            current = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
        }
        if code.contains(needle) {
            hits.push((line, current.clone()));
        }
    }
    hits
}

/// Instrument check (ARCH §18.4): a scanner that finds nothing proves nothing.
/// Before trusting either bar, confirm this scanner sees the sites that are
/// known to be there, and does NOT see the four `#[cfg(test)]` modules it is
/// supposed to be skipping.
#[test]
fn the_scanner_sees_the_production_sink_and_skips_the_test_modules() {
    let src = slot_source();

    let prod = production_lines(&src);
    assert!(
        prod.len() > 4_000,
        "the test-module skipper ate the file: {} production lines out of {} \
         total. It must skip four interleaved `#[cfg(test)]` modules, not \
         truncate at the first.",
        prod.len(),
        src.lines().count()
    );

    let tokens = sites(&src, "StreamFrame::Token(");
    assert!(
        !tokens.is_empty(),
        "scanner found no token construction at all in model_slot.rs — the \
         scan is broken, and a broken scan passes every bar below for free"
    );

    // The liveness tests build `StreamFrame::Token("first".into())` as a
    // fixture. If those show up, the skipper is not skipping.
    for (line, _) in &tokens {
        assert!(
            !src.lines().nth(line - 1).unwrap().contains("\"first\""),
            "line {line} is a test fixture — the `#[cfg(test)]` skipper failed"
        );
    }
}

/// IN-10. A token frame reaches the client through ONE door.
#[test]
fn exactly_one_function_constructs_a_stream_token_for_delivery() {
    let src = slot_source();
    let doors: BTreeSet<String> = sites(&src, "StreamFrame::Token(")
        .into_iter()
        .map(|(_, f)| f)
        .collect();

    assert_eq!(
        doors,
        BTreeSet::from([TOKEN_DOOR.to_string()]),
        "streaming send policy must live in `StreamSink::{TOKEN_DOOR}` and \
         nowhere else (ARCH §10.6, ledger IN-10). A second construction site \
         is a second policy, and history says only one of them gets hardened: \
         before nc-17 three existed and the deadline guarded the dead one, \
         which is how a half-open SSE client pinned the slot indefinitely."
    );
}

/// IN-10, the reachability half. Constructing the frame is not the only way to
/// grow a second policy — a bare send of a frame built elsewhere is the same
/// bug. Pin every function that can touch the channel.
#[test]
fn only_the_sink_puts_frames_on_the_client_channel() {
    let src = slot_source();
    let mut senders: BTreeSet<String> = BTreeSet::new();
    for needle in [".blocking_send(", ".try_send(", ".send(", ".send_timeout("] {
        senders.extend(sites(&src, needle).into_iter().map(|(_, f)| f));
    }

    let allowed: BTreeSet<String> = CHANNEL_DOORS.iter().map(|s| s.to_string()).collect();
    let extra: Vec<&String> = senders.difference(&allowed).collect();
    assert!(
        extra.is_empty(),
        "these functions send on a channel outside `StreamSink`: {extra:?}. \
         Every streaming delivery must go through the sink, which is where the \
         inference deadline is enforced; a send elsewhere is unbounded against \
         a half-open consumer and pins the slot's context mutex. Allowed: \
         {allowed:?}"
    );
    assert!(
        senders.contains(TOKEN_DOOR),
        "the sink's own send disappeared from the scan — instrument failure, \
         not a clean bill of health"
    );
}
