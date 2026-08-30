// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn doctor` — diagnose and optionally repair the full stack.
//!
//! Checks are organized into three layers:
//!   Sovereign  — server, indexes, config
//!   Commonwealth — daemon, mesh membership, inference capability
//!   OmO        — skill file, hooks, MCP round-trip
//!
//! Exit codes: 0 = all checks pass (warnings don't count), 1 = any failure.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use sovereign_cli_shared::dirs::sovereign_root;

mod checks_commonwealth;
mod checks_freshness;
mod checks_omo;
mod checks_sovereign;
mod probe;
mod repair;

use checks_commonwealth as cw;
use checks_freshness as fresh;
use checks_omo as omo;
use checks_sovereign as sov;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Layer {
    Sovereign,
    Commonwealth,
    Omo,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Passed,
    Failed,
    Warning,
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum Repair {
    Executable(String),
    /// Multiple commands to run in sequence (e.g. one per corpus).
    MultiExecutable(Vec<String>),
    Manual(String),
    None,
}

/// True when `cmd` still carries an unresolved `<placeholder>`.
///
/// `Repair::executable("svrn code index <path> --corpus-id X")` shipped as an
/// executable repair and could never run: `doctor --fix` would have invoked a
/// literal `<path>`. It was a `Manual` wearing an `Executable`'s coat, and
/// nothing caught it because no test ever watched the repair succeed
/// (ARCH_PRINCIPLES §18.1 — a gate you have not watched fail is not a gate).
fn has_placeholder(cmd: &str) -> bool {
    let mut chars = cmd.char_indices();
    while let Some((i, c)) = chars.next() {
        if c != '<' {
            continue;
        }
        // `<foo>` with no whitespace inside is a template slot; `a < b` is not.
        if let Some(close) = cmd[i + 1..].find('>') {
            let inner = &cmd[i + 1..i + 1 + close];
            if !inner.is_empty() && !inner.contains(char::is_whitespace) {
                return true;
            }
        }
    }
    false
}

impl Repair {
    /// Mint an executable repair. A command that still holds a `<placeholder>`
    /// is demoted to [`Repair::Manual`] rather than handed to `--fix`: the user
    /// still sees it, but the fixer never pretends it can run it. Refuse or
    /// name the substitution — never silently run the wrong thing (§18.3).
    fn executable(cmd: impl Into<String>) -> Self {
        let cmd = cmd.into();
        if has_placeholder(&cmd) {
            // NOT a debug_assert: this repo builds debug by default, so an
            // assert here would panic `doctor` — the one command an operator
            // runs when things are already broken. Degrade loudly instead.
            tracing::warn!(
                command = %cmd,
                "doctor: repair carries an unresolved <placeholder>; demoted to manual \
                 (resolve it via registered_root, or construct Repair::Manual directly)"
            );
            return Repair::Manual(cmd);
        }
        Repair::Executable(cmd)
    }
}

/// Repo root for a registered corpus, straight from `~/.svrnmesh/projects.json`.
///
/// Deliberately reads the FILE rather than `/v1/projects`: doctor has to work
/// when the daemon is down, which is exactly when a repair path matters most.
/// This is also the only source that can answer for a corpus whose
/// `_corpus_meta.json` does not exist yet — the never-indexed case, where the
/// index dir itself cannot say where the code lives.
fn registered_root(corpus_id: &str) -> Option<String> {
    let raw = std::fs::read_to_string(sovereign_root().join("projects.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.as_array()?
        .iter()
        .find(|p| p.get("corpus_id").and_then(|c| c.as_str()) == Some(corpus_id))
        .and_then(|p| p.get("root")?.as_str().map(str::to_string))
}

#[derive(Debug, Clone, Serialize)]
struct CheckResult {
    name: &'static str,
    layer: Layer,
    status: CheckStatus,
    message: String,
    repair: Repair,
}

// ── Run all checks ─────────────────────────────────────────────────────────────

async fn run_checks(sovereign_dir: &std::path::Path) -> Vec<CheckResult> {
    let mut results = Vec::new();

    // ── Sovereign layer ──────────────────────────────────────────
    // ORDERED FOR THE READER OF THE REPORT, not by module. The `fresh::` calls
    // interleaved below sit next to the `sov::` check they are about — SCIP
    // exporters/rebuilds beside `check_scip_indexed`, corpus visibility beside
    // `check_code_indexed` — so a human scanning the output sees each subject
    // once. Do not sort these into module order to make the prefixes tidy; that
    // would scatter every subject across the page. (The prefixes only became
    // visible when this file was split along its three declared layers; the
    // interleaving predates that and is deliberate.)
    // First on the Sovereign page: every check below reads differently
    // depending on the answer — a terminal SHOULD have no local slots.
    results.push(sov::check_node_class());
    results.push(sov::check_server_running().await);
    results.push(sov::check_server_tools().await);
    results.push(sov::check_embed_slot().await);
    results.push(sov::check_scip_indexed().await);
    results.push(fresh::check_scip_exporters());
    results.push(fresh::check_rebuild_outcomes().await);
    results.push(fresh::check_watcher_freshness().await);
    results.push(sov::check_code_indexed().await);
    results.push(fresh::check_code_tools_see_corpora().await);
    results.push(sov::check_project_indexed());
    results.push(sov::check_notes_db());
    results.push(sov::check_test_runner(sovereign_dir));
    results.push(sov::check_lint_runner(sovereign_dir));
    results.push(sov::check_watcher_live(sovereign_dir).await);
    results.push(sov::check_log_dir_size());
    // Config-only, and intentionally not gated on the daemon being up — see the
    // function's doc comment.
    results.push(sov::check_distributed_primary_contained());

    // Freshness pipeline: registry-level checks that report on the
    // daemon's project watchers and the integrity of their SCIP
    // databases. These run only when the daemon is live; otherwise
    // we'd be testing files the daemon may be about to overwrite.
    if probe::tcp_connectable("127.0.0.1", 9741).await {
        results.push(fresh::check_project_watchers().await);
        results.push(fresh::check_scip_integrity());
        results.push(fresh::check_legacy_hooks());
    }

    // ── Commonwealth layer (skip if not configured) ──────────────
    // The embedded daemon serves two listeners:
    //   - `:9741` — client surface: /status, /v1/*, /mcp, /v1/mesh/*.
    //   - `:9742` — internal surface: /internal/*, used for gossip
    //     and per-node activity reporting.
    // Checks split between the two accordingly. An explicit
    // override in sovereign-server.toml still wins for compatibility
    // with standalone Commonwealth deployments.
    let client_url = detect_commonwealth_url(sovereign_dir)
        .unwrap_or_else(|| "http://127.0.0.1:9741".to_string());
    // Resolved from `[daemon] internal_port`, not hardcoded: doctor probing the
    // default port while the daemon listens elsewhere reports a false "down".
    let internal_url = sovereign_core::setup_config::internal_daemon_base();
    // Supervision check runs UNCONDITIONALLY — outside the :9741
    // reachability gate below — because it matters most precisely
    // when the daemon is down: an unsupervised daemon that crashed is
    // the incident this check exists to prevent.
    results.push(sov::check_daemon_supervised());
    if probe::tcp_connectable("127.0.0.1", 9741).await {
        results.push(cw::check_daemon_running().await);
        results.push(sov::check_daemon_memory(&client_url).await);
        results.push(cw::check_mesh_member(&client_url).await);
        results.push(cw::check_iroh_egress(&client_url).await);
        results.push(cw::check_inference_capable(&client_url).await);
        results.push(cw::check_activity_reporting(&internal_url).await);
    }
    // else: daemon not running — skip layer silently

    // ── OmO layer (skip if `opencode` not in PATH) ───────────────
    let opencode_available = std::process::Command::new("which")
        .arg("opencode")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if opencode_available {
        results.push(omo::check_skill_file());
        results.push(omo::check_opencode_config());
        results.push(omo::check_mcp_live().await);
    }

    results
}

fn detect_commonwealth_url(sovereign_dir: &std::path::Path) -> Option<String> {
    // Read sovereign-server config to find commonwealth.url.
    let config_path = sovereign_dir.join("server.toml");
    if !config_path.exists() {
        return None;
    }
    let contents = std::fs::read_to_string(&config_path).ok()?;
    let table: toml::Value = toml::from_str(&contents).ok()?;
    table
        .get("commonwealth")
        .and_then(|c| c.get("url"))
        .and_then(|u| u.as_str())
        .map(|s| s.to_string())
}

// ── Output formatters ─────────────────────────────────────────────────────────

fn status_symbol(s: &CheckStatus) -> &'static str {
    match s {
        CheckStatus::Passed => "✓",
        CheckStatus::Failed => "✗",
        CheckStatus::Warning => "⚠",
        CheckStatus::Skipped => "–",
    }
}

fn print_human(results: &[CheckResult]) {
    let layers = [
        (Layer::Sovereign, "Sovereign"),
        (Layer::Commonwealth, "Commonwealth"),
        (Layer::Omo, "OmO"),
    ];

    let mut total_issues = 0usize;

    for (layer, label) in &layers {
        let layer_results: Vec<_> = results.iter().filter(|r| &r.layer == layer).collect();
        if layer_results.is_empty() {
            continue;
        }

        println!("\n  {label}:");
        for r in &layer_results {
            let sym = status_symbol(&r.status);
            println!("    {sym}  {}  —  {}", r.name, r.message);
            if r.status == CheckStatus::Failed || r.status == CheckStatus::Warning {
                match &r.repair {
                    Repair::Executable(cmd) | Repair::Manual(cmd) => {
                        println!("       → {cmd}");
                    }
                    Repair::MultiExecutable(cmds) => {
                        for cmd in cmds {
                            println!("       → {cmd}");
                        }
                    }
                    Repair::None => {}
                }
                if r.status == CheckStatus::Failed {
                    total_issues += 1;
                }
            }
        }
    }

    println!();
    if total_issues == 0 {
        println!("  All checks passed.");
    } else {
        println!("  {total_issues} issue(s) found. Run `svrn doctor --fix` to auto-repair where possible.");
    }
}

fn print_json(results: &[CheckResult]) {
    let issues: Vec<_> = results
        .iter()
        .filter(|r| r.status == CheckStatus::Failed)
        .collect();
    let out = serde_json::json!({
        "issues": issues.len(),
        "checks": results,
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
}

// ── Entry point ───────────────────────────────────────────────────────────────

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn doctor",
    summary: "Diagnose setup and daemon health across the Sovereign / Commonwealth / OmO layers.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn doctor [--fix] [--watch] [--json]"),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            ("--fix", "Attempt automatic repair for failing checks"),
            (
                "--watch",
                "Re-run periodically (every 5s) with a clear screen",
            ),
            (
                "--json",
                "Emit structured JSON (one object per check) for scripting",
            ),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Checks three layers: Sovereign (server, indexes, config), Commonwealth (daemon,\n\
             mesh, inference), OmO (skill file, MCP round-trip). Exit 0 = all pass (warnings\n\
             don't count), exit 1 = any failure.",
        ),
    ],
};

pub async fn run_doctor(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP);
        return 0;
    }
    let fix = args.iter().any(|a| a == "--fix");
    let watch = args.iter().any(|a| a == "--watch");
    let json = args.iter().any(|a| a == "--json");

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let sovereign_dir = find_sovereign_dir_or_cwd(&cwd);

    if watch {
        return run_watch(&sovereign_dir, json).await;
    }

    let results = run_checks(&sovereign_dir).await;

    if json {
        print_json(&results);
    } else {
        print_human(&results);
        if fix {
            println!("\n  Running repairs...");
            repair::run_fix(&results, &sovereign_dir).await;
        }
    }

    let has_failures = results.iter().any(|r| r.status == CheckStatus::Failed);
    if has_failures {
        1
    } else {
        0
    }
}

fn find_sovereign_dir_or_cwd(cwd: &std::path::Path) -> PathBuf {
    let mut dir = cwd;
    loop {
        let candidate = dir.join(".sovereign");
        if candidate.exists() {
            return candidate;
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }
    cwd.join(".sovereign")
}

async fn run_watch(sovereign_dir: &std::path::Path, json: bool) -> i32 {
    let is_tty = std::io::stdout().is_terminal();
    let mut prev_statuses: Vec<(&'static str, CheckStatus)> = Vec::new();

    loop {
        let results = run_checks(sovereign_dir).await;
        let now = {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let h = (secs / 3600) % 24;
            let m = (secs / 60) % 60;
            let s = secs % 60;
            format!("{h:02}:{m:02}:{s:02}")
        };

        // Diff against previous run.
        let changed: Vec<_> = results
            .iter()
            .filter(|r| {
                let prev = prev_statuses.iter().find(|(n, _)| *n == r.name);
                match prev {
                    None => true, // new check
                    Some((_, s)) => s != &r.status,
                }
            })
            .collect();

        if changed.is_empty() && !prev_statuses.is_empty() {
            if !json {
                println!("[{now}] No changes.");
            }
        } else {
            if is_tty && !json {
                // Clear screen for TTY.
                print!("\x1b[2J\x1b[H");
            }
            if json {
                let issues: Vec<_> = results
                    .iter()
                    .filter(|r| r.status == CheckStatus::Failed)
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "timestamp": now.to_string(),
                        "issues": issues.len(),
                        "checks": &results,
                    }))
                    .unwrap_or_default()
                );
            } else {
                println!("[{now}] Changes:");
                let changed_owned: Vec<CheckResult> = changed.into_iter().cloned().collect();
                print_human(&changed_owned);
            }
        }

        prev_statuses = results.iter().map(|r| (r.name, r.status.clone())).collect();
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_detects_unresolved_templates() {
        assert!(has_placeholder("svrn code index <path> --corpus-id x"));
        assert!(has_placeholder("svrn project watch restart <corpus_id>"));
        assert!(has_placeholder("svrn project register --root <repo>"));
    }

    #[test]
    fn placeholder_allows_real_commands() {
        assert!(!has_placeholder("svrn daemon restart"));
        assert!(!has_placeholder(
            "svrn code index /Users/me/dev/repo --corpus-id repo"
        ));
        // A comparison inside a message is not a template slot.
        assert!(!has_placeholder("rss 19042 MiB < 32768 MiB soft limit"));
        assert!(!has_placeholder("nothing here"));
    }

    /// The defect this guard exists for: `code_indexed` shipped
    /// `Repair::Executable("svrn code index <path> …")`, which `--fix` could
    /// never run. A template must degrade to Manual, visibly, rather than be
    /// handed to the fixer (§18.3 — never silently substitute).
    #[test]
    fn executable_demotes_a_template_to_manual() {
        match Repair::executable("svrn code index <path> --corpus-id demo") {
            Repair::Manual(cmd) => assert!(cmd.contains("<path>")),
            other => panic!("expected Manual demotion, got {other:?}"),
        }
    }

    #[test]
    fn executable_keeps_a_resolved_command() {
        match Repair::executable("svrn code index /tmp/repo --corpus-id demo") {
            Repair::Executable(cmd) => assert!(cmd.ends_with("--corpus-id demo")),
            other => panic!("expected Executable, got {other:?}"),
        }
    }
}
