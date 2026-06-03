//! `sovereign plan` — top-level plan-related commands.
//!
//! Two surfaces:
//!
//! - `sovereign plan [--allow-open]` — legacy: derive
//!   IMPLEMENTATION_PLAN.md (delegates to project_cmd::cmd_plan).
//! - `sovereign plan validate <path>` — new: lint a plan markdown
//!   file for the four alignment sections (Context, What this
//!   extends, What this removes, Restraint patterns, Could this
//!   be done with less?). Used by the PreToolUse hook on
//!   ExitPlanMode to gate plan completion.

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
        eprintln!("Usage: sovereign plan validate <path>");
        eprintln!();
        eprintln!("Lint a plan markdown file for the four alignment sections.");
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
        println!("✓ {} has all 5 required sections", path.display());
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
        let body = "# title\n\n## Context\n...\n\n## What this extends\n...\n\n\
                    ## What this removes\n...\n\n## Restraint patterns\n...\n\n\
                    ## Could this be done with less?\n...\n";
        assert!(missing_sections(body).is_empty());
    }

    #[test]
    fn missing_sections_lists_absent_headings() {
        let body = "# title\n## Context\nx\n## What this extends\nx\n";
        let m = missing_sections(body);
        assert_eq!(m.len(), 3);
        assert!(m.contains(&"## What this removes"));
        assert!(m.contains(&"## Restraint patterns"));
        assert!(m.contains(&"## Could this be done with less?"));
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
