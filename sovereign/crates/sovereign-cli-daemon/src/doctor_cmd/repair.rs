// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn doctor --fix` — the write half.
//!
//! Kept apart from the checks on purpose: everything in the `checks_*` modules
//! is read-only and safe to run anywhere, and everything here MUTATES the
//! host. That boundary is the reviewable one, so it is a module boundary.

use super::checks_omo::{find_opencode_skill_dir, skill_frontmatter_ok};
use super::{CheckResult, CheckStatus, Repair};

// ── Default config templates (embedded at compile time) ──────────────────────

pub(super) const DEFAULT_TEST_RUNNER_TOML: &str = r#"
[test_runner]
command = "scripts/sovereign-test.sh"
working_dir = "."
timeout_secs = 120
debounce_ms = 2000
"#;

pub(super) const DEFAULT_LINT_RUNNER_TOML: &str = r#"
[lint_runner]
command = "scripts/sovereign-lint.sh"
working_dir = "."
timeout_secs = 60
debounce_ms = 800
"#;

pub(super) const SKILL_MD_TEMPLATE: &str =
    include_str!("../../../../.opencode/skills/sovereign-code/SKILL.md");

// ── Inline repair helpers ─────────────────────────────────────────────────────

/// Write a default `.sovereign/sovereign.toml` with both runners configured,
/// appending only the sections that are missing when the file already exists.
pub(super) fn attempt_write_runner_config(sovereign_dir: &std::path::Path) {
    let toml_path = sovereign_dir.join("sovereign.toml");
    if toml_path.exists() {
        let existing = std::fs::read_to_string(&toml_path).unwrap_or_default();
        let mut append = String::new();
        if !existing.contains("[test_runner]") {
            append.push_str(DEFAULT_TEST_RUNNER_TOML);
        }
        if !existing.contains("[lint_runner]") {
            append.push_str(DEFAULT_LINT_RUNNER_TOML);
        }
        if append.is_empty() {
            println!("  – runners: already configured, no changes needed");
            return;
        }
        match std::fs::OpenOptions::new()
            .append(true)
            .open(&toml_path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, append.as_bytes()))
        {
            Ok(_) => println!("  ✓ runners: appended to {}", toml_path.display()),
            Err(e) => println!("  ✗ runners: could not write {}: {e}", toml_path.display()),
        }
        return;
    }
    if let Some(parent) = toml_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = format!("{DEFAULT_TEST_RUNNER_TOML}{DEFAULT_LINT_RUNNER_TOML}");
    match std::fs::write(&toml_path, &content) {
        Ok(_) => println!("  ✓ runners: wrote {}", toml_path.display()),
        Err(e) => println!("  ✗ runners: could not write {}: {e}", toml_path.display()),
    }
}

/// Write the OmO SKILL.md to `.opencode/skills/sovereign-code/SKILL.md`.
///
/// Writes into the directory `check_skill_file` actually inspected when
/// there is one, falling back to cwd. They used to disagree — the check
/// walks UP from cwd, the writer always wrote INTO cwd — so running
/// `--fix` from a monorepo subdirectory could create a second skill dir
/// that the check would never look at.
pub(super) fn attempt_write_skill_file() {
    let skill_dir = find_opencode_skill_dir().unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(".opencode")
            .join("skills")
            .join("sovereign-code")
    });
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        println!(
            "  ✗ skill_file: could not create directory {}: {e}",
            skill_dir.display()
        );
        return;
    }
    let skill_md = skill_dir.join("SKILL.md");
    if skill_md.exists() {
        // Present but unloadable is the interesting case, and it is not
        // ours to silently overwrite — the user may have edited it.
        match std::fs::read_to_string(&skill_md) {
            Ok(body) => match skill_frontmatter_ok(&body) {
                Ok(()) => println!("  – skill_file: already valid at {}", skill_md.display()),
                Err(why) => println!(
                    "  – skill_file: {} exists but will not load ({why}); \
                     edit it or delete it and re-run --fix",
                    skill_md.display()
                ),
            },
            Err(e) => println!("  ✗ skill_file: cannot read {}: {e}", skill_md.display()),
        }
        return;
    }
    match std::fs::write(&skill_md, SKILL_MD_TEMPLATE) {
        Ok(_) => println!("  ✓ skill_file: wrote {}", skill_md.display()),
        Err(e) => println!(
            "  ✗ skill_file: could not write {}: {e}",
            skill_md.display()
        ),
    }
}

// ── Fix runner ────────────────────────────────────────────────────────────────

pub(super) async fn run_fix(results: &[CheckResult], sovereign_dir: &std::path::Path) {
    let fixable: Vec<_> = results
        .iter()
        .filter(|r| r.status == CheckStatus::Failed || r.status == CheckStatus::Warning)
        .collect();

    if fixable.is_empty() {
        println!("  Nothing to auto-repair.");
        return;
    }

    // ── Executable repairs ────────────────────────────────────────
    for r in fixable
        .iter()
        .filter(|r| matches!(r.repair, Repair::Executable(_)))
    {
        let Repair::Executable(cmd) = &r.repair else {
            continue;
        };
        println!("  Repairing {}: {cmd}", r.name);
        let mut parts = cmd.splitn(2, ' ');
        let prog = parts.next().unwrap_or(cmd);
        let rest: Vec<&str> = parts
            .next()
            .map(|s| s.split_whitespace().collect())
            .unwrap_or_default();
        let status = std::process::Command::new(prog).args(&rest).status();
        match status {
            Ok(s) if s.success() => println!("  ✓ {} repaired", r.name),
            Ok(s) => println!("  ✗ {} repair exited {s}", r.name),
            Err(e) => println!("  ✗ {} repair failed: {e}", r.name),
        }
    }

    // ── MultiExecutable repairs (e.g. one per stale SCIP corpus) ─
    for r in fixable
        .iter()
        .filter(|r| matches!(r.repair, Repair::MultiExecutable(_)))
    {
        let Repair::MultiExecutable(cmds) = &r.repair else {
            continue;
        };
        println!("  Repairing {} ({} commands):", r.name, cmds.len());
        let mut all_ok = true;
        for cmd in cmds {
            println!("    {cmd}");
            let mut parts = cmd.splitn(2, ' ');
            let prog = parts.next().unwrap_or(cmd);
            let rest: Vec<&str> = parts
                .next()
                .map(|s| s.split_whitespace().collect())
                .unwrap_or_default();
            let status = std::process::Command::new(prog).args(&rest).status();
            match status {
                Ok(s) if s.success() => {}
                Ok(s) => {
                    println!("    ✗ exited {s}");
                    all_ok = false;
                }
                Err(e) => {
                    println!("    ✗ {e}");
                    all_ok = false;
                }
            }
        }
        if all_ok {
            println!("  ✓ {} repaired", r.name);
        }
    }

    // ── Inline repairs for checks that need file-writing logic ───
    let mut repaired_inline: Vec<&str> = Vec::new();
    for r in fixable.iter() {
        match r.name {
            "test_runner" | "lint_runner" => {
                attempt_write_runner_config(sovereign_dir);
                repaired_inline.push(r.name);
            }
            "skill_file" => {
                attempt_write_skill_file();
                repaired_inline.push(r.name);
            }
            _ => {}
        }
    }

    // ── Print manual hints ────────────────────────────────────────
    // Anything just repaired inline is excluded: printing "do X
    // yourself" directly under "✓ wrote X" is how `--fix` used to read.
    let manual: Vec<_> = fixable
        .iter()
        .filter(|r| matches!(r.repair, Repair::Manual(_)))
        .filter(|r| !repaired_inline.contains(&r.name))
        .collect();
    if !manual.is_empty() {
        println!("\n  Manual repairs needed:");
        for r in &manual {
            if let Repair::Manual(hint) = &r.repair {
                println!("    {}: {hint}", r.name);
            }
        }
    }
}
