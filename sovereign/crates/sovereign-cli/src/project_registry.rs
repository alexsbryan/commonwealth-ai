// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn project register` / `unregister` / `list` / `watch` — the
//! daemon-facing project registry, running IN the shipped dispatcher.
//!
//! # Why this lives here and not in the `sovereign-cli-dev` sibling
//!
//! The daemon already contains the entire code-intelligence pipeline: it
//! builds the `Reindexer` at boot and replays `~/.sovereign/projects.json`
//! (`sovereign-mesh` bootstrap), then runs `scip_export::export_all` per
//! project. A user who installed via `curl | sh` is therefore ONE HTTP POST
//! away from working `callers` / `callees` / `blast` — and could not make
//! that POST, because `project` was a dev verb and the binary that served
//! it was never in the release tarball.
//!
//! These four subcommands are pure HTTP + `serde_json` + `dirs`. They add
//! ZERO dependencies to this crate (verified against `Cargo.toml`: every
//! import below was already present), so there is no size or link argument
//! for keeping them behind the workbench boundary.
//!
//! # What this module is NOT
//!
//! It does not index, parse, or embed anything, and it must not grow to.
//! Registration hands the work to the daemon, which owns the one
//! tree-sitter/SCIP/llama.cpp stack in the system. The heavier verbs
//! (`project init`, `code index`) reach the daemon over the same loopback
//! HTTP surface for embeddings; nothing in the CLI ever loads a model.
//!
//! Ported from `sovereign-cli-dev/src/project_cmd/registry_watch.rs`. The
//! four `daemon_*` / `derive_corpus_id` helpers came from that crate's
//! `project_cmd/mod.rs` and are still duplicated there.
//!
//! The original note here said hoisting them was blocked because it would
//! "pull `sovereign-core`'s `SetupConfig` into every consumer" of
//! sovereign-cli-shared. That premise is wrong and was corrected 2026-08-07:
//! `SetupConfig` is defined in `sovereign-contracts`
//! (`setup_config.rs:32`) — sovereign-core only re-exports it — and
//! sovereign-cli-shared already depends on sovereign-contracts. So the hoist
//! is available whenever the duplication starts costing something; see
//! `sovereign_cli_shared::models`, which took exactly that route.

use std::path::{Path, PathBuf};

use sovereign_cli_shared::repo::find_repo_root;

/// Subcommands this module serves in-process. Anything else under
/// `svrn project` still routes to the `sovereign-cli-dev` sibling.
///
/// Kept as an explicit list rather than a catch-all so that adding a verb
/// here is a deliberate act: every name in this array is a promise that the
/// shipped binary can honour it with no sibling present.
/// `init` is on this list only under `code-intel`, and that is the whole
/// point of the split: without an indexer the verb cannot honour its promise,
/// so it must fall through to the refusal rather than half-run.
#[cfg(feature = "code-intel")]
const IN_PROCESS: &[&str] = &["register", "unregister", "list", "watch", "init"];
#[cfg(not(feature = "code-intel"))]
const IN_PROCESS: &[&str] = &["register", "unregister", "list", "watch"];

/// Returns `Some(exit_code)` when this module owns the subcommand, `None`
/// when the caller should fall through to the sibling binary.
pub async fn try_run(args: &[String]) -> Option<i32> {
    let sub = args.first()?;
    if !IN_PROCESS.contains(&sub.as_str()) {
        return None;
    }
    let rest = &args[1..];
    Some(match sub.as_str() {
        "register" => cmd_register(rest).await,
        "unregister" => cmd_unregister(rest).await,
        "list" => cmd_list(rest).await,
        "watch" => cmd_watch(rest).await,
        // `svrn project init` and `svrn init` are the same handler. The flat
        // verb additionally chains `serve --background` (see `init.rs`); this
        // path stays a pure alias, which is what it has always been.
        //
        // The `announce` is load-bearing, not decoration: the old name used to
        // be reached through `project_cmd::run_project`, which announced there.
        // Moving the verb here dropped the banner silently — `aliases.rs`
        // (`alias_init`) caught it. Every other `project <leaf>` alias still
        // announces from the sibling, so without this the deprecation surface
        // would be inconsistent for exactly the verb users type most.
        #[cfg(feature = "code-intel")]
        "init" => {
            sovereign_cli_shared::deprecation::announce("svrn project init", "svrn init");
            crate::project_init::cmd_init(rest).await
        }
        _ => unreachable!("guarded by IN_PROCESS"),
    })
}

/// Refuse a `project` subcommand that still lives in the workbench sibling,
/// naming the ones this binary CAN serve.
///
/// The old blanket intercept exited 2 with "restore it with `cargo build -p
/// sovereign-cli --features dev-tools`" — advice a `curl | sh` user cannot
/// act on, since they have no checkout. This says what is true instead: the
/// subcommand is not in this build, and here is what is.
pub fn refuse_workbench_subcommand(sub: Option<&str>) -> i32 {
    // `project refresh` did not move to the workbench — it was RENAMED to the
    // top-level `svrn refresh`, and it does ship. Sending that user to the
    // generic refusal below would be telling them a capability they have is
    // missing, which is the same class of untruth this function exists to end.
    if sub == Some("refresh") {
        eprintln!("svrn project refresh: renamed to `svrn refresh`.");
        eprintln!();
        eprintln!("  Run: svrn refresh");
        return 2;
    }
    match sub {
        Some(s) => eprintln!("svrn project {s}: not available in this build."),
        None => eprintln!("svrn project: missing subcommand."),
    }
    eprintln!();
    eprintln!("  Available here:");
    #[cfg(feature = "code-intel")]
    eprintln!("    svrn project init            (or just `svrn init`)");
    eprintln!("    svrn project register [--root <path>] [--corpus-id <id>]");
    eprintln!("    svrn project unregister <corpus_id>");
    eprintln!("    svrn project list");
    eprintln!("    svrn project watch status|restart|logs");
    eprintln!();
    eprintln!("  Registering a repo is enough for the daemon to build and keep");
    eprintln!("  its SCIP graph — that is what powers callers/callees/blast.");
    2
}

async fn cmd_register(args: &[String]) -> i32 {
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
            // prints today and operators have it in muscle memory and scripts.
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
            // Registration is the moment the user points us at a repo, so it
            // is the moment to say what is missing. `svrn doctor` already
            // reports this (doctor_cmd.rs:2087) but doctor is what you run
            // AFTER something feels wrong — by then the user has a silently
            // empty call graph and no idea why.
            #[cfg(feature = "code-intel")]
            warn_missing_exporters(&root);
            0
        }
        Err(e) => {
            eprintln!("error: daemon call failed: {e}");
            eprintln!("hint: is the daemon running? try `svrn daemon status`.");
            1
        }
    }
}

/// Name the SCIP indexers this repo needs and does not have.
///
/// Code intelligence is TWO gates, not one. `svrn code index` builds the chunk
/// corpus with tree-sitter, which ships in-process. The call graph is built by
/// EXTERNAL binaries — `scip-go`, `scip-typescript`, `rust-analyzer` — invoked
/// by the daemon's reindexer. Miss one and `callers` / `callees` / `blast`
/// return nothing, with no error anywhere the user looks: the export fails
/// inside the daemon and the graph is simply empty.
///
/// `check_exporters` was written for exactly this ("so callers can show
/// actionable install instructions instead of silently producing an empty call
/// graph", corpus-engine-scip/src/scip_export.rs) and only `doctor` called it.
#[cfg(feature = "code-intel")]
fn warn_missing_exporters(root: &Path) {
    use corpus_engine_scip::scip_export;

    // Match the daemon's own root resolution, or the globs look in the wrong
    // place for a workspace with nested members.
    let mut roots = scip_export::find_cargo_workspace_roots(root);
    if roots.is_empty() {
        roots.push(root.to_path_buf());
    }
    let check = scip_export::check_exporters(&roots);
    if check.missing.is_empty() {
        return;
    }
    eprintln!();
    eprintln!(
        "  \u{26a0} This repo has code in {} language(s) whose SCIP indexer is not on PATH:",
        check.missing.len()
    );
    for m in &check.missing {
        eprintln!("      {} ({}) — {}", m.language_id, m.command, m.install_hint);
    }
    eprintln!("    Until then the call graph stays empty for those languages, so");
    eprintln!("    callers/callees/blast return nothing. Text and chunk search are");
    eprintln!("    unaffected. Re-check any time with `svrn doctor`.");
}

async fn cmd_unregister(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        print_simple_help(
            "svrn project unregister",
            "Stop the daemon from watching a project.",
            &["svrn project unregister <corpus_id>"],
        );
        return 0;
    }
    let Some(corpus_id) = args.first().cloned() else {
        eprintln!("error: missing corpus_id. usage: svrn project unregister <corpus_id>");
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

async fn cmd_list(args: &[String]) -> i32 {
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
                let age_str = match p["graph_age_secs"].as_u64() {
                    Some(s) => format_graph_age(s),
                    None => "never built".to_string(),
                };
                let in_flight = p["rebuild_in_flight"].as_bool().unwrap_or(false);
                let failures = p["rebuild_failures"].as_u64().unwrap_or(0);
                println!(
                    "    {id}  ({age_str}){}{}",
                    if in_flight { "  [rebuilding]" } else { "" },
                    if failures > 0 {
                        format!("  [REBUILD FAILING ×{failures}]")
                    } else {
                        String::new()
                    }
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

async fn cmd_watch(args: &[String]) -> i32 {
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
        // A repeating rebuild failure means the graph is frozen at its last
        // indexed commit — the single most operator-relevant fact here.
        let failures = p["rebuild_failures"].as_u64().unwrap_or(0);
        if failures > 0 {
            let err = p["last_rebuild_error"][0].as_str().unwrap_or("?");
            println!("    rebuild:   FAILING — {failures} consecutive failure(s)");
            println!("               last error: {err}");
            println!("               the graph is frozen at its last indexed commit until this is fixed");
        }
    }
    0
}

async fn cmd_watch_restart(args: &[String]) -> i32 {
    let Some(corpus_id) = args.first().cloned() else {
        eprintln!("error: usage: svrn project watch restart <corpus_id>");
        return 1;
    };
    // For the MVP, "restart" just means "trigger a rebuild". A full
    // per-watcher restart (re-spawn a Disabled test runner, for example)
    // requires state plumbing that lands in a later step; rebuild is the
    // action users reach for 90% of the time.
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
        eprintln!("error: usage: svrn project watch logs <corpus_id> [<watcher>]");
        return 1;
    };
    let watcher = args.get(1).cloned().unwrap_or_else(|| "scip".to_string());
    let log_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".sovereign")
        .join("logs")
        .join(format!("watch-{corpus_id}-{watcher}.log"));
    if !log_path.exists() {
        eprintln!(
            "no log file at {} — the daemon writes per-watcher logs here once the first cycle runs.",
            log_path.display()
        );
        return 1;
    }
    // Print the file contents. `tail -f` semantics would be nicer but pulling
    // in a tailer adds complexity; reading once and exiting is predictable
    // and scriptable.
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

/// Best-guess corpus id for a project root. Must match the logic `cmd_init`
/// uses so `register` and `init` produce the same registration key by
/// default — a mismatch here silently registers a SECOND project pointing at
/// the same root.
pub(crate) fn derive_corpus_id(root: &Path) -> String {
    root.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project")
        .to_string()
}

/// Base URL the CLI uses to talk to the local daemon.
///
/// Loopback-only by design — the freshness HTTP surface never talks to a
/// remote host — but the PORT comes from the operator's config. This was once
/// a `const` pinned to `:9741`, which meant every `project` subcommand
/// reported "daemon call failed" against a perfectly healthy daemon whenever
/// `[daemon] client_port` was set to anything else. Found 2026-07-28 by the
/// journey harness's sandbox, which runs its daemon on :19741.
fn daemon_base() -> String {
    let port = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.daemon.client_port)
        .unwrap_or(9741);
    format!("http://127.0.0.1:{port}")
}

async fn daemon_get(path: &str) -> Result<serde_json::Value, String> {
    let url = format!("{}{path}", daemon_base());
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

pub(crate) async fn daemon_post(path: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of this module: every name here must be servable with
    /// no sibling binary present. If someone adds a subcommand that needs the
    /// workbench, this list is where the mistake becomes visible.
    #[test]
    fn in_process_set_is_exactly_what_this_build_can_serve() {
        let daemon_facing = ["register", "unregister", "list", "watch"];
        if cfg!(feature = "code-intel") {
            // `init` is the fifth only when there is an indexer to back it.
            assert_eq!(IN_PROCESS.len(), 5);
            assert_eq!(&IN_PROCESS[..4], &daemon_facing);
            assert_eq!(IN_PROCESS[4], "init");
        } else {
            assert_eq!(IN_PROCESS, &daemon_facing);
        }
    }

    #[tokio::test]
    async fn try_run_declines_subcommands_it_does_not_own() {
        // Every name here must be unowned in EVERY build shape, because
        // `try_run` DISPATCHES: a wrong entry does not fail the assertion, it
        // EXECUTES the command. `init` was in this list until 2026-08-07 and
        // the moment it moved in-process this test ran a real `project init`
        // against the checkout's own working directory — it rewrote the repo's
        // `.sovereign/project.toml` and then hung 180 s on `cmd_init`'s stdin
        // prompt. Add a verb here only if this module can never serve it.
        for sub in ["serve", "status", "found", "design"] {
            assert!(
                try_run(&[sub.to_string()]).await.is_none(),
                "{sub} must fall through to the sibling"
            );
        }
    }

    #[tokio::test]
    async fn try_run_declines_empty_args() {
        assert!(try_run(&[]).await.is_none());
    }

    #[tokio::test]
    async fn owned_subcommands_are_claimed() {
        // `--help` short-circuits before any daemon call, so this exercises
        // the dispatch without needing a live daemon.
        for sub in IN_PROCESS {
            let args = vec![sub.to_string(), "--help".to_string()];
            assert_eq!(
                try_run(&args).await,
                Some(0),
                "{sub} must be served in-process"
            );
        }
    }

    #[test]
    fn derive_corpus_id_uses_the_directory_name() {
        assert_eq!(derive_corpus_id(Path::new("/a/b/my-repo")), "my-repo");
        // A root with no file_name (e.g. `/`) must still produce a usable id
        // rather than panicking or yielding an empty registration key.
        assert_eq!(derive_corpus_id(Path::new("/")), "project");
    }

    #[test]
    fn graph_age_reads_in_the_largest_unit_that_fits() {
        assert_eq!(format_graph_age(0), "0s old");
        assert_eq!(format_graph_age(59), "59s old");
        assert_eq!(format_graph_age(60), "1m old");
        assert_eq!(format_graph_age(3599), "59m old");
        assert_eq!(format_graph_age(3600), "1h old");
        assert_eq!(format_graph_age(86399), "23h old");
        assert_eq!(format_graph_age(86400), "1d old");
    }
}
