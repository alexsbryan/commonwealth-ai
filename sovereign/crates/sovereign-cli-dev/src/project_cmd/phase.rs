// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn project phase` — PHASES.md progression (advisory in Phase 6).
//!
//! Parses PHASES.md, runs or manually verifies a phase's stop condition,
//! writes `phase-N.md`, advances `lifecycle.current_phase`, and logs a
//! decision note. Self-contained: no other command refuses to run on
//! `current_phase`. Split out of `project_cmd` (2026-07-13); pure move.
//! Shared helpers (`find_repo_root`, `today_iso`,
//! `git_committer_identity_for_amend`) resolve through `use super::*`.

use super::*;

// ─── sovereign project phase (M7.1) ──────────────────────────
//
// Phase progression: parse PHASES.md, run or manually verify a
// phase's stop condition, write phase-N.md, advance
// lifecycle.current_phase, write a decision note. The artifact
// trail closes the "was Phase N actually verified?" gap.
//
// **Phase 6 status: advisory.** No other command in the
// codebase refuses to run because of `lifecycle.current_phase` —
// the phase machinery is self-contained (it manages its own
// PHASES.md artifact and increments its own counter on `pass`).
// Users who want explicit phase progression keep using this
// surface; users who don't can write a spec, commit, and work
// without ever invoking it. `cmd_audit` reads the phase table
// for display only; the approval_gate / MCP tools do not consult
// `current_phase` at all.

pub(crate) async fn cmd_phase(args: &[String]) -> i32 {
    let Some(sub) = args.first().cloned() else {
        eprintln!("phase: missing subcommand (status | pass)");
        return 2;
    };
    let rest = &args[1..];
    match sub.as_str() {
        "status" => cmd_phase_status(rest).await,
        "pass" => cmd_phase_pass(rest).await,
        "--help" | "-h" => {
            println!("svrn project phase <status|pass [N]>");
            println!();
            println!("status       Show current phase and what's next per PHASES.md");
            println!("pass [N]     Run (or manually confirm) Phase N's stop condition,");
            println!("             write phase-N.md, advance lifecycle.current_phase.");
            println!("             Default N = current_phase + 1.");
            0
        }
        other => {
            eprintln!("phase: unknown subcommand '{other}'");
            2
        }
    }
}

pub(crate) async fn cmd_phase_status(_args: &[String]) -> i32 {
    let (repo_root, project_toml, _) = match load_phase_context() {
        Ok(c) => c,
        Err(code) => return code,
    };
    let phases_path = crate::phases::phases_md_path(&repo_root);
    let md = match std::fs::read_to_string(&phases_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("phase status: cannot read PHASES.md: {e}");
            return 1;
        }
    };
    let phases = crate::phases::parse_phases(&md);
    println!();
    println!("  Phase progression");
    println!("  {}", "─".repeat(54));
    println!();
    println!("  current_phase = {}", project_toml.lifecycle.current_phase);
    println!();
    for p in &phases {
        let marker = if p.deferred {
            "⋯"
        } else if p.ordinal < project_toml.lifecycle.current_phase {
            "✓"
        } else if p.ordinal == project_toml.lifecycle.current_phase {
            "▶"
        } else {
            " "
        };
        println!("  {marker} {}", p.heading);
        if !p.stop_text.is_empty() {
            println!("      stop: {}", p.stop_text);
        }
    }
    println!();
    let next = phases
        .iter()
        .find(|p| !p.deferred && p.ordinal > project_toml.lifecycle.current_phase);
    match next {
        Some(p) => println!("  Next: `svrn project phase pass {}`", p.ordinal),
        None => println!("  All numbered phases complete."),
    }
    0
}

pub(crate) async fn cmd_phase_pass(args: &[String]) -> i32 {
    let (repo_root, mut project_toml, project_toml_path) = match load_phase_context() {
        Ok(c) => c,
        Err(code) => return code,
    };

    let phases_path = crate::phases::phases_md_path(&repo_root);
    let md = match std::fs::read_to_string(&phases_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("phase pass: cannot read PHASES.md: {e}");
            return 1;
        }
    };
    let phases = crate::phases::parse_phases(&md);

    // Resolve which phase to pass. Positional arg OR default to
    // current_phase + 1 (since `found` seeds current_phase = 0,
    // the first pass targets Phase 0 only once — after that we
    // advance linearly).
    let target_ordinal: u32 = if let Some(s) = args.first() {
        match s.parse() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("phase pass: ordinal must be an integer, got '{s}'");
                return 2;
            }
        }
    } else if project_toml.lifecycle.current_phase == 0 && !already_passed(&repo_root, 0) {
        // Haven't passed phase 0 yet.
        0
    } else {
        project_toml.lifecycle.current_phase + 1
    };

    let phase = match phases
        .iter()
        .find(|p| !p.deferred && p.ordinal == target_ordinal)
    {
        Some(p) => p.clone(),
        None => {
            eprintln!(
                "phase pass: no numbered Phase {target_ordinal} in PHASES.md. \
                 Deferred phases (3+) aren't passed via this command — add them \
                 via `svrn project amend` first, or use a different ordinal."
            );
            return 1;
        }
    };

    // Decide execution mode. Heuristic guard: the naive "single
    // backticked block" parse mis-extracts a quoted dep name
    // (`reqwest`) from prose. Treat any extracted "command" that
    // doesn't contain a space OR a shell operator as suspicious
    // and drop to manual confirmation.
    let executable = phase
        .stop_command
        .as_ref()
        .filter(|c| looks_shell_runnable(c));

    println!();
    println!("  Phase pass: {}", phase.heading);
    println!("  {}", "─".repeat(54));
    println!();
    if phase.stop_text.is_empty() {
        println!("  No stop condition recorded for this phase.");
    } else {
        println!("  Stop condition: {}", phase.stop_text);
    }
    println!();

    let outcome = match executable {
        Some(cmd) => {
            println!("  Running: {cmd}");
            let out = crate::phases::run_stop_command(cmd);
            println!(
                "  {} exit={} duration={}ms",
                if out.passed {
                    "\u{2713} PASSED"
                } else {
                    "\u{2717} FAILED"
                },
                out.exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into()),
                out.duration_ms
            );
            out
        }
        None => {
            // Manual confirmation path.
            println!("  No single unambiguous shell command extracted from the stop text.");
            println!("  Manual verification — run the stop condition yourself, then answer here.");
            print!("  Did it pass? [y/N] ");
            use std::io::Write;
            let _ = std::io::stdout().flush();
            let answer = crate::found::stdin_read_line().to_lowercase();
            let passed = matches!(answer.chars().next(), Some('y'));
            crate::phases::PhasePassOutcome {
                passed,
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: 0,
                verification: crate::phases::Verification::ManualConfirm,
            }
        }
    };

    let date = today_iso();
    let committer = git_committer_identity_for_amend(&repo_root)
        .unwrap_or_else(|| "<unknown committer>".to_string());
    let report = crate::phases::render_phase_report(&phase, &outcome, &date, &committer);
    let report_path = crate::phases::phase_report_path(&repo_root, phase.ordinal);
    if let Err(e) = std::fs::write(&report_path, &report) {
        eprintln!("phase pass: could not write {}: {e}", report_path.display());
        return 1;
    }
    println!("    \u{2713} {}", report_path.display());

    // Durable decision note. Captures enough for `audit` to build
    // a rollup without re-parsing the phase artifact.
    let sovereign_dir = repo_root.join(".sovereign");
    if let Ok(note_store) = corpus_engine_notes::NoteStore::open(&sovereign_dir.join("notes.db")) {
        let verdict = if outcome.passed { "PASSED" } else { "FAILED" };
        let body = format!(
            "Phase {} · {}\n\nVerdict: {}\nVerification: {}\nDate: {}\nCommitter: {}\n\nStop condition:\n{}\n",
            phase.ordinal,
            phase.heading,
            verdict,
            match &outcome.verification {
                crate::phases::Verification::RanCommand { command } => {
                    format!("ran `{command}` (exit {})", outcome.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()))
                }
                crate::phases::Verification::ManualConfirm => "manually confirmed".into(),
            },
            date,
            committer,
            if phase.stop_text.is_empty() {
                "_(none)_"
            } else {
                phase.stop_text.as_str()
            },
        );
        let rt = tokio::runtime::Handle::current();
        let _ = tokio::task::block_in_place(|| {
            rt.block_on(note_store.write_note_scoped(
                "decision",
                &body,
                Vec::new(),
                Vec::new(),
                &format!("phase-{}-pass", phase.ordinal),
                corpus_engine_notes::NoteScope::Global,
                None,
            ))
        });
    }

    // Advance current_phase ONLY when the run passed. Failing a
    // phase is a recorded artifact (phase-N.md exists) but
    // doesn't advance the counter — the operator retries.
    if outcome.passed {
        if phase.ordinal >= project_toml.lifecycle.current_phase {
            project_toml.lifecycle.current_phase = phase.ordinal + 1;
            if let Err(e) = project_toml.write(&project_toml_path) {
                eprintln!("phase pass: could not update project.toml: {e}");
                return 1;
            }
            println!(
                "    \u{2713} project.toml: current_phase = {}",
                project_toml.lifecycle.current_phase
            );
        }
        0
    } else {
        println!();
        println!(
            "  Phase {} FAILED — current_phase unchanged.",
            phase.ordinal
        );
        println!(
            "  Read `{}` for the captured output.",
            report_path.display()
        );
        1
    }
}

/// Load the usual project triple: repo root, parsed project.toml,
/// project.toml path. Returns a reusable `Err(i32)` exit code
/// when any precondition fails.
fn load_phase_context() -> Result<(PathBuf, crate::project_toml::ProjectTomlFile, PathBuf), i32> {
    let repo_root = find_repo_root()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let project_toml_path = repo_root.join(".sovereign").join("project.toml");
    if !project_toml_path.exists() {
        eprintln!();
        eprintln!(
            "  sovereign project phase: no .sovereign/project.toml found.\n\
             Run `svrn project init` then `svrn project found` first."
        );
        return Err(1);
    }
    let project_toml = match crate::project_toml::ProjectTomlFile::read(&project_toml_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  sovereign project phase: cannot read project.toml: {e}");
            return Err(1);
        }
    };
    if !project_toml.lifecycle.founded {
        eprintln!();
        eprintln!(
            "  sovereign project phase: this project hasn't been founded yet.\n\
             Run `svrn project found` first — PHASES.md is produced at founding."
        );
        return Err(1);
    }
    Ok((repo_root, project_toml, project_toml_path))
}

/// Heuristic: does this extracted "command" look like a shell
/// invocation, or is it likely a quoted identifier from prose?
/// A real command usually has a space (for args), a shell
/// operator (`&&`, `||`, `;`, `|`, `>`, `$`), or a path hint.
fn looks_shell_runnable(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return false;
    }
    t.contains(' ')
        || t.contains("&&")
        || t.contains("||")
        || t.contains(';')
        || t.contains('|')
        || t.contains('>')
        || t.contains('$')
        || t.starts_with("./")
        || t.starts_with('/')
        || t.starts_with("cargo ")
        || t.starts_with("npm ")
        || t.starts_with("go ")
        || t.starts_with("python ")
        || t.starts_with("pytest")
        || t.starts_with("make")
}

fn already_passed(repo_root: &Path, ordinal: u32) -> bool {
    crate::phases::phase_report_path(repo_root, ordinal).exists()
}
