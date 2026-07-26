// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sibling-binary staleness warning.
//!
//! `sovereign` is a thin dispatcher that execs sibling binaries
//! (`sovereign-cli-daemon`, `sovereign-cli-dev`, `sovereign-cli-llm`).
//! The known footgun: edit a sibling crate's code, rebuild only
//! `sovereign-cli` (or only a different sibling), run the verb — the
//! dispatcher silently execs the STALE sibling and the change appears
//! to have no effect. That has cost real debugging sessions.
//!
//! Detection is a build-artifact mtime comparison: if the sibling
//! binary is meaningfully older than the dispatcher, print ONE stderr
//! line naming the exact `cargo build -p <crate>` to run. Warn-only —
//! never blocks, never changes the exit code; a false "maybe stale"
//! costs a glance, a silent stale exec costs a session.
//!
//! Why mtime and not a baked git SHA: SHA stamping needs a build.rs in
//! all four binaries plus a subprocess handshake per dispatch, and it
//! flaps in exactly the loop that matters — dirty trees stamp `-dirty`
//! on both sides while their code differs. mtime catches the actual
//! failure ("sibling not rebuilt since the dispatcher was") for free.
//!
//! The comparison only runs when the sibling lives in the SAME
//! directory as the dispatcher (co-built target artifacts). A sibling
//! pinned via `$SOVEREIGN_CLI_*_BIN` or found on PATH in another
//! location is an operator decision — mtimes across install locations
//! are meaningless. Mute entirely with `SOVEREIGN_NO_STALE_WARN=1`.

use std::path::Path;
use std::time::SystemTime;

/// Tolerance below which mtime differences are ignored. Workspace
/// builds finish crates seconds apart; only a sibling that predates
/// the dispatcher by more than this is worth a warning.
const SLACK_SECS: u64 = 2;

/// Warn (once, on stderr) when `sibling_bin` looks stale relative to
/// the running dispatcher. `crate_name` feeds the rebuild hint.
pub fn warn_if_stale(sibling_bin: &Path, crate_name: &str) {
    if std::env::var_os("SOVEREIGN_NO_STALE_WARN").is_some() {
        return;
    }
    let Ok(dispatcher) = std::env::current_exe() else {
        return;
    };
    let Ok(dispatcher) = std::fs::canonicalize(&dispatcher) else {
        return;
    };
    let Ok(sibling) = std::fs::canonicalize(sibling_bin) else {
        return;
    };
    // Only co-located artifacts are comparable (see module doc).
    if dispatcher.parent() != sibling.parent() {
        return;
    }
    let (Some(disp_mtime), Some(sib_mtime)) = (mtime(&dispatcher), mtime(&sibling)) else {
        return;
    };
    if let Some(lag) = staleness_secs(disp_mtime, sib_mtime) {
        eprintln!(
            "warning: {crate_name} binary is {} older than the `sovereign` dispatcher — \
             if you changed its code, rebuild: cargo build -p {crate_name}  \
             (silence: SOVEREIGN_NO_STALE_WARN=1)",
            human_duration(lag)
        );
    }
}

/// Sibling that only exists in a developer checkout. Its presence next to
/// the dispatcher is what distinguishes "someone is developing here" from
/// "this is an end-user install where the gate is correct".
const DEV_SIBLING: &str = "sovereign-cli-dev";

/// Warn when a DEV checkout is running a dispatcher built without
/// `--features dev-tools`.
///
/// The footgun (cost a session on 2026-07-26): `cargo build -p sovereign-cli`
/// without the feature silently REPLACES a working `target/debug/sovereign-cli`
/// with one that has lost every developer verb — `notes`, `code`, `project`,
/// `atos`, `tools`. Nothing fails at build time; the loss surfaces minutes
/// later as `notes: … not in the default build` on an unrelated command, which
/// reads as a missing feature rather than as "your last build downgraded your
/// install". Same family as the stale-sibling warning above: what you are
/// running is not what you think you built.
///
/// Warn-only, and only in a dev tree — an end-user install has no
/// `sovereign-cli-dev` next to it and is correctly gated.
pub fn warn_if_dev_tools_missing(has_dev_tools: bool) {
    let muted = std::env::var_os("SOVEREIGN_NO_STALE_WARN").is_some();
    let sibling_present = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(exe_name(DEV_SIBLING))))
        .is_some_and(|p| p.is_file());
    if !should_warn_dev_tools(has_dev_tools, sibling_present, muted) {
        return;
    }
    eprintln!(
        "warning: this `sovereign` was built WITHOUT `--features dev-tools`, but a \
         {DEV_SIBLING} sits beside it — the developer verbs (notes, code, project, \
         atos, tools) are missing from this binary. A bare `cargo build -p \
         sovereign-cli` does this silently. Restore them: \
         cargo build -p sovereign-cli --features dev-tools  \
         (silence: SOVEREIGN_NO_STALE_WARN=1)"
    );
}

/// Windows needs the `.exe` suffix; every other target uses the bare name.
fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

/// Pure decision behind [`warn_if_dev_tools_missing`], split out so the
/// policy is testable without touching the filesystem or env.
fn should_warn_dev_tools(has_dev_tools: bool, sibling_present: bool, muted: bool) -> bool {
    !has_dev_tools && sibling_present && !muted
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

/// Pure comparison: seconds the sibling lags the dispatcher, when the
/// lag exceeds the build-skew slack. `None` = not stale.
fn staleness_secs(dispatcher: SystemTime, sibling: SystemTime) -> Option<u64> {
    let lag = dispatcher.duration_since(sibling).ok()?.as_secs();
    (lag > SLACK_SECS).then_some(lag)
}

fn human_duration(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn sibling_newer_or_equal_is_not_stale() {
        let now = SystemTime::now();
        assert_eq!(staleness_secs(now, now), None);
        assert_eq!(staleness_secs(now, now + Duration::from_secs(60)), None);
    }

    #[test]
    fn skew_within_slack_is_not_stale() {
        let now = SystemTime::now();
        assert_eq!(staleness_secs(now, now - Duration::from_secs(2)), None);
    }

    #[test]
    fn sibling_older_than_slack_is_stale() {
        let now = SystemTime::now();
        assert_eq!(
            staleness_secs(now, now - Duration::from_secs(120)),
            Some(120)
        );
    }

    #[test]
    fn dev_tools_warning_fires_only_in_a_dev_tree_that_lost_the_feature() {
        // The failure case: dev sibling present (so this is a checkout), but
        // the dispatcher was built without the feature.
        assert!(should_warn_dev_tools(false, true, false));
        // Feature present — nothing lost.
        assert!(!should_warn_dev_tools(true, true, false));
        // End-user install: no dev sibling, the gate is intended.
        assert!(!should_warn_dev_tools(false, false, false));
        // Muted by the same knob as the staleness warning.
        assert!(!should_warn_dev_tools(false, true, true));
    }

    #[test]
    fn exe_name_matches_the_host_convention() {
        let n = exe_name("sovereign-cli-dev");
        if cfg!(windows) {
            assert_eq!(n, "sovereign-cli-dev.exe");
        } else {
            assert_eq!(n, "sovereign-cli-dev");
        }
    }

    #[test]
    fn human_durations_render_coarsely() {
        assert_eq!(human_duration(45), "45s");
        assert_eq!(human_duration(840), "14m");
        assert_eq!(human_duration(7_200), "2h");
        assert_eq!(human_duration(200_000), "2d");
    }
}
