// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn project init` post-scaffold setup: the git auto-with-confirm flow
//! (`resolve_git` / `GitOutcome`) and the observation-report renderer
//! (`print_observation_report` / `ObservationReportContext`). Both are
//! driven by `super::cmd_init`; `run_git_init` is private. Split out of
//! `project_cmd` (2026-07-13); pure move. Reaches the shared plumbing in
//! `project_cmd` via `use super::super::*`.

use super::super::*;

// ─── Git auto-with-confirm (step 2a) ────────────────────────────────
//
// A fresh user who forgot to `git init` gets prompted once, kindly,
// with the reasons git unlocks value in the Sovereign workflow. Prior
// behavior was to silently treat git-absence as a "deferred" note in
// the observation report — they'd never see it until much later when
// an ATOS feature needed git to gate something. Making the prompt
// explicit up-front is the difference between "tool feels
// suffocating" and "tool is a collaborator."

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GitOutcome {
    Present,
    InitializedNow,
    DeclinedByUser,
    DeclinedPreviously,
    NonInteractiveSkipped,
}

pub(super) fn resolve_git(
    repo_root: &Path,
    has_git: bool,
    override_flag: Option<bool>,
    design_exists: bool,
    git_declined_previously: bool,
) -> GitOutcome {
    if has_git {
        return GitOutcome::Present;
    }

    match override_flag {
        Some(true) => {
            run_git_init(repo_root);
            return if repo_root.join(".git").exists() {
                GitOutcome::InitializedNow
            } else {
                eprintln!("    \u{2717} --yes-git set but `git init` did not create .git/");
                GitOutcome::NonInteractiveSkipped
            };
        }
        Some(false) => {
            println!();
            println!(
                "    \u{2026} git skipped (--no-git). ATOS features that need git stay disabled."
            );
            return GitOutcome::DeclinedByUser;
        }
        None => {}
    }

    // Respect a previous declination — don't re-badger the user on
    // every subsequent `init`. They already said no; it sticks.
    if git_declined_previously {
        return GitOutcome::DeclinedPreviously;
    }

    // Non-TTY stdin (piped / CI) without explicit flag: auto-init.
    // The rationale: scripts running `svrn project init` in
    // fresh repos are almost always setting up a dev environment,
    // and git is what every downstream ATOS command assumes. If the
    // user truly wants no git, they pass --no-git.
    if !sovereign_cli_shared::prompts::stdin_is_tty() {
        println!();
        println!(
            "    No git repo; initializing (non-interactive default — pass --no-git to opt out)."
        );
        run_git_init(repo_root);
        return if repo_root.join(".git").exists() {
            GitOutcome::InitializedNow
        } else {
            GitOutcome::NonInteractiveSkipped
        };
    }

    // Interactive prompt. Kind, specific, and (when a design doc is
    // imminent) names the single most concrete win: per-revision
    // diffs of the DESIGN.md the user is about to author.
    eprintln!();
    eprintln!("  No git repo here yet. Sovereign works without one, but git unlocks:");
    if design_exists {
        eprintln!("    \u{00b7} per-revision diff of your DESIGN.md as you iterate with the agent");
    } else {
        eprintln!("    \u{00b7} per-revision diff of your DESIGN.md + CHARTER.md as they evolve");
    }
    eprintln!("    \u{00b7} `atos feature approve` gates");
    eprintln!("    \u{00b7} amendment history that survives machine changes");
    eprintln!();

    let accept = sovereign_cli_shared::prompts::confirm("  Run `git init` here now?", true);
    if accept {
        run_git_init(repo_root);
        if repo_root.join(".git").exists() {
            eprintln!("    \u{2713} Initialized git repo.");
            GitOutcome::InitializedNow
        } else {
            eprintln!("    \u{2717} `git init` did not create .git/ — continuing without git.");
            GitOutcome::NonInteractiveSkipped
        }
    } else {
        eprintln!(
            "    \u{2026} git skipped. Run `git init` manually later if you change your mind."
        );
        GitOutcome::DeclinedByUser
    }
}

fn run_git_init(repo_root: &Path) {
    let status = std::process::Command::new("git")
        .arg("init")
        .current_dir(repo_root)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("    \u{2717} `git init` exited with status {s}");
        }
        Err(e) => {
            eprintln!("    \u{2717} could not spawn `git init`: {e} (is git installed?)");
        }
    }
}

/// Contextual flags the report uses to decide whether a missing
/// toolchain / git is ACTIONABLE (fix it now) or DEFERRED (we know
/// why it's fine). Keeps `print_observation_report` side-effect free
/// while letting `cmd_init` pass in what it knows.
pub(super) struct ObservationReportContext {
    /// True when `<repo>/DESIGN.md` exists. A pre-code project with a
    /// design doc is a legitimate state — "no languages detected"
    /// becomes "indexing deferred" rather than an actionable error.
    pub(super) design_exists: bool,
    /// User declined git (either this session or previously).
    /// Suppresses the "no git" deferred-bucket nudge — they already
    /// made the call.
    pub(super) git_declined: bool,
}

pub(super) fn print_observation_report(
    obs: &crate::observation::ProjectObservation,
    ctx: &ObservationReportContext,
) {
    use crate::observation::{DepKind, ScipTooling};

    let mut ready: Vec<String> = Vec::new();
    let mut actionable: Vec<(String, &'static str)> = Vec::new();
    let mut deferred: Vec<String> = Vec::new();

    // Languages & SCIP tooling. On a pre-code project with a design
    // doc present, "no languages" is expected — soft-path the
    // warning into the deferred bucket instead of treating it as a
    // gap the user must close right now.
    if obs.languages.is_empty() {
        if ctx.design_exists {
            deferred.push(
                "Pre-code project (DESIGN.md present, no source yet). Language detection runs on the next init."
                    .into(),
            );
        } else {
            actionable.push((
                "No supported languages detected (Rust, TypeScript, JavaScript, Go, Python, Java)."
                    .into(),
                "",
            ));
        }
    } else {
        for lang in &obs.languages {
            match &lang.scip_tooling {
                ScipTooling::Available { binary } => {
                    ready.push(format!("{} ({binary} on PATH)", lang.display));
                }
                ScipTooling::NotRequired => {
                    ready.push(lang.display.clone());
                }
                ScipTooling::Missing {
                    binary,
                    install_cmd,
                } => {
                    actionable.push((
                        format!(
                            "{} detected. Call-graph navigation requires `{binary}`:",
                            lang.display
                        ),
                        *install_cmd,
                    ));
                }
            }
        }
    }

    if obs.has_git {
        ready.push("Git repository".into());
    } else if !ctx.git_declined {
        // Not an actionable gap in the strict sense — git is
        // optional for init — but worth surfacing so the user knows
        // approvals-via-git won't be available.
        //
        // When the user explicitly declined git (now or previously),
        // suppress this note — they've already seen the tradeoff.
        deferred.push("No git repository — `atos feature approve` covers the gap.".into());
    }

    if obs.embed_model_available {
        ready.push("Embed model".into());
    } else {
        actionable.push((
            "Embed model not found (documentation search will be degraded).".into(),
            "svrn setup",
        ));
    }

    // External dependencies — noted for `project found` (Stage 2
    // fault lines draws on this list). Not resolved at init time.
    let direct_deps: Vec<&crate::observation::DetectedDependency> = obs
        .deps
        .iter()
        .filter(|d| d.kind == DepKind::Direct)
        .collect();
    if !direct_deps.is_empty() {
        let n = direct_deps.len();
        deferred.push(format!(
            "{n} direct external dependenc{y} detected — surfaced to `svrn project found`.",
            y = if n == 1 { "y" } else { "ies" }
        ));
    }

    // Render.
    if !ready.is_empty() {
        println!();
        for r in &ready {
            println!("    \u{2713} {r}");
        }
    }

    if !actionable.is_empty() {
        println!();
        for (desc, cmd) in &actionable {
            println!("    \u{26a0} {desc}");
            if !cmd.is_empty() {
                println!();
                println!("{cmd}");
                println!();
            }
        }
    }

    if !deferred.is_empty() {
        println!();
        for d in &deferred {
            println!("    \u{2026} {d}");
        }
    }
}

// ─── Language detection ──────────────────────────────────────
