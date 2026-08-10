// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn project register` / `unregister` / `list` / `watch` — the
//! daemon-facing project registry and watcher-control subcommands, plus
//! their local helpers (`daemon_get`, `format_graph_age`,
//! `print_simple_help`). The shared `daemon_post` / `derive_corpus_id` /
//! `daemon_base()` stay in `super` (they're also used by init/refresh) and
//! resolve through `use super::*`. Split out of `project_cmd` (2026-07-13);
//! pure move.

use super::*;

pub(crate) async fn cmd_register(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        print_simple_help(
            "svrn project register",
            "Register the current directory with the daemon's freshness pipeline.",
            &[
                "svrn project register",
                "svrn project register --root /path/to/repo",
                "svrn project register --name my-monorepo",
                "svrn project register --corpus-id my-monorepo   # alias of --name",
                "svrn project register --force   # override the nested-root guard",
            ],
        );
        return 0;
    }

    let mut root: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut force = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                root = args.get(i).map(PathBuf::from);
            }
            // `--corpus-id` is the name the registry itself uses (`project
            // list` prints `corpus_id`, `project unregister` takes one) and is
            // what .claude/CLAUDE.md documents as the watcher-registry repair.
            // It was never parsed, and the catch-all below swallowed it, so
            // `project register --corpus-id foo` registered the DERIVED id and
            // printed "✓ Registered project "<derived>"" — a success message
            // for a registration the operator did not ask for. Accepted as an
            // alias rather than renamed, because `--name` is what `doctor`
            // prints today (doctor_cmd.rs:1723) and operators have it in
            // muscle memory and scripts.
            "--name" | "--corpus-id" => {
                i += 1;
                name = args.get(i).cloned();
            }
            "--force" => force = true,
            // An unrecognised flag is an ERROR, not a no-op. The catch-all that
            // used to live here is how the above went unnoticed: a typo'd or
            // renamed flag silently changed WHICH project got registered while
            // still exiting 0. Positionals stay tolerated — only `-`-prefixed
            // tokens are rejected, so this cannot break an unquoted path.
            other if other.starts_with('-') => {
                eprintln!("error: unknown flag `{other}` for `svrn project register`.");
                eprintln!("hint: valid flags are --root <path>, --name|--corpus-id <id>, --force.");
                return 2;
            }
            _ => {}
        }
        i += 1;
    }

    let root = root
        .or_else(find_repo_root)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let root = root.canonicalize().unwrap_or(root);
    let corpus_id = name.unwrap_or_else(|| derive_corpus_id(&root));

    let body = serde_json::json!({
        "corpus_id": corpus_id,
        "root": root.display().to_string(),
        "force": force,
    });
    match daemon_post("/v1/projects/register", body).await {
        Ok(resp) => {
            let created = resp["created"].as_bool().unwrap_or(false);
            println!(
                "  \u{2713} {} project \"{}\" at {}",
                if created { "Registered" } else { "Updated" },
                corpus_id,
                root.display()
            );
            println!("    The daemon is now watching this project. Use `svrn project watch status` to inspect.");
            0
        }
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            eprintln!("hint: is the daemon running? try `svrn daemon status`.");
            1
        }
    }
}

pub(crate) async fn cmd_unregister(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        print_simple_help(
            "svrn project unregister",
            "Stop the daemon from watching a project.",
            &["svrn project unregister <corpus_id>"],
        );
        return 0;
    }
    let Some(corpus_id) = args.first().cloned() else {
        eprintln!("error: missing corpus_id. usage: sovereign project unregister <corpus_id>");
        return 1;
    };
    match daemon_post(
        &format!("/v1/projects/{corpus_id}/unregister"),
        serde_json::json!({}),
    )
    .await
    {
        Ok(resp) => {
            let removed = resp["removed"].as_bool().unwrap_or(false);
            if removed {
                println!("  \u{2713} Unregistered \"{corpus_id}\".");
            } else {
                println!("  \"{corpus_id}\" was not registered — nothing to do.");
            }
            0
        }
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            1
        }
    }
}

pub(crate) async fn cmd_list(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        print_simple_help(
            "svrn project list",
            "List every project the daemon is watching.",
            &["svrn project list"],
        );
        return 0;
    }
    match daemon_get("/v1/projects").await {
        Ok(resp) => {
            let Some(projects) = resp["projects"].as_array() else {
                println!("  (empty response)");
                return 0;
            };
            if projects.is_empty() {
                println!("  No projects registered yet. Run `svrn project register` in a repo to add one.");
                return 0;
            }
            println!("  Registered projects:");
            for p in projects {
                let id = p["corpus_id"].as_str().unwrap_or("?");
                let root = p["root"].as_str().unwrap_or("?");
                let age = p["graph_age_secs"].as_u64();
                let age_str = match age {
                    Some(s) => format_graph_age(s),
                    None => "never built".to_string(),
                };
                let in_flight = p["rebuild_in_flight"].as_bool().unwrap_or(false);
                println!(
                    "    {id}  ({age_str}){}",
                    if in_flight { "  [rebuilding]" } else { "" }
                );
                println!("      root: {root}");
            }
            0
        }
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            eprintln!("hint: is the daemon running? try `svrn daemon status`.");
            1
        }
    }
}

pub(crate) async fn cmd_watch(args: &[String]) -> i32 {
    if args.is_empty() || sovereign_cli_shared::help::wants_help(args) {
        print_simple_help(
            "svrn project watch",
            "Inspect or control per-project watchers.",
            &[
                "svrn project watch status [<id>]",
                "svrn project watch restart <id> [<watcher>]",
                "svrn project watch logs <id> <watcher>",
            ],
        );
        return if args.is_empty() { 1 } else { 0 };
    }
    match args[0].as_str() {
        "status" => cmd_watch_status(&args[1..]).await,
        "restart" => cmd_watch_restart(&args[1..]).await,
        "logs" => cmd_watch_logs(&args[1..]).await,
        other => {
            eprintln!("Unknown watch subcommand: {other}");
            1
        }
    }
}

async fn cmd_watch_status(args: &[String]) -> i32 {
    let target = args.first().cloned();
    let resp = match daemon_get("/v1/projects").await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            return 1;
        }
    };
    let Some(projects) = resp["projects"].as_array() else {
        return 1;
    };
    let filtered: Vec<_> = projects
        .iter()
        .filter(|p| match target.as_deref() {
            Some(id) => p["corpus_id"].as_str() == Some(id),
            None => true,
        })
        .collect();
    if filtered.is_empty() {
        if let Some(id) = target {
            eprintln!(
                "\"{id}\" is not registered. run `svrn project list` to see registered projects."
            );
        } else {
            println!("  no projects registered yet.");
        }
        return 1;
    }
    for p in filtered {
        let id = p["corpus_id"].as_str().unwrap_or("?");
        println!("  {id}");
        let Some(status) = p["status"].as_object() else {
            continue;
        };
        for (watcher, s) in status {
            let state = s["state"].as_str().unwrap_or("?");
            let extra = match state {
                "crashed" => {
                    let reason = s["reason"].as_str().unwrap_or("?");
                    let count = s["count"].as_u64().unwrap_or(0);
                    format!(" — {count} crashes, last: {reason}")
                }
                "disabled" => {
                    let reason = s["reason"].as_str().unwrap_or("?");
                    format!(" — {reason}")
                }
                _ => String::new(),
            };
            println!("    {watcher:8}  {state}{extra}");
        }
        if let Some(age) = p["graph_age_secs"].as_u64() {
            println!("    graph age: {}", format_graph_age(age));
        }
    }
    0
}

async fn cmd_watch_restart(args: &[String]) -> i32 {
    let Some(corpus_id) = args.first().cloned() else {
        eprintln!("error: usage: sovereign project watch restart <corpus_id>");
        return 1;
    };
    // For the MVP, "restart" just means "trigger a rebuild". A
    // full per-watcher restart (re-spawn a Disabled test runner,
    // for example) requires state plumbing that lands in a later
    // step; rebuild is the action users reach for 90% of the time.
    match daemon_post(
        &format!("/v1/projects/{corpus_id}/rebuild"),
        serde_json::json!({ "reason": "manual restart via CLI" }),
    )
    .await
    {
        Ok(_) => {
            println!("  \u{2713} Rebuild nudged for \"{corpus_id}\".");
            println!("    Check progress with `svrn project watch status {corpus_id}`.");
            0
        }
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            1
        }
    }
}

async fn cmd_watch_logs(args: &[String]) -> i32 {
    let Some(corpus_id) = args.first().cloned() else {
        eprintln!("error: usage: sovereign project watch logs <corpus_id> [<watcher>]");
        return 1;
    };
    let watcher = args.get(1).cloned().unwrap_or_else(|| "scip".to_string());
    let log_path = sovereign_cli_shared::dirs::sovereign_root()
        .join("logs")
        .join(format!("watch-{corpus_id}-{watcher}.log"));
    if !log_path.exists() {
        eprintln!(
            "no log file at {} — the daemon writes per-watcher logs here once the first cycle runs.",
            log_path.display()
        );
        return 1;
    }
    // Print the file contents. `tail -f` semantics would be nicer
    // but pulling in a tailer adds complexity; reading once and
    // exiting is predictable and scriptable.
    match std::fs::read_to_string(&log_path) {
        Ok(s) => {
            print!("{s}");
            0
        }
        Err(e) => {
            eprintln!("error: read {}: {e}", log_path.display());
            1
        }
    }
}

/// Render a duration-since in a compact, human-readable form.
/// Used by `project list` and `project watch status`. Named
/// `format_graph_age` to avoid colliding with the older helper
/// in this module that produces a different phrasing.
fn format_graph_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s old")
    } else if secs < 3600 {
        format!("{}m old", secs / 60)
    } else if secs < 86400 {
        format!("{}h old", secs / 3600)
    } else {
        format!("{}d old", secs / 86400)
    }
}

async fn daemon_get(path: &str) -> Result<serde_json::Value, String> {
    let url = format!("{}{path}", super::daemon_base());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("GET {path}: {e}"))?;
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

fn print_simple_help(command: &str, summary: &str, examples: &[&str]) {
    println!();
    println!("  {command}");
    println!("  {}", "─".repeat(50));
    println!("  {summary}");
    println!();
    println!("  Usage:");
    for ex in examples {
        println!("    {ex}");
    }
    println!();
}
