// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn plan` — top-level plan-related commands.
//!
//! Two surfaces:
//!
//! - `svrn plan [--allow-open]` — legacy: derive
//!   IMPLEMENTATION_PLAN.md (delegates to project_cmd::cmd_plan).
//! - `svrn plan validate <path>` — new: lint a plan markdown
//!   file for the six alignment sections (Context, Principles at
//!   stake, What this extends, What this removes, Restraint
//!   patterns, Could this be done with less?). Used by the
//!   PreToolUse hook on ExitPlanMode to gate plan completion.
//!
//! `Principles at stake` (added 2026-08-02) is the architecture
//! gate. The other five are restraint framing — they ask whether
//! the change is *small*, never whether it is *aligned*. A session
//! boots holding a task frame and no architecture, so the moment a
//! design decision crystallises is the last cheap place to ask
//! which numbered `ARCH_PRINCIPLES.md` section governs it. Naming
//! the section forces opening it; recalling a principle from
//! memory is what §11.1 exists to forbid.

use std::path::Path;

pub async fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("validate") => cmd_validate(&args[1..]).await,
        _ => crate::dev_bin::exec("project-plan", args),
    }
}

/// Required H2 sections, in canonical order. Mirrors
/// `~/.claude/plans/_TEMPLATE.md` and the
/// `feedback_plan_alignment_sections.md` memory rule.
const REQUIRED_SECTIONS: &[&str] = &[
    "## Context",
    "## Principles at stake",
    "## What this extends",
    "## What this removes",
    "## Restraint patterns",
    "## Could this be done with less?",
];

async fn cmd_validate(args: &[String]) -> i32 {
    if matches!(
        args.first().map(String::as_str),
        Some("--help" | "-h" | "help")
    ) {
        eprintln!("Usage: svrn plan validate <path>");
        eprintln!();
        eprintln!("Lint a plan markdown file for the six alignment sections.");
        eprintln!("Exit codes:");
        eprintln!("  0  all required sections present");
        eprintln!("  1  one or more sections missing (list on stderr)");
        eprintln!("  2  file unreadable or argument error");
        return 0;
    }
    let Some(path) = args.first() else {
        eprintln!("error: sovereign plan validate <path> — path required");
        return 2;
    };
    let path = Path::new(path);
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", path.display());
            return 2;
        }
    };

    let missing = missing_sections(&body);
    if missing.is_empty() {
        println!(
            "✓ {} has all {} required sections",
            path.display(),
            REQUIRED_SECTIONS.len()
        );
        return 0;
    }

    eprintln!(
        "✗ {} is missing {} required section(s):",
        path.display(),
        missing.len()
    );
    for s in &missing {
        eprintln!("    - {s}");
    }
    eprintln!();
    eprintln!("  Required sections (in any order, but conventionally first):");
    for s in REQUIRED_SECTIONS {
        eprintln!("    {s}");
    }
    if missing.contains(&"## Principles at stake") {
        eprintln!();
        eprintln!("  `## Principles at stake` is the architecture gate. Name the");
        eprintln!("  numbered ARCH_PRINCIPLES.md sections this change touches, and");
        eprintln!("  for each one say how the plan complies — or that it deliberately");
        eprintln!("  deviates, and why. Open the section before you write the line;");
        eprintln!("  citing a principle from memory is what §11.1 forbids. The");
        eprintln!("  \"Which door to open\" table in .claude/CLAUDE.md maps a decision");
        eprintln!("  to its section. \"None — mechanical change\" is a valid answer,");
        eprintln!("  but it is an answer you have to defend, not a blank to skip.");
    }
    eprintln!();
    eprintln!("  Template at ~/.claude/plans/_TEMPLATE.md.");
    eprintln!("  Rationale in feedback_plan_alignment_sections.md.");
    1
}

/// Return the subset of [`REQUIRED_SECTIONS`] that are absent from
/// `body`. Section detection is line-anchored exact match — a
/// heading must appear at the start of its line, optionally with
/// trailing whitespace, exactly as listed.
fn missing_sections(body: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    for &needle in REQUIRED_SECTIONS {
        let found = body.lines().any(|line| line.trim_end() == needle);
        if !found {
            out.push(needle);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_sections_empty_when_all_present() {
        let body = "# title\n\n## Context\n...\n\n## Principles at stake\n...\n\n\
                    ## What this extends\n...\n\n\
                    ## What this removes\n...\n\n## Restraint patterns\n...\n\n\
                    ## Could this be done with less?\n...\n";
        assert!(missing_sections(body).is_empty());
    }

    #[test]
    fn missing_sections_lists_absent_headings() {
        let body = "# title\n## Context\nx\n## What this extends\nx\n";
        let m = missing_sections(body);
        assert_eq!(m.len(), 4);
        assert!(m.contains(&"## Principles at stake"));
        assert!(m.contains(&"## What this removes"));
        assert!(m.contains(&"## Restraint patterns"));
        assert!(m.contains(&"## Could this be done with less?"));
    }

    /// The architecture gate is the reason this validator was
    /// extended (2026-08-02). A plan that satisfies every restraint
    /// section but names no principle must still fail — restraint
    /// asks whether the change is small, never whether it is aligned.
    #[test]
    fn restraint_sections_alone_do_not_satisfy_the_architecture_gate() {
        let body = "# title\n\n## Context\nx\n\n## What this extends\nx\n\n\
                    ## What this removes\nx\n\n## Restraint patterns\nx\n\n\
                    ## Could this be done with less?\nx\n";
        assert_eq!(missing_sections(body), vec!["## Principles at stake"]);
    }

    #[test]
    fn missing_sections_ignores_partial_or_decorated_headings() {
        // `## Context-something-else` is not the same as `## Context`.
        let body = "## Context-extra\n";
        assert!(missing_sections(body).contains(&"## Context"));
        // Trailing whitespace is fine.
        let body = "## Context   \n";
        assert!(!missing_sections(body).contains(&"## Context"));
    }
}
