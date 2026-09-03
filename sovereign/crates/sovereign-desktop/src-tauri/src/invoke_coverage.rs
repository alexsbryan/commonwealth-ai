// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which Tauri commands did this run actually reach?
//!
//! # Why this exists
//!
//! The desktop exposes 251 commands via `generate_handler!`. The real-mode e2e
//! suite issues 11 of them by name; the rest of its reach is whatever the UI
//! happens to invoke when Playwright clicks around — a number nobody could
//! state. "How much of the app do our tests touch?" had no answer, only
//! opinions.
//!
//! This records the answer. It is deliberately a COVERAGE measure rather than
//! an assertion count, because assertion counts are trivially inflated: you can
//! add ten asserts to a spec that already passes and move the number without
//! testing anything new. The only way to move THIS number is to reach a command
//! you were not reaching before.
//!
//! # Where it hooks in
//!
//! One wrapper around the invoke handler in `main.rs`, which is the single
//! chokepoint both paths share:
//!   - the frontend's `invoke()` (so UI-driven clicks count, which is the whole
//!     point — six `src/` modules import `invoke` directly, so a JS-side
//!     wrapper around one module would undercount), and
//!   - `command_bridge`, whose `/invoke` route dispatches through
//!     `webview.on_message` — the production path, not a parallel one.
//!
//! # Cost when disabled
//!
//! One `OnceLock` read per invoke. Recording is off unless
//! `SOVEREIGN_INVOKE_COVERAGE` names an output path, so shipped builds do
//! nothing but that check.
//!
//! # Reading the output
//!
//! JSONL, one `{"cmd": "<name>"}` row per command, first sighting only, and
//! flushed as it happens so a crashed run still reports what it reached.
//!
//! THE FORMAT IS NOT ARBITRARY — it is the one every other invoke ledger in
//! this crate already writes (`tests/e2e/fixtures/test-base.ts` for the
//! synthetic tier, `SOVEREIGN_COMMAND_BRIDGE_LEDGER` for the real one), so a
//! file produced here merges into the same reader as those:
//!
//! ```text
//! node tests/e2e/scripts/coverage-report.mjs <this file> ...
//! ```
//!
//! It used to write bare newline-delimited names read by a second tool of its
//! own, and that tool reported `0/260 reached (0.0%)` when handed a real
//! ledger — an unparseable input rendered as a zero result rather than as
//! could-not-parse (ARCH §18.3). One writer format, one reader.

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Env var naming the output file. Unset = recording off.
const ENV_PATH: &str = "SOVEREIGN_INVOKE_COVERAGE";

struct Recorder {
    path: PathBuf,
    seen: Mutex<HashSet<String>>,
}

static RECORDER: OnceLock<Option<Recorder>> = OnceLock::new();

fn recorder() -> Option<&'static Recorder> {
    RECORDER
        .get_or_init(|| {
            let path = PathBuf::from(std::env::var_os(ENV_PATH)?);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Truncate any previous run's file: the record must describe THIS
            // run, or a stale file inflates coverage with commands nobody
            // reached this time.
            match std::fs::File::create(&path) {
                Ok(_) => Some(Recorder {
                    path,
                    seen: Mutex::new(HashSet::new()),
                }),
                Err(e) => {
                    // Do not fail the app because telemetry could not open a
                    // file, but do not pretend it worked either.
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "invoke-coverage: cannot open output; recording disabled"
                    );
                    None
                }
            }
        })
        .as_ref()
}

/// One ledger row for `cmd`, as the shared reader expects it.
///
/// Extracted so the FORMAT has a test that does not need the recorder running:
/// the recorder is behind a process-wide `OnceLock` and an env var, so a test
/// that enabled it would fight `recording_is_off_without_the_env_var` for the
/// same lock. serde_json rather than a hand-rolled quote — a command name is
/// only *usually* a bare identifier, and this row has to parse.
fn ledger_row(cmd: &str) -> String {
    serde_json::json!({ "cmd": cmd }).to_string()
}

/// Note that `cmd` was invoked. First sighting per process is written; repeats
/// are dropped, so a chatty poll does not swamp the file.
pub fn record(cmd: &str) {
    let Some(rec) = recorder() else { return };
    // A poisoned lock here means another thread panicked mid-record. The set is
    // still structurally sound and this is diagnostics — recover rather than
    // take the app down with it.
    let mut seen = rec.seen.lock().unwrap_or_else(|p| p.into_inner());
    if !seen.insert(cmd.to_string()) {
        return;
    }
    // Append while holding the lock: two threads meeting a new command at once
    // would otherwise interleave partial lines.
    match std::fs::OpenOptions::new().append(true).open(&rec.path) {
        Ok(mut f) => {
            let _ = writeln!(f, "{}", ledger_row(cmd));
            let _ = f.flush();
        }
        Err(e) => tracing::warn!(error = %e, cmd, "invoke-coverage: append failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row must PARSE in the reader every other invoke ledger feeds
    /// (`tests/e2e/scripts/coverage-report.mjs`), which skips any line that is
    /// not JSON carrying `cmd`. This file used to write bare names, and the
    /// tool that read them reported `0/260 reached (0.0%)` against a real
    /// ledger rather than saying it could not parse.
    #[test]
    fn a_ledger_row_is_json_the_shared_reader_can_read() {
        let row = ledger_row("send_message_stream");
        let parsed: serde_json::Value =
            serde_json::from_str(&row).expect("a ledger row must be JSON");
        assert_eq!(parsed["cmd"], "send_message_stream");
        assert!(
            !row.contains('\n'),
            "one row per line, or appends interleave"
        );
    }

    /// The reason this uses serde_json and not `format!`.
    #[test]
    fn a_command_name_needing_escapes_still_produces_one_parseable_row() {
        let row = ledger_row(r#"weird"name\with\escapes"#);
        let parsed: serde_json::Value =
            serde_json::from_str(&row).expect("escaping must survive the round trip");
        assert_eq!(parsed["cmd"], r#"weird"name\with\escapes"#);
    }

    /// `record` must be inert when the env var is unset — that is what makes it
    /// safe to leave wired into shipped builds.
    #[test]
    fn recording_is_off_without_the_env_var() {
        // The OnceLock is process-wide and this test binary sets no env var, so
        // this also pins that a bare call neither panics nor creates anything.
        record("some_command");
        assert!(
            std::env::var_os(ENV_PATH).is_none(),
            "this test asserts the DISABLED path; it is meaningless if the \
             harness sets {ENV_PATH}"
        );
        assert!(recorder().is_none());
    }
}
