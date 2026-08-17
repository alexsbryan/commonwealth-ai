// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn project` subcommand — one-shot workspace setup for code intelligence.
//!
//! Run `svrn project init` from any repo root and the entire code
//! intelligence stack is wired up: tree-sitter symbol index, SCIP call
//! graph, `.claude/settings.json`, `SOVEREIGN.md`, git hooks, and a
//! filesystem watcher. Two minutes from first run to fully working tools.

use std::io::{self, BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use corpus_engine::{CorpusEngine, CorpusSpec, EmbedFn, IngestProgress};

// ─── Command submodules (god-file breakup — see quality/CLEANUP.md) ───
mod audit;
pub(crate) use audit::cmd_audit;
mod phase;
pub(crate) use phase::{cmd_phase, cmd_phase_pass};
mod charter_amend;
pub(crate) use charter_amend::{cmd_amend, cmd_charter};
mod registry_watch;
pub(crate) use registry_watch::{cmd_list, cmd_register, cmd_unregister, cmd_watch, daemon_get};
mod hooks;
use hooks::cmd_install_hooks;
mod serve;
pub(crate) use serve::cmd_serve;
mod refresh;
pub(crate) use refresh::cmd_refresh;
mod design_plan;
pub(crate) use design_plan::{cmd_design, cmd_plan};
// `init` + `scaffold` moved to `sovereign-cli::project_init` (2026-08-07) —
// `svrn init` and `svrn project init` are served by the shipped dispatcher
// now, so this binary is never asked for them.

/// Human-readable identifier for the embed model this user has set up,
/// used as the `expected_embedding_model` on the `CorpusEngine` so the
/// log line and `_corpus_meta.json` reflect what they actually loaded
/// (e.g. `qwen3-embedding-0.6b-q8_0`) instead of the engine's default.
///
/// Sources `SetupConfig::load()` and falls back to the default when
/// the user hasn't run `svrn setup` yet (in which case the
/// engine's default is harmless — code indexes are FTS-only).
// Moved to `sovereign_cli_shared::models` (2026-08-07): `project init` now
// stamps the same label from the shipped dispatcher, and the two binaries must
// agree on the embed model's name or a corpus's metadata contradicts the
// daemon that built it.
use sovereign_cli_shared::models::configured_embed_model_name;

// ─── Dispatch ────────────────────────────────────────────────

pub async fn run_project(args: &[String]) -> i32 {
    // Top-level `project --help` / `project -h` / `project help`.
    // Specific sub-subcommand help (e.g. `project init --help`) is
    // handled inside each cmd_* function via `util::help::wants_help`.
    if args.is_empty() {
        sovereign_cli_shared::help::print(&HELP);
        return 1;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        sovereign_cli_shared::help::print(&HELP);
        return 0;
    }

    // Every leaf below is also reachable as a top-level `sovereign
    // <leaf>` after the namespace collapse. The shims here keep the
    // old `project <leaf>` working — they print a one-time banner
    // and forward to the same handler the new top-level arm uses,
    // so behaviour is identical modulo the banner. Suppress with
    // SOVEREIGN_QUIET_DEPRECATIONS=1.
    use sovereign_cli_shared::deprecation::announce;
    match args[0].as_str() {
        "design" => {
            announce("svrn project design", "svrn design");
            cmd_design(&args[1..]).await
        }
        "plan" => {
            announce("svrn project plan", "svrn plan");
            cmd_plan(&args[1..]).await
        }
        "charter" => {
            announce("svrn project charter", "svrn charter");
            cmd_charter(&args[1..]).await
        }
        "found" => cmd_found(&args[1..]).await,
        "amend" => {
            announce("svrn project amend", "svrn amend");
            cmd_amend(&args[1..]).await
        }
        "phase" => cmd_phase(&args[1..]).await,
        "audit" => {
            announce("svrn project audit", "svrn audit");
            cmd_audit(&args[1..]).await
        }
        "status" => {
            announce("svrn project status", "svrn status");
            cmd_status(&args[1..]).await
        }
        "refresh" => {
            announce("svrn project refresh", "svrn refresh");
            cmd_refresh(&args[1..]).await
        }
        "serve" => {
            announce("svrn project serve", "svrn serve");
            cmd_serve(&args[1..]).await
        }
        "install-hooks" => cmd_install_hooks(&args[1..]).await,
        "register" => cmd_register(&args[1..]).await,
        "unregister" => cmd_unregister(&args[1..]).await,
        "list" => cmd_list(&args[1..]).await,
        "watch" => cmd_watch(&args[1..]).await,
        other => {
            eprintln!("Unknown project subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP);
            1
        }
    }
}

const HELP: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn project",
    summary: "Per-project code intelligence: indexes, call graphs, and the MCP tool server.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn project <subcommand> [flags]"),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            // `init` is absent on purpose: it ships in the dispatcher
            // (`svrn init` / `svrn project init`) and never reaches this
            // binary, so listing it here would advertise a verb we'd reject.
            ("design",         "Agent-collaborative DESIGN.md session (opencode-first). --solo to skip the agent"),
            ("plan",           "Compose IMPLEMENTATION_PLAN.md from DESIGN.md + OPEN_QUESTIONS.md; indexes plan items in .sovereign/plan.db"),
            ("charter",        "Write / edit the free-form team CHARTER.md (governance, culture, onboarding); separate from DESIGN.md"),
            ("found",          "Once per project: structured conversation that produces CHARTER.md + PHASES.md"),
            ("amend",          "Edit CHARTER.md with an adversarial review — every amendment logs who, why, and what was argued against"),
            ("phase",          "phase status | phase pass [N] — track PHASES.md progression, run stop conditions, write phase-N.md"),
            ("audit",          "One-page reviewer rollup: founding, phases passed, notes-by-kind, drift status, open questions, red-team findings"),
            ("status",         "Show the status of code intelligence"),
            ("refresh",        "Nudge the daemon to rebuild the SCIP graph now"),
            ("serve",          "Foreground watcher mode for debugging test/lint scripts"),
            ("register",       "Tell the daemon to watch this project (run once per repo)"),
            ("unregister",     "Remove a project from the daemon's watch list"),
            ("list",           "List every project the daemon is watching"),
            ("watch",          "Inspect or control watchers: `watch status | restart | logs`"),
            ("install-hooks",  "Deprecated — the daemon now owns freshness; prints migration hint"),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Run `svrn project <subcommand> --help` for subcommand-specific flags.",
        ),
    ],
};

const HELP_STATUS: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn project status",
    summary: "Show the status of code intelligence for the current project.",
    sections: &[sovereign_cli_shared::help::HelpSection::Usage(
        "svrn project status",
    )],
};

// ─── Status ──────────────────────────────────────────────────

pub(crate) async fn cmd_status(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_STATUS);
        return 0;
    }
    let mut data_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        if args[i].as_str() == "--data-dir" {
            i += 1;
            data_dir = args.get(i).map(PathBuf::from);
        }
        i += 1;
    }

    let repo_root = match find_repo_root() {
        Some(r) => r,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    let data_dir = data_dir
        .or_else(default_data_dir)
        .unwrap_or_else(|| PathBuf::from("./sovereign-indexes"));

    // Load project config if available.
    let config = load_project_config(&repo_root);
    let corpus_id = config
        .as_ref()
        .and_then(|c| c["corpus_id"].as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            repo_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("project")
                .to_string()
        });
    let port = config
        .as_ref()
        .and_then(|c| c["port"].as_u64())
        .unwrap_or(9741) as u16;

    println!();
    println!("  {corpus_id}");
    println!("  {}", "─".repeat(50));
    println!();

    // Index
    let index_path = data_dir.join(&corpus_id);
    if index_path.exists() {
        match corpus_engine::CorpusIndex::open(&index_path).await {
            Ok(idx) => match idx.info().await {
                Ok(info) => {
                    let age = format_age(info.last_updated);
                    println!(
                        "  Index         \u{2713} {} symbols  last updated {age}",
                        info.chunk_count
                    );
                }
                Err(_) => {
                    println!("  Index         \u{2713} present (cannot read stats)");
                }
            },
            Err(_) => {
                println!("  Index         \u{2717} corrupt or unreadable");
            }
        }
    } else {
        println!("  Index         \u{2717} not found");
        println!("                  Run: sovereign project init");
    }

    // Call graph
    let scip_graph_path = data_dir.join(&corpus_id).join("scip_graph.db");
    if scip_graph_path.exists() {
        match corpus_engine_scip::ScipGraph::open(&scip_graph_path, &corpus_id) {
            Ok(graph) => {
                let sym_count = graph.symbol_count().await;
                let ref_count = graph.ref_count().await;
                let stale_count = graph.stale_file_count().await;
                if stale_count > 0 {
                    println!(
                        "  Call graph    \u{26a0} {} symbols, {} edges  ({stale_count} files modified since last export)",
                        sym_count, ref_count
                    );
                    println!("                  Run: sovereign project refresh");
                } else {
                    println!(
                        "  Call graph    \u{2713} {} symbols, {} edges",
                        sym_count, ref_count
                    );
                }
                // scip_meta records the LATEST rebuild outcome; a failure
                // entry means the daemon's reindexer is failing and the
                // graph above is frozen at its last indexed commit — the ✓
                // counts alone would misread as "maintained".
                if let Some((err, at)) = graph.last_rebuild_failure().await {
                    println!("  Rebuild       \u{2717} last attempt FAILED ({at})");
                    println!("                  {err}");
                    println!("                  Diagnose: sovereign doctor · sovereign project watch status");
                }
            }
            Err(_) => {
                println!("  Call graph    \u{2717} corrupt or unreadable");
            }
        }
    } else {
        let scip_enabled = config
            .as_ref()
            .and_then(|c| c["scip_enabled"].as_bool())
            .unwrap_or(true);
        if scip_enabled {
            println!("  Call graph    \u{2717} not exported");
            println!("                  Run: sovereign project refresh");
        } else {
            println!("  Call graph    \u{2500} disabled (--no-scip)");
        }
    }

    // Watched — is anything actually KEEPING the two lines above fresh?
    //
    // Without this, `status` stats the artifacts and reports ✓/✓ for an
    // index nobody has maintained in weeks: freshness is owned by the
    // daemon's Reindexer, which builds one ProjectHandle (FS watcher, git
    // HEAD poll, rebuild queue) per REGISTERED project. An empty
    // `projects.json` means zero watchers, and every stat above still
    // reads green because the files are sitting right there. Observed
    // 2026-07-24: a 27-day-old chunk index and an 11-day-old call graph
    // both reporting ✓ on a repo that had been unregistered since June.
    // Same failure the lint/test surface fixed with `watcher.live` /
    // `watcher_down`; the code-intel surface never got it.
    match sovereign_mesh::projects::Registry::load() {
        Ok(registry) => match registry.entries().iter().find(|e| e.corpus_id == corpus_id) {
            Some(entry) if entry.root == repo_root => {
                let mut on: Vec<&str> = Vec::new();
                if entry.watchers.scip {
                    on.push("scip");
                }
                if entry.watchers.lint {
                    on.push("lint");
                }
                if entry.watchers.test {
                    on.push("test");
                }
                let watchers = if on.is_empty() {
                    "all watchers disabled".to_string()
                } else {
                    on.join(", ")
                };
                println!("  Watched       \u{2713} registered ({watchers})");
            }
            Some(entry) => {
                println!(
                    "  Watched       \u{26a0} registered under a different root: {}",
                    entry.root.display()
                );
                println!(
                    "                  Nothing is watching {}",
                    repo_root.display()
                );
                println!("                  Run: sovereign project register");
            }
            None => {
                println!("  Watched       \u{2717} NOT registered — nothing refreshes this");
                println!("                  The Index/Call graph above are whatever was");
                println!("                  last built by hand; no watcher maintains them.");
                println!("                  Run: sovereign project register");
            }
        },
        Err(e) => {
            println!("  Watched       \u{2717} cannot read project registry: {e}");
        }
    }

    // MCP server
    let mcp_url = format!("http://localhost:{port}/mcp");
    if check_mcp_server(&mcp_url).await {
        println!("  MCP server    \u{2713} {mcp_url}");
    } else {
        println!("  MCP server    \u{2717} not running");
        println!("                  Run: sovereign-server --config <config.toml>");
    }

    // SOVEREIGN.md
    let sovereign_md = repo_root.join(".sovereign").join("SOVEREIGN.md");
    if sovereign_md.exists() {
        println!("  SOVEREIGN.md  \u{2713} .sovereign/SOVEREIGN.md");
    } else {
        println!("  SOVEREIGN.md  \u{2717} not found");
    }

    // Claude config
    let claude_settings = repo_root.join(".claude").join("settings.json");
    if claude_settings.exists() {
        // Check if it has sovereign MCP config.
        let has_sovereign = std::fs::read_to_string(&claude_settings)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v["mcpServers"]["sovereign"].as_object().cloned())
            .is_some();
        if has_sovereign {
            println!("  Claude config \u{2713} .claude/settings.json");
        } else {
            println!("  Claude config \u{26a0} .claude/settings.json (no sovereign MCP entry)");
        }
    } else {
        println!("  Claude config \u{2717} not found");
    }

    // Git hook
    let hook_path = repo_root.join(".git").join("hooks").join("post-commit");
    if hook_path.exists() {
        let contents = std::fs::read_to_string(&hook_path).unwrap_or_default();
        if contents.contains(SOVEREIGN_HOOK_MARKER) {
            println!("  Git hook      \u{2713} installed (v3: symbols + SCIP)");
        } else if contents.contains("sovereign") && contents.contains("project refresh") {
            println!(
                "  Git hook      \u{26a0} prior version (refreshes SCIP only) — run \
                 `svrn project install-hooks` to upgrade"
            );
        } else {
            println!("  Git hook      \u{2717} exists but missing sovereign refresh");
        }
    } else if repo_root.join(".git").exists() {
        println!("  Git hook      \u{2717} not installed");
    }

    // Tools available
    println!();
    println!("  Tools available:");
    print!("    symbol_lookup    recent_changes    code_search");
    let scip_available = scip_graph_path.exists();
    if scip_available {
        println!();
        println!("    find_callers     find_callees");
    } else {
        println!();
    }

    println!();
    println!("  Run `svrn project refresh` to update the call graph.");
    println!();

    0
}

// `MergedGraphSummary`, `load_merged_graph`, and `snapshot_graph_mtimes`
// moved to `sovereign-cli-shared::scip` so the new `sovereign-cli-atos`
// binary can share one implementation with `tools_cmd::registry`. The
// re-exports below preserve the prior `crate::project_cmd::…` call sites.
pub(crate) use sovereign_cli_shared::scip::{load_merged_graph, snapshot_graph_mtimes};

// `--orchestrate` (which sequenced DESIGN.md + CHARTER.md +
// IMPLEMENTATION_PLAN.md + PHASES.md composition) is retired in
// favour of the explicit `svrn design` / `svrn charter`
// / `svrn plan` triad.
async fn cmd_found(_args: &[String]) -> i32 {
    sovereign_cli_shared::deprecation::announce_retired(
        "svrn project found",
        "Founding is implicit now: `svrn init` + a committed          spec is sufficient. Use `svrn charter` if you want          to define team conventions, or `svrn plan` to write          PHASES.md from a design doc.",
    );
    0
}

fn git_committer_identity_for_amend(repo_root: &Path) -> Option<String> {
    let name = std::process::Command::new("git")
        .args(["config", "user.name"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    let email = std::process::Command::new("git")
        .args(["config", "user.email"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !name.status.success() || !email.status.success() {
        return None;
    }
    Some(format!(
        "{} <{}>",
        String::from_utf8_lossy(&name.stdout).trim(),
        String::from_utf8_lossy(&email.stdout).trim(),
    ))
}

fn today_iso() -> String {
    let secs = unix_now_secs();
    let days = secs / 86400;
    // Howard Hinnant's civil-from-days (same algorithm as
    // middleware/artifact_surface::rfc3339_to_unix, in reverse).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn derive_project_id(repo_root: &Path) -> String {
    repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string()
}

use sovereign_core::time::unix_now as unix_now_secs;

// ─── Observation report (M6.1) ──────────────────────────────
//
// Consumes `crate::observation::ProjectObservation` and renders it
// in three buckets (Ready / Actionable / Deferred-to-found) per the
// M6 requirements. Actionable items state the install command on
// its own line, copy-pasteable, unindented — the user can paste and
// run without editing.

// ─── Git helpers ─────────────────────────────────────────────
// Implementations moved to `sovereign-cli-shared::repo`; re-exported
// for in-crate callers that reference `project_cmd::find_*`.
pub(crate) use sovereign_cli_shared::repo::{find_repo_root, find_sovereign_dir};

/// Base URL the CLI uses to talk to the local daemon.
///
/// Loopback-only by design — the freshness HTTP surface never talks to a
/// remote host — but the PORT comes from the operator's config. This was a
/// `const` pinned to `:9741`, which meant every `project` subcommand
/// (`list`, `register`, `refresh`, `status`) reported "daemon call failed"
/// against a perfectly healthy daemon whenever `[daemon] client_port` was
/// set to anything else. `code_map.rs` was already resolving the port this
/// way; this side simply had not been updated. Found 2026-07-28 by the
/// journey harness's sandbox, which runs its daemon on :19741.
fn daemon_base() -> String {
    let port = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.daemon.client_port)
        .unwrap_or(9741);
    format!("http://127.0.0.1:{port}")
}

// ─── Small helpers used by the new subcommands ───────────────

/// Best-guess corpus id for a project root. Matches the logic
/// `cmd_init` uses so `register` and `init` produce the same
/// registration key by default.
fn derive_corpus_id(root: &Path) -> String {
    root.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string()
}

async fn daemon_post(path: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let url = format!("{}{path}", daemon_base());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("POST {path}: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .unwrap_or(serde_json::json!({"error": "non-JSON response"}));
    if !status.is_success() {
        return Err(format!("{status}: {body}"));
    }
    Ok(body)
}

/// Cheap TCP + `GET /v1/models` probe. Matches what the desktop's
/// bootstrap does (see `sovereign-desktop/src-tauri/src/bootstrap.rs`).
/// Used by `cmd_serve` to decide whether to refuse the legacy path.
pub(crate) async fn daemon_is_running() -> bool {
    let tcp = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tokio::net::TcpStream::connect(("127.0.0.1", 9741)),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false);
    if !tcp {
        return false;
    }
    match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1))
        .build()
    {
        Ok(c) => c
            .get("http://127.0.0.1:9741/v1/models")
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// Scan `.git/hooks/post-commit` for a `SOVEREIGN_HOOK_V*` marker
/// and remove the whole file (we were the sole owner). Returns
/// `Ok(true)` when a hook was removed, `Ok(false)` when none was
/// found. If the hook file contains both sovereign content and
/// other content, we leave it alone — the user is expected to
/// clean it up manually.
// Moved to `sovereign_cli_shared::repo` (2026-08-07). `project init` (shipped
// dispatcher) removes legacy hooks and `project install-hooks` (here) writes
// them, so the marker they agree on cannot live in one binary.
pub(crate) use sovereign_cli_shared::repo::{remove_legacy_hook, SOVEREIGN_HOOK_MARKER};

// ─── Git hooks (deprecated installer — kept for migration tests only) ──
//
// The post-commit hook installer used to be wired into `cmd_init`, but
// freshness is now handled by the daemon's watcher (see `corpus-engine`
// `update::watcher` and the daemon's reindex loop). The CLI still
// recognizes `svrn project install-hooks` as a deprecated
// subcommand that prints a migration hint, but no production code path
// installs a hook anymore.
//
// The installer + stripper functions below are retained because they
// still pin the hook-block format invariants in tests
// (`strip_prior_sovereign_block_*` etc.) — those tests run against a
// fixed string corpus so a regression in the format could still trip a
// real user with a legacy hook installed by an older binary.
// ─── MCP check ───────────────────────────────────────────────

// Moved to `sovereign_cli_shared::mcp_client` (2026-08-07) alongside
// `remove_legacy_hook` — `project init` probes the same endpoint at the tail
// of its run, from the other binary.
use sovereign_cli_shared::mcp_client::check_mcp_server;

// ─── Helpers ─────────────────────────────────────────────────

// `default_data_dir` lives in `sovereign-cli-shared::dirs`; re-exported
// for `crate::project_cmd::default_data_dir` callers.
pub(crate) use sovereign_cli_shared::dirs::default_data_dir;

fn tempfile_dir() -> std::io::Result<PathBuf> {
    let base = std::env::temp_dir();
    let suffix = format!("sovereign-project-{}", std::process::id());
    let path = base.join(suffix);
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn load_project_config(root: &Path) -> Option<serde_json::Value> {
    let path = root.join(".sovereign").join("project.json");
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Format a Unix timestamp as a human-readable relative time.
fn format_age(unix_ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if unix_ts == 0 {
        return "unknown".to_string();
    }

    let diff = now.saturating_sub(unix_ts);
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{} min ago", diff / 60)
    } else if diff < 86400 {
        format!("{} hours ago", diff / 3600)
    } else {
        format!("{} days ago", diff / 86400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── git hook helpers ─────────────────────────────────────────

    #[tokio::test]
    async fn snapshot_graph_mtimes_tracks_files() {
        let tmp = tempfile::tempdir().unwrap();
        let corpus_dir = tmp.path().join("test-corpus");
        std::fs::create_dir(&corpus_dir).unwrap();
        let graph_path = corpus_dir.join("scip_graph.db");
        std::fs::write(&graph_path, b"stub").unwrap();

        let snap = snapshot_graph_mtimes(tmp.path());
        assert_eq!(snap.len(), 1);
        assert!(snap.contains_key(&graph_path));

        // Empty dir → empty snapshot.
        let empty_tmp = tempfile::tempdir().unwrap();
        let empty_snap = snapshot_graph_mtimes(empty_tmp.path());
        assert!(empty_snap.is_empty());
    }
}
