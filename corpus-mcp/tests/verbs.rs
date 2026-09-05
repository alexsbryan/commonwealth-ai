// SPDX-License-Identifier: AGPL-3.0-or-later
//! The three verbs of `EPISTEMIC_INDEX.md` §4, and every refusal they promise.
//!
//! ## Why this file exists rather than a `cli-contract.toml` row
//!
//! `sovereign contract census` — the surface that asks "is the CLI I just
//! changed covered by anything?" — cannot represent these verbs, and that is
//! structural, not an oversight:
//!
//! - `sovereign/docs/cli-contract.toml` line 1 declares itself "the single
//!   source of truth for the commands `sovereign` promises";
//! - `Contract`'s `binary` field is a closed enum of the four sovereign
//!   siblings (`sovereign-cli-shared/src/cli_contract.rs:94` — Dispatcher |
//!   Dev | Llm | Daemon), and `path` is documented as "argv path minus
//!   `sovereign`";
//! - a `[[journey.step]]`'s `run` is argv handed to the `svrn` runner.
//!
//! `corpus-mcp` is a separate binary that `sovereign-cli` never dispatches, so
//! a row for `corpus recipe new` is unrepresentable without widening the enum,
//! the path semantics and `cli-journey-verify.sh`. (`corpus-mcp ingest`, landed
//! at ei-5b, has no row either, for the same reason.) Widening a shared
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

// ── `recipe new` ────────────────────────────────────────────────────────────

#[test]
fn recipe_new_lists_the_builtin_templates() {
    let tmp = tempfile::tempdir().unwrap();
    let r = run(tmp.path(), &["recipe", "new", "--ontology", "list"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    // Not a count: the set is generated from directories, so asserting on a
    // number would break every time a template lands. Two that the spec's own
    // worked examples name.
    for name in ["numismatics", "literary"] {
        assert!(
            r.stdout.lines().any(|l| l.trim() == name),
            "`--ontology list` did not name `{name}`:\n{}",
            r.stdout
        );
    }
}

#[test]
fn recipe_new_writes_id_dot_toml_with_the_id_substituted() {
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
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let written = tmp.path().join("my-coins.toml");
    assert!(written.is_file(), "no my-coins.toml:\n{}", r.stdout);
    let text = std::fs::read_to_string(&written).unwrap();
    assert!(
        text.contains("id = \"my-coins\""),
        "the id was not substituted into the scaffold:\n{text}"
    );
    // `path = "REPLACE_ME"` SURVIVES on purpose and this asserts that it does.
    // `instantiate` substitutes identity (`id`, `name`) and nothing else; the
    // source path is the one thing only the author knows, and a scaffold that
    // guessed it would produce a recipe that ingests the wrong directory
    // without ever asking. The stdout line below points at the fields to fill.
    assert!(
        text.contains("path = \"REPLACE_ME\""),
        "the scaffold no longer leaves the source path for the author:\n{text}"
    );
    // The next command in the sequence, named. §4 is three commands and a
    // person should not have to guess the second one.
    assert!(
        r.stdout.contains("corpus-mcp ingest"),
        "the scaffold does not point at the next command:\n{}",
        r.stdout
    );
}

/// The one destructive mistake this verb could make. A scaffold that clobbers
/// eats the types the author just spent an hour writing.
#[test]
fn recipe_new_never_overwrites() {
    let tmp = tempfile::tempdir().unwrap();
    let args = [
        "recipe",
        "new",
        "--ontology",
        "numismatics",
        "--id",
        "my-coins",
    ];
    assert_eq!(run(tmp.path(), &args).code, 0);
    std::fs::write(tmp.path().join("my-coins.toml"), "MINE\n").unwrap();
    let r = run(tmp.path(), &args);
    assert_eq!(r.code, 1, "a second scaffold did not refuse:\n{}", r.stdout);
    assert!(
        r.stderr.contains("never overwrites"),
        "the refusal does not say why:\n{}",
        r.stderr
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("my-coins.toml")).unwrap(),
        "MINE\n",
        "the refusal still overwrote the file"
    );
}

/// ARCH §4: an unknown id is LOUD and names the whole set.
#[test]
fn recipe_new_refuses_an_unknown_ontology_and_names_the_real_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let r = run(tmp.path(), &["recipe", "new", "--ontology", "nope"]);
    assert_ne!(r.code, 0, "an unknown template exited 0");
    assert!(
        r.stderr.contains("nope") && r.stderr.contains("numismatics"),
        "the error names neither the bad id nor the available ones:\n{}",
        r.stderr
    );
}

/// No `--id` and no `--out` means no name to give a file — so it goes to
/// stdout rather than inventing `REPLACE_ME.toml` in someone's directory.
#[test]
fn recipe_new_without_an_id_writes_nothing_to_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let r = run(tmp.path(), &["recipe", "new", "--ontology", "literary"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    assert!(
        r.stdout.contains("[corpus]"),
        "no recipe on stdout:\n{}",
        r.stdout
    );
    let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
    assert!(left.is_empty(), "stdout form wrote {} file(s)", left.len());
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
