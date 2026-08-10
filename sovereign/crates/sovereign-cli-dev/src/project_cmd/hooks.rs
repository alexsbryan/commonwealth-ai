// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn project install-hooks` — the standalone post-commit-hook upgrade
//! path (the daemon now owns freshness, so this is largely deprecated), plus
//! the hook-writing/stripping internals. `remove_legacy_hook`,
//! `SOVEREIGN_HOOK_MARKER`, and `check_mcp_server` stay in `super` (shared
//! with init/status) and resolve through `use super::*`. Split out of
//! `project_cmd` (2026-07-13); pure move.

use super::*;

const HELP_INSTALL_HOOKS: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn project install-hooks",
    summary: "Upgrade (or install) the post-commit hook in the current repo.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn project install-hooks"),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Use this when you've upgraded sovereign-cli and want the hook to pick up the new\n\
             binary without re-running `svrn project init`.",
        ),
    ],
};

// ─── Install hooks (standalone upgrade path) ──────────────────

/// Upgrade (or install) the post-commit hook in the current repo without
/// running the full `project init` pipeline. Safe to re-run; detects and
/// rewrites prior-version hook blocks in place.
pub(super) async fn cmd_install_hooks(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_INSTALL_HOOKS);
        return 0;
    }
    // Deprecated. The daemon's Reindexer now keeps the graph fresh
    // via FS watcher + git HEAD poll + startup catch-up, so the old
    // post-commit hook is no longer useful (and its failure modes
    // were the reason for the rewrite). Remove any legacy hook we
    // find and tell the user why.
    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => {
            eprintln!("error: not inside a git repository");
            return 1;
        }
    };
    match remove_legacy_hook(&repo_root) {
        Ok(removed) => {
            if removed {
                println!(
                    "  \u{2713} Removed legacy post-commit hook from {}/.git/hooks/post-commit",
                    repo_root.display()
                );
            } else {
                println!("  No legacy sovereign hook found — nothing to do.");
            }
            println!(
                "\n  The daemon now owns freshness. Register this project with:\n\
                 \n    sovereign project register\n\n\
                 The FS watcher + git-HEAD poll keep the graph fresh without a hook.",
            );
            0
        }
        Err(e) => {
            eprintln!("error: could not clean up hook: {e}");
            1
        }
    }
}

// ─── Daemon-owned project lifecycle (register / list / watch) ───

#[allow(dead_code)]
fn install_post_commit_hook(root: &Path, corpus_id: &str) -> std::io::Result<()> {
    let hook_path = root.join(".git/hooks/post-commit");
    let _ = corpus_id; // corpus_id resolved from project.json by refresh

    // Resolve the binary path: use the current executable if it exists,
    // otherwise fall back to "sovereign" on PATH. This way the hook
    // works both for developers running from a local build and for
    // global installs.
    let current_exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());

    // The hook runs BOTH passes so every tool stays fresh:
    //   1. `project init --no-scip` — re-ingests symbols so symbol_lookup,
    //      code_search, and recent_changes return up-to-date results.
    //   2. `project refresh` — exports SCIP + call graph so find_callers
    //      and find_callees reflect the new commit.
    //
    // Output is redirected to ~/.svrnmesh/hooks.log so failures are
    // visible (a silent `&` swallows errors and leaves the user
    // wondering why MCP still serves stale data).
    //
    // We use a POSIX group command `{ ... } </dev/null &` rather than
    // `setsid` because setsid is Linux-only (util-linux) and not available
    // on macOS. The group command backgrounded with `&` is sufficient to
    // prevent git from waiting on the refresh, and `/dev/null` on stdin
    // prevents any accidental blocking reads.
    let hook_block = if let Some(ref exe) = current_exe {
        format!(
            r#"{marker}
# Sovereign: keep code intelligence fresh after each commit.
# Runs `project init --no-scip` (symbols) + `project refresh` (SCIP) in
# the background; output streams to ~/.svrnmesh/hooks.log.
LOG="$HOME/.sovereign/hooks.log"
mkdir -p "$(dirname "$LOG")"
SOVEREIGN="{exe}"
if [ ! -x "$SOVEREIGN" ]; then
  command -v sovereign >/dev/null 2>&1 || exit 0
  SOVEREIGN=sovereign
fi
{{
  printf "[%s] post-commit refresh (pid $$)\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$LOG"
  "$SOVEREIGN" project init --no-scip --no-hooks --no-claude-config >> "$LOG" 2>&1
  status_init=$?
  "$SOVEREIGN" project refresh --quiet >> "$LOG" 2>&1
  status_refresh=$?
  printf "[%s] done — init=%d refresh=%d\n\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$status_init" "$status_refresh" >> "$LOG"
}} </dev/null &
"#,
            marker = SOVEREIGN_HOOK_MARKER,
            exe = exe.display()
        )
    } else {
        // Fall back to PATH lookup for global installs.
        format!(
            r#"{marker}
# Sovereign: keep code intelligence fresh after each commit.
LOG="$HOME/.sovereign/hooks.log"
mkdir -p "$(dirname "$LOG")"
command -v sovereign >/dev/null 2>&1 || exit 0
{{
  printf "[%s] post-commit refresh (pid $$)\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$LOG"
  sovereign project init --no-scip --no-hooks --no-claude-config >> "$LOG" 2>&1
  status_init=$?
  sovereign project refresh --quiet >> "$LOG" 2>&1
  status_refresh=$?
  printf "[%s] done — init=%d refresh=%d\n\n" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$status_init" "$status_refresh" >> "$LOG"
}} </dev/null &
"#,
            marker = SOVEREIGN_HOOK_MARKER
        )
    };

    if hook_path.exists() {
        let existing = std::fs::read_to_string(&hook_path)?;

        if existing.contains(SOVEREIGN_HOOK_MARKER) {
            // Already on the current version — idempotent no-op.
            return Ok(());
        }

        if existing.contains("sovereign") && existing.contains("project refresh") {
            // Prior-version hook present; rewrite it by stripping the
            // Sovereign block and appending the new one. The "prior block"
            // is everything from the `# Sovereign: refresh` comment to the
            // first blank line after the closing `fi`.
            let rewritten = strip_prior_sovereign_block(&existing);
            let mut content = rewritten;
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&hook_block);
            std::fs::write(&hook_path, content)?;
        } else {
            // Foreign hook — append ours without touching theirs.
            let mut content = existing;
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content.push('\n');
            content.push_str(&hook_block);
            std::fs::write(&hook_path, content)?;
        }
    } else {
        let content = format!("#!/bin/sh\n{hook_block}");
        std::fs::write(&hook_path, content)?;
    }

    // Make executable on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&hook_path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook_path, perms)?;
    }

    Ok(())
}

/// Remove the Sovereign-managed block from a prior-version hook so we
/// can rewrite it without clobbering user-added content. The prior block
/// starts at the `# Sovereign: refresh` comment and runs until the `fi`
/// that closes the if/elif statement (or EOF, whichever comes first).
///
/// `dead_code` allowed because `install_post_commit_hook` (the only
/// production caller) is itself deprecated; the function is still
/// exercised by `strip_prior_sovereign_block_*` tests that pin the
/// legacy-hook detection format so users running an old binary that
/// installed a V1/V2 hook can still get it cleanly removed if they ever
/// upgrade.
#[allow(dead_code)]
fn strip_prior_sovereign_block(existing: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    let mut is_v1 = false;
    let mut saw_background = false;

    for line in existing.lines() {
        let trimmed = line.trim_start();

        // Start of any Sovereign hook block: V1 used a comment without a version
        // marker; V2+ use `# SOVEREIGN_HOOK_V<N>`.
        if !inside
            && (trimmed.starts_with("# SOVEREIGN_HOOK_V")
                || trimmed.starts_with("# Sovereign: refresh"))
        {
            inside = true;
            is_v1 = trimmed.starts_with("# Sovereign: refresh");
            saw_background = false;
            continue;
        }

        if inside {
            if is_v1 {
                // V1 blocks end at `fi` on its own line.
                if trimmed == "fi" {
                    inside = false;
                }
                continue;
            } else {
                // V2+ blocks end with a background job line (`... &`) followed
                // by a blank line.  The blank line after `&` is the terminator.
                if trimmed.ends_with('&') {
                    saw_background = true;
                    continue;
                }
                if saw_background && trimmed.is_empty() {
                    inside = false;
                    continue; // consume the blank terminator line
                }
                continue;
            }
        }
        out.push(line);
    }

    // Drop trailing blank lines left behind by stripping.
    while out.last().map(|l| l.is_empty()).unwrap_or(false) {
        out.pop();
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_prior_removes_old_sovereign_block() {
        let existing = r#"#!/bin/sh
# existing user hook content
echo "user step"

# Sovereign: refresh call graph after commit
if [ -x "/path/to/sovereign-cli" ]; then
  "/path/to/sovereign-cli" project refresh --quiet &
elif command -v sovereign >/dev/null 2>&1; then
  sovereign project refresh --quiet &
fi
"#;
        let stripped = strip_prior_sovereign_block(existing);
        assert!(!stripped.contains("project refresh"));
        assert!(stripped.contains("echo \"user step\""));
    }

    #[test]
    fn strip_prior_no_op_when_no_sovereign_block() {
        let existing = "#!/bin/sh\necho hello\n";
        let stripped = strip_prior_sovereign_block(existing);
        assert_eq!(stripped, "#!/bin/sh\necho hello");
    }

    #[test]
    fn strip_prior_removes_v2_sovereign_block() {
        let existing = "#!/bin/sh\n# user hook\necho \"user step\"\n\n# SOVEREIGN_HOOK_V2\n# Sovereign: keep code intelligence fresh after each commit.\nLOG=\"$HOME/.sovereign/hooks.log\"\nmkdir -p \"$(dirname \"$LOG\")\"\nSOVEREIGN=\"/path/to/sovereign-cli\"\nif [ ! -x \"$SOVEREIGN\" ]; then\n  command -v sovereign >/dev/null 2>&1 || exit 0\n  SOVEREIGN=sovereign\nfi\nsetsid sh -c 'printf \"hi\" >> \"$LOG\"' < /dev/null > /dev/null 2>&1 &\n\n";
        let stripped = strip_prior_sovereign_block(existing);
        assert!(
            !stripped.contains("SOVEREIGN_HOOK_V2"),
            "V2 marker should be stripped"
        );
        assert!(
            !stripped.contains("setsid"),
            "setsid line should be stripped"
        );
        assert!(
            stripped.contains("echo \"user step\""),
            "user content preserved"
        );
    }
}
