// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cargo xtask concept-gate` — the concept-duplication ratchet, relayed.
//!
//! One noun, one owner. The ratchet number is "how many names are defined as a
//! type in more than one first-party crate, WHERE at least two of those crates'
//! definitions are already referenced across a crate boundary", frozen in
//! `quality/baselines/concepts.txt`; the gate is red when that number RISES,
//! which is a duplicate ADDED. It fires on additions by construction and never
//! on pre-existing code — the whole register is already inside the baseline.
//!
//! The reachability clause landed 2026-08-21 and re-minted the baseline in the
//! same commit. Before it, 87% of the rows named a collision between two local
//! helpers that no amount of adoption could retire; see
//! `corpus_engine_scip::converge::cross_crate_reached`. A baseline stamped
//! before that commit is not comparable to one stamped after it (279 -> 33). The relayed
//! `--json` body carries `colliding_names` (every collision) beside
//! `duplicated_names` (the countable ones), so the narrowing is visible here
//! rather than only in the tool that applies it.
//!
//! This gate does NOT recount. `svrn code converge status` owns the number and
//! this is a relay (§10.6, one decider): the count comes from the SCIP graph
//! via `corpus_engine_scip::converge::duplicate_count`, and a second
//! implementation living here would be a specimen of the disease the register
//! exists to cure. xtask stays std-plus-three-crates on purpose, so the relay
//! is a process call to the already-built sibling rather than a link.
//!
//! ADVISORY inside `cargo xtask quality`, and the reason is in the mechanism:
//! the number is derived from the graph at the LAST INDEXED COMMIT, not from
//! the working tree this habit-run is gating. Failing a pre-push run for a
//! duplicate the developer cannot see yet — or for an indexer that is eight
//! minutes behind — is the false-positive machine that gets a gate switched off
//! inside a week. It is a HARD gate where the graph is authoritative: CI and
//! every landing verdict call `svrn code converge status` directly and gate on
//! its exit code.
//!
//! Four verdicts, not two (§18.2). The sibling binary reports 0 pass, 1 a
//! duplicate was added, 3 the graph cannot speak for this commit, 4 no baseline
//! yet — and a MISSING sibling is 4 as well, never a silent zero.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common::{baselines_dir, repo_root};

/// Exit codes, spelled once. Mirrors `converge_cmd::cmd_status`.
const PASS: i32 = 0;
const ADDED: i32 = 1;
const CANNOT_JUDGE: i32 = 3;
const NEVER_RAN: i32 = 4;

const BUILD_CMD: &str = "cargo build -p sovereign-cli --features dev-tools -p sovereign-cli-dev \
     -p sovereign-cli-daemon -p sovereign-cli-llm";

pub fn run(args: &[String]) -> i32 {
    let root = repo_root();
    let baseline = baselines_dir(&root).join("concepts.txt");
    let flags = crate::common::baseline_flags(args);

    let Some(cli) = locate_sibling(&root) else {
        eprintln!(
            "NEVER-RAN — `sovereign-cli-dev` is not built, so the concept count was never taken.\n\
             This is not a pass and it is not a zero. Build it (DEBUG — the deployed symlink\n\
             points at target/debug, so a release-only build is invisible here):\n  \
             {BUILD_CMD}"
        );
        return NEVER_RAN;
    };

    // `--update-baseline` snapshots the current number; `--tighten` banks an
    // improvement only. Same contract as the five sibling gates, so a habit-run
    // can never silently move a ratchet.
    if flags.update {
        return mint(&cli, &root, &baseline);
    }

    let out = match invoke(&cli, &root, &baseline, &["--json"]) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("NEVER-RAN — could not run {}: {e}", cli.display());
            return NEVER_RAN;
        }
    };
    let code = out.code;
    let body: serde_json::Value =
        serde_json::from_str(&out.stdout).unwrap_or(serde_json::Value::Null);

    let n = body.get("duplicated_names").and_then(|v| v.as_u64());
    let prior = body.get("baseline").and_then(|v| v.as_u64());
    let delta = body.get("delta").and_then(|v| v.as_i64());
    // A response with no graph-lag field is a NUMBER WITHOUT A COMMIT, and
    // rendering PASS on it is the silent substitution this gate exists to
    // prevent (§18.3) — observed live 2026-08-20, which is why this arm exists.
    //
    // THE KEY IS `graph_lag`, and this arm read `freshness` from the day it
    // was written until 2026-08-27. `converge status --json` has always
    // published `graph_lag` (converge_cmd.rs, where the JSON key mirrors the
    // Rust field); `freshness` is what the SIBLING `redirect` command spells
    // the same `lag.verdict_word()` as, and the arm was written against that
    // spelling. So the gate could never reach a verdict on any sibling, ever,
    // and its own error text sent every reader to a two-minute rebuild that
    // could not possibly fix it. Watched failing, then watched passing, before
    // this line changed (§18.1: a gate you have not watched fail is not a
    // gate). The two spellings for one concept are ARCH §10.6's own smell and
    // are why `freshness` is NOT accepted here as a fallback — one decider,
    // one name, and the name is the one the relayed command publishes.
    let Some(freshness) = body.get("graph_lag").and_then(|v| v.as_str()) else {
        eprintln!(
            "COULD-NOT-JUDGE — {} reported a count with no `graph_lag` field, so which\n\
             commit the number is about is unknown. Either the sibling predates graph-lag\n\
             reporting, or it renamed the key out from under this relay — check\n\
             `converge status --json` by hand before rebuilding:\n  {BUILD_CMD}",
            cli.display()
        );
        return CANNOT_JUDGE;
    };
    let note = body
        .get("freshness_note")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match (n, prior, delta) {
        (Some(n), Some(p), Some(d)) => {
            eprintln!("duplicated names: {n}   baseline: {p}   delta: {d:+}   graph: {freshness}");
        }
        _ => {
            // The relay could not read a number out of its own tool. Say that,
            // rather than rendering a verdict on nothing (§18.3).
            eprintln!(
                "COULD-NOT-JUDGE — `converge status --json` returned no number (exit {code}).\n\
                 stdout: {}\n  stderr: {}",
                truncate(&out.stdout),
                truncate(&out.stderr)
            );
            return CANNOT_JUDGE;
        }
    }
    if !note.is_empty() {
        for line in note.lines() {
            eprintln!("  {line}");
        }
    }

    if flags.tighten {
        return tighten(&cli, &root, &baseline, delta.unwrap_or(0));
    }

    match code {
        PASS => PASS,
        ADDED => {
            eprintln!(
                "\nRATCHET BROKEN — a concept name is now defined as a type in one more crate\n\
                 than the baseline allows. Find it and decide:\n  \
                 sovereign code converge census --limit 0\n  \
                 sovereign code converge noun <Name>\n\
                 Converge it onto one owner, or rename it apart and say which in the\n\
                 landing verdict. Only if the rise is intentional:\n  \
                 cargo run -p xtask -- concept-gate --update-baseline"
            );
            ADDED
        }
        CANNOT_JUDGE => {
            eprintln!(
                "\nCOULD-NOT-JUDGE — the graph cannot speak for this commit (see the line above)."
            );
            CANNOT_JUDGE
        }
        NEVER_RAN => {
            eprintln!(
                "\nNEVER-RAN — no baseline yet. Mint one:\n  \
                 cargo run -p xtask -- concept-gate --update-baseline"
            );
            NEVER_RAN
        }
        other => {
            eprintln!("\nCOULD-NOT-JUDGE — `converge status` exited {other}, which this gate does not model.");
            CANNOT_JUDGE
        }
    }
}

fn mint(cli: &Path, root: &Path, baseline: &Path) -> i32 {
    match invoke(cli, root, baseline, &["--mint"]) {
        Ok(o) => {
            eprint!("{}", o.stdout);
            eprint!("{}", o.stderr);
            o.code
        }
        Err(e) => {
            eprintln!("could not run {}: {e}", cli.display());
            NEVER_RAN
        }
    }
}

/// Bank an improvement only — never adds, never raises (the ratchet contract).
fn tighten(cli: &Path, root: &Path, baseline: &Path, delta: i64) -> i32 {
    if delta >= 0 {
        eprintln!("nothing to tighten (delta {delta:+})");
        return PASS;
    }
    eprintln!("tightening: {delta} fewer duplicated names");
    mint(cli, root, baseline)
}

struct Output {
    code: i32,
    stdout: String,
    stderr: String,
}

fn invoke(cli: &Path, root: &Path, baseline: &Path, extra: &[&str]) -> Result<Output, String> {
    // NAME THE CORPUS. `converge status --corpus-id` defaults to "the sole
    // indexed code corpus", which is a default that expires the moment anyone
    // indexes a second one. On 2026-08-29 this host carried three
    // (commonwealth-ai, semver, tinyorders — two of them fixtures), so the
    // relayed command refused with "multiple code corpora — pass --corpus-id",
    // and the gate reported COULD-NOT-JUDGE blaming a stale sibling: it sent
    // the reader to a four-crate rebuild that could not possibly help, because
    // the sibling was fine and the QUESTION was ambiguous. That is the second
    // time this arm's message has named the wrong cause (see the `graph_lag`
    // note above), and both times the cost was a rebuild.
    //
    // The repo directory name is the corpus id by convention here.
    // SOVEREIGN_CONCEPT_CORPUS overrides for a repo indexed under another name.
    let corpus = std::env::var("SOVEREIGN_CONCEPT_CORPUS").ok().or_else(|| {
        root.file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    });
    let mut cmd = Command::new(cli);
    cmd.args(["code", "converge", "status"])
        .arg("--baseline")
        .arg(baseline);
    if let Some(id) = corpus.as_deref() {
        cmd.arg("--corpus-id").arg(id);
    }
    let out = cmd
        .args(extra)
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    Ok(Output {
        // A signal-killed child has no code; that is not a pass either.
        code: out.status.code().unwrap_or(NEVER_RAN),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// The sibling that owns the `code` verb. Debug first: the deployed symlink
/// points at `target/debug/sovereign-cli`, so a release-only build is invisible
/// to the toolchain actually running here (AGENTS.md, build-profile note).
fn locate_sibling(root: &Path) -> Option<PathBuf> {
    [
        "target/debug/sovereign-cli-dev",
        "target/release/sovereign-cli-dev",
    ]
    .iter()
    .map(|rel| root.join(rel))
    .find(|p| p.is_file())
}

fn truncate(s: &str) -> String {
    let t = s.trim();
    if t.len() <= 300 {
        t.to_string()
    } else {
        format!("{}…", &t[..300])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Debug wins when both exist — a release-only build is invisible to the
    /// symlink this host actually runs, so preferring it would silently gate on
    /// a different binary than the one under test.
    #[test]
    fn sibling_lookup_prefers_debug_and_reports_absence() {
        let dir = std::env::temp_dir().join(format!("xtask-concept-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // Absent is None — never a fabricated path, which would turn a missing
        // binary into a silent zero (§18.3).
        assert!(locate_sibling(&dir).is_none());

        std::fs::create_dir_all(dir.join("target/release")).expect("mk release");
        std::fs::write(dir.join("target/release/sovereign-cli-dev"), b"x").expect("write release");
        assert_eq!(
            locate_sibling(&dir),
            Some(dir.join("target/release/sovereign-cli-dev"))
        );

        std::fs::create_dir_all(dir.join("target/debug")).expect("mk debug");
        std::fs::write(dir.join("target/debug/sovereign-cli-dev"), b"x").expect("write debug");
        assert_eq!(
            locate_sibling(&dir),
            Some(dir.join("target/debug/sovereign-cli-dev"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tighten_never_raises() {
        let missing = Path::new("/nonexistent/sovereign-cli-dev");
        // delta 0 and delta +1 must not reach the mint path at all — if they
        // did, this would fail trying to exec a binary that is not there.
        assert_eq!(tighten(missing, Path::new("/"), Path::new("/b"), 0), PASS);
        assert_eq!(tighten(missing, Path::new("/"), Path::new("/b"), 3), PASS);
    }
}
