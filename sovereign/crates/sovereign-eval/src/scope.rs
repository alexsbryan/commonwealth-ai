//! Diff-scope analyzer (seam #1).
//!
//! For compound features inside a large existing codebase, "did the
//! agent stay inside the lines?" is a separate question from "did the
//! tests pass" or "did the judge like the code." Mechanical scorer
//! catches the former; the LLM judge sees code quality; neither
//! catches "agent silently rewrote half of `corpus-engine/`."
//!
//! Workflow:
//! 1. Operator declares allowed paths in the feature spec or via
//!    `--allowed-paths "<glob>;<glob>;…"`.
//! 2. Operator passes `--baseline-ref <git-ref>` (commit SHA or branch
//!    that represents pre-session state).
//! 3. The analyzer runs `git diff --name-status <baseline>..HEAD` and
//!    `--numstat`, classifies each file by glob match, reports
//!    in-scope vs. out-of-scope changes + a compliance ratio.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeReport {
    pub baseline_ref: String,
    pub allowed_globs: Vec<String>,
    pub in_scope_changes: Vec<ChangedFile>,
    pub out_of_scope_changes: Vec<ChangedFile>,
    pub total_changes: u32,
    pub scope_compliance: f64,
    pub additions_total: u32,
    pub deletions_total: u32,
    pub additions_out_of_scope: u32,
    pub deletions_out_of_scope: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
}

/// Default glob set when none provided. Permissive enough for
/// oicp-types-shape repos: any file under the experiment root that
/// isn't `scorer/`, `runs/`, or `.git/`.
pub fn default_allowed_globs() -> Vec<String> {
    vec!["**/*".to_string()]
}

/// Default forbidden paths — even when allowed_globs admits a file,
/// the harness's own scorer + runs directories are off-limits.
fn forbidden_prefixes() -> &'static [&'static str] {
    &["scorer/", "runs/", ".git/"]
}

pub fn analyze(
    experiment_repo: &Path,
    baseline_ref: &str,
    allowed_globs: &[String],
) -> Result<ScopeReport> {
    if !experiment_repo.exists() {
        bail!("experiment repo not found at {}", experiment_repo.display());
    }

    let status_lines = git_diff_name_status(experiment_repo, baseline_ref)?;
    let numstat_lines = git_diff_numstat(experiment_repo, baseline_ref)?;
    let counts = parse_numstat(&numstat_lines);
    let mut in_scope = Vec::new();
    let mut out_of_scope = Vec::new();
    let (mut adds_total, mut dels_total) = (0u32, 0u32);
    let (mut adds_oos, mut dels_oos) = (0u32, 0u32);

    for line in status_lines.lines().filter(|l| !l.is_empty()) {
        let mut parts = line.splitn(2, '\t');
        let status = parts.next().unwrap_or("?").to_string();
        let path = parts.next().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        let (adds, dels) = counts.get(&path).copied().unwrap_or((0, 0));
        adds_total += adds;
        dels_total += dels;
        let is_allowed = is_in_scope(&path, allowed_globs);
        let entry = ChangedFile {
            path,
            status,
            additions: adds,
            deletions: dels,
        };
        if is_allowed {
            in_scope.push(entry);
        } else {
            adds_oos += adds;
            dels_oos += dels;
            out_of_scope.push(entry);
        }
    }

    let total = (in_scope.len() + out_of_scope.len()) as u32;
    let compliance = if total == 0 {
        1.0
    } else {
        in_scope.len() as f64 / total as f64
    };

    Ok(ScopeReport {
        baseline_ref: baseline_ref.to_string(),
        allowed_globs: allowed_globs.to_vec(),
        in_scope_changes: in_scope,
        out_of_scope_changes: out_of_scope,
        total_changes: total,
        scope_compliance: compliance,
        additions_total: adds_total,
        deletions_total: dels_total,
        additions_out_of_scope: adds_oos,
        deletions_out_of_scope: dels_oos,
    })
}

fn git_diff_name_status(repo: &Path, baseline: &str) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("diff")
        .arg("--name-status")
        .arg(format!("{baseline}..HEAD"))
        .output()
        .context("running git diff --name-status")?;
    if !out.status.success() {
        bail!(
            "git diff --name-status failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_diff_numstat(repo: &Path, baseline: &str) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("diff")
        .arg("--numstat")
        .arg(format!("{baseline}..HEAD"))
        .output()
        .context("running git diff --numstat")?;
    if !out.status.success() {
        bail!(
            "git diff --numstat failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn parse_numstat(text: &str) -> std::collections::HashMap<String, (u32, u32)> {
    let mut out = std::collections::HashMap::new();
    for line in text.lines() {
        // Format: "<adds>\t<dels>\t<path>" — `-` for binary files
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() != 3 {
            continue;
        }
        let adds = parts[0].parse::<u32>().unwrap_or(0);
        let dels = parts[1].parse::<u32>().unwrap_or(0);
        out.insert(parts[2].to_string(), (adds, dels));
    }
    out
}

fn is_in_scope(path: &str, allowed_globs: &[String]) -> bool {
    for prefix in forbidden_prefixes() {
        if path.starts_with(prefix) {
            return false;
        }
    }
    if allowed_globs.is_empty() {
        return false;
    }
    allowed_globs.iter().any(|g| glob_match(g, path))
}

/// Tiny glob matcher: `*` matches any run of non-`/` chars; `**` matches
/// any sequence including `/`. No char classes, no escapes — enough for
/// the path patterns the harness uses (`src/**`, `commonwealth/**/*.rs`,
/// `.sovereign/features/<id>/**`).
fn glob_match(pattern: &str, path: &str) -> bool {
    let p_bytes = pattern.as_bytes();
    let s_bytes = path.as_bytes();
    glob_match_inner(p_bytes, 0, s_bytes, 0)
}

fn glob_match_inner(p: &[u8], pi: usize, s: &[u8], si: usize) -> bool {
    if pi == p.len() {
        return si == s.len();
    }
    if pi + 1 < p.len() && p[pi] == b'*' && p[pi + 1] == b'*' {
        // ** consumes any chars including `/`
        let mut i = si;
        loop {
            // skip optional `/` after `**`
            let next_pi = if pi + 2 < p.len() && p[pi + 2] == b'/' {
                pi + 3
            } else {
                pi + 2
            };
            if glob_match_inner(p, next_pi, s, i) {
                return true;
            }
            if i == s.len() {
                return false;
            }
            i += 1;
        }
    }
    if p[pi] == b'*' {
        // * consumes any non-`/` chars
        let mut i = si;
        loop {
            if glob_match_inner(p, pi + 1, s, i) {
                return true;
            }
            if i == s.len() || s[i] == b'/' {
                return false;
            }
            i += 1;
        }
    }
    if si < s.len() && p[pi] == s[si] {
        return glob_match_inner(p, pi + 1, s, si + 1);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_double_star_anywhere() {
        assert!(glob_match("**", "src/lib.rs"));
        assert!(glob_match("src/**", "src/inner/mod.rs"));
        assert!(glob_match("src/**", "src/lib.rs"));
        assert!(!glob_match("src/**", "tests/foo.rs"));
    }

    #[test]
    fn glob_single_star_does_not_cross_slash() {
        assert!(glob_match("*.rs", "lib.rs"));
        assert!(!glob_match("*.rs", "inner/lib.rs"));
        assert!(glob_match("src/*.rs", "src/lib.rs"));
        assert!(!glob_match("src/*.rs", "src/inner/lib.rs"));
    }

    #[test]
    fn forbidden_prefixes_always_out_of_scope() {
        assert!(!is_in_scope(
            "scorer/golden/Cargo.toml",
            &["**".to_string()]
        ));
        assert!(!is_in_scope("runs/abc/manifest.json", &["**".to_string()]));
        assert!(!is_in_scope(".git/HEAD", &["**".to_string()]));
    }

    #[test]
    fn allowed_glob_admits_matching_path() {
        let allowed = vec![
            "src/**".to_string(),
            ".sovereign/features/foo/**".to_string(),
        ];
        assert!(is_in_scope("src/lib.rs", &allowed));
        assert!(is_in_scope(".sovereign/features/foo/spec.md", &allowed));
        assert!(!is_in_scope("commonwealth/crates/x.rs", &allowed));
    }

    #[test]
    fn parse_numstat_extracts_counts() {
        let text = "10\t2\tsrc/a.rs\n5\t0\tsrc/b.rs\n-\t-\timg/foo.png\n";
        let counts = parse_numstat(text);
        assert_eq!(counts.get("src/a.rs"), Some(&(10, 2)));
        assert_eq!(counts.get("src/b.rs"), Some(&(5, 0)));
        // binary file shows as "- - path"; we parse to (0, 0).
        assert_eq!(counts.get("img/foo.png"), Some(&(0, 0)));
    }
}
