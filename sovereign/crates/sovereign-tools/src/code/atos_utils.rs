//! ATOS shared utilities extracted from `atos_cmd/run.rs`.
//!
//! Pure validators and shell runners used by both the CLI
//! runner (`sovereign atos run`) and the MCP tools
//! (`atos_verify`, `atos_validate`).
//!
//! Extracted 2026-05-07 to avoid duplicating these across
//! sovereign-cli and sovereign-tools.

use std::path::Path;
use std::process::Stdio;
use std::time::SystemTime;

use sha2::{Digest, Sha256};

pub fn is_weak_verify(cmd: &str, is_scaffold: bool) -> bool {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Trivially-passing commands.
    if matches!(trimmed, "true" | "exit 0") || trimmed.starts_with("echo ") {
        return true;
    }
    if is_scaffold {
        return false;
    }
    // Rust: `cargo test --test <file>` is weak unless it carries a
    // `-- test_name` filter. Without the filter cargo accepts an
    // empty test target, so the verify passes vacuously when the
    // step never wrote any tests.
    let rest_after_test = trimmed
        .strip_prefix("cargo test --test ")
        .or_else(|| {
            trimmed
                .strip_prefix("cargo test -p ")
                .and_then(|s| s.split_once(" --test ").map(|(_, after)| after))
        });
    if let Some(rest) = rest_after_test {
        let separator = " -- ";
        match rest.find(separator) {
            None => return true,
            Some(idx) if rest[idx + separator.len()..].trim().is_empty() => return true,
            _ => {}
        }
    }
    false
}

pub fn step_goal_is_scaffold(goal: &str) -> bool {
    let g = goal.to_lowercase();
    g.contains("scaffold")
        || g.contains("init")
        || g.starts_with("create the ")
        || g.starts_with("create a ")
        || g.contains("new project")
        || g.contains("new crate")
        || g.contains("project scaffold")
}

pub fn detect_missing_scaffold(
    verify_cmds: &[String],
    step01_files: &[String],
    workdir: &Path,
) -> Option<String> {
    // Map build-tool references to the files they need at workdir root.
    // Each sentinel must be matched at *command position* (start of
    // line or after a shell separator) — naive substring search
    // false-positives because e.g. "cargo test" contains "go test".
    let build_sentinels: &[(&str, &str)] = &[
        ("cargo ", "Cargo.toml"),
        ("go build", "go.mod"),
        ("go test", "go.mod"),
        ("npm ", "package.json"),
        ("yarn ", "package.json"),
        ("npx ", "package.json"),
        ("pnpm ", "package.json"),
        ("pip ", "requirements.txt"),
        ("python -m pytest", "requirements.txt"),
        ("pytest", "requirements.txt"),
    ];
    for (sentinel, required_file) in build_sentinels {
        let uses_tool = verify_cmds.iter().any(|cmd| cmd_invokes_tool(cmd, sentinel));
        if !uses_tool {
            continue;
        }
        if workdir.join(required_file).exists() {
            continue;
        }
        let step01_creates_it = step01_files
            .iter()
            .any(|f| f == required_file || f.ends_with(&format!("/{required_file}")));
        if step01_creates_it {
            continue;
        }
        return Some(format!(
            "plan references `{}` but `{required_file}` is missing from the workdir and not in step-01's files_touched. The first step must scaffold the project before later steps can build or test.",
            sentinel.trim_end()
        ));
    }
    None
}

/// Does `cmd` invoke `sentinel` as a top-level shell command?
///
/// "Top-level" = at the start of the cmd, or after a shell separator
/// (`;`, `&&`, `||`, `|`, newline). For sentinels that don't end in
/// whitespace, also require a non-word boundary on the right so
/// `"pytest"` doesn't match inside `"pytestify"` and `"go test"` is
/// caught only when it really starts a command.
fn cmd_invokes_tool(cmd: &str, sentinel: &str) -> bool {
    let separators = [';', '|', '&', '\n'];
    let starts = std::iter::once(0).chain(
        cmd.match_indices(|c: char| separators.contains(&c))
            .map(|(i, _)| i + 1),
    );
    for start in starts {
        let rest = cmd[start..].trim_start();
        if !rest.starts_with(sentinel) {
            continue;
        }
        let last_sentinel_char = sentinel.chars().last();
        let after = rest.as_bytes().get(sentinel.len()).copied();
        let boundary_ok = match (last_sentinel_char, after) {
            (Some(c), _) if c.is_ascii_whitespace() => true,
            (_, None) => true,
            (_, Some(b)) => !(b.is_ascii_alphanumeric() || b == b'_'),
        };
        if boundary_ok {
            return true;
        }
    }
    false
}

pub fn strip_failure_cruft(raw: &str) -> String {
    let trimmed = raw.trim();
    // Markers that signal "everything after this is failure log, not
    // rationale prose."
    let cut_markers: &[&str] = &[
        "\n```\nverify_cmd exited",
        "\n```\nstep claimed completion",
        "\n---stderr---\n",
        "\n<details><summary>last_failure",
        "\nverify output:\n",
    ];
    let mut cut_at = trimmed.len();
    for marker in cut_markers {
        if let Some(pos) = trimmed.find(marker) {
            cut_at = cut_at.min(pos);
        }
    }
    trimmed[..cut_at].trim().to_string()
}

pub fn split_state_marker(line: &str) -> (String, Option<String>) {
    if let Some(open) = line.rfind('[') {
        if let Some(close_rel) = line[open..].find(']') {
            let close = open + close_rel;
            let marker = line[open + 1..close].to_string();
            let head = line[..open].trim().to_string();
            return (head, Some(marker));
        }
    }
    (line.trim().to_string(), None)
}

pub fn parse_inline_list(payload: &str) -> Vec<String> {
    payload
        .split(',')
        .map(|s| {
            let trimmed = s.trim().trim_matches('`').trim();
            // Strip parenthesized inline commentary: `lib.rs (Capability)` → `lib.rs`
            // where the agent appended a type-name in parens after the file path.
            if let Some(idx) = trimmed.find('(') {
                if idx > 0 && trimmed.as_bytes()[idx - 1] == b' ' {
                    return trimmed[..idx].trim_end().to_string();
                }
            }
            trimmed.to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Pull a runnable shell command out of a `**Verify:**` line payload.
///
/// Agents reliably write the command inside backticks but frequently
/// append commentary after the closing backtick (observed 2026-05-06:
/// `` `cargo test --test x` (expects exit code 1 if Y)` ``). A naive
/// `trim_matches('`')` strips one outer backtick on each side and
/// leaves the inner backtick + prose embedded in the command — the
/// shell then chokes on the stray backtick.
///
/// Strategy:
/// 1. If there is a backtick-delimited chunk, take the *first* one.
/// 2. Otherwise, fall back to the trimmed line, then drop any
///    parenthesised commentary that follows the command (observed:
///    `cargo test --test x -- y (expects exit 0)`).
pub fn extract_verify_cmd(payload: &str) -> String {
    let trimmed = payload.trim();
    if let Some(open) = trimmed.find('`') {
        if let Some(close_rel) = trimmed[open + 1..].find('`') {
            let close = open + 1 + close_rel;
            return trimmed[open + 1..close].trim().to_string();
        }
    }
    let de_ticked = trimmed.trim_matches('`').trim();
    strip_trailing_paren_commentary(de_ticked).to_string()
}

fn strip_trailing_paren_commentary(s: &str) -> &str {
    // Split off `... (commentary)` only when the paren is preceded
    // by whitespace — embedded parens inside a command (e.g. shell
    // subshells `$(...)`) must be preserved.
    if let Some(idx) = s.find('(') {
        if idx > 0 && s.as_bytes()[idx - 1] == b' ' {
            return s[..idx].trim_end();
        }
    }
    s
}

pub fn detect_hollow_files(workdir: &Path, files_touched: &[String]) -> Option<String> {
    let mut hollow: Vec<String> = Vec::new();
    for f in files_touched {
        let p = workdir.join(f);
        if !p.exists() {
            hollow.push(format!("`{f}` (missing)"));
            continue;
        }
        let bytes = std::fs::read(&p).unwrap_or_default();
        let non_ws = bytes.iter().filter(|b| !b.is_ascii_whitespace()).count();
        if non_ws < 16 {
            hollow.push(format!("`{f}` ({non_ws} non-whitespace bytes)"));
        }
    }
    if hollow.is_empty() {
        None
    } else {
        Some(format!(
            "step's verify_cmd exited 0 but the files it promised to touch are empty/near-empty: {}. Either the step needs real content or files_touched was wrong.",
            hollow.join(", ")
        ))
    }
}

pub fn snapshot_file_mtimes(workdir: &Path, files: &[String]) -> Vec<Option<SystemTime>> {
    files
        .iter()
        .map(|f| {
            let p = workdir.join(f);
            std::fs::metadata(&p).and_then(|m| m.modified()).ok()
        })
        .collect()
}

pub fn detect_untouched_files(
    workdir: &Path,
    files: &[String],
    pre_snapshot: &[Option<SystemTime>],
) -> Option<String> {
    let real_files: Vec<(usize, &String)> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            let trimmed = f.trim();
            !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("N/A")
        })
        .collect();
    if real_files.is_empty() {
        return None;
    }
    for (i, _f) in &real_files {
        let p = workdir.join(files[*i].as_str());
        let post = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        let pre = pre_snapshot.get(*i).copied().flatten();
        match (pre, post) {
            (None, Some(_)) => return None,
            (Some(a), Some(b)) if b > a => return None,
            _ => continue,
        }
    }
    let names: Vec<String> = real_files
        .iter()
        .map(|(_, f)| format!("`{}`", f.as_str()))
        .collect();
    Some(format!(
        "step claimed completion but none of {} were modified during this iteration — agent silently no-op'd. Either the step's `Files:` list was wrong or the agent exited without writing the changes.",
        names.join(", ")
    ))
}

pub async fn run_verify_cmd(workdir: &Path, cmd: &str) -> (bool, String) {
    if cmd.trim().is_empty() {
        return (false, "verify_cmd is empty (strict mode)".into());
    }
    let output = match tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
    {
        Ok(o) => o,
        Err(e) => return (false, format!("verify spawn failed: {e}")),
    };
    let mut combined = String::new();
    combined.push_str(&String::from_utf8_lossy(&output.stdout));
    if !output.stderr.is_empty() {
        combined.push_str("\n---stderr---\n");
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    let truncated = truncate(&combined, 8 * 1024);
    (output.status.success(), truncated)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

pub fn truncate(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        format!("{}\n... (truncated to {} bytes)", &s[..limit], limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmd_invokes_tool_matches_at_command_position() {
        // Top-level at start.
        assert!(cmd_invokes_tool("cargo test --test x -- y", "cargo "));
        // After `;`.
        assert!(cmd_invokes_tool("cd foo; go test ./...", "go test"));
        // After `&&`.
        assert!(cmd_invokes_tool("cd foo && go build ./...", "go build"));
        // Bare pytest at start.
        assert!(cmd_invokes_tool("pytest -q", "pytest"));
    }

    #[test]
    fn cmd_invokes_tool_rejects_substring_false_positives() {
        // Regression: "cargo test" contains substring "go test" but
        // the cargo command does not invoke `go test`.
        assert!(
            !cmd_invokes_tool("cargo test --test x -- y", "go test"),
            "`cargo test ...` must NOT match the `go test` sentinel"
        );
        assert!(!cmd_invokes_tool("cargo build", "go build"));
        // Tool name as prefix of a different word.
        assert!(!cmd_invokes_tool("pytestify -q", "pytest"));
    }

    #[test]
    fn detect_missing_scaffold_does_not_false_positive_rust_against_go() {
        // Rust workdir with Cargo.toml present + a `cargo test`
        // verify_cmd. Previously this fired `go test` → `go.mod`
        // missing, because "cargo test" contains "go test" as a
        // substring.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname = \"foo\"\n",
        )
        .unwrap();
        let verify_cmds = vec!["cargo test --test test_foo -- bar".to_string()];
        let step01_files = vec!["Cargo.toml".to_string(), "src/lib.rs".to_string()];
        let result = detect_missing_scaffold(&verify_cmds, &step01_files, tmp.path());
        assert!(
            result.is_none(),
            "expected no scaffold gap for a normal Rust plan; got {result:?}"
        );
    }

    #[test]
    fn detect_missing_scaffold_flags_missing_cargo_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let verify_cmds = vec!["cargo build -p foo".to_string()];
        // Step 01 promises *only* an unrelated file.
        let step01_files = vec!["src/main.rs".to_string()];
        let result = detect_missing_scaffold(&verify_cmds, &step01_files, tmp.path());
        let msg = result.expect("expected scaffold gap detection");
        assert!(msg.contains("Cargo.toml"), "msg should mention Cargo.toml: {msg}");
    }

    #[test]
    fn detect_missing_scaffold_passes_when_step01_creates_required_file() {
        let tmp = tempfile::tempdir().unwrap();
        let verify_cmds = vec!["cargo build -p foo".to_string()];
        let step01_files = vec!["Cargo.toml".to_string(), "src/lib.rs".to_string()];
        assert!(detect_missing_scaffold(&verify_cmds, &step01_files, tmp.path()).is_none());
    }

    #[test]
    fn is_weak_verify_rejects_bare_cargo_test() {
        assert!(is_weak_verify("cargo test --test test_foo", false));
        assert!(is_weak_verify("cargo test --test test_foo -- ", false));
        assert!(!is_weak_verify("cargo test --test test_foo -- name", false));
        // Scaffold steps may use `cargo build` / `cargo check`.
        assert!(!is_weak_verify("cargo check -p foo", true));
        // Trivially-passing.
        assert!(is_weak_verify("true", false));
        assert!(is_weak_verify("echo ok", false));
        assert!(is_weak_verify("", false));
    }

    #[test]
    fn extract_verify_cmd_strips_parenthesised_commentary() {
        // Backtick form (canonical).
        assert_eq!(
            extract_verify_cmd("`cargo test --test x -- y` (note)"),
            "cargo test --test x -- y"
        );
        // Bare command followed by parenthesised commentary — agent
        // forgot the backticks. Without this stripping, the shell
        // would choke on the parens.
        assert_eq!(
            extract_verify_cmd("cargo test --test x -- y (expects exit 0)"),
            "cargo test --test x -- y"
        );
        // Plain command, no commentary.
        assert_eq!(extract_verify_cmd("cargo build"), "cargo build");
    }
}

