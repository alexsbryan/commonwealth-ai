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
    /// Multiple commands to run in sequence (e.g. one per corpus).
    MultiExecutable(Vec<String>),
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
    let up = tcp_connectable("127.0.0.1", 9741).await;
    if up {
        CheckResult {
            name: "server_running",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "sovereign server is reachable at :9741".into(),
            repair: Repair::None,
        }
    } else {
        CheckResult {
            name: "server_running",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "sovereign server not reachable at :9741".into(),
            repair: Repair::Executable("sovereign project serve".into()),
        }
    }
}

async fn check_server_tools() -> CheckResult {
    // MCP uses JSON-RPC 2.0 over a single `/mcp` endpoint. The previous
    // version POSTed to `/mcp/tools/list` (non-existent), which either
    // 404'd or returned an unparseable body — the "Warning:
    // unparseable" message users saw.
    let resp = http_post_json(
        "http://localhost:9741/mcp",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
        }),
    )
    .await;
    match resp {
        None => CheckResult {
            name: "server_tools",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "could not reach /mcp endpoint".into(),
            repair: Repair::Executable("sovereign project serve".into()),
        },
        Some(r) => match r.json::<serde_json::Value>().await {
            Ok(json) => {
                let count = json["result"]["tools"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0);
                // Canonical daemon registry is 12 tools (symbol/code
                // search, recent_changes, callers, callees, blast_radius,
                // 3× notes, session_reflection, project_context,
                // check_doc_paths). `sovereign project serve` adds the
                // watcher-backed set (test_status, lint_status,
                // run_tests, get_run_output, get_lint_output) for 17.
                if count >= 12 {
                    CheckResult {
                        name: "server_tools",
                        layer: Layer::Sovereign,
                        status: CheckStatus::Passed,
                        message: format!("{count} tools registered"),
                        repair: Repair::None,
                    }
                } else if count > 0 {
                    CheckResult {
                        name: "server_tools",
                        layer: Layer::Sovereign,
                        status: CheckStatus::Warning,
                        message: format!(
                            "only {count} tools registered (expected ≥12) — possible version mismatch"
                        ),
                        repair: Repair::Manual(
                            "Check server logs for tool registration errors".into(),
                        ),
                    }
                } else {
                    CheckResult {
                        name: "server_tools",
                        layer: Layer::Sovereign,
                        status: CheckStatus::Warning,
                        message: "tools/list returned no tools".into(),
                        repair: Repair::Manual("Check server logs".into()),
                    }
                }
            }
            Err(_) => CheckResult {
                name: "server_tools",
                layer: Layer::Sovereign,
                status: CheckStatus::Warning,
                message: "/mcp tools/list returned unparseable response".into(),
                repair: Repair::Manual("Check server logs".into()),
            },
        },
    }
}

fn check_scip_indexed() -> CheckResult {
    // SCIP graphs are per-corpus: `~/.sovereign/indexes/<corpus_id>/scip_graph.db`.
    // The previous version looked for a top-level `_scip_graph.db`
    // that never existed. Walk the indexes directory, count any
    // non-empty scip_graph.db files, and report the total.
    let indexes_dir = home_dir().join(".sovereign").join("indexes");
    let mut populated = Vec::new();
    let mut empty = 0usize;
    if let Ok(entries) = std::fs::read_dir(&indexes_dir) {
        for entry in entries.flatten() {
            let db = entry.path().join("scip_graph.db");
            match db.metadata() {
                Ok(m) if m.len() > 4096 => {
                    if let Some(name) =
                        entry.path().file_name().and_then(|n| n.to_str())
                    {
                        populated.push(name.to_string());
                    }
                }
                Ok(_) => empty += 1,
                Err(_) => {}
            }
        }
    }
    if !populated.is_empty() {
        CheckResult {
            name: "scip_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: format!(
                "SCIP graph DB present in {} corpus index(es): {}",
                populated.len(),
                populated.join(", "),
            ),
            repair: Repair::None,
        }
    } else if empty > 0 {
        CheckResult {
            name: "scip_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: format!("{empty} SCIP graph DB(s) found but empty — not yet indexed"),
            repair: Repair::Executable("sovereign project refresh".into()),
        }
    } else {
        CheckResult {
            name: "scip_indexed",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: "no SCIP graph DB found — call graph tools unavailable".into(),
            repair: Repair::Executable("sovereign project init".into()),
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
    // Lives under the indexes directory, not directly in ~/.sovereign/.
    let project_db = home_dir()
        .join(".sovereign")
        .join("indexes")
        .join("project_docs.db");
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

async fn check_mesh_member(client_url: &str) -> CheckResult {
    // The real status endpoint lives on the client listener
    // (`:9741/status`), not the internal port. Shape is
    // `{node_id, mesh: {name, members_online, members_total, ...}, ...}`.
    let url = format!("{client_url}/status");
    match http_get_json(&url).await {
        Some(json) => {
            let total = json["mesh"]["members_total"].as_u64().unwrap_or(0);
            let online = json["mesh"]["members_online"].as_u64().unwrap_or(0);
            let name = json["mesh"]["name"].as_str().unwrap_or("<unknown>");
            if total > 1 {
                CheckResult {
                    name: "mesh_member",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Passed,
                    message: format!(
                        "member of \"{name}\" — {online}/{total} online"
                    ),
                    repair: Repair::None,
                }
            } else if total == 1 {
                // Solo mesh is the default on a freshly-setup single
                // machine. Not an error — just informational.
                CheckResult {
                    name: "mesh_member",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Passed,
                    message: format!(
                        "solo mesh \"{name}\" — run `sovereign mesh create` to invite peers"
                    ),
                    repair: Repair::None,
                }
            } else {
                CheckResult {
                    name: "mesh_member",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Warning,
                    message: "daemon running but no mesh formed yet".into(),
                    repair: Repair::Manual(
                        "Run `sovereign mesh create` or accept a join link".into(),
                    ),
                }
            }
        }
        None => CheckResult {
            name: "mesh_member",
            layer: Layer::Commonwealth,
            status: CheckStatus::Failed,
            message: format!("could not reach {url}"),
            repair: Repair::Executable("sovereign daemon restart".into()),
        },
    }
}

async fn check_inference_capable(client_url: &str) -> CheckResult {
    // The daemon exposes `inference.loaded_models` on `/status`.
    // Earlier versions had a flat `inference_capable` bool at the top
    // level; that field no longer exists — infer capability from the
    // loaded-models array instead. Empty = cold-start (no model yet
    // loaded), but `/v1/models` still lists available slots.
    let url = format!("{client_url}/status");
    match http_get_json(&url).await {
        Some(json) => {
            let loaded = json["inference"]["loaded_models"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            let models_url = format!("{client_url}/v1/models");
            let registered = http_get_json(&models_url)
                .await
                .and_then(|j| j["data"].as_array().map(|a| a.len()))
                .unwrap_or(0);
            let capable = loaded > 0 || registered > 0;
            if capable {
                CheckResult {
                    name: "inference_capable",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Passed,
                    message: format!(
                        "{registered} model(s) registered, {loaded} currently resident"
                    ),
                    repair: Repair::None,
                }
            } else {
                CheckResult {
                    name: "inference_capable",
                    layer: Layer::Commonwealth,
                    status: CheckStatus::Warning,
                    message: "no models registered — /v1/models is empty. restart the daemon after `sovereign setup` completes.".into(),
                    repair: Repair::Executable(
                        "sovereign daemon restart".into(),
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

async fn check_activity_reporting(internal_url: &str) -> CheckResult {
    // The endpoint expects `{level: "hot"|"warm"|"cool"|..., reason: "..."}`
    // and replies 204. Passing `activity_level: 0.0` (the previous
    // payload) yielded a 422 — it's a string enum, not a float.
    let url = format!("{internal_url}/internal/node/activity");
    let resp = http_post_json(
        &url,
        serde_json::json!({
            "level": "cool",
            "reason": "doctor health check"
        }),
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
    // MCP is JSON-RPC over `/mcp`; tools/list is a method, not a
    // path. See the same fix in `check_server_tools`.
    let resp = http_post_json(
        "http://localhost:9741/mcp",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
        }),
    )
    .await;
    match resp {
        Some(r) if r.status().is_success() => CheckResult {
            name: "mcp_live",
            layer: Layer::Omo,
            status: CheckStatus::Passed,
            message: "MCP /mcp tools/list round-trip succeeded".into(),
            repair: Repair::None,
        },
        _ => CheckResult {
            name: "mcp_live",
            layer: Layer::Omo,
            status: CheckStatus::Failed,
            message: "MCP /mcp unreachable — agents cannot use sovereign tools".into(),
            repair: Repair::Executable("sovereign daemon restart".into()),
        },
    }
}

// ── Freshness-pipeline checks ────────────────────────────────

/// Query the daemon's `/v1/projects` endpoint and surface the
/// aggregate watcher health. Passes when every registered project
/// has all its watchers healthy; downgrades to Warning when any
/// watcher is Crashed; Failed when any watcher is Disabled (the
/// daemon has given up auto-restarting and needs operator action).
async fn check_project_watchers() -> CheckResult {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            return CheckResult {
                name: "project_watchers",
                layer: Layer::Sovereign,
                status: CheckStatus::Warning,
                message: "could not build HTTP client".into(),
                repair: Repair::None,
            };
        }
    };

    let resp = match client.get("http://127.0.0.1:9741/v1/projects").send().await {
        Ok(r) => r,
        Err(_) => {
            return CheckResult {
                name: "project_watchers",
                layer: Layer::Sovereign,
                status: CheckStatus::Warning,
                message: "daemon unreachable — /v1/projects did not answer".into(),
                repair: Repair::Executable("sovereign daemon restart".into()),
            };
        }
    };

    if resp.status().as_u16() == 404 {
        return CheckResult {
            name: "project_watchers",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "/v1/projects returned 404 — project_http_router not mounted (restart the daemon)".into(),
            repair: Repair::Executable("sovereign daemon restart".into()),
        };
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => {
            return CheckResult {
                name: "project_watchers",
                layer: Layer::Sovereign,
                status: CheckStatus::Warning,
                message: "/v1/projects returned unexpected shape".into(),
                repair: Repair::Manual("inspect daemon logs".into()),
            };
        }
    };

    let Some(projects) = body["projects"].as_array() else {
        return CheckResult {
            name: "project_watchers",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "/v1/projects returned unexpected shape".into(),
            repair: Repair::Manual("inspect daemon logs".into()),
        };
    };
    if projects.is_empty() {
        return CheckResult {
            name: "project_watchers",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: "daemon is running but no projects registered".into(),
            repair: Repair::Manual(
                "cd <repo-root> && sovereign project register  (run from each repo root)".into(),
            ),
        };
    }

    let mut crashed: Vec<String> = Vec::new();
    let mut disabled: Vec<String> = Vec::new();
    let mut active_ok = 0usize;
    for p in projects {
        let id = p["corpus_id"].as_str().unwrap_or("?");
        let Some(status) = p["status"].as_object() else {
            continue;
        };
        for (kind, s) in status {
            let state = s["state"].as_str().unwrap_or("?");
            match state {
                "idle" | "active" | "pending" => active_ok += 1,
                "crashed" => crashed.push(format!("{id}:{kind}")),
                "disabled" => disabled.push(format!("{id}:{kind}")),
                _ => {}
            }
        }
    }

    if !disabled.is_empty() {
        return CheckResult {
            name: "project_watchers",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!(
                "{} watcher(s) disabled after repeated crashes: {}",
                disabled.len(),
                disabled.join(", ")
            ),
            repair: Repair::Executable(
                "sovereign project watch restart <corpus_id>".into(),
            ),
        };
    }
    if !crashed.is_empty() {
        return CheckResult {
            name: "project_watchers",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: format!(
                "{} watcher(s) crashed but auto-restarting: {}",
                crashed.len(),
                crashed.join(", ")
            ),
            repair: Repair::Manual(
                "tail ~/.sovereign/logs/watch-<id>-<watcher>.log for details".into(),
            ),
        };
    }
    CheckResult {
        name: "project_watchers",
        layer: Layer::Sovereign,
        status: CheckStatus::Passed,
        message: format!(
            "{} project(s), {} watcher(s) healthy",
            projects.len(),
            active_ok
        ),
        repair: Repair::None,
    }
}

/// Run `PRAGMA integrity_check` (via `ScipGraph::open_with_integrity`)
/// on every per-corpus `scip_graph.db`. A corrupt DB is quarantined
/// by `open_with_integrity` as a side effect; we surface that to
/// the operator so they can trigger an immediate rebuild. This is
/// the doctor-level complement of the daemon's automatic
/// quarantine-on-open behaviour.
fn check_scip_integrity() -> CheckResult {
    let indexes_dir = home_dir().join(".sovereign").join("indexes");
    let Ok(entries) = std::fs::read_dir(&indexes_dir) else {
        return CheckResult {
            name: "scip_integrity",
            layer: Layer::Sovereign,
            status: CheckStatus::Skipped,
            message: format!("no indexes dir at {}", indexes_dir.display()),
            repair: Repair::None,
        };
    };

    let mut checked = 0usize;
    let mut corrupt: Vec<String> = Vec::new();
    let mut stale_schema: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let db = entry.path().join("scip_graph.db");
        if !db.exists() {
            continue;
        }
        let corpus = entry
            .file_name()
            .to_string_lossy()
            .to_string();
        match corpus_engine::ScipGraph::open_with_integrity(&db, &corpus) {
            Ok(_) => {
                checked += 1;
            }
            Err(corpus_engine::OpenError::Corrupt { .. }) => {
                corrupt.push(corpus);
            }
            Err(corpus_engine::OpenError::SchemaMismatch { .. }) => {
                stale_schema.push(corpus);
            }
            Err(_) => {
                // Other errors (IO, transient DB issues) shouldn't
                // trip the integrity check — the daemon will log
                // them on its next open.
            }
        }
    }

    if !corrupt.is_empty() {
        return CheckResult {
            name: "scip_integrity",
            layer: Layer::Sovereign,
            status: CheckStatus::Failed,
            message: format!(
                "{} SCIP DB(s) were corrupt and moved aside: {}",
                corrupt.len(),
                corrupt.join(", ")
            ),
            repair: Repair::MultiExecutable(
                corrupt
                    .iter()
                    .map(|id| format!("sovereign project refresh --name {id} --local"))
                    .collect(),
            ),
        };
    }
    if !stale_schema.is_empty() {
        return CheckResult {
            name: "scip_integrity",
            layer: Layer::Sovereign,
            status: CheckStatus::Warning,
            message: format!(
                "{} SCIP DB(s) have an outdated schema: {}",
                stale_schema.len(),
                stale_schema.join(", ")
            ),
            repair: Repair::MultiExecutable(
                stale_schema
                    .iter()
                    .map(|id| format!("sovereign project refresh --name {id} --local"))
                    .collect(),
            ),
        };
    }
    CheckResult {
        name: "scip_integrity",
        layer: Layer::Sovereign,
        status: CheckStatus::Passed,
        message: format!("{checked} SCIP DB(s) integrity OK"),
        repair: Repair::None,
    }
}

/// Scan registered project roots for legacy `SOVEREIGN_HOOK_V*`
/// post-commit hooks. The daemon owns freshness now, so any
/// surviving hook is a ticking footgun (stale binary path, silent
/// failures into `~/.sovereign/hooks.log`). Surface them with a
/// one-shot cleanup hint.
fn check_legacy_hooks() -> CheckResult {
    // Best-effort: read the registry directly. If the registry
    // isn't loadable, skip this check — the `warn_orphaned_indexes`
    // path at daemon startup will cover the miss.
    let registry = match sovereign_mesh::projects::Registry::load() {
        Ok(r) => r,
        Err(_) => {
            return CheckResult {
                name: "legacy_hooks",
                layer: Layer::Sovereign,
                status: CheckStatus::Skipped,
                message: "project registry not loadable".into(),
                repair: Repair::None,
            };
        }
    };
    let mut stale: Vec<String> = Vec::new();
    for entry in registry.entries() {
        let hook = entry.root.join(".git/hooks/post-commit");
        let Ok(contents) = std::fs::read_to_string(&hook) else {
            continue;
        };
        if contents.contains("SOVEREIGN_HOOK_V") {
            stale.push(entry.corpus_id.clone());
        }
    }
    if stale.is_empty() {
        return CheckResult {
            name: "legacy_hooks",
            layer: Layer::Sovereign,
            status: CheckStatus::Passed,
            message: "no legacy post-commit hooks found".into(),
            repair: Repair::None,
        };
    }
    CheckResult {
        name: "legacy_hooks",
        layer: Layer::Sovereign,
        status: CheckStatus::Warning,
        message: format!(
            "{} project(s) still carry a legacy sovereign post-commit hook: {}",
            stale.len(),
            stale.join(", ")
        ),
        repair: Repair::Executable(
            "sovereign project install-hooks  (in the affected repo — removes the legacy hook)".into(),
        ),
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

    // Freshness pipeline: registry-level checks that report on the
    // daemon's project watchers and the integrity of their SCIP
    // databases. These run only when the daemon is live; otherwise
    // we'd be testing files the daemon may be about to overwrite.
    if tcp_connectable("127.0.0.1", 9741).await {
        results.push(check_project_watchers().await);
        results.push(check_scip_integrity());
        results.push(check_legacy_hooks());
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
    let internal_url = "http://127.0.0.1:9742".to_string();
    if tcp_connectable("127.0.0.1", 9741).await {
        results.push(check_daemon_running().await);
        results.push(check_mesh_member(&client_url).await);
        results.push(check_inference_capable(&client_url).await);
        results.push(check_activity_reporting(&internal_url).await);
    }
    // else: daemon not running — skip layer silently

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

// ── Default config templates (embedded at compile time) ──────────────────────

const DEFAULT_TEST_RUNNER_TOML: &str = r#"
[test_runner]
command = "scripts/sovereign-test.sh"
working_dir = "."
timeout_secs = 120
debounce_ms = 2000
"#;

const DEFAULT_LINT_RUNNER_TOML: &str = r#"
[lint_runner]
command = "scripts/sovereign-lint.sh"
working_dir = "."
timeout_secs = 60
debounce_ms = 800
"#;

const SKILL_MD_TEMPLATE: &str =
    include_str!("../../../.opencode/skills/sovereign-code/SKILL.md");
const HOOK_CONFIG_TEMPLATE: &str =
    include_str!("../../../.opencode/oh-my-opencode.jsonc");

// ── Inline repair helpers ─────────────────────────────────────────────────────

/// Write a default `.sovereign/sovereign.toml` with both runners configured,
/// appending only the sections that are missing when the file already exists.
fn attempt_write_runner_config(sovereign_dir: &std::path::Path) {
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

/// Write the OmO SKILL.md to `.opencode/skills/sovereign-code/SKILL.md`
/// under the current working directory.
fn attempt_write_skill_file() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let skill_dir = cwd
        .join(".opencode")
        .join("skills")
        .join("sovereign-code");
    if let Err(e) = std::fs::create_dir_all(&skill_dir) {
        println!(
            "  ✗ skill_file: could not create directory {}: {e}",
            skill_dir.display()
        );
        return;
    }
    let skill_md = skill_dir.join("SKILL.md");
    if skill_md.exists() {
        println!("  – skill_file: already exists at {}", skill_md.display());
        return;
    }
    match std::fs::write(&skill_md, SKILL_MD_TEMPLATE) {
        Ok(_) => println!("  ✓ skill_file: wrote {}", skill_md.display()),
        Err(e) => println!("  ✗ skill_file: could not write {}: {e}", skill_md.display()),
    }
}

/// Write the OmO hook config to `.opencode/oh-my-opencode.jsonc`
/// under the current working directory.
fn attempt_write_hook_config() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let opencode_dir = cwd.join(".opencode");
    if let Err(e) = std::fs::create_dir_all(&opencode_dir) {
        println!(
            "  ✗ hook_config: could not create directory {}: {e}",
            opencode_dir.display()
        );
        return;
    }
    let hook_file = opencode_dir.join("oh-my-opencode.jsonc");
    if hook_file.exists() {
        println!("  – hook_config: already exists at {}", hook_file.display());
        return;
    }
    match std::fs::write(&hook_file, HOOK_CONFIG_TEMPLATE) {
        Ok(_) => println!("  ✓ hook_config: wrote {}", hook_file.display()),
        Err(e) => println!("  ✗ hook_config: could not write {}: {e}", hook_file.display()),
    }
}

// ── Fix runner ────────────────────────────────────────────────────────────────

async fn run_fix(results: &[CheckResult], sovereign_dir: &std::path::Path) {
    let fixable: Vec<_> = results
        .iter()
        .filter(|r| {
            r.status == CheckStatus::Failed || r.status == CheckStatus::Warning
        })
        .collect();

    if fixable.is_empty() {
        println!("  Nothing to auto-repair.");
        return;
    }

    // ── Executable repairs ────────────────────────────────────────
    for r in fixable.iter().filter(|r| matches!(r.repair, Repair::Executable(_))) {
        let Repair::Executable(cmd) = &r.repair else { continue };
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
    for r in fixable.iter().filter(|r| matches!(r.repair, Repair::MultiExecutable(_))) {
        let Repair::MultiExecutable(cmds) = &r.repair else { continue };
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
    for r in fixable.iter() {
        match r.name {
            "test_runner" | "lint_runner" => {
                attempt_write_runner_config(sovereign_dir);
            }
            "skill_file" => {
                attempt_write_skill_file();
            }
            "hook_config" => {
                attempt_write_hook_config();
            }
            _ => {}
        }
    }

    // ── Print manual hints ────────────────────────────────────────
    let manual: Vec<_> = fixable
        .iter()
        .filter(|r| matches!(r.repair, Repair::Manual(_)))
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

// ── Entry point ───────────────────────────────────────────────────────────────

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign doctor",
    summary: "Diagnose setup and daemon health across the Sovereign / Commonwealth / OmO layers.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign doctor [--fix] [--watch] [--json]",
        ),
        crate::util::help::HelpSection::Flags(&[
            ("--fix",    "Attempt automatic repair for failing checks"),
            ("--watch",  "Re-run periodically (every 5s) with a clear screen"),
            ("--json",   "Emit structured JSON (one object per check) for scripting"),
        ]),
        crate::util::help::HelpSection::Notes(
            "Checks three layers: Sovereign (server, indexes, config), Commonwealth (daemon,\n\
             mesh, inference), OmO (skill file, MCP round-trip). Exit 0 = all pass (warnings\n\
             don't count), exit 1 = any failure.",
        ),
    ],
};

pub async fn run_doctor(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
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
            run_fix(&results, &sovereign_dir).await;
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
