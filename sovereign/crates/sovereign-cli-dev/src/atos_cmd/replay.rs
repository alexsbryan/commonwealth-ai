// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atos replay` — reconstruct a historical commit as an
//! ATOS Runner task.
//!
//! Flow:
//!   1. Resolve the commit's parent SHA and verify both exist.
//!   2. Check out a fresh branch at the parent SHA.
//!   3. Ask the Fast slot to synthesize `DESIGN.md` + `CHARTER.md`
//!      from the commit message and diff — what the design WOULD have
//!      looked like before the commit.
//!   4. Hand off to `svrn atos run` with the synthesized artifacts
//!      in the workdir; the existing drive loop runs unchanged.
//!
//! After the loop terminates, `git diff <parent_sha>..<commit_sha>` is
//! the ground-truth comparison for the `sovereign-eval scope` analyzer
//! — a future `--score` flag wires this in.
//!
//! See `sovereign/docs/ATOS_RUNNER.md` § replay primitive.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::args::{get_flag, split_args};

const DEFAULT_SYNTH_MODEL: &str = "commonwealth/primary";
const DEFAULT_DAEMON_URL: &str = "http://localhost:9741";
const SYNTH_MAX_TOKENS: u32 = 4096;
const SYNTH_TEMPERATURE: f32 = 0.0;
const SYNTH_SEED: u64 = 0xA705;
const SYNTH_TIMEOUT_SECS: u64 = 600;
/// Diffs above this size get truncated before being shown to the
/// synthesizer — the goal is intent recovery, not byte-perfect echo.
const MAX_DIFF_BYTES: usize = 64 * 1024;

pub async fn cmd_replay(args: &[String]) -> i32 {
    let (_positional, flags) = split_args(args);

    let commit = match get_flag(&flags, "--commit") {
        Some(s) => s,
        None => {
            eprintln!("atos replay: missing --commit <sha>");
            print_help();
            return 2;
        }
    };
    let workdir = match get_flag(&flags, "--workdir") {
        Some(s) => PathBuf::from(s),
        None => {
            eprintln!("atos replay: missing --workdir <path>");
            print_help();
            return 2;
        }
    };
    if !workdir.is_dir() {
        eprintln!(
            "atos replay: --workdir is not a directory: {}",
            workdir.display()
        );
        return 2;
    }

    let driver = get_flag(&flags, "--driver").unwrap_or_else(|| "opencode".to_string());
    let daemon_url =
        get_flag(&flags, "--daemon-url").unwrap_or_else(|| DEFAULT_DAEMON_URL.to_string());
    let synth_model =
        get_flag(&flags, "--synth-model").unwrap_or_else(|| DEFAULT_SYNTH_MODEL.to_string());
    let custom_branch = get_flag(&flags, "--branch-name");
    let dry_run = flags.iter().any(|(k, _)| k == "dry-run");
    let max_iters = get_flag(&flags, "--max-iters").unwrap_or_else(|| "8".to_string());

    println!("atos replay: workdir = {}", workdir.display());
    println!("  commit       = {commit}");
    println!("  driver       = {driver}");
    println!("  synth model  = {synth_model}");

    // ── 1. Resolve commit + parent SHA ───────────────────────────
    let commit_sha = match git_rev_parse(&workdir, &commit) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("atos replay: cannot resolve commit `{commit}`: {e}");
            return 1;
        }
    };
    let parent_sha = match git_rev_parse(&workdir, &format!("{commit_sha}^")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("atos replay: cannot resolve parent of `{commit_sha}`: {e}");
            return 1;
        }
    };
    println!("  parent       = {parent_sha}");
    println!("  target       = {commit_sha}");

    // ── 2. Read commit message + diff BEFORE checking out ────────
    let commit_message = match git_commit_message(&workdir, &commit_sha) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("atos replay: cannot read commit message: {e}");
            return 1;
        }
    };
    let mut diff_text = match git_diff(&workdir, &parent_sha, &commit_sha) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("atos replay: cannot compute diff: {e}");
            return 1;
        }
    };
    let diff_truncated = diff_text.len() > MAX_DIFF_BYTES;
    if diff_truncated {
        diff_text.truncate(MAX_DIFF_BYTES);
        diff_text.push_str("\n... (diff truncated)\n");
    }
    let changed_files = match git_changed_files(&workdir, &parent_sha, &commit_sha) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("atos replay: cannot list changed files: {e}");
            return 1;
        }
    };
    println!(
        "  diff         = {} bytes ({} files){}",
        diff_text.len(),
        changed_files.len(),
        if diff_truncated { " [truncated]" } else { "" }
    );

    // ── 3. Check out a fresh branch at parent SHA ────────────────
    let branch_name = custom_branch.unwrap_or_else(|| {
        let short = &commit_sha[..7.min(commit_sha.len())];
        let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
        format!("atos-replay-{short}-{stamp}")
    });
    if let Err(e) = git_checkout_new_branch(&workdir, &branch_name, &parent_sha) {
        eprintln!("atos replay: branch checkout failed: {e}");
        return 1;
    }
    println!("  branch       = {branch_name} @ {parent_sha}");

    // ── 4. Fast slot synthesizes DESIGN.md and CHARTER.md ────────
    let design_md = match synthesize_design(
        &daemon_url,
        &synth_model,
        &commit_message,
        &diff_text,
        &changed_files,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("atos replay: design synthesis failed: {e}");
            return 1;
        }
    };
    let charter_md = match synthesize_charter(
        &daemon_url,
        &synth_model,
        &commit_message,
        &diff_text,
        &changed_files,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("atos replay: charter synthesis failed: {e}");
            return 1;
        }
    };

    let design_path = workdir.join("DESIGN.md");
    let charter_path = workdir.join("CHARTER.md");
    if let Err(e) = std::fs::write(&design_path, &design_md) {
        eprintln!("atos replay: write {}: {e}", design_path.display());
        return 1;
    }
    if let Err(e) = std::fs::write(&charter_path, &charter_md) {
        eprintln!("atos replay: write {}: {e}", charter_path.display());
        return 1;
    }
    println!(
        "  design       = {} ({} bytes)",
        design_path.display(),
        design_md.len()
    );
    println!(
        "  charter      = {} ({} bytes)",
        charter_path.display(),
        charter_md.len()
    );

    // Snapshot the synthesized artifacts to the runs dir for triage.
    // The actual run-id is allocated by `cmd_run`, so write to a
    // sidecar keyed by branch_name; operators can correlate via the
    // branch name printed above.
    let sidecar = sovereign_root()
        
        .join("replay-synth")
        .join(&branch_name);
    let _ = std::fs::create_dir_all(&sidecar);
    let _ = std::fs::write(sidecar.join("DESIGN.md"), &design_md);
    let _ = std::fs::write(sidecar.join("CHARTER.md"), &charter_md);
    let _ = std::fs::write(sidecar.join("commit_message.txt"), &commit_message);
    let _ = std::fs::write(sidecar.join("diff.patch"), &diff_text);

    if dry_run {
        println!("\natos replay: --dry-run set — synthesized artifacts only, not spawning driver.");
        println!("  synth dir  = {}", sidecar.display());
        return 0;
    }

    // ── 5. Delegate to `svrn atos run` ──────────────────────
    let workdir_str = workdir.to_string_lossy().to_string();
    let run_args = vec![
        "--workdir".to_string(),
        workdir_str,
        "--driver".to_string(),
        driver,
        "--max-iters".to_string(),
        max_iters,
        "--daemon-url".to_string(),
        daemon_url,
    ];
    super::run::cmd_run(&run_args).await
}

// ─── Synthesis prompts ───────────────────────────────────────────

const DESIGN_SYNTH_SYSTEM: &str = "You are an architect drafting a DESIGN.md for a software change. \
Output ONLY the markdown body — no surrounding prose, no fences, no commentary. \
The document should describe what the change accomplishes, the load-bearing anchors (files / functions / interfaces affected), and the success conditions a reviewer would check. \
Write it as if BEFORE the change was implemented — describe intent, not what happened.";

const CHARTER_SYNTH_SYSTEM: &str = "You are drafting a CHARTER.md that constrains an agent implementing the change described. \
Output ONLY the markdown body — no surrounding prose, no fences, no commentary. \
Include a `**Stop condition:**` line whose value is a single shell command that mechanically verifies the change. \
Prefer a `cargo test` / `cargo check` invocation scoped to the affected crate(s) when tests changed; otherwise use `cargo check --workspace`. \
Include short subsections on scope, out-of-scope, and what the reviewer should reject.";

async fn synthesize_design(
    daemon_url: &str,
    model: &str,
    commit_message: &str,
    diff_text: &str,
    changed_files: &[String],
) -> Result<String, String> {
    let user_prompt =
        build_synth_user_prompt("DESIGN.md", commit_message, diff_text, changed_files);
    call_synth(daemon_url, model, DESIGN_SYNTH_SYSTEM, &user_prompt).await
}

async fn synthesize_charter(
    daemon_url: &str,
    model: &str,
    commit_message: &str,
    diff_text: &str,
    changed_files: &[String],
) -> Result<String, String> {
    let user_prompt =
        build_synth_user_prompt("CHARTER.md", commit_message, diff_text, changed_files);
    call_synth(daemon_url, model, CHARTER_SYNTH_SYSTEM, &user_prompt).await
}

fn build_synth_user_prompt(
    artifact: &str,
    commit_message: &str,
    diff_text: &str,
    changed_files: &[String],
) -> String {
    let files_list = if changed_files.is_empty() {
        "(none listed)".to_string()
    } else {
        changed_files
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "Draft a {artifact} that an agent could implement against this codebase to reproduce the change described below.\n\n\
         ## Commit message\n\n{commit_message}\n\n\
         ## Files changed\n\n{files_list}\n\n\
         ## Diff (may be truncated)\n\n```\n{diff_text}\n```\n"
    )
}

async fn call_synth(
    daemon_url: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    let url = format!("{}/v1/chat/completions", daemon_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "temperature": SYNTH_TEMPERATURE,
        "top_p": 1.0,
        "max_tokens": SYNTH_MAX_TOKENS,
        "seed": SYNTH_SEED,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(SYNTH_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST /v1/chat/completions: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "daemon returned {status}: {}",
            text.chars().take(1024).collect::<String>()
        ));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("parse daemon response: {e}"))?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    if content.is_empty() {
        return Err("daemon returned empty content".into());
    }
    Ok(content)
}

// ─── Git helpers ─────────────────────────────────────────────────

fn git_rev_parse(workdir: &Path, refspec: &str) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .arg("rev-parse")
        .arg("--verify")
        .arg(refspec)
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_commit_message(workdir: &Path, sha: &str) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .arg("log")
        .arg("-1")
        .arg("--format=%B")
        .arg(sha)
        .output()
        .map_err(|e| format!("spawn git log: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_diff(workdir: &Path, base: &str, head: &str) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .arg("diff")
        .arg(format!("{base}..{head}"))
        .output()
        .map_err(|e| format!("spawn git diff: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_changed_files(workdir: &Path, base: &str, head: &str) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .arg("diff")
        .arg("--name-only")
        .arg(format!("{base}..{head}"))
        .output()
        .map_err(|e| format!("spawn git diff --name-only: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect())
}

fn git_checkout_new_branch(workdir: &Path, branch: &str, base_sha: &str) -> Result<(), String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .arg("checkout")
        .arg("-b")
        .arg(branch)
        .arg(base_sha)
        .output()
        .map_err(|e| format!("spawn git checkout: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git checkout -b {branch} {base_sha}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Branded per-user data root (rebrand-aware path SSOT — prefers a
/// populated `~/.svrnmesh`, honors `SOVEREIGN_DATA_DIR` via callers of
/// `rebrand::data_dir`; derivation lives in sovereign-cli-shared).
fn sovereign_root() -> PathBuf {
    sovereign_cli_shared::dirs::sovereign_root()
}

fn print_help() {
    eprintln!(
        "svrn atos replay — reconstruct a historical commit as a Runner task\n\
         \n\
         USAGE\n    sovereign atos replay --commit <sha> --workdir <repo> [flags]\n\
         \n\
         REQUIRED\n\
         \x20   --commit <sha>          The historical commit whose work the agent should reproduce.\n\
         \x20   --workdir <path>        Path to the git repo containing the commit.\n\
         \n\
         OPTIONAL\n\
         \x20   --driver opencode|claude  Driver to use (default: opencode).\n\
         \x20   --branch-name <name>      Override the auto-generated branch name.\n\
         \x20   --synth-model <id>        Model to synthesize DESIGN.md + CHARTER.md (default: commonwealth/primary).\n\
         \x20   --daemon-url <url>        Daemon base URL (default: http://localhost:9741).\n\
         \x20   --max-iters <N>           Max Runner iterations (default: 8).\n\
         \x20   --dry-run                 Synthesize artifacts only; don't spawn the driver.\n"
    );
}
