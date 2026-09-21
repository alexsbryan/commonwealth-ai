// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for takeover — see `daemon.rs`.
//!
//! Their own file because keeping them inline put that file past its
//! arch-gate slack (ARCH §3.1). `#[path]`, so the names are unchanged.

//! Unit tests for `takeover_serve_at` — Phase 3 daemon-takeover of
//! the standalone `sovereign serve --background` process. We
//! exercise the deterministic branches (no file, malformed pid,
//! self-pid) here. The real-process SIGTERM branch needs a child
//! to kill, which lives in the manual lifecycle verification per
//! the Phase 3 plan.

use super::*;

#[test]
fn missing_pid_file_is_a_noop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.pid");
    // No file exists — function must return without panicking
    // and without creating the file.
    takeover_serve_at(&path);
    assert!(!path.exists(), "takeover must not create the pid file");
}

#[test]
fn malformed_pid_file_is_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.pid");
    std::fs::write(&path, "not-a-number\n").unwrap();
    takeover_serve_at(&path);
    assert!(
        !path.exists(),
        "malformed pid file must be removed so a future bind can rewrite it"
    );
}

#[test]
fn self_pid_is_cleared_without_signal() {
    // The self-pid branch defends against the daemon being
    // launched in a context where it inherited its own pid file
    // (test harness, in-process spawn). The function must remove
    // the file and not attempt to SIGTERM ourselves.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.pid");
    let me = std::process::id() as i32;
    std::fs::write(&path, format!("{me}\n")).unwrap();
    takeover_serve_at(&path);
    assert!(!path.exists(), "self-pid file must be removed");
    // If the function had SIGTERM'd us, the test process would be
    // dead — reaching this assertion proves the self-skip works.
}

#[test]
fn stale_pid_file_for_dead_process_is_cleared() {
    // A pid that's almost certainly not a live process. We use
    // 999_999, which is well above macOS's default pid_max and
    // Linux's default 32_768. /bin/kill returns non-zero, the
    // function logs "stale" and removes the file.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.pid");
    std::fs::write(&path, "999999\n").unwrap();
    takeover_serve_at(&path);
    assert!(
        !path.exists(),
        "stale pid file must be removed so the daemon can write a new one"
    );
}
