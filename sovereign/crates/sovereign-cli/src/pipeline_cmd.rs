//! `sovereign pipeline …` — generic ingestion-pipeline driver.
//!
//! Surface:
//!
//! ```text
//! sovereign pipeline run    <recipe.toml> [--db <path>] [--seed-only]
//! sovereign pipeline status <recipe-id>   [--db <path>]
//! sovereign pipeline list   [--db <path>]
//! sovereign pipeline pause  <recipe-id>   [--force]
//! ```
//!
//! State lives in `--db` (defaults to `~/.sovereign/pipeline.db`).
//! Multiple recipes can share one DB; rows are keyed by `recipe_id`.
//! See `sovereign_pipeline` crate docs for the worklist semantics.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sovereign_pipeline::driver::{DriverConfig, Shutdown};
use sovereign_pipeline::{
    ledger, pod,
    recipe::Recipe,
    run_recipe, status,
    worklist::Worklist,
};
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
            (
                "pause <recipe-id>",
                "Gracefully stop the active driver for this recipe (SIGTERM → drain → \
                 exit). Worklist state persists; `sovereign pipeline run` resumes from \
                 where it left off. Use --force to skip drain (SIGKILL).",
            ),
            (
                "pod up",
                "Launch a Vast.ai pod with the sovereign CUDA image, join the mesh, \
                 register in the cost ledger.",
            ),
            (
                "pod list",
                "Show every pod the ledger knows about with accrued cost.",
            ),
            (
                "pod down <vast-id>",
                "Destroy a Vast pod, close its ledger entry, print final cost.",
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
            (
                "sovereign pipeline pause sep-core-v1",
                "Pause the SEP ingest mid-run. In-flight slugs drain, then the driver exits; \
                 resume with the same `run` invocation.",
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
        "pause" => cmd_pause(&args[1..]).await,
        "pod" => cmd_pod(&args[1..]).await,
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

/// Identify the live driver(s) for a given recipe-id by walking
/// `/proc/<pid>/cmdline`. Robust to driver invocation shape: we
/// recognize either the explicit recipe-id form (rare) or the
/// recipe-toml-path form (common, since `pipeline run` takes a
/// path). For the latter we parse the recipe's `[recipe].id` and
/// match.
///
/// Returns every PID running that recipe so multiple drivers
/// (operator misuse — but it happens) all get the signal.
fn find_driver_pids(recipe_id: &str) -> Vec<u32> {
    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut pids = Vec::new();
    for entry in proc_dir.flatten() {
        let Some(name) = entry.file_name().to_str().map(|s| s.to_string()) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let cmdline_path = format!("/proc/{pid}/cmdline");
        let Ok(raw) = std::fs::read(&cmdline_path) else {
            continue;
        };
        // /proc/<pid>/cmdline is NUL-separated argv.
        let argv: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).to_string())
            .collect();
        // Match shape: `... sovereign pipeline run <recipe-path>`.
        // Anything else isn't a pipeline driver we care about.
        let is_driver = argv.windows(3).any(|w| {
            (w[0].ends_with("sovereign") || w[0].ends_with("sovereign-cli"))
                && w[1] == "pipeline"
                && w[2] == "run"
        });
        if !is_driver {
            continue;
        }
        // The argument after `run` is the recipe path. Read it
        // and check the `[recipe].id` matches.
        let recipe_arg_idx = argv.iter().position(|s| s == "run").map(|i| i + 1);
        let Some(idx) = recipe_arg_idx else {
            continue;
        };
        let Some(recipe_arg) = argv.get(idx) else {
            continue;
        };
        let candidate = PathBuf::from(recipe_arg);
        if !candidate.exists() {
            continue;
        }
        match Recipe::load(&candidate) {
            Ok(r) if r.recipe.id == recipe_id => pids.push(pid),
            _ => {}
        }
    }
    pids
}

async fn cmd_pause(args: &[String]) -> i32 {
    let Some(recipe_id) = args.first().cloned() else {
        eprintln!("usage: sovereign pipeline pause <recipe-id> [--force]");
        return 2;
    };
    let mut force = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "--force" => force = true,
            other => {
                eprintln!("unknown flag: {other}");
                return 2;
            }
        }
    }

    let pids = find_driver_pids(&recipe_id);
    if pids.is_empty() {
        println!("no active driver for recipe `{recipe_id}` — nothing to pause");
        return 0;
    }

    let signum = if force { libc::SIGKILL } else { libc::SIGTERM };
    let signame = if force { "SIGKILL" } else { "SIGTERM" };
    for pid in &pids {
        println!("{signame} driver pid {pid} for recipe `{recipe_id}`");
        // Safety: libc::kill is just a syscall — no Rust invariants
        // to uphold. A bad pid returns -1 and sets errno; we read
        // errno separately rather than unwrap.
        let rc = unsafe { libc::kill(*pid as libc::pid_t, signum) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            eprintln!("  ✗ kill({pid}, {signame}) failed: {err}");
            // Carry on — other pids may still be killable.
        }
    }

    if force {
        // SIGKILL is immediate; in-flight enrich subprocesses
        // become orphans (their parent shell `/bin/sh -c …` dies
        // with the driver). We don't wait — caller knows what they
        // asked for.
        return 0;
    }

    // SIGTERM path: wait for the driver(s) to drain. The driver's
    // shutdown handler finishes any in-flight unit before exiting
    // — that's the whole point of `pause` vs `--force`. Poll
    // /proc once a second; cap the wait at 10 minutes so a wedged
    // driver doesn't hang the operator's terminal forever (any
    // longer than that and they'll want --force anyway).
    println!("waiting for drain (Ctrl-C if you'd rather --force) …");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        let alive: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|pid| std::path::Path::new(&format!("/proc/{pid}")).exists())
            .collect();
        if alive.is_empty() {
            println!("✓ paused cleanly");
            return 0;
        }
        if std::time::Instant::now() >= deadline {
            eprintln!(
                "drain timed out after 10m; still alive: {alive:?}. Retry with --force \
                 or send SIGKILL manually."
            );
            return 1;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

async fn cmd_pod(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("usage: sovereign pipeline pod <up | list | down> [flags]");
        return 2;
    }
    match args[0].as_str() {
        "up" => cmd_pod_up(&args[1..]).await,
        "list" => cmd_pod_list(&args[1..]),
        "down" => cmd_pod_down(&args[1..]),
        other => {
            eprintln!("unknown pod subcommand: {other}");
            2
        }
    }
}

async fn cmd_pod_up(args: &[String]) -> i32 {
    // Defaults tuned for the SEP fanout use case — single 48 GB GPU,
    // sovereign CUDA image, 80 GB disk for model cache.
    let mut gpu_name: String = "L40S".into();
    let mut image: Option<String> = std::env::var("SOVEREIGN_VAST_IMAGE").ok();
    let mut disk_gb: u32 = 80;
    let mut recipe_id: String = "ad-hoc".into();
    let mut label: Option<String> = None;
    let mut mesh_join_link: Option<String> = std::env::var("MESH_JOIN_LINK").ok();
    let mut founder_addr: Option<String> = std::env::var("SOVEREIGN_FOUNDER_ADDR").ok();
    let mut tailscale_authkey: Option<String> = std::env::var("TAILSCALE_AUTHKEY").ok();
    let mut max_price: f64 = 0.80;
    let mut dry_run: bool = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--gpu" => { i += 1; gpu_name = args[i].clone(); }
            "--image" => { i += 1; image = Some(args[i].clone()); }
            "--disk" => { i += 1; disk_gb = args[i].parse().unwrap_or(disk_gb); }
            "--recipe-id" => { i += 1; recipe_id = args[i].clone(); }
            "--label" => { i += 1; label = Some(args[i].clone()); }
            "--mesh-join-link" => { i += 1; mesh_join_link = Some(args[i].clone()); }
            "--founder-addr" => { i += 1; founder_addr = Some(args[i].clone()); }
            "--max-price" => { i += 1; max_price = args[i].parse().unwrap_or(max_price); }
            "--dry-run" => { dry_run = true; }
            other => {
                eprintln!("unknown flag: {other}");
                return 2;
            }
        }
        i += 1;
    }

    let Some(image) = image else {
        eprintln!(
            "no container image. Pass `--image <ref>` or set SOVEREIGN_VAST_IMAGE.\n\
             Example: --image ghcr.io/<you>/sovereign-cuda:latest"
        );
        return 2;
    };
    let Some(join_link) = mesh_join_link else {
        eprintln!(
            "no mesh-join link. Pass `--mesh-join-link cwth-…` or set MESH_JOIN_LINK.\n\
             Get one with: sovereign mesh status (look for the join-link line)"
        );
        return 2;
    };
    let Some(founder) = founder_addr else {
        eprintln!(
            "no founder address. Pass `--founder-addr <ip>` or set SOVEREIGN_FOUNDER_ADDR.\n\
             This is the Tailscale IPv4 of the mesh founder (your laptop, usually)."
        );
        return 2;
    };
    let Some(ts_key) = tailscale_authkey.take() else {
        eprintln!(
            "no Tailscale auth key. Pass via TAILSCALE_AUTHKEY env var.\n\
             Generate one in the Tailscale admin: Settings → Keys → Generate auth key (reusable)."
        );
        return 2;
    };

    // Build the search query — verified hosts only, CUDA ≥ 12.4 so
    // the image's CUDA runtime matches at runtime.
    let query = format!(
        "gpu_name={gpu_name} verified=true rentable=true cuda_max_good>=12.4 \
         dph_total<={max_price}"
    );
    let offers = match pod::search_offers(&query, 50) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vastai search failed: {e}");
            return 1;
        }
    };
    let Some(pick) = pod::pick_offer(&offers) else {
        eprintln!("no offers matched: {query}");
        return 1;
    };

    println!(
        "selected offer: id={} gpu={} ${:.3}/hr rel={:.2} verified={} loc={}",
        pick.id, pick.gpu_name, pick.price_per_hour, pick.reliability, pick.verified, pick.geolocation,
    );

    // Build the onstart command: export env vars, then exec the image
    // entrypoint. Vast's SSH instance type ignores image ENTRYPOINT
    // by default, so we invoke it explicitly.
    let onstart_cmd = format!(
        "set -eu\n\
         export TAILSCALE_AUTHKEY='{ts_key}'\n\
         export MESH_JOIN_LINK='{join_link}'\n\
         export MESH_SEED_ADDR='{founder}'\n\
         export SOVEREIGN_FOUNDER_ADDR='{founder}'\n\
         exec /entrypoint.sh\n",
    );

    let label_value = label.unwrap_or_else(|| format!("{recipe_id}-pod"));
    let req = pod::CreateRequest {
        offer_id: pick.id,
        image: &image,
        disk_gb,
        onstart_cmd: &onstart_cmd,
        env: "",
        label: &label_value,
        ssh: true,
    };

    if dry_run {
        println!(
            "DRY RUN: would create instance via vastai create instance {} \
             --image {} --disk {} --label {} --ssh --onstart-cmd <…>",
            pick.id, image, disk_gb, label_value
        );
        return 0;
    }

    let created = match pod::create_instance(&req, pick) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("vastai create failed: {e}");
            return 1;
        }
    };

    let rec = ledger::PodRecord {
        vast_id: created.vast_id.clone(),
        label: label_value,
        recipe_id,
        gpu_name: created.gpu_name,
        image: created.image,
        started_at: ledger::unix_now(),
        ended_at: None,
        cost_per_hour: created.cost_per_hour,
        status: ledger::PodStatus::Running,
    };
    if let Err(e) = ledger::append(ledger::default_path(), rec) {
        eprintln!(
            "WARNING: pod {} launched but ledger append failed: {e}\n\
             Track manually until `pod list` resolves.",
            created.vast_id
        );
    }

    println!();
    println!("pod launched:");
    println!("  vast id     {}", created.vast_id);
    println!("  $/hr        {:.3}", created.cost_per_hour);
    println!("  image       {}", image);
    println!();
    println!("Watch it come online with:");
    println!("  vastai logs {} --tail 50", created.vast_id);
    println!("Then verify the mesh saw it:");
    println!("  sovereign mesh status");
    0
}

fn cmd_pod_list(_args: &[String]) -> i32 {
    let path = ledger::default_path();
    let pods = match ledger::read(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ledger read failed: {e}");
            return 1;
        }
    };
    if pods.is_empty() {
        println!("(no pods in {})", path.display());
        return 0;
    }
    println!(
        "{:<10} {:<6} {:<22} {:<12} {:>8} {:>10}  {}",
        "vast_id", "state", "label", "gpu", "$/hr", "accrued", "started_at"
    );
    let mut running_total = 0.0;
    for p in &pods {
        let cost = ledger::accrued_cost(p);
        let state = match p.status {
            ledger::PodStatus::Running => "live",
            ledger::PodStatus::Closed => "down",
        };
        let started = chrono::DateTime::<chrono::Local>::from(
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(p.started_at as u64),
        );
        println!(
            "{:<10} {:<6} {:<22} {:<12} {:>8.3} {:>9.2}$  {}",
            p.vast_id,
            state,
            truncate(&p.label, 22),
            truncate(&p.gpu_name, 12),
            p.cost_per_hour,
            cost,
            started.format("%Y-%m-%d %H:%M")
        );
        if p.status == ledger::PodStatus::Running {
            running_total += cost;
        }
    }
    println!();
    println!("running pods accruing: ${:.2}", running_total);
    0
}

fn cmd_pod_down(args: &[String]) -> i32 {
    let Some(vast_id) = args.first().cloned() else {
        eprintln!("usage: sovereign pipeline pod down <vast-id>");
        return 2;
    };
    let path = ledger::default_path();
    let cost_before = ledger::read(&path)
        .ok()
        .and_then(|pods| pods.into_iter().find(|p| p.vast_id == vast_id))
        .map(|p| (p.cost_per_hour, ledger::accrued_cost(&p)));

    if let Err(e) = pod::destroy_instance(&vast_id) {
        eprintln!("vastai destroy failed: {e}");
        // Continue to ledger close — operator may have already
        // destroyed the pod manually and just wants to clean up.
    }
    match ledger::close(&path, &vast_id) {
        Ok(rec) => {
            let total = ledger::accrued_cost(&rec);
            let hours = ledger::elapsed_hours(&rec);
            println!("pod {} destroyed.", vast_id);
            println!("  elapsed     {:.2} h", hours);
            println!("  $/hr        {:.3}", rec.cost_per_hour);
            println!("  total cost  ${:.2}", total);
        }
        Err(ledger::LedgerError::NotFound(_)) => {
            if let Some((rate, accrued)) = cost_before {
                println!("pod {} destroyed (was not in running set).", vast_id);
                println!("  last-seen rate {:.3} $/hr", rate);
                println!("  last accrued   ${:.2}", accrued);
            } else {
                println!("pod {} destroyed (no ledger entry).", vast_id);
            }
        }
        Err(e) => {
            eprintln!("ledger close failed: {e}");
            return 1;
        }
    }
    0
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
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
