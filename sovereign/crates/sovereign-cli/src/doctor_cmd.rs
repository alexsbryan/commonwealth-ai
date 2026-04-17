//! `sovereign doctor` — diagnose and optionally repair the full stack.
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
use tokio::net::TcpStream;
use tokio::time::timeout;

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
    Manual(String),
    None,
}

#[derive(Debug, Clone, Serialize)]
struct CheckResult {
    name: &'static str,
    layer: Layer,
    status: CheckStatus,
    message: String,
    repair: Repair,
}

// ── TCP probe ─────────────────────────────────────────────────────────────────

async fn tcp_connectable(host: &str, port: u16) -> bool {
    timeout(
        Duration::from_secs(2),
        TcpStream::connect((host, port)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────

async fn http_get_json(url: &str) -> Option<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    let resp = client.get(url).send().await.ok()?;
    if resp.status().is_success() {
        resp.json().await.ok()
    } else {
        None
    }
}

async fn http_post_json(url: &str, body: serde_json::Value) -> Option<reqwest::Response> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .ok()?;
    client.post(url).json(&body).send().await.ok()
}

// ── Checks ────────────────────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

async fn check_server_running() -> CheckResult {
    let up = tcp_connectable("127.0.0.1", 8080).await;
    if up {
        CheckResult {
            name: "server_running",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "sovereign server is reachable at :8080".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "server_running",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "sovereign server not reachable at :8080".into(),
            repair: Repair::Executable("sovereign project serve".into()),
        }
    }
}

async fn check_server_tools() -> CheckResult {
    let resp = http_post_json(
        "http://localhost:8080/mcp/tools/list",
        serde_json::json!({}),
    )
    .await;
    match resp {
        None => CheckResult {
            name: "server_tools",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "could not reach tools/list endpoint".into(),
            repair: Repair::Executable("sovereign project serve".into()),
        },
        Some(r) => {
            if let Ok(json) = r.json::<serde_json::Value>().await {
                let count = json["tools"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0);
                if count >= 14 {
                    CheckResult {
                        name: "server_tools",
                        layer: Layer::Sovereign,
                        status: CheckStatus::Passed,
                        message: format!("{count} tools registered"),
                        repair: Repair::None,
                    }
                } else {
                    CheckResult {
                        name: "server_tools",
                        layer: Layer::Sovereign,
                        status: CheckStatus::Warning,
                        message: format!("only {count} tools registered (expected ≥14) — possible version mismatch"),
                        repair: Repair::Manual("Check server logs for tool registration errors".into()),
                    }
                }
            } else {
                CheckResult {
                    name: "server_tools",
                    layer: Layer::Sovereign,
                    status: CheckStatus::Warning,
                    message: "tools/list returned unparseable response".into(),
                    repair: Repair::Manual("Check server logs".into()),
                }
            }
        }
    }
}

fn check_scip_indexed() -> CheckResult {
    let db = home_dir()
        .join(".sovereign")
        .join("indexes")
        .join("_scip_graph.db");
    if db.exists() && db.metadata().map(|m| m.len() > 4096).unwrap_or(false) {
        CheckResult {
            name: "scip_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: format!("SCIP graph DB present ({})", db.display()),
            repair: Repair::None,
        }
    } else if db.exists() {
        CheckResult {
            name: "scip_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "SCIP graph DB is empty — not yet indexed".into(),
            repair: Repair::Executable("sovereign corpus scip".into()),
        }
    } else {
        CheckResult {
            name: "scip_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "SCIP graph DB not found — call graph tools unavailable".into(),
            repair: Repair::Executable("sovereign corpus scip".into()),
        }
    }
}

fn check_code_indexed() -> CheckResult {
    let indexes_dir = home_dir().join(".sovereign").join("indexes");
    let has_index = std::fs::read_dir(&indexes_dir)
        .ok()
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    if has_index {
        CheckResult {
            name: "code_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: format!("code indexes present at {}", indexes_dir.display()),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "code_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "no code indexes found — semantic code search unavailable".into(),
            repair: Repair::Executable("sovereign code index .".into()),
        }
    }
}

fn check_notes_db() -> CheckResult {
    let db = home_dir().join(".sovereign").join("notes.db");
    if db.exists() {
        CheckResult {
            name: "notes_db",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "notes.db present".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "notes_db",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "notes.db not found — note tools unavailable".into(),
            repair: Repair::Executable("sovereign init".into()),
        }
    }
}

fn check_project_indexed() -> CheckResult {
    let project_db = home_dir().join(".sovereign").join("project_docs.db");
    if project_db.exists() {
        CheckResult {
            name: "project_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "project docs index present".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "project_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "project docs index not found — project_context search unavailable".into(),
            repair: Repair::Executable("sovereign index project".into()),
        }
    }
}

fn check_test_runner(sovereign_dir: &std::path::Path) -> CheckResult {
    let cfg = corpus_engine::SovereignConfig::load_or_default(sovereign_dir);
    if cfg.test_runner.is_some() {
        CheckResult {
            name: "test_runner",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "test_runner configured in sovereign.toml".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "test_runner",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "test_runner not configured — test_status tool unavailable".into(),
            repair: Repair::Manual(
                "Add [test_runner] section to .sovereign/sovereign.toml".into(),
            ),
        }
    }
}

fn check_lint_runner(sovereign_dir: &std::path::Path) -> CheckResult {
    let cfg = corpus_engine::SovereignConfig::load_or_default(sovereign_dir);
    if cfg.lint_runner.is_some() {
        CheckResult {
            name: "lint_runner",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "lint_runner configured in sovereign.toml".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "lint_runner",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "lint_runner not configured — lint_status tool unavailable".into(),
            repair: Repair::Manual(
                "Add [lint_runner] section to .sovereign/sovereign.toml".into(),
            ),
        }
    }
}

// Commonwealth checks

async fn check_daemon_running() -> CheckResult {
    let up = tcp_connectable("127.0.0.1", 9741).await;
    if up {
        CheckResult {
            name: "daemon_running",
            layer: Layer::Commonwealth,
            status: CheckStatus::Passed,
            message: "commonwealth daemon reachable at :9741".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "daemon_running",
            layer: Layer::Commonwealth,
            status: CheckStatus::Failed,
            message: "commonwealth daemon not reachable at :9741".into(),
            repair: Repair::Executable("commonwealth daemon start".into()),
        }
    }
}

async fn check_mesh_member(commonwealth_url: &str) -> CheckResult {
    let url = format!("{commonwealth_url}/status");
    match http_get_json(&url).await {
        Some(json) => {
            let member_count = json["members"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            if member_count > 0 {
                CheckResult {
                    name: "mesh_member",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Passed,
                    message: format!("node is mesh member ({member_count} peers)"),
                    repair: Repair::None,
                }
            } else {
                CheckResult {
                    name: "mesh_member",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Warning,
                    message: "no mesh peers — node may not have joined yet".into(),
                    repair: Repair::Manual("Run `commonwealth join <key>` to join a mesh".into()),
                }
            }
        }
        None => CheckResult {
            name: "mesh_member",
            layer: Layer::Commonwealth,
            status: CheckStatus::Failed,
            message: format!("could not reach {url}"),
            repair: Repair::Executable("commonwealth daemon start".into()),
        },
    }
}

async fn check_inference_capable(commonwealth_url: &str) -> CheckResult {
    let url = format!("{commonwealth_url}/status");
    match http_get_json(&url).await {
        Some(json) => {
            let capable = json["inference_capable"].as_bool().unwrap_or(false);
            if capable {
                CheckResult {
                    name: "inference_capable",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Passed,
                    message: "local node reports inference_capable: true".into(),
                    repair: Repair::None,
                }
            } else {
                CheckResult {
                    name: "inference_capable",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Warning,
                    message: "local node reports inference_capable: false — this node will not receive inference routing".into(),
                    repair: Repair::Manual(
                        "Check daemon startup logs for probe failure reason".into(),
                    ),
                }
            }
        }
        None => CheckResult {
            name: "inference_capable",
            layer: Layer::Commonwealth,
            status: CheckStatus::Skipped,
            message: "commonwealth daemon unreachable — skipping".into(),
            repair: Repair::None,
        },
    }
}

async fn check_activity_reporting(commonwealth_url: &str) -> CheckResult {
    let url = format!("{commonwealth_url}/internal/node/activity");
    let resp = http_post_json(
        &url,
        serde_json::json!({"activity_level": 0.0}),
    )
    .await;
    match resp {
        Some(r) if r.status().as_u16() == 204 || r.status().is_success() => CheckResult {
            name: "activity_reporting",
            layer: Layer::Commonwealth,
            status: CheckStatus::Passed,
            message: "activity reporting endpoint reachable".into(),
            repair: Repair::None,
        },
        Some(r) => CheckResult {
            name: "activity_reporting",
            layer: Layer::Commonwealth,
            status: CheckStatus::Warning,
            message: format!("activity endpoint returned HTTP {}", r.status()),
            repair: Repair::Manual(
                "Add commonwealth url to sovereign server config".into(),
            ),
        },
        None => CheckResult {
            name: "activity_reporting",
            layer: Layer::Commonwealth,
            status: CheckStatus::Warning,
            message: "could not reach activity reporting endpoint".into(),
            repair: Repair::Manual(
                "Add commonwealth url to sovereign server config".into(),
            ),
        },
    }
}

// OmO checks

fn find_opencode_skill_dir() -> Option<PathBuf> {
    // Walk up from cwd looking for .opencode/
    let cwd = std::env::current_dir().ok()?;
    let mut dir = cwd.as_path();
    loop {
        let candidate = dir.join(".opencode").join("skills").join("sovereign-code");
        if candidate.exists() {
            return Some(candidate);
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => break,
        }
    }
    None
}

fn check_skill_file() -> CheckResult {
    match find_opencode_skill_dir() {
        Some(skill_dir) => {
            let skill_md = skill_dir.join("SKILL.md");
            if skill_md.exists() {
                CheckResult {
                    name: "skill_file",
                    layer: Layer::Omo,
                    status: CheckStatus::Passed,
                    message: format!("SKILL.md present at {}", skill_md.display()),
                    repair: Repair::None,
                }
            } else {
                CheckResult {
                    name: "skill_file",
                    layer: Layer::Omo,
                    status: CheckStatus::Failed,
                    message: format!("SKILL.md missing from {}", skill_dir.display()),
                    repair: Repair::Manual(
                        "Copy SKILL.md from sovereign/.opencode/skills/sovereign-code/".into(),
                    ),
                }
            }
        }
        None => CheckResult {
            name: "skill_file",
            layer: Layer::Omo,
            status: CheckStatus::Warning,
            message: "no .opencode/skills/sovereign-code/ directory found — OmO not configured for this project".into(),
            repair: Repair::Manual(
                "Create .opencode/skills/sovereign-code/SKILL.md from sovereign template".into(),
            ),
        },
    }
}

fn check_hook_config() -> CheckResult {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let hook_file = cwd.join(".opencode").join("oh-my-opencode.jsonc");
    if hook_file.exists() {
        CheckResult {
            name: "hook_config",
            layer: Layer::Omo,
            status: CheckStatus::Passed,
            message: "oh-my-opencode.jsonc present".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "hook_config",
            layer: Layer::Omo,
            status: CheckStatus::Warning,
            message: "oh-my-opencode.jsonc not found".into(),
            repair: Repair::Manual(
                "Copy from sovereign/.opencode/oh-my-opencode.jsonc template".into(),
            ),
        }
    }
}

async fn check_mcp_live() -> CheckResult {
    let resp = http_post_json(
        "http://localhost:8080/mcp/tools/list",
        serde_json::json!({}),
    )
    .await;
    match resp {
        Some(r) if r.status().is_success() => CheckResult {
            name: "mcp_live",
            layer: Layer::Omo,
            status: CheckStatus::Passed,
            message: "MCP tools/list round-trip succeeded".into(),
            repair: Repair::None,
        },
        _ => CheckResult {
            name: "mcp_live",
            layer: Layer::Omo,
            status: CheckStatus::Failed,
            message: "MCP tools/list unreachable — agents cannot use sovereign tools".into(),
            repair: Repair::Executable("sovereign project serve".into()),
        },
    }
}

// ── Run all checks ─────────────────────────────────────────────────────────────

async fn run_checks(sovereign_dir: &std::path::Path) -> Vec<CheckResult> {
    let mut results = Vec::new();

    // ── Sovereign layer ──────────────────────────────────────────
    results.push(check_server_running().await);
    results.push(check_server_tools().await);
    results.push(check_scip_indexed());
    results.push(check_code_indexed());
    results.push(check_project_indexed());
    results.push(check_notes_db());
    results.push(check_test_runner(sovereign_dir));
    results.push(check_lint_runner(sovereign_dir));

    // ── Commonwealth layer (skip if not configured) ──────────────
    // Try to detect commonwealth URL from sovereign-server config, or use default.
    let commonwealth_url = detect_commonwealth_url(sovereign_dir);
    if let Some(ref url) = commonwealth_url {
        results.push(check_daemon_running().await);
        results.push(check_mesh_member(url).await);
        results.push(check_inference_capable(url).await);
        results.push(check_activity_reporting(url).await);
    } else {
        // If daemon is running on the default port, check it even without config.
        if tcp_connectable("127.0.0.1", 9741).await {
            let url = "http://127.0.0.1:9742";
            results.push(check_daemon_running().await);
            results.push(check_mesh_member(url).await);
            results.push(check_inference_capable(url).await);
            results.push(check_activity_reporting(url).await);
        }
        // else: commonwealth not configured and not running — skip layer silently
    }

    // ── OmO layer (skip if `opencode` not in PATH) ───────────────
    let opencode_available = std::process::Command::new("which")
        .arg("opencode")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if opencode_available {
        results.push(check_skill_file());
        results.push(check_hook_config());
        results.push(check_mcp_live().await);
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
            if r.status == CheckStatus::Failed {
                if let Repair::Executable(cmd) | Repair::Manual(cmd) = &r.repair {
                    println!("       → {cmd}");
                }
                total_issues += 1;
            }
        }
    }

    println!();
    if total_issues == 0 {
        println!("  All checks passed.");
    } else {
        println!("  {total_issues} issue(s) found. Run `sovereign doctor --fix` to auto-repair where possible.");
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

async fn run_fix(results: &[CheckResult]) {
    let fixable: Vec<_> = results
        .iter()
        .filter(|r| r.status == CheckStatus::Failed)
        .filter_map(|r| {
            if let Repair::Executable(cmd) = &r.repair {
                Some((r.name, cmd.clone()))
            } else {
                None
            }
        })
        .collect();

    if fixable.is_empty() {
        println!("  Nothing to auto-repair.");
        return;
    }

    for (name, cmd) in &fixable {
        println!("  Repairing {name}: {cmd}");
        // Split into program + args at the first space boundary.
        let mut parts = cmd.splitn(2, ' ');
        let prog = parts.next().unwrap_or(cmd);
        let rest: Vec<&str> = parts.next().map(|s| s.split_whitespace().collect()).unwrap_or_default();
        let status = std::process::Command::new(prog).args(&rest).status();
        match status {
            Ok(s) if s.success() => println!("  ✓ {name} repaired"),
            Ok(s) => println!("  ✗ {name} repair exited {s}"),
            Err(e) => println!("  ✗ {name} repair failed: {e}"),
        }
    }

    // Print manual hints for non-executable repairs.
    let manual: Vec<_> = results
        .iter()
        .filter(|r| r.status == CheckStatus::Failed)
        .filter_map(|r| {
            if let Repair::Manual(hint) = &r.repair {
                Some((r.name, hint.clone()))
            } else {
                None
            }
        })
        .collect();

    if !manual.is_empty() {
        println!("\n  Manual repairs needed:");
        for (name, hint) in &manual {
            println!("    {name}: {hint}");
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

pub async fn run_doctor(args: &[String]) -> i32 {
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
            run_fix(&results).await;
        }
    }

    let has_failures = results.iter().any(|r| r.status == CheckStatus::Failed);
    if has_failures { 1 } else { 0 }
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
                let issues: Vec<_> = results.iter().filter(|r| r.status == CheckStatus::Failed).collect();
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
