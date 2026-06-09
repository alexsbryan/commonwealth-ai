// SPDX-License-Identifier: AGPL-3.0-or-later
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
use sovereign_pipeline::{ledger, pod, recipe::Recipe, run_recipe, status, worklist::Worklist};
use tokio::sync::Mutex;

use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign pipeline",
    summary: "Generic ingestion-pipeline driver — durable worklist + retry + pause-resume.",
    sections: &[
        HelpSection::Usage("sovereign pipeline <run | status | list> [flags]"),
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
            ("list", "List every recipe-id known to the worklist DB."),
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
        recipe.source = sovereign_pipeline::recipe::Source::Inline {
            keys: keys_override,
        };
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
                println!(
                    "seeded {n} new work unit(s) for recipe `{}`",
                    recipe.recipe.id
                );
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
        if summary.paused {
            "paused (shutdown requested)"
        } else {
            "complete"
        }
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
        return Err(MeshPauseError::Other(
            "local daemon doesn't expose /internal/pipeline/pause — rebuild + restart it to \
             enable mesh-aware pause, or pass --local-only to use the legacy path"
                .to_string(),
        ));
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
        eprintln!("usage: sovereign pipeline pod <up | pool | list | down> [flags]");
        return 2;
    }
    match args[0].as_str() {
        "up" => cmd_pod_up(&args[1..]).await,
        "pool" => cmd_pod_pool(&args[1..]).await,
        "list" => cmd_pod_list(&args[1..]),
        "down" => cmd_pod_down(&args[1..]),
        other => {
            eprintln!("unknown pod subcommand: {other}");
            2
        }
    }
}

async fn cmd_pod_up(args: &[String]) -> i32 {
    // EPHEMERAL_WORKER_PODS.md path. The pod boots in worker mode,
    // owned by exactly this CLI invocation for the lifetime of the
    // job. No Tailscale, no R2, no mesh join.
    //
    // The MVP performs: search → create → wait-for-address →
    // wait-for-health → uploads (if any) → print handle. The pod is
    // left running for follow-up dispatch. `pipeline pod down` (or
    // SIGINT here) tears it down.
    let mut gpu_name: String = "L40S".into();
    let mut image: Option<String> = std::env::var("SOVEREIGN_VAST_IMAGE").ok();
    let mut disk_gb: u32 = 80;
    let mut label: Option<String> = None;
    let mut max_price: f64 = 0.80;
    let mut num_gpus: Option<u32> = None;
    let mut dry_run: bool = false;
    let mut job_id: Option<String> = None;
    let mut uploads: Vec<std::path::PathBuf> = Vec::new();
    // Each entry is (name, sha256-hex, url) — the pod will fetch the
    // URL itself instead of waiting for an owner upload. Right for
    // GGUFs staged in R2/B2/S3 with multi-Gbps egress.
    let mut upload_urls: Vec<(String, String, String)> = Vec::new();
    // When set, every `--upload <path>` flag is translated into a
    // URL-backed entry at `<base>/<filename>`. The local file is
    // read only to compute SHA-256 — the pod fetches bytes from the
    // URL. This is the ergonomic primitive for the common case
    // where every model is staged in one R2/B2 bucket.
    let mut upload_from_base: Option<String> = None;
    // Token TTL covers the longest plausible owner-side workflow. SEP and
    // wikipedia ingest runs commonly take 30-50h; the prior 12h default
    // expired mid-run, after which every owner request to the pod 401'd
    // with `token expired` while still billing for the GPU. 48h is the
    // largest window that's still bounded for blast-radius purposes (the
    // bootstrap blob is the credential — a leaked blob is good until
    // expiry). Operators on multi-day jobs should pass `--ttl-hours 72`
    // or higher explicitly.
    let mut bootstrap_ttl_hours: u64 = 48;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--gpu" => {
                i += 1;
                gpu_name = args[i].clone();
            }
            "--image" => {
                i += 1;
                image = Some(args[i].clone());
            }
            "--disk" => {
                i += 1;
                disk_gb = args[i].parse().unwrap_or(disk_gb);
            }
            "--label" => {
                i += 1;
                label = Some(args[i].clone());
            }
            "--max-price" => {
                i += 1;
                max_price = args[i].parse().unwrap_or(max_price);
            }
            "--num-gpus" => {
                i += 1;
                num_gpus = args[i].parse().ok();
            }
            "--job-id" => {
                i += 1;
                job_id = Some(args[i].clone());
            }
            "--upload" => {
                i += 1;
                uploads.push(std::path::PathBuf::from(args[i].clone()));
            }
            "--upload-from-base-url" => {
                i += 1;
                // Strip trailing slash so we can paste either with or
                // without — `<base>/<filename>` is always a clean join.
                upload_from_base = Some(args[i].trim_end_matches('/').to_string());
            }
            "--upload-url" => {
                i += 1;
                // Format: name=sha256-hex=url. Three '=' separators
                // are unambiguous because SHA-256 hex is fixed 64
                // chars and contains no '=', and presigned URLs have
                // their own '=' inside the query string — so we
                // splitn(3, '=') instead of a naive split.
                let raw = args[i].clone();
                let mut parts = raw.splitn(3, '=');
                let name = parts.next().unwrap_or("");
                let sha = parts.next().unwrap_or("");
                let url = parts.next().unwrap_or("");
                if name.is_empty() || sha.len() != 64 || url.is_empty() {
                    eprintln!(
                        "--upload-url expects `name=sha256-hex=url`. Got: {raw}\n\
                         (sha256 hex must be exactly 64 chars; name and url non-empty.)"
                    );
                    return 2;
                }
                upload_urls.push((name.to_string(), sha.to_string(), url.to_string()));
            }
            "--ttl-hours" => {
                i += 1;
                bootstrap_ttl_hours = args[i].parse().unwrap_or(bootstrap_ttl_hours);
            }
            "--dry-run" => {
                dry_run = true;
            }
            other => {
                eprintln!("unknown flag: {other}");
                eprintln!(
                    "usage: sovereign pipeline pod up \\\n\
                    \x20\x20[--gpu <name>] [--image <ref>] [--disk <gb>] [--label <s>]\\\n\
                    \x20\x20[--max-price <usd>] [--job-id <s>] \\\n\
                    \x20\x20[--upload <path>]... [--upload-url <name>=<sha256-hex>=<url>]... \\\n\
                    \x20\x20[--upload-from-base-url <base-url>] \\\n\
                    \x20\x20[--ttl-hours <h>] [--dry-run]\n\
                    \n\
                    --upload streams the file from your laptop (slow over residential\n\
                    upload). --upload-url has the pod fetch the file itself from\n\
                    R2/B2/S3 at data-center speeds — paste a presigned URL with a\n\
                    short TTL.\n\
                    \n\
                    --upload-from-base-url <base> is the ergonomic shortcut: each\n\
                    --upload <local-path> becomes a URL-backed entry at\n\
                    `<base>/<filename>` with SHA computed from the local copy. Right\n\
                    for the common case where every model is staged in one R2 bucket.\n\
                    \n\
                    SHA validation is owner-signed in every case."
                );
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
    let job_id = job_id.unwrap_or_else(|| {
        format!(
            "job-{}",
            uuid::Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(12)
                .collect::<String>()
        )
    });
    // ─── Vast offer search ─────────────────────────────────────────
    // CUDA ≥ 12.4 so the image's runtime matches. `direct_port_count>=2`
    // ensures the pod will get a public host port for the worker daemon
    // on :9742 (see EPHEMERAL_WORKER_PODS.md §"Provider connectivity
    // audit"). `reliability>=0.95` is the real quality filter — Vast's
    // `verified=true` is a much narrower premium-host program (often
    // zero offers in our price band when checked 2026-05-18). We rank
    // verified higher inside `pick_offer`, so dropping the search-side
    // gate just widens the candidate pool when verified hosts are
    // unavailable.
    let num_gpus_clause = num_gpus
        .map(|n| format!(" num_gpus={n}"))
        .unwrap_or_default();
    let query = format!(
        "gpu_name={gpu_name} rentable=true cuda_max_good>=12.4 \
         direct_port_count>=2 reliability>=0.95 dph_total<={max_price}{num_gpus_clause}"
    );
    let offers = match pod::search_offers(&query, 50) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vastai search failed: {e}");
            return 1;
        }
    };
    let pick = match pod::pick_offer(&offers).cloned() {
        Some(p) => p,
        None => {
            eprintln!("no offers matched: {query}");
            return 1;
        }
    };
    println!(
        "selected offer: id={} gpu={} ${:.3}/hr rel={:.2} verified={} loc={}",
        pick.id,
        pick.gpu_name,
        pick.price_per_hour,
        pick.reliability,
        pick.verified,
        pick.geolocation,
    );

    // ─── Hash uploads up front ─────────────────────────────────────
    // The bootstrap blob carries a `filename → SHA-256` manifest the
    // pod uses to validate streamed uploads. We compute SHAs locally
    // before paying for the pod so a missing file aborts cheaply.
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    let mut upload_specs: BTreeMap<String, sovereign_mesh::worker_controller::UploadFile> =
        BTreeMap::new();
    for path in &uploads {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => {
                eprintln!(
                    "--upload path has no filename component: {}",
                    path.display()
                );
                return 2;
            }
        };
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("--upload read failed for {}: {e}", path.display());
                return 1;
            }
        };
        let mut h = Sha256::new();
        h.update(&bytes);
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&h.finalize());
        // If --upload-from-base-url is set, the local file is just a
        // SHA source — the pod fetches bytes from <base>/<filename>
        // instead. This is the right primitive for "all my models are
        // already staged in one R2 bucket".
        let upload_file = if let Some(base) = upload_from_base.as_ref() {
            let url = format!("{base}/{name}");
            sovereign_mesh::worker_controller::UploadFile::fetch_url(url, sha)
        } else {
            sovereign_mesh::worker_controller::UploadFile::local(path.clone(), sha)
        };
        upload_specs.insert(name, upload_file);
    }
    // URL-backed entries — pod fetches itself.
    for (name, sha_hex, url) in &upload_urls {
        let sha_bytes = match hex::decode(sha_hex) {
            Ok(v) if v.len() == 32 => v,
            _ => {
                eprintln!("--upload-url SHA-256 hex must decode to 32 bytes; got: {sha_hex}");
                return 2;
            }
        };
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&sha_bytes);
        upload_specs.insert(
            name.clone(),
            sovereign_mesh::worker_controller::UploadFile::fetch_url(url.clone(), sha),
        );
    }

    let label_value = label.unwrap_or_else(|| format!("{job_id}-pod"));

    if dry_run {
        println!(
            "DRY RUN: would create instance via vastai create instance {} \
             --image {} --disk {} --label {} --ssh, then mint a worker bootstrap blob \
             with {} upload(s), upload to :9742 over TLS-pinned reqwest.",
            pick.id,
            image,
            disk_gb,
            label_value,
            upload_specs.len(),
        );
        return 0;
    }

    // ─── Owner key + controller ─────────────────────────────────────
    let owner_key = match crate::worker_pod_provider::load_or_create_owner_key() {
        Ok(k) => k,
        Err(e) => {
            eprintln!(
                "could not load/create owner key at {}: {e}",
                crate::worker_pod_provider::owner_key_path().display(),
            );
            return 1;
        }
    };
    let provider = std::sync::Arc::new(crate::worker_pod_provider::VastWorkerProvider::new(
        image.clone(),
        disk_gb,
        label_value.clone(),
        pick.clone(),
    ));
    let mut ctrl_config = sovereign_mesh::worker_controller::ControllerConfig::default();
    ctrl_config.bootstrap_ttl_seconds = bootstrap_ttl_hours.saturating_mul(3600);
    let controller =
        sovereign_mesh::worker_controller::WorkerController::new(provider, owner_key, ctrl_config);

    // ─── JobSpec ────────────────────────────────────────────────────
    // No units list yet — `pod up` boots the pod and leaves it ready
    // for follow-up dispatch. A future `pipeline pod dispatch <handle>
    // <manifest.json>` command will POST the units to the worker.
    let spec = sovereign_mesh::worker_controller::JobSpec {
        job_id: job_id.clone(),
        image: image.clone(),
        disk_gb,
        gpu_name: pick.gpu_name.clone(),
        max_price_per_hour: pick.price_per_hour,
        label: label_value.clone(),
        uploads: upload_specs,
        units: Vec::new(),
        runner_config: serde_json::json!({}),
    };

    // create_and_run_with_blob mints the blob, calls vastai create,
    // polls for address, waits for health, uploads files. It does
    // NOT dispatch a job (units is empty); the pod stays in
    // "uploads ready" state for follow-up commands. The `_with_blob`
    // variant also yields the bootstrap blob so we can persist a
    // pinned-pod snapshot for the inference scheduler.
    let (handle, instance, blob, _client) = match controller.create_and_run_with_blob(&spec).await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("pod up failed: {e}");
            // Best-effort cleanup: if `instance_id` was created but
            // the rest of the lifecycle failed, the operator can run
            // `vastai destroy instance <id>` manually. Surfacing the
            // error is more important than guessing the instance id
            // here.
            return 1;
        }
    };

    // Persist a pinned-pod snapshot so the daemon's inference
    // scheduler picks the pod up on next startup (or via the
    // `--extra-worker` flag). Failure to write the snapshot is
    // non-fatal — the pod still runs; the operator just won't get
    // automatic inference routing to it until they re-run pod up
    // or write the file themselves.
    // Spec: docs/PINNED_WORKER_AS_INFERENCE_PEER.md §3.6.
    // Capture token expiry before `blob` is moved into the snapshot —
    // we re-print it at the end of this command for operator visibility.
    let expires_unix = blob.expires_unix;
    if let Some(dir) = sovereign_mesh::pinned_pod_snapshot::default_snapshot_dir() {
        let capabilities = capabilities_for_gpu(&instance.gpu_name);
        let snapshot = sovereign_mesh::pinned_pod_snapshot::PinnedPodSnapshot::new(
            instance.instance_id.clone(),
            handle.host(),
            handle.port(),
            blob,
            capabilities,
        );
        match sovereign_mesh::pinned_pod_snapshot::save_snapshot(&dir, &snapshot) {
            Ok(p) => println!(
                "wrote snapshot at {} (inference routing enabled)",
                p.display()
            ),
            Err(e) => {
                eprintln!("warning: snapshot write failed ({e}) — inference routing disabled")
            }
        }
    }

    let rec = ledger::PodRecord {
        vast_id: instance.instance_id.clone(),
        label: label_value,
        recipe_id: job_id.clone(),
        gpu_name: instance.gpu_name.clone(),
        image: image.clone(),
        started_at: ledger::unix_now(),
        ended_at: None,
        cost_per_hour: instance.cost_per_hour,
        status: ledger::PodStatus::Running,
    };
    if let Err(e) = ledger::append(ledger::default_path(), rec) {
        eprintln!(
            "WARNING: pod {} launched but ledger append failed: {e}\n\
             Track manually until `pod list` resolves.",
            instance.instance_id
        );
    }

    // Surface the token expiry prominently. The 2026-05-18 SEP-on-Vast
    // run wedged silently when a 12h token expired mid-job — the operator
    // had no signal that auth was about to break beyond a buried JSON
    // field in `~/.sovereign/worker-pods/<id>.json`. Print the expiry
    // time + remaining hours alongside the rest of the launch summary.
    // `expires_unix` was captured above before `blob` was moved into
    // the snapshot.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ttl_remaining_h = expires_unix.saturating_sub(now) as f64 / 3600.0;
    let expires_display = chrono::DateTime::<chrono::Utc>::from(
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(expires_unix),
    )
    .format("%Y-%m-%d %H:%M:%S UTC");

    println!();
    println!("worker pod ready:");
    println!("  vast id          {}", instance.instance_id);
    println!("  job id           {job_id}");
    println!("  $/hr             {:.3}", instance.cost_per_hour);
    println!("  worker address   {}", handle.base_url());
    println!(
        "  pinned thumbprint {}",
        hex::encode(handle.pod_pubkey_thumbprint())
    );
    println!("  uploads          {}", spec.uploads.len());
    println!(
        "  token expires    {expires_display}  (in {ttl_remaining_h:.1}h — \
         re-launch with --ttl-hours <N> if your job runs longer)"
    );
    println!();
    println!("Pod is in 'uploads ready' state. Dispatch a job with the worker token:");
    println!("  (token printed once — keep it; future invocations will be `pipeline pod dispatch <vast-id>`)");
    println!("  token: {}", handle.worker_token());
    println!();
    println!("Tear down with:");
    println!("  sovereign pipeline pod down {}", instance.instance_id);
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
        "{:<10} {:<6} {:<22} {:<12} {:>8} {:>10}  started_at",
        "vast_id", "state", "label", "gpu", "$/hr", "accrued"
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

    // Remove the pinned-pod snapshot so the daemon's inference
    // scheduler stops considering this pod. Idempotent — a pod that
    // never wrote a snapshot (older pod-up before the inference
    // wiring shipped) just returns false here.
    // Spec: docs/PINNED_WORKER_AS_INFERENCE_PEER.md §3.6.
    if let Some(dir) = sovereign_mesh::pinned_pod_snapshot::default_snapshot_dir() {
        match sovereign_mesh::pinned_pod_snapshot::delete_snapshot(&dir, &vast_id) {
            Ok(true) => println!("removed pinned-pod snapshot for {vast_id}"),
            Ok(false) => {}
            Err(e) => eprintln!("warning: snapshot delete failed: {e}"),
        }
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

/// Operator-stamped capabilities for a rented GPU. Best-effort:
/// covers the GPU families we routinely rent on Vast (L40S, A6000,
/// H100, RTX 4090) with a default fallback. Tuning these tighter is
/// future work; the inference scheduler's throughput-observation
/// loop self-corrects once real traffic flows.
///
/// `system_ram_gb` is the Vast offer's *host* RAM — the pod's child
/// daemon reads this for slot sizing. A miscalibration just biases
/// routing, no correctness risk.
fn capabilities_for_gpu(gpu_name: &str) -> sovereign_mesh::pinned_worker_source::PodCapabilities {
    let upper = gpu_name.to_ascii_uppercase();
    let system_ram_gb = if upper.contains("H100") {
        192
    } else if upper.contains("L40S") || upper.contains("A6000") || upper.contains("L40") {
        128
    } else {
        64
    };
    sovereign_mesh::pinned_worker_source::PodCapabilities {
        system_ram_gb,
        benchmark: None,
        current_in_flight: None,
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

/// `sovereign pipeline pod pool` — multi-pod variant of `pod up`.
///
/// Reads a JSONL manifest of work units, fans them out across `N`
/// Vast pods (round-robin), drains completions to an output file,
/// and (optionally) destroys all pods on completion. One command,
/// full lifecycle — useful for kicking off batch ingest runs from
/// the shell.
///
/// Spec: `sovereign/docs/EPHEMERAL_WORKER_PODS.md` §"Multi-pod jobs".
async fn cmd_pod_pool(args: &[String]) -> i32 {
    let mut pod_count: usize = 0;
    let mut gpu_name: String = "L40S".into();
    let mut image: Option<String> = std::env::var("SOVEREIGN_VAST_IMAGE").ok();
    let mut disk_gb: u32 = 80;
    let mut label: Option<String> = None;
    let mut max_price: f64 = 0.80;
    let mut dry_run: bool = false;
    let mut job_id: Option<String> = None;
    let mut manifest_path: Option<std::path::PathBuf> = None;
    let mut output_path: Option<std::path::PathBuf> = None;
    let mut keep_alive: bool = false;
    let mut uploads: Vec<std::path::PathBuf> = Vec::new();
    let mut upload_urls: Vec<(String, String, String)> = Vec::new();
    let mut upload_from_base: Option<String> = None;
    // Token TTL covers the longest plausible owner-side workflow. SEP and
    // wikipedia ingest runs commonly take 30-50h; the prior 12h default
    // expired mid-run, after which every owner request to the pod 401'd
    // with `token expired` while still billing for the GPU. 48h is the
    // largest window that's still bounded for blast-radius purposes (the
    // bootstrap blob is the credential — a leaked blob is good until
    // expiry). Operators on multi-day jobs should pass `--ttl-hours 72`
    // or higher explicitly.
    let mut bootstrap_ttl_hours: u64 = 48;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pods" => {
                i += 1;
                pod_count = args[i].parse().unwrap_or(0);
            }
            "--gpu" => {
                i += 1;
                gpu_name = args[i].clone();
            }
            "--image" => {
                i += 1;
                image = Some(args[i].clone());
            }
            "--disk" => {
                i += 1;
                disk_gb = args[i].parse().unwrap_or(disk_gb);
            }
            "--label" => {
                i += 1;
                label = Some(args[i].clone());
            }
            "--max-price" => {
                i += 1;
                max_price = args[i].parse().unwrap_or(max_price);
            }
            "--job-id" => {
                i += 1;
                job_id = Some(args[i].clone());
            }
            "--manifest" => {
                i += 1;
                manifest_path = Some(std::path::PathBuf::from(args[i].clone()));
            }
            "--output" => {
                i += 1;
                output_path = Some(std::path::PathBuf::from(args[i].clone()));
            }
            "--keep-alive" => {
                keep_alive = true;
            }
            "--upload" => {
                i += 1;
                uploads.push(std::path::PathBuf::from(args[i].clone()));
            }
            "--upload-from-base-url" => {
                i += 1;
                upload_from_base = Some(args[i].trim_end_matches('/').to_string());
            }
            "--upload-url" => {
                i += 1;
                let raw = args[i].clone();
                let mut parts = raw.splitn(3, '=');
                let name = parts.next().unwrap_or("");
                let sha = parts.next().unwrap_or("");
                let url = parts.next().unwrap_or("");
                if name.is_empty() || sha.len() != 64 || url.is_empty() {
                    eprintln!("--upload-url expects `name=sha256-hex=url`. Got: {raw}");
                    return 2;
                }
                upload_urls.push((name.to_string(), sha.to_string(), url.to_string()));
            }
            "--ttl-hours" => {
                i += 1;
                bootstrap_ttl_hours = args[i].parse().unwrap_or(bootstrap_ttl_hours);
            }
            "--dry-run" => {
                dry_run = true;
            }
            other => {
                eprintln!("unknown flag: {other}");
                eprintln!(
                    "usage: sovereign pipeline pod pool \\\n\
                    \x20\x20--pods <N> --manifest <units.jsonl> [--output <results.jsonl>]\\\n\
                    \x20\x20[--gpu <name>] [--image <ref>] [--disk <gb>] [--label <s>]\\\n\
                    \x20\x20[--max-price <usd>] [--job-id <s>] [--keep-alive] \\\n\
                    \x20\x20[--upload <path>]... [--upload-from-base-url <base>]\\\n\
                    \x20\x20[--upload-url <name>=<sha>=<url>]... [--ttl-hours <h>] [--dry-run]\n\
                    \n\
                    Creates N Vast pods in parallel, partitions the manifest \n\
                    round-robin across them, drains completions, and destroys \n\
                    all pods unless --keep-alive is set."
                );
                return 2;
            }
        }
        i += 1;
    }
    if pod_count == 0 {
        eprintln!("--pods <N> is required (N ≥ 1)");
        return 2;
    }
    let Some(image) = image else {
        eprintln!("no container image. Pass `--image <ref>` or set SOVEREIGN_VAST_IMAGE.");
        return 2;
    };
    let Some(manifest_path) = manifest_path else {
        eprintln!(
            "--manifest <units.jsonl> is required (one WorkUnit JSON per line, \
             with unit_id >= 1 — see EPHEMERAL_WORKER_PODS.md)"
        );
        return 2;
    };
    let job_id = job_id.unwrap_or_else(|| {
        format!(
            "pool-{}",
            uuid::Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(12)
                .collect::<String>()
        )
    });
    let label_value = label.unwrap_or_else(|| format!("{job_id}-pool"));

    // ─── Parse manifest ─────────────────────────────────────────────
    let manifest_bytes = match std::fs::read(&manifest_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("read manifest {}: {e}", manifest_path.display());
            return 1;
        }
    };
    let manifest_text = match std::str::from_utf8(&manifest_bytes) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("manifest not utf-8: {e}");
            return 1;
        }
    };
    let mut units: Vec<sovereign_mesh::worker_http::WorkUnit> = Vec::new();
    for (line_no, line) in manifest_text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match serde_json::from_str::<sovereign_mesh::worker_http::WorkUnit>(trimmed) {
            Ok(u) => {
                if u.unit_id == 0 {
                    eprintln!(
                        "manifest line {}: unit_id must be >= 1 (cursor uses > since semantics)",
                        line_no + 1
                    );
                    return 2;
                }
                units.push(u);
            }
            Err(e) => {
                eprintln!("manifest line {}: parse error: {e}", line_no + 1);
                return 2;
            }
        }
    }
    if units.is_empty() {
        eprintln!("manifest is empty (no parseable WorkUnit lines)");
        return 2;
    }
    let total_units = units.len();
    if pod_count > total_units {
        eprintln!(
            "warning: --pods {pod_count} > manifest units {total_units}; \
             {} pods will receive no units and stay idle",
            pod_count - total_units
        );
    }

    // ─── Vast offer search — pick top N ─────────────────────────────
    // See `cmd_pod_up` for rationale on dropping `verified=true` from
    // the search query; `reliability>=0.95` is the real quality gate.
    let query = format!(
        "gpu_name={gpu_name} rentable=true cuda_max_good>=12.4 \
         direct_port_count>=2 reliability>=0.95 dph_total<={max_price}"
    );
    let offers = match pod::search_offers(&query, (pod_count as u32).saturating_mul(3).max(50)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("vastai search failed: {e}");
            return 1;
        }
    };
    if offers.len() < pod_count {
        eprintln!(
            "only {} offers matched query (need {pod_count}): {query}",
            offers.len()
        );
        return 1;
    }
    // Sort by (verified desc, reliability desc, price asc) — same
    // criteria as pick_offer.
    let mut ranked = offers.clone();
    ranked.sort_by(|a, b| {
        b.verified
            .cmp(&a.verified)
            .then(
                b.reliability
                    .partial_cmp(&a.reliability)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(
                a.price_per_hour
                    .partial_cmp(&b.price_per_hour)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    let chosen: Vec<_> = ranked.into_iter().take(pod_count).collect();
    println!("selected {pod_count} offers:");
    for (i, o) in chosen.iter().enumerate() {
        println!(
            "  pod {i}: id={} gpu={} ${:.3}/hr rel={:.2} loc={}",
            o.id, o.gpu_name, o.price_per_hour, o.reliability, o.geolocation
        );
    }

    // ─── Hash uploads ───────────────────────────────────────────────
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    let mut upload_specs: BTreeMap<String, sovereign_mesh::worker_controller::UploadFile> =
        BTreeMap::new();
    for path in &uploads {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => {
                eprintln!(
                    "--upload path has no filename component: {}",
                    path.display()
                );
                return 2;
            }
        };
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("--upload read failed for {}: {e}", path.display());
                return 1;
            }
        };
        let mut h = Sha256::new();
        h.update(&bytes);
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&h.finalize());
        let upload_file = if let Some(base) = upload_from_base.as_ref() {
            let url = format!("{base}/{name}");
            sovereign_mesh::worker_controller::UploadFile::fetch_url(url, sha)
        } else {
            sovereign_mesh::worker_controller::UploadFile::local(path.clone(), sha)
        };
        upload_specs.insert(name, upload_file);
    }
    for (name, sha_hex, url) in &upload_urls {
        let sha_bytes = match hex::decode(sha_hex) {
            Ok(v) if v.len() == 32 => v,
            _ => {
                eprintln!("--upload-url SHA-256 hex must decode to 32 bytes; got: {sha_hex}");
                return 2;
            }
        };
        let mut sha = [0u8; 32];
        sha.copy_from_slice(&sha_bytes);
        upload_specs.insert(
            name.clone(),
            sovereign_mesh::worker_controller::UploadFile::fetch_url(url.clone(), sha),
        );
    }

    if dry_run {
        let total_cost_per_hour: f64 = chosen.iter().map(|o| o.price_per_hour).sum();
        println!(
            "DRY RUN: would create {pod_count} pods (~${:.3}/hr total), \
             upload {} file(s), dispatch {} units, drain completions to {}",
            total_cost_per_hour,
            upload_specs.len(),
            total_units,
            output_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<stdout>".into()),
        );
        return 0;
    }

    // ─── Owner key + coordinator ────────────────────────────────────
    let owner_key = match crate::worker_pod_provider::load_or_create_owner_key() {
        Ok(k) => k,
        Err(e) => {
            eprintln!(
                "could not load/create owner key at {}: {e}",
                crate::worker_pod_provider::owner_key_path().display(),
            );
            return 1;
        }
    };
    let provider = std::sync::Arc::new(
        crate::worker_pod_provider::MultiOfferVastWorkerProvider::new(
            image.clone(),
            disk_gb,
            label_value.clone(),
            chosen,
        ),
    );
    let mut ctrl_config = sovereign_mesh::worker_controller::ControllerConfig::default();
    ctrl_config.bootstrap_ttl_seconds = bootstrap_ttl_hours.saturating_mul(3600);
    let coord_config = sovereign_mesh::multi_pod_coordinator::CoordinatorConfig::default();
    let coordinator = sovereign_mesh::multi_pod_coordinator::MultiPodCoordinator::new(
        provider,
        owner_key,
        ctrl_config,
        coord_config,
    );

    let spec = sovereign_mesh::worker_controller::JobSpec {
        job_id: job_id.clone(),
        image: image.clone(),
        disk_gb,
        gpu_name: gpu_name.clone(),
        max_price_per_hour: max_price,
        label: label_value.clone(),
        uploads: upload_specs,
        units,
        runner_config: serde_json::json!({}),
    };

    // ─── Launch ─────────────────────────────────────────────────────
    println!();
    println!("launching {pod_count} pods (this can take a few minutes per pod)…");
    let pool = match coordinator.launch(spec, pod_count).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("pool launch failed: {e}");
            return 1;
        }
    };
    println!();
    println!("pool live — {pod_count} pods running:");
    for snap in pool.snapshot().await {
        println!(
            "  pod {} vast={} gpu={} ${:.3}/hr addr={} units={}",
            snap.pod_index,
            snap.instance_id,
            snap.gpu_name,
            snap.cost_per_hour,
            snap.worker_address,
            snap.assigned_units
        );
    }

    // ─── Drain ──────────────────────────────────────────────────────
    let output_handle: std::sync::Arc<std::sync::Mutex<Option<std::fs::File>>> =
        std::sync::Arc::new(std::sync::Mutex::new(match &output_path {
            Some(p) => match std::fs::File::create(p) {
                Ok(f) => Some(f),
                Err(e) => {
                    eprintln!("could not open --output {}: {e}", p.display());
                    return 1;
                }
            },
            None => None,
        }));
    let received = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let received_h = received.clone();
    let output_h = output_handle.clone();
    println!();
    println!("draining completions…");
    let summary = match pool
        .poll_until_complete(coordinator.controller(), move |pod_idx, unit| {
            received_h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let row = serde_json::json!({
                "pod_index": pod_idx,
                "unit": unit,
            });
            let line = serde_json::to_string(&row).unwrap_or_else(|_| "{}".into());
            if let Ok(mut guard) = output_h.lock() {
                if let Some(file) = guard.as_mut() {
                    use std::io::Write;
                    let _ = writeln!(file, "{line}");
                } else {
                    println!("{line}");
                }
            }
        })
        .await
    {
        Ok(s) => s,
        Err(e) => {
            eprintln!("poll failed: {e}");
            if !keep_alive {
                eprintln!("tearing down pool…");
                let _ = pool.destroy_all(coordinator.controller()).await;
            }
            return 1;
        }
    };

    println!();
    println!(
        "drain complete: {} units received in {:.1}s ({} poll errors){}",
        summary.total_received,
        summary.elapsed.as_secs_f64(),
        summary.total_errors,
        if summary.timed_out {
            " [TIMED OUT]"
        } else {
            ""
        },
    );

    // ─── Tear down ──────────────────────────────────────────────────
    if !keep_alive {
        let destroy_results = pool.destroy_all(coordinator.controller()).await;
        let failures = destroy_results.iter().filter(|(_, r)| r.is_err()).count();
        if failures == 0 {
            println!("all {pod_count} pods destroyed.");
        } else {
            eprintln!(
                "{}/{pod_count} pod destroys failed — check `vastai show instances` and \
                 `sovereign pipeline pod down <id>` to clean up.",
                failures
            );
            for (i, r) in destroy_results.iter() {
                if let Err(e) = r {
                    eprintln!("  pod {i}: {e}");
                }
            }
        }
    } else {
        println!();
        println!("--keep-alive set: {pod_count} pods left running.");
        for snap in pool.snapshot().await {
            println!("  pod {} vast={}", snap.pod_index, snap.instance_id);
        }
        println!("destroy each with `sovereign pipeline pod down <vast-id>`.");
    }
    if summary.timed_out {
        1
    } else {
        0
    }
}
