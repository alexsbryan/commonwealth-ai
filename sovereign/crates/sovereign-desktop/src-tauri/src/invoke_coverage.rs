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
//! Newline-delimited command names, first sighting only, flushed as they
//! happen so a crashed run still reports what it reached. Diff it against the
//! registry with `scripts/desktop-invoke-coverage.py`.

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
            let _ = writeln!(f, "{cmd}");
            let _ = f.flush();
        }
        Err(e) => tracing::warn!(error = %e, cmd, "invoke-coverage: append failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
