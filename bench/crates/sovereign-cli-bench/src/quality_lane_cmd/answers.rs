// SPDX-License-Identifier: AGPL-3.0-or-later
//! The answers a lane judged, kept beside its verdict. A row that fails on
//! what an answer said (chat-ask's `both halves answered`) cannot be read
//! from a duration and a verdict; on the router flip of 2026-10-25 the
//! failing q1 answers were gone the moment the lane exited.

use std::io::Write;

use super::LaneCtx;

/// Append one JSON line per answer to `<out_dir>/<lane>-answers.jsonl`.
/// Outside the runner (no out dir) nothing is written, and a write that
/// fails is a warning on stderr, never a lane failure: the verdict does not
/// rest on this file.
pub(crate) fn keep(ctx: &LaneCtx, lane: &str, answers: &[serde_json::Value]) {
    let Some(dir) = ctx.out_dir.as_ref() else {
        return;
    };
    let path = dir.join(format!("{lane}-answers.jsonl"));
    let wrote = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| answers.iter().try_for_each(|a| writeln!(f, "{a}")));
    match wrote {
        Ok(()) => eprintln!(
            "  [{lane}] {} answer(s) kept in {}",
            answers.len(),
            path.display()
        ),
        Err(e) => eprintln!("  [{lane}] answers not kept ({}): {e}", path.display()),
    }
}
