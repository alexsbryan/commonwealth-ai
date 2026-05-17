//! Stamp the build's git short-SHA and dirty-tree flag into a compile-
//! time env var so the worker daemon can log them at startup.
//!
//! Two-stage fallback so the same crate builds inside a container
//! (where `.git` is not COPY'd in) and on a dev laptop (where it is):
//!
//! 1. **Explicit env override** — `SOVEREIGN_GIT_SHA` set at invoke time
//!    wins. Use this from a Containerfile via `--build-arg
//!    GIT_SHA=$(git rev-parse --short HEAD)` + `ENV SOVEREIGN_GIT_SHA=$GIT_SHA`.
//! 2. **`git rev-parse`** — works locally when `.git` is reachable.
//! 3. **`"unknown"`** — last-resort fallback; the worker daemon will
//!    log `git_sha=unknown` and an operator should treat that as
//!    "rebuild in an environment that has either the env var or `.git`".
//!
//! **2026-05-16 incident** that justifies this:  a stale container running
//! pre-streaming-fix code blocked the SEP-on-Vast smoke; symptoms were
//! identical to a fresh bug. With this stamp, the worker daemon's first
//! log line names exactly which build is running.

fn main() {
    let sha = std::env::var("SOVEREIGN_GIT_SHA").ok().unwrap_or_else(|| {
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "unknown".into())
    });
    // Dirty-tree flag (suffix `-dirty`) when `git status` shows any
    // unstaged or staged-but-uncommitted changes. Skipped silently
    // when the env override is used (operator owns the value).
    let dirty = if std::env::var("SOVEREIGN_GIT_SHA").is_ok() {
        ""
    } else {
        match std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .output()
        {
            Ok(o) if o.status.success() && !o.stdout.is_empty() => "-dirty",
            _ => "",
        }
    };
    println!("cargo:rustc-env=SOVEREIGN_GIT_SHA={sha}{dirty}");
    // Re-run when HEAD moves so the SHA stays current without a clean
    // build. `.git/HEAD` changes on commit / branch switch.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");
    println!("cargo:rerun-if-env-changed=SOVEREIGN_GIT_SHA");
}
