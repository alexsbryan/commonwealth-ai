//! `sovereign pipeline …` — generic ingestion-pipeline driver.
//!
//! Surface:
//!
//! ```text
//! sovereign pipeline run    <recipe.toml> [--db <path>] [--seed-only]
//! sovereign pipeline status <recipe-id>   [--db <path>]
//! sovereign pipeline list   [--db <path>]
//! ```
//!
//! State lives in `--db` (defaults to `~/.sovereign/pipeline.db`).
//! Multiple recipes can share one DB; rows are keyed by `recipe_id`.
//! See `sovereign_pipeline` crate docs for the worklist semantics.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sovereign_pipeline::driver::{DriverConfig, Shutdown};
use sovereign_pipeline::{recipe::Recipe, run_recipe, status, worklist::Worklist};
use tokio::sync::Mutex;

use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign pipeline",
    summary: "Generic ingestion-pipeline driver — durable worklist + retry + pause-resume.",
    sections: &[
        HelpSection::Usage(
            "sovereign pipeline <run | status | list> [flags]",
        ),
        HelpSection::Subcommands(&[
            (
                "run <recipe.toml>",
                "Seed + sweep + drive the recipe to completion. \
                 SIGINT/SIGTERM drains in-flight units, then exits cleanly. \
                 Re-running picks up where the previous run left off.",
            ),
            (
                "status <recipe-id>",
                "Print pending/done/failed counts, last-hour throughput, ETA, failure buckets.",
            ),
            (
                "list",
                "List every recipe-id known to the worklist DB.",
            ),
        ]),
        HelpSection::Flags(&[
            (
                "--db <path>",
                "Override the worklist DB path. Default: ~/.sovereign/pipeline.db.",
            ),
            (
                "--seed-only",
                "(run) Seed the worklist from the recipe's source and exit without dispatching.",
            ),
            (
                "--slugs <path>",
                "(run) Use this newline-separated file as the key source, overriding the \
                 recipe's `[source]` block. Use for curated/partial runs.",
            ),
            (
                "--key <slug>",
                "(run) Enqueue just this one key (repeatable). Overrides `[source]`. \
                 Handy for retrying a single failed slug.",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign pipeline run sovereign-recipes/sep/pipelines/sep-core-v1.toml",
                "Drive the SEP ingest. Safe to Ctrl-C; resumes on next run.",
            ),
            (
                "sovereign pipeline status sep-core-v1",
                "Read-only summary — useful while a driver is running or after it paused.",
            ),
        ]),
        HelpSection::Notes(
            "The driver shells out to the recipe's `[enrich].command` for each work unit, \
             treating `{key}` as the work-unit slug. Failures are bucketed (timeout / \
             refused / vram_thrash / mismatch / model_missing / unknown) and retried up to \
             `[dispatch].max_attempts` before landing in `failed`. Add an `[schedule]` \
             block with `active_hours = \"HH:MM-HH:MM\"` to auto-pause outside that window.",
        ),
    ],
};

pub async fn run_pipeline(args: &[String]) -> i32 {
    if help::wants_help(args) || args.is_empty() {
        help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }

    match args[0].as_str() {
        "run" => cmd_run(&args[1..]).await,
        "status" => cmd_status(&args[1..]).await,
        "list" => cmd_list(&args[1..]).await,
        other => {
            eprintln!("unknown subcommand: {other}");
            help::print(&HELP);
            2
        }
    }
}

async fn cmd_run(args: &[String]) -> i32 {
    let Some(recipe_path) = args.first().map(PathBuf::from) else {
        eprintln!(
            "usage: sovereign pipeline run <recipe.toml> \
             [--db <path>] [--seed-only] [--slugs <path>] [--key <slug>]"
        );
        return 2;
    };
    let mut db_path: Option<PathBuf> = None;
    let mut seed_only = false;
    let mut slugs_path: Option<PathBuf> = None;
    let mut keys_override: Vec<String> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => {
                i += 1;
                db_path = Some(PathBuf::from(&args[i]));
            }
            "--seed-only" => seed_only = true,
            "--slugs" => {
                i += 1;
                slugs_path = Some(PathBuf::from(&args[i]));
            }
            "--key" => {
                i += 1;
                keys_override.push(args[i].clone());
            }
            other => {
                eprintln!("unknown flag: {other}");
                return 2;
            }
        }
        i += 1;
    }

    let mut recipe = match Recipe::load(&recipe_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to load recipe `{}`: {e}", recipe_path.display());
            return 1;
        }
    };

    // Apply source overrides. --key wins over --slugs wins over recipe.
    if !keys_override.is_empty() {
        recipe.source = sovereign_pipeline::recipe::Source::Inline { keys: keys_override };
    } else if let Some(path) = slugs_path {
        recipe.source = sovereign_pipeline::recipe::Source::SlugList { path };
        // Override paths from the CLI are relative to the user's cwd,
        // not the recipe dir — clear base_dir so absolute resolution
        // applies. Absolute paths work either way.
        recipe.base_dir = None;
    }
    let db_path = db_path.unwrap_or_else(default_db_path);
    if let Some(parent) = db_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("cannot create db parent `{}`: {e}", parent.display());
            return 1;
        }
    }
    let worklist = match Worklist::open(&db_path) {
        Ok(w) => Arc::new(Mutex::new(w)),
        Err(e) => {
            eprintln!("cannot open worklist db `{}`: {e}", db_path.display());
            return 1;
        }
    };

    if seed_only {
        let keys = match recipe.load_keys() {
            Ok(k) => k,
            Err(e) => {
                eprintln!("cannot load keys: {e}");
                return 1;
            }
        };
        let mut wl = worklist.lock().await;
        match wl.seed(&recipe.recipe.id, keys) {
            Ok(n) => {
                println!("seeded {n} new work unit(s) for recipe `{}`", recipe.recipe.id);
                return 0;
            }
            Err(e) => {
                eprintln!("seed failed: {e}");
                return 1;
            }
        }
    }

    // Wire SIGINT/SIGTERM → Shutdown. We do not abort in-flight tasks;
    // the driver drains them and exits cleanly. This is what makes
    // the day-pause workflow safe — Ctrl-C never loses a unit.
    let shutdown = Shutdown::default();
    spawn_signal_handler(shutdown.clone());

    let cfg = DriverConfig::default();
    let recipe_id = recipe.recipe.id.clone();
    let summary = match run_recipe(recipe, worklist.clone(), cfg, shutdown).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("driver error: {e}");
            return 1;
        }
    };

    let elapsed = (summary.finished_at_unix - summary.started_at_unix).max(1);
    let rate = summary.succeeded as f64 * 3600.0 / elapsed as f64;
    println!();
    println!("recipe:    {recipe_id}");
    println!("succeeded: {}", summary.succeeded);
    println!("failed:    {}", summary.failed);
    println!("remaining: {}", summary.pending_remaining);
    println!("elapsed:   {}s", elapsed);
    println!("rate:      {rate:.1} / hr");
    println!(
        "exit:      {}",
        if summary.paused { "paused (shutdown requested)" } else { "complete" }
    );
    0
}

async fn cmd_status(args: &[String]) -> i32 {
    let Some(recipe_id) = args.first().cloned() else {
        eprintln!("usage: sovereign pipeline status <recipe-id> [--db <path>]");
        return 2;
    };
    let mut db_path: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => {
                i += 1;
                db_path = Some(PathBuf::from(&args[i]));
            }
            other => {
                eprintln!("unknown flag: {other}");
                return 2;
            }
        }
        i += 1;
    }
    let db_path = db_path.unwrap_or_else(default_db_path);
    let worklist = match Worklist::open(&db_path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("cannot open worklist db `{}`: {e}", db_path.display());
            return 1;
        }
    };
    let report = match status::report(&worklist, &recipe_id) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("status read failed: {e}");
            return 1;
        }
    };
    print!("{}", report.render());
    0
}

async fn cmd_list(args: &[String]) -> i32 {
    let mut db_path: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => {
                i += 1;
                db_path = Some(PathBuf::from(&args[i]));
            }
            other => {
                eprintln!("unknown flag: {other}");
                return 2;
            }
        }
        i += 1;
    }
    let db_path = db_path.unwrap_or_else(default_db_path);
    if !Path::new(&db_path).exists() {
        println!("no worklist db at {} — nothing to list", db_path.display());
        return 0;
    }
    // Open read-only.
    let worklist = match Worklist::open(&db_path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("cannot open worklist db `{}`: {e}", db_path.display());
            return 1;
        }
    };
    let ids = match worklist.list_recipe_ids() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("query failed: {e}");
            return 1;
        }
    };
    if ids.is_empty() {
        println!("(no recipes in {})", db_path.display());
    } else {
        for id in ids {
            println!("{id}");
        }
    }
    0
}

fn default_db_path() -> PathBuf {
    let base = dirs::home_dir()
        .map(|h| h.join(".sovereign"))
        .unwrap_or_else(|| PathBuf::from(".sovereign"));
    base.join("pipeline.db")
}

#[cfg(unix)]
fn spawn_signal_handler(shutdown: Shutdown) {
    use tokio::signal::unix::{signal, SignalKind};
    tokio::spawn(async move {
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => return,
        };
        tokio::select! {
            _ = sigint.recv() => {
                eprintln!("\nshutdown requested (SIGINT) — draining in-flight units, please wait…");
                shutdown.request();
            }
            _ = sigterm.recv() => {
                eprintln!("\nshutdown requested (SIGTERM) — draining in-flight units, please wait…");
                shutdown.request();
            }
        }
    });
}

#[cfg(not(unix))]
fn spawn_signal_handler(shutdown: Shutdown) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\nshutdown requested — draining in-flight units, please wait…");
            shutdown.request();
        }
    });
}
