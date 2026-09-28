// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn-ingest ingest`'s refusals, asserted on the sentence a person reads.
//! Moved from corpus-mcp's tests/verbs.rs with the verb (pb-ingest-cli).
//! Nothing here needs a live endpoint.

use std::path::Path;
use std::process::Command;

struct Run {
    code: i32,
    stderr: String,
}

fn run(dir: &Path, args: &[&str]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_svrn-ingest"))
        .current_dir(dir)
        .args(args)
        // The ladder's daemon rung points at a dead port, so a developer's
        // running daemon cannot change the result.
        .env("SOVEREIGN_DAEMON_URL", "http://127.0.0.1:1")
        .output()
        .expect("svrn-ingest did not start");
    Run {
        code: out.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ── `ingest` — the endpoint combinations, none of which need an endpoint ────

/// §18.3: a half-named pair is refused, never guessed at. Guessing would send
/// phase 1's chat calls to the embedding process.
#[test]
fn ingest_refuses_a_half_specified_endpoint_pair() {
    let tmp = tempfile::tempdir().unwrap();
    let recipe = tmp.path().join("r.toml");
    std::fs::write(&recipe, "# never read: the refusal precedes any disk\n").unwrap();
    let rp = recipe.to_str().unwrap();

    let r = run(tmp.path(), &["ingest", rp, "--chat-url", "http://x/v1"]);
    assert_ne!(r.code, 0);
    assert!(
        r.stderr.contains("--embed-url"),
        "the refusal does not name the missing flag:\n{}",
        r.stderr
    );

    let r = run(tmp.path(), &["ingest", rp, "--embed-url", "http://x/v1"]);
    assert_ne!(r.code, 0);
    assert!(
        r.stderr.contains("--chat-url"),
        "the refusal does not name the missing flag:\n{}",
        r.stderr
    );

    let r = run(
        tmp.path(),
        &[
            "ingest",
            rp,
            "--base-url",
            "http://x/v1",
            "--chat-url",
            "http://y/v1",
        ],
    );
    assert_ne!(r.code, 0);
    assert!(
        r.stderr.contains("--base-url"),
        "mixing --base-url with a half-pair was not refused by name:\n{}",
        r.stderr
    );
}

/// `corpus ingest` with no flags at all is the literal §4 command line, so it
/// walks the same ladder — one implementation of "which endpoint", shared with
/// `serve` (ARCH §10.6). Same assertion, other verb: if `ingest` ever grew its
/// own ladder, one of these two would drift.
#[test]
fn ingest_with_no_endpoint_walks_the_same_ladder() {
    let tmp = tempfile::tempdir().unwrap();
    let recipe = tmp.path().join("r.toml");
    std::fs::write(&recipe, "# never read\n").unwrap();
    let r = run(tmp.path(), &["ingest", recipe.to_str().unwrap()]);
    assert_ne!(r.code, 0);
    for rung in ["ollama", "llama-server", "oicp daemon"] {
        assert!(
            r.stderr.contains(rung),
            "`ingest` did not walk the `{rung}` rung:\n{}",
            r.stderr
        );
    }
}

// ── `pull` — serve's pull-if-absent, moved here with the engine it needs ────

/// `corpus-mcp serve --corpus <absent>` execs this verb (pb-corpus-mcp-reads).
/// A named endpoint that does not answer is refused carrying that URL, before
/// any recipe is resolved or any byte downloaded: the restore probe cannot
/// judge a snapshot against an endpoint that is not there.
#[test]
fn pull_refuses_a_named_endpoint_that_does_not_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().to_str().unwrap();
    let r = run(
        tmp.path(),
        &[
            "pull",
            "--corpus",
            "sep",
            "--base-url",
            "http://127.0.0.1:1/v1",
            "--data-dir",
            data,
        ],
    );
    assert_ne!(
        r.code, 0,
        "a pull with no endpoint succeeded:\n{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("127.0.0.1:1"),
        "the refusal does not carry the URL that was named:\n{}",
        r.stderr
    );
    assert!(
        !tmp.path().join("indexes").join("sep").exists(),
        "a refused pull wrote an index"
    );
}
