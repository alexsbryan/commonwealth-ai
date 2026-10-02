// SPDX-License-Identifier: AGPL-3.0-or-later
//! The dispatcher's sibling execs: the one exec tail, the dev-tools
//! warning, and (re-exported from the host kit, which cli-daemon shares)
//! the stale-sibling warning.

use std::ffi::OsString;
use std::path::Path;

pub use host_kit::sibling::warn_if_stale;

/// Hand the process to `bin` as `bin <verb> <args…>`: on Unix an `exec`
/// that only returns (126) if it failed, elsewhere a child whose exit code
/// is returned. The one tail every `*_bin::exec` shares.
pub fn exec_into(bin: &Path, verb: &str, args: &[String]) -> i32 {
    let mut argv: Vec<OsString> = Vec::with_capacity(args.len() + 1);
    argv.push(OsString::from(verb));
    for a in args {
        argv.push(OsString::from(a));
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(bin).args(&argv).exec();
        eprintln!("sovereign: exec {} failed: {err}", bin.display());
        126
    }

    #[cfg(not(unix))]
    {
        match std::process::Command::new(bin).args(&argv).status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(e) => {
                eprintln!("sovereign: spawn {} failed: {e}", bin.display());
                126
            }
        }
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
/// with one that has lost every developer verb — `notes`, `code`, `tools`.
/// Nothing fails at build time; the loss surfaces minutes
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
    // `project` is deliberately NOT in this list any more: its registry
    // subcommands (register/unregister/list/watch) ship in every build via
    // `project_registry`, so naming it wholesale would tell a developer that
    // verbs they can actually run are missing.
    eprintln!(
        "warning: this `sovereign` was built WITHOUT `--features dev-tools`, but a \
         {DEV_SIBLING} sits beside it — the developer verbs (notes, code, \
         tools, and the `project` lifecycle subcommands) are missing from this \
         binary. A bare `cargo build -p sovereign-cli` does this silently. \
         Restore them: cargo build -p sovereign-cli --features dev-tools  \
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
