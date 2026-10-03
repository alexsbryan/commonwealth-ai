// SPDX-License-Identifier: AGPL-3.0-or-later
//! The three verbs of `EPISTEMIC_INDEX.md` §4, and every refusal they promise.
//!
//! ## Why this file exists rather than a `cli-contract.toml` row
//!
//! `sovereign contract census` — the surface that asks "is the CLI I just
//! changed covered by anything?" — cannot represent these verbs, and that is
//! structural, not an oversight:
//!
//! - `svrn/docs/cli-contract.toml` line 1 declares itself "the single
//!   source of truth for the commands `sovereign` promises";
//! - `Contract`'s `binary` field is a closed enum of the four sovereign
//!   siblings (`sovereign-cli-shared/src/cli_contract.rs:94` — Dispatcher |
//!   Dev | Llm | Daemon), and `path` is documented as "argv path minus
//!   `sovereign`";
//! - a `[[journey.step]]`'s `run` is argv handed to the `svrn` runner.
//!
//! `corpus-mcp` is a separate binary that `sovereign-cli` never dispatches, so
//! a row for `corpus serve` is unrepresentable without widening the enum,
//! the path semantics and `cli-journey-verify.sh`. (`ingest` and `recipe`
//! left this binary at pb-ingest-cli and answer with pointers.) Widening a shared
//! quality instrument is not this order's to do; it is banked under
//! `contract-census-cannot-represent-corpus-mcp`.
//!
//! So the verbs get the thing a contract row would have bought — a step that
//! ASSERTS OUTPUT rather than an exit code — in the crate that owns the
//! binary. Every case below was watched failing before it was watched passing
//! (ARCH §18.1): the assertions are on the sentence a person reads, so a
//! refusal that silently became a default reddens here.
//!
//! What is NOT here: anything needing a live endpoint. `serve`'s discovery
//! ladder, the probe, and the pull are exercised by `acceptance.sh` against a
//! real `llama-server`; a unit test that stubbed them would be asserting on
//! its own stub.

use std::path::Path;
use std::process::Command;

/// The binary under test. `CARGO_BIN_EXE_<name>` is set by cargo for the
/// crate's own `[[bin]]`, so this cannot drift from what was built.
fn corpus_mcp() -> Command {
    Command::new(env!("CARGO_BIN_EXE_corpus-mcp"))
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(dir: &Path, args: &[&str]) -> Run {
    let out = corpus_mcp()
        .current_dir(dir)
        .args(args)
        // The ladder must never be walked by a test: a developer with Ollama
        // running would otherwise get different results from CI. Every case
        // here either names an endpoint or fails before one is needed.
        .env("SOVEREIGN_DAEMON_URL", "http://127.0.0.1:1")
        .output()
        .expect("corpus-mcp did not start");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ── the verbs exist and are the three §4 names ──────────────────────────────

#[test]
fn help_names_the_three_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let r = run(tmp.path(), &["--help"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    for verb in ["recipe", "ingest", "serve"] {
        assert!(
            r.stdout.contains(verb),
            "`--help` does not list `{verb}`:\n{}",
            r.stdout
        );
    }
}

/// The bare form still serves. This is the compatibility promise ei-5b made
/// and the reason `ServeArgs` is ONE struct flattened in two places: if the
/// top-level flags ever stopped being serve's, this fails.
#[test]
fn the_bare_form_still_takes_serves_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let r = run(tmp.path(), &["--help"]);
    for flag in ["--base-url", "--corpus", "--embed-model", "--data-dir"] {
        assert!(
            r.stdout.contains(flag),
            "top-level `--help` lost serve's `{flag}`:\n{}",
            r.stdout
        );
    }
}

// ── `recipe` — scaffolding is `svrn recipe new`'s ─────────────────────────

/// `recipe new` was a second copy of `svrn recipe new` (same flags, same
/// `corpus_engine::recipe_templates`) and was deleted at pb-ingest-cli. The
/// verb answers where scaffolding lives and exits 2; it never writes a file.
#[test]
fn recipe_points_at_svrn_recipe_new() {
    let tmp = tempfile::tempdir().unwrap();
    let r = run(
        tmp.path(),
        &[
            "recipe",
            "new",
            "--ontology",
            "numismatics",
            "--id",
            "my-coins",
        ],
    );
    assert_eq!(r.code, 2, "stderr: {}", r.stderr);
    assert!(
        r.stderr.contains("svrn recipe new"),
        "the pointer does not name `svrn recipe new`:\n{}",
        r.stderr
    );
    let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
    assert!(left.is_empty(), "the pointer wrote {} file(s)", left.len());
}

// ── pull-if-absent — ingest's `svrn-ingest pull`, named when missing ───────

/// `serve --corpus <absent>` installs through ingest's own CLI
/// (pb-corpus-mcp-reads), so with no `svrn-ingest` reachable it refuses and
/// names the binary and its override, before any endpoint is probed. The
/// binary runs from a directory holding no sibling, with an override that
/// points nowhere and an empty `PATH`: the three places the one locator looks.
#[test]
fn an_absent_corpus_without_the_pull_binary_is_refused_by_name() {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let lone = dir.path().join("corpus-mcp");
    if std::fs::hard_link(env!("CARGO_BIN_EXE_corpus-mcp"), &lone).is_err() {
        std::fs::copy(env!("CARGO_BIN_EXE_corpus-mcp"), &lone).unwrap();
    }
    let data = dir.path().join("data");
    let out = Command::new(&lone)
        .args(["serve", "--corpus", "sep", "--data-dir"])
        .arg(&data)
        .env(
            "SOVEREIGN_INGEST_BIN",
            dir.path().join("no-such-svrn-ingest"),
        )
        .env("PATH", "")
        .env("SOVEREIGN_DAEMON_URL", "http://127.0.0.1:1")
        .output()
        .expect("corpus-mcp did not start");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        out.status.code(),
        Some(0),
        "served an absent corpus:\n{stderr}"
    );
    assert!(
        stderr.contains("svrn-ingest") && stderr.contains("SOVEREIGN_INGEST_BIN"),
        "the refusal does not name the pull binary and its override:\n{stderr}"
    );
    assert!(
        stderr.contains("sep"),
        "the refusal does not name the absent corpus:\n{stderr}"
    );
    assert!(
        !stderr.contains("11434"),
        "the endpoint ladder ran before the local refusal:\n{stderr}"
    );
}

// ── discovery — the ladder, and the rule that a NAMED endpoint is never
//    substituted (ARCH §18.3) ───────────────────────────────────────────────

/// The whole ladder is walked and every rung is NAMED, whichever way it goes.
/// Run against ports nothing serves, so the assertion is on the report rather
/// than on what happened to be up.
#[test]
fn discovery_names_every_rung_it_tried() {
    let tmp = tempfile::tempdir().unwrap();
    let r = run(tmp.path(), &["serve"]);
    assert_ne!(r.code, 0, "serving succeeded with no endpoint anywhere");
    for rung in ["ollama", "llama-server", "oicp daemon"] {
        assert!(
            r.stderr.contains(rung),
            "the ladder did not name the `{rung}` rung:\n{}",
            r.stderr
        );
    }
    assert!(
        r.stderr.contains("11434") && r.stderr.contains("8080"),
        "the ladder did not name the ports it tried:\n{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("no inference endpoint found"),
        "the failure is not reported as an absence:\n{}",
        r.stderr
    );
}

/// The rule that matters most: `--base-url` that does not answer is a REFUSAL
/// carrying that URL's own finding. Falling through to Ollama would serve a
/// different model than the one asked for, successfully and silently.
#[test]
fn a_named_endpoint_is_never_substituted() {
    let tmp = tempfile::tempdir().unwrap();
    let r = run(
        tmp.path(),
        &["serve", "--base-url", "http://127.0.0.1:1/v1"],
    );
    assert_ne!(r.code, 0);
    assert!(
        r.stderr.contains("--base-url") && r.stderr.contains("127.0.0.1:1"),
        "the refusal does not carry the URL that was named:\n{}",
        r.stderr
    );
    for other in ["11434", "8080"] {
        assert!(
            !r.stderr.contains(other),
            "a named endpoint fell through to the ladder (found `{other}`):\n{}",
            r.stderr
        );
    }
}

/// `ingest` moved to ingest's own CLI (pb-ingest-cli). The verb answers where
/// it went and exits 2; it never reports an unknown subcommand and never
/// starts a build.
#[test]
fn ingest_points_at_its_new_home() {
    let tmp = tempfile::tempdir().unwrap();
    let r = run(
        tmp.path(),
        &["ingest", "r.toml", "--base-url", "http://x/v1"],
    );
    assert_eq!(r.code, 2, "stderr: {}", r.stderr);
    assert!(
        r.stderr.contains("svrn ingest"),
        "the pointer does not name the new home:\n{}",
        r.stderr
    );
}
