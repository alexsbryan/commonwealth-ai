// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn init` post-scaffold setup: the git auto-with-confirm flow
//! (`resolve_git` / `GitOutcome`), driven by `super::cmd_init`;
//! `run_git_init` is private. The observation-report renderer moved to the
//! code program with the project model (sovereign-cli-dev
//! `project_cmd::observe`, pb-code-cli-base).
//!
//! Moved with `init` into `sovereign-cli` (2026-08-07). Imports are explicit
//! now: this used to reach `project_cmd`'s plumbing through
//! `use super::super::*`, a glob two levels up that made the real dependency
//! surface invisible — it turned out to be `std::path::Path` and the
//! observation types, nothing else.

use std::path::Path;

// ─── Git auto-with-confirm (step 2a) ────────────────────────────────
//
// A fresh user who forgot to `git init` gets prompted once, kindly,
// with the reasons git unlocks value in the Sovereign workflow. Prior
// behavior was to silently treat git-absence as a "deferred" note in
// the observation report — they'd never see it until much later when
// a git-dependent feature surfaced it. Making the prompt
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
            println!("    \u{2026} git skipped (--no-git). Git-dependent features stay disabled.");
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
    // and git is what the downstream workflow assumes. If the
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

// ─── Language detection ──────────────────────────────────────
