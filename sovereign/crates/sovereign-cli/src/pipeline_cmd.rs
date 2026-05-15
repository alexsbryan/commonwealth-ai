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
                "Gracefully stop active drivers for this recipe across the mesh \
                 (SIGTERM → drain → exit). Worklist state persists; \
                 `sovereign pipeline run` resumes from where it left off. \
                 Use --force for SIGKILL. Use --local-only to skip the mesh \
                 fanout and only signal local PIDs.",
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
            (
                "--concurrency <N>",
                "(run) Override `[dispatch].concurrency` for this invocation. Use to fan out a \
                 single-laptop recipe across mesh peers — pass `<peers_online>` and let the \
                 daemon's load balancer distribute units. Adaptive backoff still applies if \
                 capacity isn't really there.",
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
             refused / vram_thrash / gpu_vulkan / gpu_rocm / inference_json_parse / \
             inference_5xx / daemon_down / stale_cache / mismatch / model_missing / \
             phase_failed / build_step_failed / unknown) and retried up to \
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
             [--db <path>] [--seed-only] [--slugs <path>] [--key <slug>] \
             [--concurrency <N>]"
        );
        return 2;
    };
    let mut db_path: Option<PathBuf> = None;
    let mut seed_only = false;
    let mut slugs_path: Option<PathBuf> = None;
    let mut keys_override: Vec<String> = Vec::new();
    let mut concurrency_override: Option<u32> = None;
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
            "--concurrency" => {
                i += 1;
                match args[i].parse::<u32>() {
                    Ok(n) if n >= 1 => concurrency_override = Some(n),
                    Ok(_) => {
                        eprintln!("--concurrency must be >= 1");
                        return 2;
                    }
                    Err(_) => {
                        eprintln!("--concurrency: '{}' is not an integer", args[i]);
                        return 2;
                    }
                }
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

    // Concurrency override — runtime knob for fanning a single-laptop
    // recipe across mesh peers without editing the recipe file. The
    // adaptive layer still backs off on failure signals, so an
    // optimistic value is safe.
    if let Some(n) = concurrency_override {
        let prior = recipe.dispatch.concurrency;
        if prior != n {
            eprintln!("concurrency override: recipe {prior} → CLI {n}");
        }
        recipe.dispatch.concurrency = n;
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
        eprintln!("usage: sovereign pipeline pause <recipe-id> [--force] [--local-only]");
        return 2;
    };
    let mut force = false;
    let mut local_only = false;
    for arg in &args[1..] {
        match arg.as_str() {
            "--force" => force = true,
            "--local-only" => local_only = true,
            other => {
                eprintln!("unknown flag: {other}");
                return 2;
            }
        }
    }

    // Default path: ask the local daemon to fan the pause out over the
    // mesh. The pipeline driver runs locally on each peer against its
    // own worklist DB, so a local /proc walk on this host stops only
    // the driver on this host — peer drivers keep claiming work. The
    // daemon's handler hits its own /proc + forwards the same request
    // to every online peer with fanout=false.
    //
    // `--local-only` (and the daemon-down fallback) keeps today's
    // behavior for the rare case where the operator deliberately only
    // wants to stop this host.
    if !local_only {
        match mesh_pause_via_daemon(&recipe_id, force).await {
            Ok(rendered) => {
                print!("{rendered}");
                return 0;
            }
            Err(MeshPauseError::DaemonDown) => {
                eprintln!(
                    "local daemon unreachable on :9742 — falling back to local-only pause; \
                     peer drivers will keep running until you restart the daemon or run \
                     `pipeline pause --local-only` on each peer."
                );
                // Fall through to the local-only path below.
            }
            Err(MeshPauseError::Other(msg)) => {
                eprintln!("{msg}");
                return 1;
            }
        }
    }

    // ── Local-only path (legacy / fallback) ─────────────────────────
    let pids = find_driver_pids(&recipe_id);
    if pids.is_empty() {
        println!("no active driver for recipe `{recipe_id}` on this host — nothing to pause");
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

/// Failures from the mesh-pause path that the caller routes
/// differently. `DaemonDown` falls through to local-only; `Other`
/// surfaces and exits non-zero.
enum MeshPauseError {
    DaemonDown,
    Other(String),
}

/// Per-node pause result returned by the daemon's
/// `/internal/pipeline/pause` aggregate response.
#[derive(serde::Deserialize)]
struct PausePerNode {
    node: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    pids_signaled: Vec<u32>,
    #[serde(default)]
    drained: bool,
    #[serde(default)]
    error: Option<String>,
}

/// Aggregate response from the daemon — local result plus per-peer.
#[derive(serde::Deserialize)]
struct PauseAggregate {
    local: PausePerNode,
    #[serde(default)]
    peers: Vec<PausePerNode>,
}

/// POST `/internal/pipeline/pause` on the local daemon (:9742, the
/// peer-accessible internal router, also reachable from localhost).
/// The daemon does the local /proc walk + concurrently forwards to
/// every online peer with `fanout: false`, returning an aggregate.
/// Render the result for the operator.
async fn mesh_pause_via_daemon(
    recipe_id: &str,
    force: bool,
) -> std::result::Result<String, MeshPauseError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| MeshPauseError::Other(format!("build http client: {e}")))?;

    let body = serde_json::json!({
        "recipe_id": recipe_id,
        "force": force,
        "fanout": true,
    });

    let url = "http://127.0.0.1:9742/internal/pipeline/pause";
    let resp = match client.post(url).json(&body).send().await {
        Ok(r) => r,
        Err(e) if e.is_connect() || e.is_timeout() => {
            return Err(MeshPauseError::DaemonDown);
        }
        Err(e) => return Err(MeshPauseError::Other(format!("POST {url}: {e}"))),
    };

    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(MeshPauseError::Other(format!(
            "local daemon doesn't expose /internal/pipeline/pause — rebuild + restart it to \
             enable mesh-aware pause, or pass --local-only to use the legacy path"
        )));
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(MeshPauseError::Other(format!(
            "{url} returned {status}: {body}"
        )));
    }

    let agg: PauseAggregate = resp
        .json()
        .await
        .map_err(|e| MeshPauseError::Other(format!("parse response: {e}")))?;

    let mut out = String::new();
    out.push_str(&render_pause_node(&agg.local, recipe_id, force));
    if agg.peers.is_empty() {
        out.push_str("(no other peers online — local pause only)\n");
    } else {
        for peer in &agg.peers {
            out.push_str(&render_pause_node(peer, recipe_id, force));
        }
    }
    Ok(out)
}

fn render_pause_node(n: &PausePerNode, recipe_id: &str, force: bool) -> String {
    let label = match n.name.as_deref() {
        Some(name) => format!("{name} ({})", n.node),
        None => n.node.clone(),
    };
    if let Some(err) = n.error.as_deref() {
        return format!("✗ {label}: {err}\n");
    }
    if n.pids_signaled.is_empty() {
        return format!("· {label}: no active `{recipe_id}` driver — nothing to pause\n");
    }
    let signame = if force { "SIGKILL" } else { "SIGTERM" };
    let drained_note = if n.drained {
        "drained cleanly"
    } else {
        "drain timed out — driver may be wedged; retry with --force"
    };
    format!(
        "✓ {label}: {signame} pids {:?} ({drained_note})\n",
        n.pids_signaled
    )
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
    // The pod entrypoint reads `TS_AUTHKEY`; humans following the
    // Tailscale docs export `TAILSCALE_AUTHKEY`. We accept either —
    // `TAILSCALE_AUTHKEY` wins when both are set so the operator can
    // override the bashrc-stored TS_AUTHKEY without unsetting it.
    let mut tailscale_authkey: Option<String> = std::env::var("TAILSCALE_AUTHKEY")
        .ok()
        .or_else(|| std::env::var("TS_AUTHKEY").ok());
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
    // ─── Env-var contract for the pod's onstart command ─────────────
    // The pod entrypoint (sovereign/container/entrypoint.sh:41-45) hard-
    // requires the following via `: "${VAR:?...}"`. The CLI must export
    // every one of them, and validate their presence up front so the
    // user sees the whole missing set at once instead of round-tripping
    // through 90-second pod boot timeouts. When the entrypoint adds a
    // new `:?` check, update this block AND the runbook
    // (sovereign-recipes/sep/RUNBOOK_VAST.md) in lockstep.
    //
    // Future structural fix: switch the entrypoint to `sovereign mesh
    // fetch-model` (see project_pod_mesh_fetch_refactor memory) — that
    // drops R2_* from this contract entirely.
    let mut missing: Vec<&'static str> = Vec::new();
    let ts_key = tailscale_authkey.take().filter(|s| !s.is_empty());
    if ts_key.is_none() { missing.push("TAILSCALE_AUTHKEY (or TS_AUTHKEY)"); }
    let r2_endpoint = std::env::var("R2_ENDPOINT").ok().filter(|s| !s.is_empty());
    if r2_endpoint.is_none() { missing.push("R2_ENDPOINT"); }
    let r2_access_key = std::env::var("R2_ACCESS_KEY").ok().filter(|s| !s.is_empty());
    if r2_access_key.is_none() { missing.push("R2_ACCESS_KEY"); }
    let r2_secret_key = std::env::var("R2_SECRET_KEY").ok().filter(|s| !s.is_empty());
    if r2_secret_key.is_none() { missing.push("R2_SECRET_KEY"); }
    if !missing.is_empty() {
        eprintln!(
            "missing required env vars (pod entrypoint will reject):\n\
             {}\n\
             \n\
             Set them in ~/.bashrc (or this shell) and re-run.\n\
             Full prereqs: sovereign-recipes/sep/RUNBOOK_VAST.md",
            missing.iter().map(|v| format!("  - {v}")).collect::<Vec<_>>().join("\n")
        );
        return 2;
    }
    let ts_key = ts_key.unwrap();
    let r2_endpoint = r2_endpoint.unwrap();
    let r2_access_key = r2_access_key.unwrap();
    let r2_secret_key = r2_secret_key.unwrap();
    // R2_BUCKET is optional — entrypoint defaults to "sovereign-models".
    let r2_bucket = std::env::var("R2_BUCKET").ok().filter(|s| !s.is_empty());

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
    // Export every env var the pod entrypoint expects. Notes:
    //   - TS_AUTHKEY is exported alongside TAILSCALE_AUTHKEY because
    //     entrypoint.sh reads the former while everything else in our
    //     toolchain (CLI flag, runbook, ~/.bashrc) uses the latter.
    //   - MESH_SEED_ADDR MUST be `host:port`. The entrypoint's beacon
    //     splits on `:` and falls back to using the full string for
    //     both halves when no colon is present (entrypoint.sh:169-170),
    //     producing `100.x.y.z:100.x.y.z` and a 60-second beacon
    //     timeout. The CLI's `--founder-addr` is documented as the
    //     tailnet IPv4 alone (no port) — match that and auto-append
    //     the founder daemon's internal port (9742) here.
    //   - SINGLE_MODEL=primary is the CLI-side default because the
    //     common Vast offer (L40S, 45 GB VRAM) cannot fit the legacy
    //     3-slot loadout — Darwin-36B alone needs ~47 GB resident.
    //     Override with PRIMARY_GGUF env if the founder's primary is
    //     different (default below matches founder's current primary
    //     on 2026-05-15; refresh when the founder's loadout changes).
    //   - R2_BUCKET only exported when explicitly set; entrypoint
    //     defaults to "sovereign-models" otherwise.
    let mesh_seed_addr = if founder.contains(':') {
        founder.clone()
    } else {
        format!("{founder}:9742")
    };
    let primary_gguf = std::env::var("PRIMARY_GGUF")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf".to_string());
    let embed_gguf = std::env::var("EMBED_GGUF")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Qwen3-Embedding-0.6B-Q8_0.gguf".to_string());

    // ─── R2 pre-flight ──────────────────────────────────────────────
    // Run the same `rclone lsf` the pod entrypoint runs, but locally
    // and BEFORE we call vastai. Catches "PRIMARY_GGUF isn't in the
    // bucket" before a single dollar is spent on a pod that would
    // FATAL ~60s into boot. Uses RCLONE_CONFIG_* env vars so no temp
    // config file with secrets is written to disk.
    {
        let bucket = r2_bucket.as_deref().unwrap_or("sovereign-models");
        let rclone_check = std::process::Command::new("rclone").arg("--version").output();
        let rclone_present = rclone_check
            .as_ref()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !rclone_present {
            eprintln!(
                "R2 pre-flight: rclone not installed locally — REQUIRED for `pod up`.\n\
                 Install: curl https://rclone.org/install.sh | sudo bash\n\
                 (We use it to verify PRIMARY_GGUF exists in r2:{bucket} before paying for a pod.)"
            );
            return 2;
        }
        let out = std::process::Command::new("rclone")
            .env("RCLONE_CONFIG_R2_TYPE", "s3")
            .env("RCLONE_CONFIG_R2_PROVIDER", "Cloudflare")
            .env("RCLONE_CONFIG_R2_REGION", "auto")
            .env("RCLONE_CONFIG_R2_ENDPOINT", &r2_endpoint)
            .env("RCLONE_CONFIG_R2_ACCESS_KEY_ID", &r2_access_key)
            .env("RCLONE_CONFIG_R2_SECRET_ACCESS_KEY", &r2_secret_key)
            .env("RCLONE_CONFIG_R2_ACL", "private")
            .arg("lsf")
            .arg(format!("r2:{bucket}"))
            .output();
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                eprintln!("R2 pre-flight: rclone failed to spawn: {e}");
                return 1;
            }
        };
        if !out.status.success() {
            eprintln!(
                "R2 pre-flight: `rclone lsf r2:{bucket}` failed:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            eprintln!(
                "Possible causes: wrong R2_ENDPOINT, bad access/secret key, \n\
                 bucket name typo, or token missing Object Read on this bucket."
            );
            return 1;
        }
        let listing = String::from_utf8_lossy(&out.stdout);
        let bucket_files: std::collections::HashSet<&str> =
            listing.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        // Verify every GGUF the pod's SINGLE_MODEL=primary sync expects
        // is in the bucket. Add to this list as the entrypoint grows
        // — the goal is "no pod boot ever fails with `missing $GGUF
        // after sync` because we caught it locally."
        let required = [
            ("PRIMARY_GGUF", primary_gguf.as_str()),
            ("EMBED_GGUF",   embed_gguf.as_str()),
        ];
        let mut missing_ggufs: Vec<(&str, &str)> = Vec::new();
        for (env_name, fname) in &required {
            if !bucket_files.contains(fname) {
                missing_ggufs.push((env_name, fname));
            }
        }
        if !missing_ggufs.is_empty() {
            eprintln!("R2 pre-flight FAILED — bucket r2:{bucket} is missing:");
            for (env_name, fname) in &missing_ggufs {
                eprintln!("  - {env_name}='{fname}'");
            }
            eprintln!("\nBucket currently contains:");
            for f in &bucket_files {
                eprintln!("  - {f}");
            }
            eprintln!(
                "\nFix: upload the missing GGUF(s) to r2:{bucket}, or override\n\
                 the relevant env var to a filename from the list above."
            );
            return 2;
        }
        println!(
            "R2 pre-flight OK — {primary_gguf} + {embed_gguf} present in r2:{bucket}"
        );
    }
    // Context size sized to fit `primary_gguf` resident on an L40S:
    //   - Darwin-36B-Q6  : ~30 GB weights + ~8 GB KV @ 16K = ~38 GB    ← fits in 41 GB available
    //   - Darwin-36B-Q6  : ~30 GB weights + ~17 GB KV @ 32K = ~47 GB   ← does NOT fit
    // 16K is the safe default for any 35B-class model on an L40S. The
    // SEP recipe processes one chunk at a time, not whole articles, so
    // 16K context is plenty in practice. Override via CONTEXT_SIZE env
    // on a beefier pod (H100 → 32K is fine).
    let context_size = std::env::var("CONTEXT_SIZE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "16384".to_string());
    let r2_bucket_line = r2_bucket
        .as_deref()
        .map(|b| format!("export R2_BUCKET='{b}'\n         "))
        .unwrap_or_default();
    let onstart_cmd = format!(
        "set -eu\n\
         export TS_AUTHKEY='{ts_key}'\n\
         export TAILSCALE_AUTHKEY='{ts_key}'\n\
         export MESH_JOIN_LINK='{join_link}'\n\
         export MESH_SEED_ADDR='{mesh_seed_addr}'\n\
         export SOVEREIGN_FOUNDER_ADDR='{founder}'\n\
         export R2_ENDPOINT='{r2_endpoint}'\n\
         export R2_ACCESS_KEY='{r2_access_key}'\n\
         export R2_SECRET_KEY='{r2_secret_key}'\n\
         {r2_bucket_line}\
         export SINGLE_MODEL='primary'\n\
         export PRIMARY_GGUF='{primary_gguf}'\n\
         export EMBED_GGUF='{embed_gguf}'\n\
         export CONTEXT_SIZE='{context_size}'\n\
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
