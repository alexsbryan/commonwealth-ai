// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh` subcommand handlers (the `corpus` half moved to
//! `corpus_cmd` in the §3.2 split that fixed the dispatch naming lie).
//!
//! These are lightweight commands that don't require loading a full model
//! or database — they manage the embedded Commonwealth daemon.

use std::path::PathBuf;

use sovereign_cli_shared::dirs::sovereign_root;
use sovereign_mesh::deep_link::{build_https_join_link, parse_join_argument};
use sovereign_mesh::EmbeddedDaemon;

/// The `SetupConfig` a `svrn mesh` one-shot binds with.
///
/// A missing `config.toml` is the ordinary first-run state and
/// [`SetupConfig::unconfigured`] is its honest value: default ports, loopback
/// client bind. A config that EXISTS but will not parse is not that state, and
/// the substitution is named on stderr rather than applied silently — before
/// this the daemon reached the same defaults through internal `None` fallbacks
/// and said nothing, so a typo in `[daemon] client_port` looked like the port
/// simply not taking effect (ARCH §18.3).
/// The `DaemonServices` a `svrn mesh create` / `svrn mesh join` one-shot
/// assembles — obtained from THE assembler, not named here.
///
/// `sovereign_mesh::assemble` is the one exhaustive match over `Launch` that
/// constructs anything (`quality/TOPOLOGY.md` §10, Falsifier 3). These two
/// sites used to name `DaemonServices::MeshAdmin` directly, which is a fourth
/// place answering "what does this invocation assemble". They now supply
/// parts and let the match answer — so a mesh verb that somehow ran under a
/// different launch mode is refused rather than quietly given a daemon.
///
/// `Launch::parse` is called here rather than threaded because this binary is
/// `exec`d by the dispatcher and its argv IS the verb invocation; parse is the
/// one sanctioned reader of that (Falsifier 1 forbids OTHER code deciding what
/// the process is, not calling the decider).
fn mesh_admin_services() -> sovereign_mesh::DaemonServices {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let launch = sovereign_contracts::launch::Launch::parse(
        &args,
        // A mesh verb reaching this code path IS a verb invocation; `Bare` is
        // the honest default for "argv named nothing this parser knows".
        sovereign_contracts::launch::Launch::Verb {
            name: "mesh".to_string(),
            args: args.clone(),
        },
    );
    match sovereign_mesh::assemble(&launch, sovereign_mesh::LaunchParts::Admin) {
        Ok(services) => services,
        Err(refusal) => {
            eprintln!("error: {refusal}");
            std::process::exit(1);
        }
    }
}

fn one_shot_setup_config() -> sovereign_core::setup_config::SetupConfig {
    match sovereign_core::setup_config::SetupConfig::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("(no usable ~/.svrnmesh/config.toml: {e} — binding the default :9741/:9742)");
            sovereign_core::setup_config::SetupConfig::unconfigured()
        }
    }
}

/// Run a mesh subcommand. Returns the exit code.
pub async fn run_mesh(args: &[String]) -> i32 {
    if args.is_empty() {
        sovereign_cli_shared::help::print(&HELP_MESH);
        return 1;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        sovereign_cli_shared::help::print(&HELP_MESH);
        return 0;
    }

    match args[0].as_str() {
        "create" => cmd_create(&args[1..]).await,
        "join" => cmd_join(&args[1..]).await,
        "list" => cmd_list(&args[1..]).await,
        "switch" => cmd_switch(&args[1..]).await,
        "forget" => cmd_forget(&args[1..]).await,
        "forget-member" => crate::mesh_member_cmd::cmd_forget_member(&args[1..]).await,
        "rotate" => cmd_rotate(&args[1..]).await,
        "grant" => crate::mesh_guest::cmd_grant(&args[1..]).await,
        "use" => crate::mesh_guest::cmd_use(&args[1..]).await,
        "status" => cmd_status(&args[1..]).await,
        "transport" => cmd_transport(&args[1..]).await,
        "balance" => cmd_balance().await,
        "leave" => cmd_leave(&args[1..]).await,
        "logs" => cmd_logs().await,
        "fetch-model" => cmd_fetch_model(&args[1..]).await,
        "warm-cache" => cmd_warm_cache(&args[1..]).await,
        "plan" => cmd_plan(&args[1..]).await,
        "bench" => crate::mesh_bench::cmd_bench(&args[1..]).await,
        "check-invariants" => cmd_check_invariants(&args[1..]).await,
        "soak-gate" => cmd_soak_gate(&args[1..]).await,
        other => {
            eprintln!("Unknown mesh subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP_MESH);
            1
        }
    }
}

/// `svrn mesh warm-cache <gguf> [--cache-dir <dir>]`
///
/// Pre-seed the RPC worker's tensor cache from a local GGUF — fully offline (no
/// network, no GPU). When the cluster later serves this model, the host's
/// tensor-hash requests are all cache hits and zero weight bytes cross the wire.
/// The companion to a thumbdrive'd GGUF: distribute the model offline, run this
/// on each worker, and a metered/throttled link never carries the weights.
async fn cmd_warm_cache(args: &[String]) -> i32 {
    let mut model: Option<std::path::PathBuf> = None;
    let mut cache_dir: Option<std::path::PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--cache-dir" => {
                i += 1;
                cache_dir = args.get(i).map(std::path::PathBuf::from);
            }
            "--help" | "-h" => {
                eprintln!("Usage: svrn mesh warm-cache <model.gguf> [--cache-dir <dir>]");
                eprintln!();
                eprintln!("  Pre-seeds the RPC tensor cache from a local GGUF so a mesh worker");
                eprintln!("  serves this model with ZERO weight transfer over the network.");
                eprintln!("  Fully offline — no network, no GPU. Default cache dir:");
                eprintln!("  ~/.svrnmesh/rpc-cache (matches the in-process worker).");
                return 0;
            }
            s if model.is_none() && !s.starts_with('-') => {
                model = Some(std::path::PathBuf::from(s));
            }
            other => {
                eprintln!("Unknown arg: {other}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(model) = model else {
        eprintln!("Usage: svrn mesh warm-cache <model.gguf> [--cache-dir <dir>]");
        return 2;
    };
    let cache_dir = match cache_dir.or_else(sovereign_inference::embedded::default_cache_dir) {
        Some(d) => d,
        None => {
            eprintln!(
                "no cache dir: SOVEREIGN_RPC_CACHE_DIR is off/0/empty (caching \
                 disabled), or HOME is unset. Pass --cache-dir to warm one anyway."
            );
            return 1;
        }
    };
    eprintln!(
        "warming RPC cache for {} → {}",
        model.display(),
        cache_dir.display()
    );
    let t0 = std::time::Instant::now();
    match sovereign_inference::embedded::warm_cache_from_gguf(&model, &cache_dir) {
        Ok(s) => {
            println!(
                "✓ {}/{} tensors cacheable (>10MB): {} written ({:.2} GB), {} already present — {:.1}s",
                s.tensors_cacheable,
                s.tensors_total,
                s.written,
                s.bytes_written as f64 / 1e9,
                s.already_present,
                t0.elapsed().as_secs_f64(),
            );
            println!("  cache dir: {}", s.cache_dir.display());
            0
        }
        Err(e) => {
            eprintln!("warm-cache failed: {e}");
            1
        }
    }
}

/// `svrn mesh check-invariants --nodes <a:port,b:port,...> [--expect-live <id,...>] [--json]`
///
/// The assertion engine for the multi-process soak (`scripts/mesh-soak.sh`):
/// polls each node's `GET /v1/mesh/status` and evaluates the HTTP-observable
/// mesh invariants (convergence / no-ghost / liveness — see [`crate::mesh_soak`]).
/// Exits non-zero if any invariant is violated, so a soak loop can `||` on it.
/// `--json` emits one machine-readable line for `mesh-soak-findings.jsonl`.
async fn cmd_check_invariants(args: &[String]) -> i32 {
    use crate::mesh_soak::{evaluate_invariants, NodeSnapshot, NodeStatusView};

    let mut nodes: Vec<String> = Vec::new();
    let mut json = false;
    let mut expect_live: Option<std::collections::BTreeSet<String>> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--nodes" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    nodes = v
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
            }
            "--expect-live" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    expect_live = Some(
                        v.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect(),
                    );
                }
            }
            "--json" => json = true,
            "--help" | "-h" => {
                eprintln!("Usage: svrn mesh check-invariants --nodes <a:port,b:port,...> [--expect-live <id,...>] [--json]");
                eprintln!();
                eprintln!("  Polls GET /v1/mesh/status on each node and asserts the mesh");
                eprintln!("  invariants: convergence (all agree on the member set), no-ghost");
                eprintln!("  (no deliberately-downed node shown live; pair with --expect-live),");
                eprintln!("  and liveness (every reachable node seen live by its peers).");
                eprintln!("  Exit 0 if all hold, 1 on violation. The assertion engine for");
                eprintln!("  scripts/mesh-soak.sh.");
                return 0;
            }
            other => {
                eprintln!("Unknown arg: {other}");
                return 2;
            }
        }
        i += 1;
    }
    if nodes.is_empty() {
        eprintln!("--nodes is required (comma-separated host:port list)");
        return 2;
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    let mut snapshots = Vec::with_capacity(nodes.len());
    for addr in &nodes {
        let url = format!("http://{addr}/v1/mesh/status");
        let snap = match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.json::<NodeStatusView>().await {
                Ok(v) => NodeSnapshot {
                    addr: addr.clone(),
                    status: Some(v),
                    error: None,
                },
                Err(e) => NodeSnapshot {
                    addr: addr.clone(),
                    status: None,
                    error: Some(format!("bad status json: {e}")),
                },
            },
            Ok(resp) => NodeSnapshot {
                addr: addr.clone(),
                status: None,
                error: Some(format!("http {}", resp.status())),
            },
            Err(e) => NodeSnapshot {
                addr: addr.clone(),
                status: None,
                error: Some(format!("unreachable: {e}")),
            },
        };
        snapshots.push(snap);
    }

    let violations = evaluate_invariants(&snapshots, expect_live.as_ref());

    if json {
        let unreachable: Vec<&str> = snapshots
            .iter()
            .filter(|s| s.status.is_none())
            .map(|s| s.addr.as_str())
            .collect();
        // Track W: nodes whose founder self-heal is currently degraded (soft
        // signal — transient under reachability chaos, gated by the
        // `founder_degraded_rate` SLI, NOT a hard invariant `ok` failure).
        let founder_degraded = crate::mesh_soak::founder_degraded_addrs(&snapshots);
        let line = serde_json::json!({
            "nodes": nodes,
            "unreachable": unreachable,
            "violations": violations
                .iter()
                .map(|v| serde_json::json!({ "invariant": v.invariant, "detail": v.detail }))
                .collect::<Vec<_>>(),
            "ok": violations.is_empty(),
            "founder_degraded": founder_degraded,
        });
        println!("{line}");
    } else {
        for s in &snapshots {
            match &s.status {
                Some(v) => println!("  {} — {} members", s.addr, v.members_total),
                None => println!(
                    "  {} — UNREACHABLE ({})",
                    s.addr,
                    s.error.as_deref().unwrap_or("?")
                ),
            }
        }
        if violations.is_empty() {
            println!("✓ mesh invariants hold across {} node(s)", nodes.len());
        } else {
            eprintln!("✘ {} invariant violation(s):", violations.len());
            for v in &violations {
                eprintln!("  [{}] {}", v.invariant, v.detail);
            }
        }
    }

    if violations.is_empty() {
        0
    } else {
        1
    }
}

/// `svrn mesh soak-gate <findings.jsonl> [--baseline <file>] [--update-baseline]`
///
/// Layer 3 of the mesh QA plan: distils `mesh-soak-findings.jsonl` into SLIs
/// (invariant violation rate, load success rate, load p50/p99) and gates each
/// against a committed baseline (direction + tolerance — the `lane_baseline`
/// pattern). Exit 1 on regression so CI can gate. `--update-baseline` captures
/// the current SLIs as the new baseline (establish-then-ratchet).
async fn cmd_soak_gate(args: &[String]) -> i32 {
    use crate::mesh_soak::{gate_slis, soak_slis};

    let mut findings: Option<String> = None;
    let mut baseline_path: Option<String> = None;
    let mut update = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--baseline" => {
                i += 1;
                baseline_path = args.get(i).cloned();
            }
            "--update-baseline" => update = true,
            "--help" | "-h" => {
                eprintln!("Usage: svrn mesh soak-gate <findings.jsonl> [--baseline <file>] [--update-baseline]");
                eprintln!();
                eprintln!("  Distils mesh-soak-findings.jsonl into SLIs (invariant violation");
                eprintln!("  rate, load success rate, load p50/p99) and gates each against a");
                eprintln!("  committed baseline. Exit 1 on regression past tolerance.");
                eprintln!("  --update-baseline writes the current SLIs as the new baseline.");
                return 0;
            }
            s if findings.is_none() && !s.starts_with('-') => findings = Some(s.to_string()),
            other => {
                eprintln!("Unknown arg: {other}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(findings) = findings else {
        eprintln!("findings.jsonl path required");
        return 2;
    };
    let text = match std::fs::read_to_string(&findings) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("read {findings}: {e}");
            return 1;
        }
    };
    let lines: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let current = soak_slis(&lines);

    if update {
        let Some(bp) = &baseline_path else {
            eprintln!("--update-baseline requires --baseline <file>");
            return 2;
        };
        return match serde_json::to_string_pretty(&current)
            .map_err(|e| e.to_string())
            .and_then(|s| std::fs::write(bp, s).map_err(|e| e.to_string()))
        {
            Ok(()) => {
                println!("✓ wrote mesh-soak baseline → {bp}");
                0
            }
            Err(e) => {
                eprintln!("write baseline {bp}: {e}");
                1
            }
        };
    }

    let baseline: Option<std::collections::BTreeMap<String, f64>> = baseline_path
        .as_ref()
        .and_then(|bp| std::fs::read_to_string(bp).ok())
        .and_then(|s| serde_json::from_str(&s).ok());
    let (rows, first_run) = gate_slis(&current, baseline.as_ref());

    eprintln!("── mesh-soak SLO gate (baseline-relative) ──");
    for r in &rows {
        let base = r
            .baseline
            .map(|b| format!("{b:.4}"))
            .unwrap_or_else(|| "—".into());
        let status = if r.regressed { "REGRESSED" } else { "ok" };
        eprintln!(
            "  {:<28} base={:>10} cur={:>10.4}  {status}",
            r.name, base, r.current
        );
    }
    if first_run {
        eprintln!("  no baseline yet — first run. Capture one with --update-baseline.");
        return 0;
    }
    let n = rows.iter().filter(|r| r.regressed).count();
    if n == 0 {
        eprintln!("  VERDICT: PASS ✓ — no SLI regressed past tolerance.");
        0
    } else {
        eprintln!("  VERDICT: FAIL ✗ — {n} SLI(s) regressed vs baseline.");
        1
    }
}

/// Run a corpus subcommand. Returns the exit code.
const HELP_MESH: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn mesh",
    summary: "Manage the local Commonwealth mesh (create / join / rotate / status).",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn mesh <subcommand> [args]"),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            (
                "create",
                "Promote the solo mesh to a joinable mesh; print invite",
            ),
            (
                "join <arg>",
                "Join an existing mesh (bare key, https url, or sovereign://)",
            ),
            (
                "rotate",
                "Generate a new shareable join key (invalidates the previous)",
            ),
            (
                "grant --model <id>",
                "Lend named models to a NON-member for a bounded window; prints a guest link",
            ),
            (
                "use <link>",
                "Accept a guest link — `svrn chat` then routes to the issuing node",
            ),
            (
                "status",
                "Show mesh members, hosted knowledge, loaded models",
            ),
            (
                "transport",
                "Show each peer's live iroh path (direct / relayed / mixed)",
            ),
            ("balance", "Show your contribution to the mesh"),
            ("list", "Show every mesh this node has joined; the active one is marked"),
            ("switch <mesh>", "Park the active mesh and bring another one up"),
            ("forget <mesh>", "Drop a parked mesh from this node"),
            (
                "forget-member <node>",
                "Retire one member row — the repair for an endpoint-key collision",
            ),
            ("leave", "Leave the current mesh"),
            ("logs", "Show mesh daemon logs"),
            (
                "fetch-model <name>",
                "Pull a GGUF from a mesh peer over the tailnet (no R2 credentials required)",
            ),
            (
                "warm-cache <gguf>",
                "Pre-seed the RPC tensor cache from a local GGUF (offline; later serves with zero weight transfer)",
            ),
            (
                "plan <gguf> --devices <gb,..>",
                "Dry-run the tensor split across a mesh — per-device fit + headroom, offline (no load)",
            ),
            (
                "bench",
                "Measure how fast the model you are running actually decodes, and record it for `plan`",
            ),
            (
                "check-invariants --nodes <a,b,..>",
                "Poll /v1/mesh/status across nodes and assert convergence/no-ghost/liveness (soak harness)",
            ),
            (
                "soak-gate <findings.jsonl>",
                "Gate mesh-soak SLIs (violation rate, load latency) against a committed baseline",
            ),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Run `svrn mesh <subcommand> --help` for subcommand-specific flags.",
        ),
    ],
};

/// One machine on the live mesh, as `mesh plan --from-mesh` sees it.
///
/// Carries identity alongside capacity because a measured throughput number is
/// only meaningful for the machines it was measured on: `name` distinguishes
/// one peer's share from another's in the placement digest, and
/// `hw_fingerprint` pins the hardware. Both are absent for a peer on an older
/// daemon, which `mesh plan` reports as "not measured" rather than guessing.
pub(crate) struct MeshDevice {
    pub(crate) name: String,
    /// What this machine's GPU could hold if nothing else were resident — the
    /// gossiped device TOTAL. A durable hardware fact, and the basis for "is
    /// this configuration even viable".
    pub(crate) vram_gb: f64,
    /// What is FREE on it right now, as the loader's own oracle reports it
    /// (`/v1/mesh/status.device_memory`, ultimately `ggml_backend_dev_memory`).
    ///
    /// `None` when no live reading exists for this peer — an older daemon, or a
    /// member with no discovered RPC worker. It is deliberately a separate field
    /// rather than a correction to `vram_gb`: the difference between them is
    /// "held by other work right now", which is the entire distinction between
    /// "this node is too small" and "this node is busy". Those have opposite
    /// repairs, so the plan reports both and names the gap instead of picking.
    pub(crate) free_vram_gb: Option<f64>,
    pub(crate) hw_fingerprint: Option<u64>,
    pub(crate) backend: Option<String>,
    /// How this machine's rpc-server would be reached, when discovery has
    /// found one for it.
    ///
    /// `None` means no worker is currently discovered for this peer — the plan
    /// then cannot say how the tensor stream would travel, which is a reason to
    /// report "not measured" rather than to assume the good case. See
    /// [`sovereign_core::mesh_measurements::LinkClass`].
    pub(crate) link: Option<sovereign_core::mesh_measurements::LinkClass>,
}

/// Read the live mesh from the running daemon's `/v1/mesh/status` and build the
/// per-device vector for `mesh plan --from-mesh`: online anchor workers first,
/// this host (`is_self`) last so the output head lands on it. Returns
/// `(devices, host index)`. Prints the resolved mesh to stderr (so `--json`
/// stays clean on stdout).
async fn devices_from_live_mesh() -> Result<(Vec<MeshDevice>, usize, Option<String>), String> {
    let port = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.daemon.client_port)
        .unwrap_or(9741);
    let url = format!("http://127.0.0.1:{port}/v1/mesh/status");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let resp = client.get(&url).send().await.map_err(|e| {
        format!("daemon at {url} not reachable: {e}\n  hint: start it (`svrn daemon start`) or pass --devices manually")
    })?;
    if !resp.status().is_success() {
        return Err(format!("daemon returned HTTP {} from {url}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("bad status JSON: {e}"))?;
    let members = body
        .get("members")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();

    // node_id → the endpoint ggml would dial for that peer's rpc-server.
    //
    // This is the consumer half of the link agreement: `mesh bench` classifies
    // the endpoints in the daemon's live *placement*, and this side classifies
    // the endpoints in the daemon's live *discovery*. They are the same strings
    // from the same daemon, run through the same `link_class_of_endpoint`, so
    // the plan asks about the link the bench would measure. A peer absent from
    // this list has no discovered worker; it stays `None` and the plan reports
    // "not measured" rather than assuming a direct link it has not seen.
    let worker_endpoints: std::collections::HashMap<String, String> = body
        .get("rpc_workers")
        .and_then(|w| w.as_array())
        .map(|ws| {
            ws.iter()
                .filter_map(|w| {
                    let id = w.get("node_id")?.as_str()?;
                    let ep = w.get("endpoint")?.as_str()?;
                    (!id.is_empty() && !ep.is_empty()).then(|| (id.to_string(), ep.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    // The LOADER's own per-device memory view, as the daemon publishes it. This
    // is the second half of the two-capacity answer: `members[].vram_gb` above is
    // the gossiped device TOTAL, and this is what is actually free right now —
    // the number the live fit gate judges against. Only the daemon can read it
    // (it holds the registered ggml devices), which is why it travels on the
    // status payload rather than being sampled here.
    //
    // Rows are keyed by RPC endpoint; the entries with NO endpoint are this
    // host's own local GPU device(s), summed to one figure because the plan
    // models the host as a single device (matching `local_gpu_total_vram_gb`).
    let dev_mem = body
        .get("device_memory")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    const MIB_PER_GB: f64 = 1024.0;
    // Lendable, not raw free: the owning node's reserve is already off the
    // capacity its loader plans against, so a preview that used raw `free_mb`
    // would describe a cut the loader would refuse to make.
    let lendable_mb = |d: &serde_json::Value| -> Option<f64> {
        let free = d.get("free_mb")?.as_f64()?;
        let reserve = d.get("reserve_mb").and_then(|v| v.as_f64()).unwrap_or(0.0);
        Some((free - reserve).max(0.0))
    };
    let free_by_endpoint: std::collections::HashMap<String, f64> = dev_mem
        .iter()
        .filter_map(|d| {
            let ep = d.get("endpoint")?.as_str()?;
            Some((ep.to_string(), lendable_mb(d)? / MIB_PER_GB))
        })
        .collect();
    // Age of the reading. It is an observation taken when the loader last planned
    // a cut, not a live sample (sampling would block on a busy worker — see
    // `last_device_memory`), so an operator has to be able to see how old it is.
    if let Some(obs) = body
        .get("device_memory_observed_unix")
        .and_then(|v| v.as_u64())
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        eprintln!(
            "  free-memory reading observed {}s ago, when the loader last planned a cut",
            now.saturating_sub(obs)
        );
    }
    let host_free_gb: Option<f64> = {
        let local: Vec<f64> = dev_mem
            .iter()
            .filter(|d| d.get("endpoint").and_then(|e| e.as_str()).is_none())
            .filter_map(lendable_mb)
            .collect();
        (!local.is_empty()).then(|| local.iter().sum::<f64>() / MIB_PER_GB)
    };

    let mut workers: Vec<MeshDevice> = Vec::new();
    let mut host: Option<MeshDevice> = None;
    for m in &members {
        let is_self = m.get("is_self").and_then(|b| b.as_bool()).unwrap_or(false);
        let endpoint = m
            .get("node_id")
            .and_then(|v| v.as_str())
            .and_then(|id| worker_endpoints.get(id));
        let dev = MeshDevice {
            name: m
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string(),
            vram_gb: m.get("vram_gb").and_then(|v| v.as_f64()).unwrap_or(0.0),
            free_vram_gb: if is_self {
                host_free_gb
            } else {
                endpoint.and_then(|ep| free_by_endpoint.get(ep).copied())
            },
            hw_fingerprint: m.get("hw_fingerprint").and_then(|v| v.as_u64()),
            backend: m
                .get("backend")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            link: endpoint.map(|ep| sovereign_core::mesh_measurements::link_class_of_endpoint(ep)),
        };
        let online = m.get("status").and_then(|s| s.as_str()) == Some("online");
        let can_anchor = m
            .get("can_anchor")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        if is_self {
            host = Some(dev);
        } else if online && can_anchor {
            workers.push(dev);
        }
    }
    let host =
        host.ok_or_else(|| "could not find this node (is_self) in the mesh status".to_string())?;

    // Glassbox both capacities per device, so the operator can see WHERE the two
    // bases disagree before reading a verdict built on either.
    eprintln!(
        "Resolved live mesh: {} online anchor worker(s) + this host",
        workers.len()
    );
    let show = |role: &str, d: &MeshDevice, suffix: &str| {
        match d.free_vram_gb {
        Some(free) => eprintln!(
            "  {role}  {}: {:.0} GB total · {:.1} GB free now ({:.1} GB held by other work){suffix}",
            d.name,
            d.vram_gb,
            free,
            (d.vram_gb - free).max(0.0)
        ),
        None => eprintln!(
            "  {role}  {}: {:.0} GB total · free now UNKNOWN (no live reading){suffix}",
            d.name, d.vram_gb
        ),
    }
    };
    for w in &workers {
        show("worker", w, "");
    }
    show("host  ", &host, "  (holds the output head)");
    if workers.is_empty() {
        eprintln!(
            "  note: no online anchor workers — the plan will show a single-node (local) load."
        );
    }
    eprintln!();

    let mut devices = workers;
    devices.push(host);
    let host_idx = devices.len() - 1;
    // The operator's block-split pin, if the daemon reports one. Not a capacity —
    // it overrides capacity entirely — so it travels beside the devices, not in
    // them.
    let pin = body
        .get("rpc_block_split_pin")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Ok((devices, host_idx, pin))
}

/// `svrn mesh plan` — dry-run a model's tensor split across a mesh, offline. Reuses
/// the daemon's own `plan_shards` + `quantize_vram` and the SAME `shard_fits`
/// decider its per-device gate runs (see `first_worker_overflow`), so the preview
/// and the load it previews reach the same verdict — with operator-set
/// `--headroom` for what-if planning instead of the configured factor.
///
/// Under `--from-mesh` it reports TWO capacity bases, never one:
/// [`Possible`](CapacityBasis::Possible) from device totals and
/// [`SafeNow`](CapacityBasis::SafeNow) from the loader's live free-memory reading.
/// They routinely disagree, and which one you need depends on the question —
/// "could this mesh ever run this model" versus "would a load right now succeed".
/// Picking one on the operator's behalf hid a real cut mismatch for weeks.
async fn cmd_plan(args: &[String]) -> i32 {
    use sovereign_inference::embedded as inf;
    // Kept as the raw spec, not a PathBuf: it may be `hf:<owner>/<repo>/<variant>`,
    // which `remote_gguf::resolve` turns into header-only stand-ins below.
    let mut model_spec: Option<String> = None;
    let mut devices_gb: Vec<f64> = Vec::new();
    let mut host_idx: Option<usize> = None;
    // Default headroom mirrors the daemon's OWN resolution order exactly, so a
    // previewed plan uses the SAME factor the load executes with: an explicit
    // `SOVEREIGN_RPC_HEADROOM` env wins (the daemon reads it directly), else the
    // `[shared_model] headroom` config (bootstrap bridges config→env), else 1.2.
    // `--headroom` overrides this for what-if planning.
    // ONE parser, ONE default (§10.6). The env read, the `>= 1.0` filter and
    // the literal `1.2` used to exist here AND in
    // `sovereign_inference::embedded::rpc_headroom_factor` — the function that
    // actually gates the load. They agreed, which is the weakest way for a
    // promise to hold: this command's whole contract is that "a previewed plan
    // uses the headroom the load executes with", and it was kept by two copies
    // of a number matching.
    //
    // The config fallback stays here, and is why the shared helper returns an
    // `Option`: this CLI can run BEFORE bootstrap has bridged
    // `[shared_model] headroom` into the environment, so it has a second
    // source the daemon does not. The filter applies to that source too — a
    // headroom below 1.0 gates on less memory than the model needs.
    let mut headroom: f64 = sovereign_inference::embedded::rpc_headroom_from_env()
        .or_else(|| {
            sovereign_core::setup_config::SetupConfig::load()
                .ok()
                .and_then(|c| c.shared_model.headroom)
                .filter(|&h| h >= 1.0)
        })
        .unwrap_or(sovereign_inference::embedded::RPC_HEADROOM_DEFAULT);
    let mut headroom_from_flag = false;
    let mut json = false;
    let mut from_mesh = false;
    // `Some` only under `--from-mesh`. A `--devices` plan describes hardware
    // that is not here, so it has no identity and can never match a
    // measurement — see `SpeedSection::NotMeasurable`.
    let mut mesh_devices: Option<Vec<MeshDevice>> = None;
    // `Some` only under `--from-mesh`, and only when the daemon reported a live
    // reading for EVERY device. `--devices` describes hardware that is not here,
    // so nothing can be free on it.
    let mut devices_free_gb: Option<Vec<f64>> = None;
    // The daemon's `SOVEREIGN_RPC_BLOCK_SPLIT`, when it reports one. Only a live
    // daemon can have a pin; `--devices` previews nothing that would honour it.
    let mut block_split_pin: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--devices" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!(
                        "--devices needs a value (per-node usable VRAM in GB, e.g. 64,32,32)"
                    );
                    return 2;
                };
                match v
                    .split(',')
                    .map(|s| s.trim().parse::<f64>())
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(d) => devices_gb = d,
                    Err(_) => {
                        eprintln!("--devices: comma-separated GB numbers, e.g. 64,32,32");
                        return 2;
                    }
                }
            }
            "--host" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(h) => host_idx = Some(h),
                    None => {
                        eprintln!("--host: a 0-based device index");
                        return 2;
                    }
                }
            }
            "--headroom" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<f64>().ok()) {
                    Some(h) if h >= 1.0 => {
                        headroom = h;
                        headroom_from_flag = true;
                    }
                    _ => {
                        eprintln!("--headroom: a number >= 1.0 (1.15 aggressive · 1.2 default · 1.4 safe)");
                        return 2;
                    }
                }
            }
            "--json" => json = true,
            "--from-mesh" => from_mesh = true,
            "--help" | "-h" => {
                sovereign_cli_shared::help::print(&HELP_MESH_PLAN);
                return 0;
            }
            s if model_spec.is_none() && !s.starts_with('-') => model_spec = Some(s.to_string()),
            other => {
                eprintln!("Unknown arg: {other}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(model_spec) = model_spec else {
        sovereign_cli_shared::help::print(&HELP_MESH_PLAN);
        return 2;
    };
    if from_mesh {
        if !devices_gb.is_empty() {
            eprintln!("--from-mesh and --devices are mutually exclusive");
            return 2;
        }
        match devices_from_live_mesh().await {
            Ok((devs, h, pin)) => {
                devices_gb = devs.iter().map(|d| d.vram_gb).collect();
                host_idx = Some(h);
                block_split_pin = pin;
                // All-or-nothing: one device missing a live reading means there
                // is no coherent "safe now" basis to plan on, and the report says
                // so rather than mixing bases. See `PlanInput::devices_free_gb`.
                devices_free_gb = devs.iter().map(|d| d.free_vram_gb).collect();
                if devices_free_gb.is_none() {
                    let blind: Vec<&str> = devs
                        .iter()
                        .filter(|d| d.free_vram_gb.is_none())
                        .map(|d| d.name.as_str())
                        .collect();
                    eprintln!(
                        "  note: no live free-memory reading for {} — the plan can show what is \n        POSSIBLE (device totals) but not what is SAFE RIGHT NOW. An older peer \n        daemon, or a member with no discovered RPC worker.\n",
                        blind.join(", ")
                    );
                }
                // Retained for the speed lookup: only a real, present mesh can
                // identify the machines a measurement would belong to.
                mesh_devices = Some(devs);
            }
            Err(e) => {
                eprintln!("--from-mesh: {e}");
                return 1;
            }
        }
    }
    if devices_gb.is_empty() {
        eprintln!("provide the mesh: --devices <gb,gb,..> (per-node VRAM) or --from-mesh (read the live mesh)");
        return 2;
    }
    if devices_gb.iter().any(|&g| g <= 0.0) {
        eprintln!(
            "device VRAM must be > 0 (a member may advertise 0 GB — pass --devices manually)"
        );
        return 2;
    }

    // Resolve the spec to something with a readable header. A filesystem path
    // passes through untouched; `hf:…` is fetched header-only so an operator
    // can size a 155 GB model before spending the download on it.
    let resolved = match crate::remote_gguf::resolve(&model_spec).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let model = resolved.path.clone();

    // Say what was actually read. A split model whose siblings are missing
    // resolves to shard 1 alone and would otherwise plan a ~Nx-too-small
    // model in confident silence — the one way this command can lie.
    if !resolved.headers_only {
        let named_split = model
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(sovereign_inference::embedded::split_shard_names)
            .map(|v| v.len());
        match named_split {
            Some(want) if resolved.shards < want => {
                eprintln!(
                    "  WARNING: {} names a {want}-shard split but only {} shard(s) are on disk.\n           Planning the shard(s) present — this UNDERSTATES the model by ~{:.1}x.\n           Fetch the missing siblings before trusting this verdict.\n",
                    model.display(),
                    resolved.shards,
                    want as f64 / resolved.shards.max(1) as f64
                );
            }
            Some(want) => eprintln!(
                "  model: {want} shards, {:.1} GB\n",
                resolved.total_bytes as f64 / 1e9
            ),
            None => {}
        }
    } else {
        // Name the basis. A header-only plan has exact tensor mass — the byte
        // counts come from dims + ggml type, not from any file length — but it
        // has not fetched a single weight, so it can say the model WOULD fit
        // and cannot say the download will succeed.
        eprintln!(
            "  basis: GGUF headers only, {} — tensor mass exact, no weights fetched\n",
            resolved.label
        );
    }

    let n_layer = match inf::gguf_block_count(&model) {
        Ok(Some(n)) if n > 0 => n,
        Ok(_) => {
            eprintln!(
                "could not read a positive block_count from {} (not a GGUF, or missing <arch>.block_count)",
                model.display()
            );
            return 1;
        }
        Err(e) => {
            eprintln!("reading {}: {e}", model.display());
            return 1;
        }
    };
    let sizes = match inf::tensor_sizes(&model) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("reading tensor table from {}: {e}", model.display());
            return 1;
        }
    };

    let host = host_idx.unwrap_or(devices_gb.len() - 1);
    if host >= devices_gb.len() {
        eprintln!(
            "--host {host} out of range (valid 0..{})",
            devices_gb.len() - 1
        );
        return 2;
    }

    // The context length the plan assumes. Same accessor the cold-start and
    // reload paths use, so a plan and the load it previews cannot disagree
    // about KV size — which is part of the measurement key.
    let n_ctx = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.effective_context_size())
        .unwrap_or(16384);

    // llama.cpp's three-term projection (KV + compute per device) — the SAME
    // numbers the live fit gate judges with, so the preview's fit column and a
    // load-time refusal can never disagree about the non-weight terms. The
    // backend guard must stay alive past the projection call (its Drop frees
    // the backend); a failed init or projection degrades the preview to
    // weights-only fit, the same fallback the live gate takes. Measured
    // ~278 ms warm on a 155 GB set (tests/device_memory_probe.rs).
    let _backend = sovereign_inference::llama::cpp::llama_backend::LlamaBackend::init();
    let overheads = sovereign_inference::embedded::projected_overheads(&model, n_ctx, false);

    // What peers have measured. A key pins the exact silicon *and* the exact
    // split, so an operator asking about a configuration they have never run will
    // essentially never get a local hit — which makes the peer half the part most
    // likely to answer the question they actually asked. Degrades to empty with
    // no daemon; never fatal.
    let peers = crate::mesh_travel::peer_history().await;
    if let Some(note) = &peers.note {
        eprintln!("mesh plan: peer measurements unavailable — {note}");
    }
    // On stderr rather than in the plan: it is a fact about the mesh, not about
    // this model. But it does have to be said — otherwise a peer running an
    // incompatible schema is indistinguishable from a peer who has measured
    // nothing, and the operator would go looking for the wrong problem.
    if peers.unreadable > 0 {
        eprintln!(
            "mesh plan: {} peer measurement(s) could not be read — a peer is probably on an \
             incompatible schema; upgrade it or ignore this",
            peers.unreadable
        );
    }

    let report = build_report(
        PlanInput {
            model_name: model
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string(),
            n_layer,
            sizes,
            devices_gb,
            devices_free_gb,
            block_split_pin,
            host,
            headroom,
            headroom_from_flag,
            mesh: mesh_devices,
            n_ctx,
            overheads,
        },
        &sovereign_core::mesh_measurements::load(),
        &peers.records,
        env!("CARGO_PKG_VERSION"),
    );

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&render_json(&report)).unwrap_or_default()
        );
    } else {
        print!("{}", render_human(&report));
    }
    report.exit_code()
}

// ---------------------------------------------------------------------------
// `mesh plan`, as a pure function of its inputs
//
// Everything below is deliberately free of I/O, so the report can be built and
// rendered in a test without a GGUF on disk, a daemon, or a GPU. `cmd_plan`
// above is the shell: it parses args, reads the header table, talks to the
// mesh, and hands the result here.
//
// The split exists because this command shipped for months with no tests at
// all — there was no seam to test at. Keep the seam: computation belongs in
// `build_report`, wording belongs in the renderers, and neither should acquire
// a file read or a network call.
// ---------------------------------------------------------------------------

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

fn gb(bytes: u64) -> f64 {
    bytes as f64 / GIB
}

/// Everything `build_report` needs, already read from disk and validated.
pub(crate) struct PlanInput {
    /// Display name of the model file.
    pub(crate) model_name: String,
    /// Transformer block count from the GGUF header.
    pub(crate) n_layer: u32,
    /// `(tensor_name, layer, nbytes)` from the GGUF tensor table.
    pub(crate) sizes: Vec<(String, Option<u32>, u64)>,
    /// Per-device usable VRAM in GB, in caller order. The
    /// [`Possible`](CapacityBasis::Possible) basis: device totals.
    pub(crate) devices_gb: Vec<f64>,
    /// Per-device LIVE FREE memory in GB, in the same order as `devices_gb` —
    /// the [`SafeNow`](CapacityBasis::SafeNow) basis.
    ///
    /// `Some` only when a live reading exists for EVERY device. That
    /// all-or-nothing rule is deliberate: a cut apportioned from a mix of
    /// live-free and device-total readings is neither basis, and would produce a
    /// third split matching nothing the loader would execute — the exact class of
    /// silent disagreement this field was added to end. Partial knowledge is
    /// reported as "unknown", not averaged into a plausible-looking number.
    pub(crate) devices_free_gb: Option<Vec<f64>>,
    /// The daemon's `SOVEREIGN_RPC_BLOCK_SPLIT` pin, verbatim, when one is set.
    ///
    /// A pin makes both capacity bases irrelevant as predictions: the loader
    /// obeys the pin and ignores VRAM. Carried as the raw string so this side
    /// validates it with the SAME `parse_block_split` the loader runs — a pin the
    /// loader would reject must be rejected here too, or the plan would confidently
    /// preview a split nothing honours.
    pub(crate) block_split_pin: Option<String>,
    /// Index into `devices_gb` of the host — the node that holds the output
    /// head. Validated in range by the caller.
    pub(crate) host: usize,
    /// Headroom multiplier applied to each device's share.
    pub(crate) headroom: f64,
    /// Whether `headroom` came from `--headroom` (a what-if) rather than the
    /// configuration the live load will actually use.
    pub(crate) headroom_from_flag: bool,
    /// Live mesh identities, in the same order as `devices_gb`. `Some` only
    /// under `--from-mesh`; a `--devices` plan describes hardware that is not
    /// here and therefore has no measurement to find.
    pub(crate) mesh: Option<Vec<MeshDevice>>,
    /// Context length the plan assumes. Part of the measurement key, because
    /// decode rate is a function of KV size.
    pub(crate) n_ctx: u32,
    /// llama.cpp's projected non-weight terms (KV + compute per device) for
    /// this model at `n_ctx` — the SAME projection the live fit gate judges
    /// with (`projected_overheads`), so the preview's fit column and the
    /// load's refusal carry identical numbers. `None` when the projection was
    /// unavailable (no backend in this process, or it failed); the fit is then
    /// weights-only, exactly like the live gate's own fallback.
    pub(crate) overheads: Option<sovereign_inference::embedded::PlanOverheads>,
}

/// What `mesh plan` can honestly say about speed.
pub(crate) enum SpeedSection {
    /// A real run against exactly this configuration.
    Measured {
        summary: Box<sovereign_core::mesh_measurements::MeasurementSummary>,
    },
    /// This configuration could be measured; nobody has. `near` names
    /// measurements of the same model in *other* configurations — as context
    /// for the operator, never as a number for this one.
    NotMeasured {
        near: Vec<sovereign_core::mesh_measurements::NearMiss>,
    },
    /// There is nothing here to have measured.
    NotMeasurable(NotMeasurable),
}

/// Why a configuration can carry no measurement at all.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum NotMeasurable {
    /// `--devices` describes hardware that is not present.
    HypotheticalDevices,
    /// The host advertises no hardware fingerprint (an older daemon), so there
    /// is no key under which a measurement could have been filed.
    HostUnidentified,
    /// A *peer* carrying part of the model advertises no hardware fingerprint,
    /// so the placement cannot say which silicon held that share.
    ///
    /// Distinct from [`HostUnidentified`](NotMeasurable::HostUnidentified)
    /// because the repair is on a different machine, and the operator needs to
    /// be told which one. Falling back to a name-only key instead would let a
    /// peer swap its GPU and keep answering with the old number.
    PeerUnidentified {
        /// The mesh member to go upgrade.
        name: String,
    },
}

/// One device's row in the plan.
pub(crate) struct DeviceRow {
    /// Index into the caller's device list — the number the operator typed in
    /// `--devices`, and the one displayed. **Not** the same as
    /// `fit.device_index`, which is in plan order (workers first, host last).
    /// Confusing the two silently attributes every row to the wrong machine,
    /// and nothing downstream can catch it.
    pub(crate) dev: usize,
    pub(crate) is_host: bool,
    pub(crate) blocks: Option<(u32, u32)>,
    pub(crate) holds_output: bool,
    /// This device's share weighed against its memory.
    ///
    /// Comes from `shard_fits` — the SAME decider the live load's per-device
    /// gate runs. That is the whole point: this command exists to preview a
    /// load, and a preview computing its own answer is a preview that can
    /// disagree with the thing it previews. It did, until 2026-07-28.
    pub(crate) fit: sovereign_inference::embedded::ShardFit,
}

impl DeviceRow {
    /// What this device has.
    pub(crate) fn vram(&self) -> u64 {
        self.fit.capacity_bytes
    }
    /// Bytes of weights this device holds.
    pub(crate) fn weight(&self) -> u64 {
        self.fit.held_bytes
    }
    /// `weight × headroom` — what must fit.
    pub(crate) fn need(&self) -> u64 {
        self.fit.need_bytes
    }
    /// Whether this device can hold its share.
    pub(crate) fn fits(&self) -> bool {
        self.fit.fits()
    }
}

/// Spread of per-block byte mass — the "is heterogeneous VRAM safe here" signal.
pub(crate) struct BlockMass {
    pub(crate) min: u64,
    pub(crate) max: u64,
    pub(crate) mean: u64,
    pub(crate) spread: f64,
    pub(crate) uniform: bool,
}

/// Hot/cold split for a mixture-of-experts model.
pub(crate) struct MoeReport {
    /// Routed-expert bytes — cold, only the router's top-k are read per token.
    pub(crate) routed_expert_bytes: u64,
    /// Resident mass touched on every token.
    pub(crate) hot_bytes: u64,
}

/// Which answer to "how much memory does each device have" a layout was built
/// from. The two are not interchangeable and the report never merges them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CapacityBasis {
    /// Device TOTAL — what the silicon could hold if nothing else were resident.
    /// Answers "is this configuration viable at all", which is a property of the
    /// hardware and does not change because something is loaded right now.
    Possible,
    /// Live FREE — what is available at this instant, and therefore what the
    /// running loader's fit gate judges against. Answers "would a load started
    /// right now succeed, and with which cut".
    SafeNow,
    /// Not derived from capacity at all: the operator pinned the per-device block
    /// counts with `SOVEREIGN_RPC_BLOCK_SPLIT`, and the loader honours the pin
    /// over any VRAM apportionment.
    ///
    /// When a pin is active it OUTRANKS both other bases as a description of what
    /// will load, because it is the only one the loader will actually obey.
    Pinned,
}

impl CapacityBasis {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Possible => "possible (device total)",
            Self::SafeNow => "safe now (live free)",
            Self::Pinned => "pinned (SOVEREIGN_RPC_BLOCK_SPLIT)",
        }
    }
}

/// One capacity basis laid across the devices: the cut that follows from ONE
/// answer to how much room each device has, plus the verdicts on it.
///
/// Exists because the two bases genuinely produce DIFFERENT cuts, and the
/// difference was invisible before 2026-07-30. On the mesh that produced the
/// first valid two-node 122B record, the totals basis apportioned 14/34 while
/// the loader — reading live free — ran 12/36. The plan therefore looked up a
/// measurement key that no run could ever file under, and reported "not
/// measured" about a configuration it had a real 10.48 tok/s number for.
pub(crate) struct Allocation {
    pub(crate) basis: CapacityBasis,
    /// Per-device rows, sorted by the operator-facing device index. Each row's
    /// `vram()` IS the capacity this basis fed in, so the basis needs no second
    /// copy of it.
    pub(crate) rows: Vec<DeviceRow>,
    pub(crate) pooled: u64,
    pub(crate) gate_pass: bool,
    pub(crate) nodes: NodesReport,
}

impl Allocation {
    /// Devices whose share does not fit their own memory on this basis.
    pub(crate) fn overflows(&self) -> Vec<&DeviceRow> {
        self.rows.iter().filter(|r| !r.fits()).collect()
    }

    /// Whether this basis clears both gates.
    pub(crate) fn fits(&self) -> bool {
        self.gate_pass && self.overflows().is_empty()
    }
}

/// Node count and the per-token hop cost that follows from it.
pub(crate) struct NodesReport {
    pub(crate) active_nodes: usize,
    pub(crate) hops_now: usize,
    pub(crate) min_nodes: usize,
    pub(crate) hops_min: usize,
}

/// The finished plan, ready to render.
pub(crate) struct PlanReport {
    pub(crate) model_name: String,
    pub(crate) n_layer: u32,
    pub(crate) total_weight: u64,
    pub(crate) output_bytes: u64,
    pub(crate) embd_bytes: u64,
    pub(crate) block_mass: BlockMass,
    pub(crate) moe: Option<MoeReport>,
    pub(crate) headroom: f64,
    pub(crate) headroom_from_flag: bool,
    pub(crate) pooled: u64,
    pub(crate) gate_need: u64,
    pub(crate) gate_pass: bool,
    pub(crate) rows: Vec<DeviceRow>,
    pub(crate) nodes: NodesReport,
    /// The same layout recomputed against LIVE FREE memory — the cut a load
    /// started right now would actually run, and the basis its fit gate judges.
    ///
    /// `None` when no live reading is available for every device: a `--devices`
    /// what-if (hardware that is not here), a daemon predating
    /// `/v1/mesh/status.device_memory`, or a peer with no discovered RPC worker.
    ///
    /// The fields above (`rows`, `pooled`, `gate_pass`, `nodes`) remain the
    /// [`Possible`](CapacityBasis::Possible) basis whether or not this is
    /// present, so their meaning never depends on what happens to be loaded.
    pub(crate) safe_now: Option<Allocation>,
    /// The cut the operator PINNED, when `SOVEREIGN_RPC_BLOCK_SPLIT` is set and
    /// valid for this model and device count.
    ///
    /// `Some` here means neither capacity basis predicts what will load, and this
    /// one does. Built with the same `plan_shards_explicit` the loader calls, so
    /// the block ranges are identical rather than merely similar.
    pub(crate) pinned: Option<Allocation>,
    /// The raw pin as the daemon reported it, even when it could NOT be applied
    /// (wrong device count, counts not summing to the block count). A pin the
    /// loader will reject is worth naming: the operator set it expecting it to
    /// take effect, and silence would let them believe it had.
    pub(crate) block_split_pin: Option<String>,
    /// What we can honestly say about how fast this configuration runs.
    ///
    /// Resolved against the cut that would ACTUALLY run — `safe_now` when we have
    /// it, else the possible basis. A measurement is filed under the placement
    /// that produced it, so looking it up under a cut the loader would not make
    /// is a guaranteed miss.
    pub(crate) speed: SpeedSection,
    /// The measurement key this plan looked up, when it had one. Emitted in
    /// `--json` so a script can correlate a plan with a `mesh bench` record.
    pub(crate) speed_key: Option<sovereign_core::mesh_measurements::MeasurementKey>,
}

impl PlanReport {
    /// Devices whose share does not fit their own memory.
    pub(crate) fn overflows(&self) -> Vec<&DeviceRow> {
        self.rows.iter().filter(|r| !r.fits()).collect()
    }

    /// `0` when the model fits both gates, `1` when it does not.
    ///
    /// Speed never participates: a plan that fits but would run slowly is still
    /// a plan that fits, and picking a tokens-per-second threshold on someone
    /// else's behalf is not this command's job.
    pub(crate) fn exit_code(&self) -> i32 {
        if self.gate_pass && self.overflows().is_empty() {
            0
        } else {
            1
        }
    }
}

/// Lay a model's blocks across a set of devices and judge the fit.
///
/// Pure. Uses the same `plan_shards_weighted` the live load uses, over the same
/// device order (workers first, host last), so the preview and the load cannot
/// disagree about where a block lands.
pub(crate) fn build_report(
    input: PlanInput,
    measurements: &sovereign_core::mesh_measurements::MeasurementFile,
    peers: &[sovereign_core::mesh_measurements::ForeignRecord],
    current_build: &str,
) -> PlanReport {
    use sovereign_inference::embedded as inf;

    let PlanInput {
        model_name,
        n_layer,
        sizes,
        devices_gb,
        devices_free_gb,
        block_split_pin,
        host,
        headroom,
        headroom_from_flag,
        mesh,
        n_ctx,
        overheads,
    } = input;

    // Per-block byte mass + global tensors (output head → last block-holder;
    // token_embd → host system RAM; other globals lumped as host overhead).
    // Routed-expert (`_exps`) mass is the COLD part of an MoE model — only the
    // router's top-k experts are read per token, so it can be ~90% of the bytes
    // yet a small fraction of the per-token work. `model_mass_from_sizes` is the
    // same decomposition the live load's planner uses.
    let mass = inf::model_mass_from_sizes(&sizes, n_layer);
    let total_weight: u64 = mass.total_bytes();

    let gate_need = (total_weight as f64 * headroom) as u64;

    // Lay the model across one capacity basis and judge it.
    //
    // Factored out so BOTH bases go through the identical arithmetic. If
    // "possible" and "safe now" were computed by two code paths, a divergence
    // between them would be indistinguishable from a divergence in the
    // capacities — which is the confusion this whole two-basis report exists to
    // remove.
    // `counts`: `Some` pins the per-device block counts (plan order) instead of
    // apportioning by capacity — the loader's `SOVEREIGN_RPC_BLOCK_SPLIT` path.
    // The capacities still matter even then, because the FIT verdict is about
    // whether the pinned share fits the memory available.
    let allocate =
        |basis: CapacityBasis, devices_gb: &[f64], counts: Option<&[u32]>| -> Allocation {
            let vram: Vec<u64> = devices_gb.iter().map(|&g| (g * GIB) as u64).collect();

            // Mirror the daemon's device order (RPC workers first, host/local GPU last) so
            // plan_shards places the output head on the host — the SAME functions the live
            // load uses, so the dry run matches reality.
            let mut order: Vec<usize> = (0..vram.len()).filter(|&d| d != host).collect();
            order.push(host);
            let weights: Vec<f32> = order
                .iter()
                .map(|&d| inf::quantize_vram(vram[d]) as f32)
                .collect();
            // Byte-mass-aware split — apportion each device a contiguous block range whose
            // BYTES (not count) are proportional to its VRAM, folding the output head onto
            // the host. The IDENTICAL call the live load makes, so the preview matches it.
            //
            // Under a pin we call `plan_shards_explicit` instead — again the identical
            // call, so the pinned ranges here ARE the ranges the loader computes, not a
            // reconstruction of them. A pin that fails to tile falls back rather than
            // wedging; `pinned` is only built from a pin `parse_block_split` accepted,
            // so this fallback is unreachable in practice and defensive only.
            let plan = counts
                .and_then(|c| inf::plan_shards_explicit(n_layer, &weights, c))
                .unwrap_or_else(|| {
                    inf::plan_shards_weighted(n_layer, &weights, &mass.block_bytes, mass.head_bytes)
                });

            // The per-device verdict, from the decider the live gate runs. Capacities go
            // in PLAN order (`order[pos]`), and the display maps back through `order`
            // below — the two index spaces look interchangeable and are not.
            let capacities: Vec<u64> = order.iter().map(|&d| vram[d]).collect();
            let fits = inf::shard_fits(&plan, &capacities, &mass, headroom, overheads.as_ref());

            let mut rows: Vec<DeviceRow> = Vec::with_capacity(vram.len());
            for (pos, &d) in order.iter().enumerate() {
                let shard = &plan[pos];
                rows.push(DeviceRow {
                    dev: d,
                    is_host: d == host,
                    blocks: shard.blocks,
                    holds_output: shard.holds_output,
                    // `shard_fits` declines to judge when the inputs don't describe each
                    // other. Every such input is validated away before we get here
                    // except one: a GGUF whose tensor table carries no per-layer mass at
                    // all. For that model every device genuinely holds zero block bytes,
                    // so a zero row is the right answer rather than a papered-over gap.
                    fit: fits
                        .as_ref()
                        .and_then(|f| f.get(pos).copied())
                        .unwrap_or(inf::ShardFit {
                            device_index: pos,
                            held_bytes: 0,
                            overhead_bytes: 0,
                            need_bytes: 0,
                            capacity_bytes: capacities[pos],
                        }),
                });
            }
            rows.sort_by_key(|r| r.dev);

            // Aggregate gate (the live daemon's model×1.2, with YOUR headroom).
            let pooled: u64 = vram.iter().sum();

            // Minimum nodes to hold the model: fewest of the LARGEST devices whose pooled
            // VRAM covers model×headroom. Single-stream pipeline decode costs (nodes-1)
            // hops/token, so fewer nodes = fewer hops. Aggregate lower bound — a very
            // skewed model may need one more node for per-device fit.
            let mut vram_desc: Vec<u64> = vram.clone();
            vram_desc.sort_unstable_by(|a, b| b.cmp(a));
            let (mut min_nodes, mut acc) = (0usize, 0u64);
            for v in &vram_desc {
                if acc >= gate_need {
                    break;
                }
                acc += *v;
                min_nodes += 1;
            }
            min_nodes = min_nodes.max(1);
            let active_nodes = rows.iter().filter(|r| r.blocks.is_some()).count().max(1);

            Allocation {
                basis,
                rows,
                pooled,
                gate_pass: pooled >= gate_need,
                nodes: NodesReport {
                    active_nodes,
                    hops_now: active_nodes - 1,
                    min_nodes,
                    hops_min: min_nodes.saturating_sub(1),
                },
            }
        };

    let possible = allocate(CapacityBasis::Possible, &devices_gb, None);
    // Only when a live reading covers EVERY device — see `PlanInput::devices_free_gb`.
    let safe_now = devices_free_gb
        .as_ref()
        .filter(|free| free.len() == devices_gb.len())
        .map(|free| allocate(CapacityBasis::SafeNow, free, None));

    // A pin outranks both derived bases as a prediction, because the loader obeys
    // it and ignores VRAM. Validated with the loader's own parser, so a pin it
    // would reject produces no `pinned` allocation here either — the report then
    // names the pin as INVALID rather than previewing a cut nobody honours.
    //
    // Judged against live free when we have it: the question a pinned split raises
    // is not "how should the blocks be divided" (that is settled) but "does the
    // share the operator pinned still fit the memory available".
    let pin_counts = block_split_pin
        .as_deref()
        .and_then(|raw| inf::parse_block_split(raw, n_layer, devices_gb.len()));
    let pinned = pin_counts.as_ref().map(|counts| {
        let basis_gb = devices_free_gb
            .as_ref()
            .filter(|f| f.len() == devices_gb.len())
            .unwrap_or(&devices_gb);
        allocate(CapacityBasis::Pinned, basis_gb, Some(counts))
    });

    // Block-mass uniformity → the "does heterogeneity stay safe" verdict.
    let nz: Vec<u64> = mass
        .block_bytes
        .iter()
        .copied()
        .filter(|&b| b > 0)
        .collect();
    let bmin = nz.iter().copied().min().unwrap_or(0);
    let bmax = nz.iter().copied().max().unwrap_or(0);
    let bmean = if nz.is_empty() {
        0
    } else {
        nz.iter().sum::<u64>() / nz.len() as u64
    };
    let spread = if bmin > 0 {
        bmax as f64 / bmin as f64
    } else {
        1.0
    };
    let block_mass = BlockMass {
        min: bmin,
        max: bmax,
        mean: bmean,
        spread,
        uniform: spread <= 1.15,
    };

    // Hot = resident mass touched every token: all block bytes minus the cold
    // routed experts, plus the output head (token_embd lives in host RAM).
    let moe = if mass.routed_expert_bytes > 0 {
        Some(MoeReport {
            routed_expert_bytes: mass.routed_expert_bytes,
            hot_bytes: mass
                .block_bytes
                .iter()
                .sum::<u64>()
                .saturating_sub(mass.routed_expert_bytes)
                + mass.head_bytes,
        })
    } else {
        None
    };

    // Speed is looked up against the cut that would ACTUALLY run — `safe_now`
    // when a live reading gave us one, else the possible basis.
    //
    // This is the fix for a silent, total miss. A measurement is filed under the
    // placement that produced it; if the plan predicts a different cut it queries
    // a key nothing will ever be stored at, and reports "not measured" about a
    // configuration it holds a real number for. Observed 2026-07-29 on the very
    // mesh whose 10.48 tok/s two-node record had just been written: totals said
    // 14/34, the loader ran 12/36, so the lookup missed by construction.
    // Precedence: a pin wins (the loader obeys it), else live free, else totals.
    let executed = pinned.as_ref().or(safe_now.as_ref()).unwrap_or(&possible);
    let (speed, speed_key) = resolve_speed(
        &executed.rows,
        mesh.as_deref(),
        &sizes,
        n_layer,
        executed.nodes.active_nodes,
        n_ctx,
        measurements,
        peers,
        current_build,
    );

    let Allocation {
        rows,
        pooled,
        gate_pass,
        nodes,
        ..
    } = possible;

    PlanReport {
        model_name,
        n_layer,
        total_weight,
        output_bytes: mass.head_bytes,
        embd_bytes: mass.embd_bytes,
        block_mass,
        moe,
        headroom,
        headroom_from_flag,
        pooled,
        gate_need,
        gate_pass,
        rows,
        nodes,
        safe_now,
        pinned,
        block_split_pin,
        speed,
        speed_key,
    }
}

/// Decide what this plan may say about speed.
///
/// Pure, and deliberately conservative at every branch. The three outcomes are
/// distinct on purpose: "measured" is a fact, "not measured" is an invitation,
/// and "not measurable" means the question does not apply to what was asked.
/// Collapsing the last two would tell a `--devices` user to run a benchmark
/// that could not produce a record matching their query.
#[allow(clippy::too_many_arguments)]
fn resolve_speed(
    rows: &[DeviceRow],
    mesh: Option<&[MeshDevice]>,
    sizes: &[(String, Option<u32>, u64)],
    n_layer: u32,
    active_nodes: usize,
    n_ctx: u32,
    measurements: &sovereign_core::mesh_measurements::MeasurementFile,
    peers: &[sovereign_core::mesh_measurements::ForeignRecord],
    current_build: &str,
) -> (
    SpeedSection,
    Option<sovereign_core::mesh_measurements::MeasurementKey>,
) {
    use sovereign_core::mesh_measurements as mm;

    // A hypothetical mesh has no machines to have measured.
    let Some(mesh) = mesh else {
        return (
            SpeedSection::NotMeasurable(NotMeasurable::HypotheticalDevices),
            None,
        );
    };

    // The host must be identifiable, or there is no key. Substituting a
    // placeholder would collide every unidentified host into one bucket and
    // serve one machine's number on another.
    let host_fp = rows
        .iter()
        .find(|r| r.is_host)
        .and_then(|r| mesh.get(r.dev))
        .and_then(|d| d.hw_fingerprint);
    let Some(host) = mm::HostIdentity::from_live_mesh(host_fp) else {
        return (
            SpeedSection::NotMeasurable(NotMeasurable::HostUnidentified),
            None,
        );
    };

    // Only the devices that actually hold something. A machine that was
    // apportioned no blocks is not part of the placement — it changes nothing
    // about how the model decodes — and including it would make the digest
    // depend on which idle peers happened to be online. It would also put this
    // side permanently out of step with `mesh bench`, which builds its shards
    // from what the daemon reports is loaded and has no idle device to report.
    // A key the producer can never reproduce is a key that never matches.
    //
    // Each shard is keyed on the machine's hardware as well as its name — a
    // peer that swaps a GPU must not keep answering with the number the old one
    // produced. A peer too old to advertise a fingerprint is reported rather
    // than keyed on its name alone, which mirrors `mesh bench`'s refusal to
    // file such a run: neither side invents an identity the other cannot check.
    let mut shards: Vec<mm::PlacementShard> = Vec::new();
    // What each of those machines *is*, so a near miss can say how a measured
    // configuration differs from this one in terms the reader can weigh. Purely
    // descriptive — see `mm::MachineWitness`; it is never hashed, so improving
    // what a peer advertises cannot orphan the records naming it.
    let mut machines: Vec<mm::MachineWitness> = Vec::new();
    for r in rows.iter().filter(|r| r.blocks.is_some() || r.holds_output) {
        let dev = mesh.get(r.dev);
        let name = dev
            .map(|d| d.name.clone())
            .unwrap_or_else(|| format!("dev{}", r.dev));
        let Some(hw) = dev.and_then(|d| d.hw_fingerprint) else {
            return (
                SpeedSection::NotMeasurable(NotMeasurable::PeerUnidentified { name }),
                None,
            );
        };
        machines.push(mm::MachineWitness {
            node_key: name.clone(),
            vram_gb: dev.map(|d| d.vram_gb.round() as u32).unwrap_or(0),
            backend: dev.and_then(|d| d.backend.clone()),
        });
        shards.push(mm::PlacementShard {
            node_key: name,
            hw: Some(hw),
            blocks: r.blocks,
            holds_output: r.holds_output,
        });
    }
    let mode = if active_nodes <= 1 {
        "local"
    } else {
        "distributed"
    };

    // The link, over the same devices the digest describes and excluding the
    // host (which has no link to itself). A peer carrying weight but with no
    // discovered worker classifies `Unknown`, which `lookup` refuses — the plan
    // then says "not measured" instead of quoting a number taken over a link it
    // cannot confirm this placement would use.
    let worker_links: Vec<mm::LinkClass> = rows
        .iter()
        .filter(|r| !r.is_host && (r.blocks.is_some() || r.holds_output))
        .map(|r| {
            mesh.get(r.dev)
                .and_then(|d| d.link)
                .unwrap_or(mm::LinkClass::Unknown)
        })
        .collect();
    let link = mm::LinkClass::summarize(&worker_links);

    // The same three values the digest is built from, so this plan can describe
    // itself to the reader on the other side of a near miss.
    let witness = mm::PlacementWitness {
        mode: mode.to_string(),
        total_blocks: n_layer,
        shards: shards.clone(),
        machines,
    };
    let key = mm::MeasurementKey::for_plan(
        host,
        mm::model_fingerprint(sizes, n_layer),
        mm::placement_digest(mode, n_layer, &shards),
        n_ctx,
        link,
    );
    debug_assert!(
        witness.explains(&key.placement_digest),
        "the witness and the key were built from different inputs"
    );

    let section = match mm::lookup(measurements, &key, current_build) {
        Some(summary) => SpeedSection::Measured {
            summary: Box::new(summary),
        },
        None => SpeedSection::NotMeasured {
            near: mm::near_misses(measurements, peers, &key, Some(&witness)),
        },
    };
    (section, Some(key))
}

/// The machine-readable plan.
///
/// Top-level fields describe the [`Possible`](CapacityBasis::Possible) basis, as
/// they always have. `safe_now` carries the live-free basis when one exists, so a
/// script can gate on "will this load right now" without reparsing prose.
pub(crate) fn render_json(r: &PlanReport) -> serde_json::Value {
    fn devices_of(rows: &[DeviceRow]) -> Vec<serde_json::Value> {
        rows.iter()
            .map(|d| {
                serde_json::json!({
                    "device": d.dev,
                    "role": if d.is_host { "host" } else { "worker" },
                    "vram_gb": gb(d.vram()),
                    "blocks": d.blocks.map(|(a, b)| [a, b]),
                    "block_count": d.blocks.map(|(a, b)| b - a + 1).unwrap_or(0),
                    "holds_output": d.holds_output,
                    "weight_gb": gb(d.weight()),
                    "need_gb": gb(d.need()),
                    "fits": d.fits(),
                })
            })
            .collect()
    }
    let devices_json = devices_of(&r.rows);
    let safe_now_json = match &r.safe_now {
        Some(sn) => serde_json::json!({
            "basis": sn.basis.label(),
            "pooled_gb": gb(sn.pooled),
            "aggregate_gate_pass": sn.gate_pass,
            "per_device_overflow_devices":
                sn.overflows().iter().map(|d| d.dev).collect::<Vec<_>>(),
            "fits": sn.fits(),
            "nodes_used": sn.nodes.active_nodes,
            "hops": sn.nodes.hops_now,
            "devices": devices_of(&sn.rows),
            // True only when no pin overrides it — a pin is what the loader
            // obeys, and `speed` is keyed on whichever basis is executed.
            "is_executed_cut": r.pinned.is_none(),
        }),
        None => serde_json::Value::Null,
    };
    serde_json::json!({
        "model": r.model_name,
        "blocks": r.n_layer,
        "weights_gb": gb(r.total_weight),
        "output_head_gb": gb(r.output_bytes),
        "token_embd_host_ram_gb": gb(r.embd_bytes),
        "block_mass_gb": {
            "min": gb(r.block_mass.min),
            "max": gb(r.block_mass.max),
            "mean": gb(r.block_mass.mean),
            "spread": r.block_mass.spread,
            "uniform": r.block_mass.uniform
        },
        "headroom": r.headroom,
        "headroom_source": if r.headroom_from_flag { "flag" } else { "config" },
        "pooled_gb": gb(r.pooled),
        "aggregate_gate_need_gb": gb(r.gate_need),
        "aggregate_gate_pass": r.gate_pass,
        "per_device_overflow_devices": r.overflows().iter().map(|d| d.dev).collect::<Vec<_>>(),
        "moe": match &r.moe {
            Some(m) => serde_json::json!({
                "routed_expert_gb": gb(m.routed_expert_bytes),
                "routed_expert_pct": 100.0 * m.routed_expert_bytes as f64 / r.total_weight as f64,
                "hot_gb": gb(m.hot_bytes),
                "hot_pct": 100.0 * m.hot_bytes as f64 / r.total_weight as f64,
            }),
            None => serde_json::Value::Null,
        },
        "nodes_used": r.nodes.active_nodes,
        "hops": r.nodes.hops_now,
        "min_nodes": r.nodes.min_nodes,
        "min_hops": r.nodes.hops_min,
        "devices": devices_json,
        "capacity_basis": CapacityBasis::Possible.label(),
        "safe_now": safe_now_json,
        "block_split_pin": r.block_split_pin,
        "pinned": match &r.pinned {
            Some(p) => serde_json::json!({
                "basis": p.basis.label(),
                "aggregate_gate_pass": p.gate_pass,
                "per_device_overflow_devices":
                    p.overflows().iter().map(|d| d.dev).collect::<Vec<_>>(),
                "fits": p.fits(),
                "nodes_used": p.nodes.active_nodes,
                "hops": p.nodes.hops_now,
                "devices": devices_of(&p.rows),
                // A pin outranks both capacity bases: `speed` is keyed on this.
                "is_executed_cut": true,
            }),
            None => serde_json::Value::Null,
        },
        "speed": render_speed_json(r),
    })
}

/// The `speed` object, always present.
///
/// Every numeric field is `null` when there is no measurement — never `0.0`.
/// A consumer will divide by a number; `null` is an absence it has to handle,
/// while zero is a lie it will happily propagate.
fn render_speed_json(r: &PlanReport) -> serde_json::Value {
    let key = match &r.speed_key {
        Some(k) => serde_json::json!({
            "probe_version": k.probe_version,
            "model_fingerprint": k.model_fingerprint,
            "placement_digest": k.placement_digest,
            "host_hw_fingerprint": k.host_hw_fingerprint,
            "n_ctx": k.n_ctx,
            "link": k.link.as_str(),
        }),
        None => serde_json::Value::Null,
    };

    let mut o = serde_json::json!({
        "status": match &r.speed {
            SpeedSection::Measured { .. } => "measured",
            SpeedSection::NotMeasured { .. } => "not_measured",
            SpeedSection::NotMeasurable(_) => "not_measurable",
        },
        "reason": match &r.speed {
            SpeedSection::Measured { .. } => serde_json::Value::Null,
            SpeedSection::NotMeasured { .. } => "no-record".into(),
            SpeedSection::NotMeasurable(NotMeasurable::HypotheticalDevices) =>
                serde_json::Value::from("hypothetical-devices"),
            SpeedSection::NotMeasurable(NotMeasurable::HostUnidentified) =>
                serde_json::Value::from("host-unidentified"),
            SpeedSection::NotMeasurable(NotMeasurable::PeerUnidentified { name }) =>
                serde_json::Value::from(format!("peer-unidentified:{name}")),
        },
        "decode_tok_s": serde_json::Value::Null,
        "decode_tok_s_min": serde_json::Value::Null,
        "decode_tok_s_max": serde_json::Value::Null,
        "ttft_ms": serde_json::Value::Null,
        "itl_p50_ms": serde_json::Value::Null,
        "itl_p95_ms": serde_json::Value::Null,
        "prefill_tok_s": serde_json::Value::Null,
        "n_ctx": r.speed_key.as_ref().map(|k| k.n_ctx),
        "backend": serde_json::Value::Null,
        "runs": serde_json::Value::Null,
        "measured_at": serde_json::Value::Null,
        "measured_build": serde_json::Value::Null,
        "stale": serde_json::Value::Null,
        "near_misses": match &r.speed {
            SpeedSection::NotMeasured { near } => near
                .iter()
                .map(|n| serde_json::json!({
                    "placement_human": n.placement_human,
                    "decode_tok_s": n.decode_tok_s,
                    "measured_at": n.measured_at,
                    "differs_by": n.differs_by,
                    // Null means this machine measured it. A name means a peer
                    // did, and a consumer must not present the two alike — one
                    // is a fact about hardware the reader controls, the other a
                    // report about hardware they have never seen.
                    "taken_by": n.taken_by,
                    // True only for a peer: an exact local hit is a hit, and
                    // `lookup` serves it above rather than as a near miss.
                    "exact": n.is_exact(),
                    // What else was running when this was taken. Null means the
                    // record predates conditions — NOT that the box was quiet.
                    "conditions": n.conditions,
                    // One entry per `differs_by` facet, in the same order.
                    // `measured`/`yours` are null where that side kept no
                    // witness to describe — a real difference we decline to
                    // characterise, not an absent one.
                    "differences": n.detail.iter().map(|d| serde_json::json!({
                        "facet": d.facet,
                        "measured": d.theirs,
                        "yours": d.ours,
                    })).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()
                .into(),
            _ => serde_json::Value::Array(Vec::new()),
        },
        "measure_command": "svrn mesh bench",
        "key": key,
    });

    if let SpeedSection::Measured { summary: s } = &r.speed {
        let m = o.as_object_mut().expect("json! built an object");
        m.insert("decode_tok_s".into(), s.decode_tok_s.into());
        m.insert("decode_tok_s_min".into(), s.decode_tok_s_min.into());
        m.insert("decode_tok_s_max".into(), s.decode_tok_s_max.into());
        m.insert("ttft_ms".into(), s.ttft_ms.into());
        m.insert("itl_p50_ms".into(), s.itl_p50_ms.into());
        m.insert("itl_p95_ms".into(), s.itl_p95_ms.into());
        m.insert("prefill_tok_s".into(), s.prefill_tok_s.into());
        m.insert("backend".into(), s.backend.clone().into());
        m.insert("runs".into(), s.runs.into());
        m.insert("measured_at".into(), s.measured_at.into());
        m.insert("measured_build".into(), s.measured_build.clone().into());
        m.insert("stale".into(), s.stale.into());
    }
    o
}

/// The operator-facing plan.
pub(crate) fn render_human(r: &PlanReport) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let headroom = r.headroom;

    let _ = writeln!(o, "svrn mesh plan — dry run (no load, no GPU)\n");
    let _ = writeln!(o, "Model:  {}", r.model_name);
    let _ = writeln!(
        o,
        "        {} blocks · {:.1} GB weights  (output head {:.1} GB · token_embd {:.1} GB on host RAM)",
        r.n_layer,
        gb(r.total_weight),
        gb(r.output_bytes),
        gb(r.embd_bytes)
    );

    let m = &r.block_mass;
    if m.uniform {
        let _ = writeln!(
            o,
            "Blocks: {:.2}–{:.2} GB (mean {:.2}) · {:.2}× spread → UNIFORM mass",
            gb(m.min),
            gb(m.max),
            gb(m.mean),
            m.spread
        );
        let _ = writeln!(o, "        VRAM-proportional block count ≈ byte-proportional, so heterogeneous VRAM is safe.");
    } else {
        let _ = writeln!(
            o,
            "Blocks: {:.2}–{:.2} GB (mean {:.2}) · {:.2}× spread → NON-UNIFORM mass  (!)",
            gb(m.min),
            gb(m.max),
            gb(m.mean),
            m.spread
        );
        let _ = writeln!(o, "        Split apportions by byte MASS (not count), so heterogeneous VRAM stays balanced — but a single block heavier than a small node's whole share still can't be split contiguously. Watch per-device fit.");
    }

    if let Some(moe) = &r.moe {
        let _ = writeln!(
            o,
            "MoE:    {:.1} GB routed experts ({:.0}% — COLD, only top-k read per token) · {:.1} GB hot skeleton ({:.0}% — every token)",
            gb(moe.routed_expert_bytes),
            100.0 * moe.routed_expert_bytes as f64 / r.total_weight as f64,
            gb(moe.hot_bytes),
            100.0 * moe.hot_bytes as f64 / r.total_weight as f64,
        );
        let _ = writeln!(o, "        Whole blocks (experts included) stay on one node, so decode keeps its {}-hop path — a layer's experts are never scattered across nodes.", r.nodes.hops_now);
    }

    let hr_note = if r.headroom_from_flag {
        "--headroom override — WHAT-IF; the load executes with the [shared_model] headroom"
    } else {
        "matches the load's configured headroom"
    };
    let _ = writeln!(o, "Headroom: {headroom:.2}× ({hr_note}) — weight × {headroom:.2} must fit each device (covers KV + buffers)\n");

    let _ = writeln!(
        o,
        "  dev  role    VRAM       blocks     n   weight     need       fit"
    );
    for d in &r.rows {
        let (blocks_s, n_s) = match d.blocks {
            Some((a, b)) => (format!("{a}-{b}"), format!("{}", b - a + 1)),
            None => ("—".to_string(), "0".to_string()),
        };
        let fit = if d.fits() {
            format!("ok  +{:.1} GB", gb(d.vram() - d.need()))
        } else {
            format!("OVERFLOW -{:.1} GB", gb(d.need() - d.vram()))
        };
        let star = if d.is_host { "*" } else { " " };
        let role = if d.is_host { "host" } else { "worker" };
        let _ = writeln!(
            o,
            "{star} {:>3}  {:<6} {:>6.1} GB  {:<8}  {:>2}  {:>6.1} GB  {:>6.1} GB  {fit}",
            d.dev,
            role,
            gb(d.vram()),
            blocks_s,
            n_s,
            gb(d.weight()),
            gb(d.need())
        );
        if d.is_host && r.embd_bytes > 0 {
            let _ = writeln!(
                o,
                "       (+ token_embd {:.1} GB in host system RAM, not VRAM)",
                gb(r.embd_bytes)
            );
        }
    }

    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "Aggregate gate: pooled {:.1} GB {} model×{headroom:.2} ({:.1} GB) → {}",
        gb(r.pooled),
        if r.gate_pass { ">=" } else { "<" },
        gb(r.gate_need),
        if r.gate_pass {
            "PASS".to_string()
        } else {
            "FAIL — cluster too small; the host reports \"forming\" and does not load".to_string()
        }
    );

    let overflows = r.overflows();
    if overflows.is_empty() {
        let _ = writeln!(o, "Per-device:     all devices fit ok");
    } else {
        let ids: Vec<String> = overflows.iter().map(|d| d.dev.to_string()).collect();
        let _ = writeln!(
            o,
            "Per-device:     {} device(s) OVERFLOW [{}] -> the LIVE load refuses this cut; its own per-device gate reports WorkerOverflow.",
            overflows.len(),
            ids.join(", ")
        );
        let _ = writeln!(o, "\nOptions:");
        let _ = writeln!(o, "   • move the host role to your largest node (--host <idx>) — the host also holds the output head");
        let _ = writeln!(o, "   • lower --headroom for a tighter pack (less KV room), or give the overflowing node more free VRAM");
        if !r.block_mass.uniform {
            let _ = writeln!(o, "   • this model is skewed enough that one block's mass exceeds a small node's share — the split is already mass-aware, so the fix is more VRAM on that node or a different --host, not a smarter split");
        }
    }

    // Nodes & hops advisor — single-stream pipeline decode costs (nodes-1) hops
    // per token, so fewer nodes = fewer hops = lower hop LATENCY. That is a
    // tradeoff, NOT a win button: on a memory-bandwidth-bound host (e.g. a
    // unified-memory APU) offloading layers frees host weight-read bandwidth and
    // can raise THROUGHPUT despite the extra hop — the measured 122B ran ~20%
    // faster distributed (36/12) than solo. So report the hop cost; don't claim
    // fewer nodes is always faster.
    let n = &r.nodes;
    let _ = writeln!(
        o,
        "Nodes:          {} holding blocks → {} network hop{} per token",
        n.active_nodes,
        n.hops_now,
        if n.hops_now == 1 { "" } else { "s" }
    );
    if n.min_nodes < n.active_nodes {
        let _ = writeln!(
            o,
            "                mass alone fits {} node{} ({} hop{}) — {} fewer node(s) would cut {} per-token hop(s) of latency. Net tok/s depends on the host: if it's memory-bandwidth-bound, keeping layers offloaded can still win. Measure both.",
            n.min_nodes,
            if n.min_nodes == 1 { "" } else { "s" },
            n.hops_min,
            if n.hops_min == 1 { "" } else { "s" },
            n.active_nodes - n.min_nodes,
            n.hops_now - n.hops_min,
        );
    }

    render_safe_now_human(&mut o, r);
    render_pin_human(&mut o, r);
    render_speed_human(&mut o, r);
    o
}

/// The `PINNED SPLIT:` block — louder than the other two bases, because when it
/// applies, everything derived from capacity above it is not what will load.
///
/// This exists because the silence was actively misleading. `SOVEREIGN_RPC_BLOCK_SPLIT`
/// has been pinned to `12,36` on this host in a systemd drop-in since 2026-07-27
/// (to match a worker's pre-built rpc-cache), while `mesh plan` went on deriving
/// 14/34 from VRAM and presenting it as the plan. Both numbers were computed
/// correctly; nothing reconciled them, so the plan and the load simply disagreed,
/// and the measurement filed under the real cut was unreachable from the plan's key.
fn render_pin_human(o: &mut String, r: &PlanReport) {
    use std::fmt::Write as _;

    let Some(raw) = &r.block_split_pin else {
        return;
    };

    let Some(p) = &r.pinned else {
        let _ = writeln!(
            o,
            "\nPINNED SPLIT:   SOVEREIGN_RPC_BLOCK_SPLIT={raw} is set but does NOT apply to this\n                model — it needs one count per device summing to {} blocks. The loader\n                REJECTS it too and falls back to the VRAM-derived split above, so the\n                pin is having no effect. Fix or remove it.",
            r.n_layer
        );
        return;
    };

    let counts: Vec<String> = p
        .rows
        .iter()
        .map(|d| {
            format!(
                "dev{} {}",
                d.dev,
                d.blocks.map(|(a, b)| b - a + 1).unwrap_or(0)
            )
        })
        .collect();
    let _ = writeln!(
        o,
        "\nPINNED SPLIT:   SOVEREIGN_RPC_BLOCK_SPLIT={raw} — the loader OBEYS this and ignores VRAM."
    );
    let _ = writeln!(
        o,
        "                Actual cut: {}  →  {}",
        counts.join(" · "),
        verdict(p.gate_pass, &p.overflows())
    );
    if p.rows.iter().map(|d| d.blocks).collect::<Vec<_>>()
        != r.rows.iter().map(|d| d.blocks).collect::<Vec<_>>()
    {
        let _ = writeln!(
            o,
            "                This is NOT the VRAM-derived cut shown in the table above. The table\n                answers 'what would capacity choose'; the pin is what will load."
        );
    }
}

/// The `Safe now:` block — the second capacity basis, and the gap between them.
///
/// Everything above this point answers "could this mesh hold the model", from
/// device totals. This answers "would a load started right now succeed", from the
/// live free memory the loader's own gate reads. Both are true statements about
/// different questions, and the operator is told which is which rather than being
/// handed one number that silently means whichever was easier to obtain.
///
/// The gap line is the load-bearing part. A device short on free memory is not a
/// device that is too small, and the two demand opposite responses: buy hardware
/// versus wait for the resident model to unload. On 2026-07-29 that ambiguity
/// turned a few seconds of teardown transience into an hours-long no-primary
/// outage, because a single collapsed `capacity_mb=20000` could not distinguish a
/// 20 GB device from a 51 GB device with 31 GB briefly held.
fn render_safe_now_human(o: &mut String, r: &PlanReport) {
    use std::fmt::Write as _;

    let Some(sn) = &r.safe_now else {
        let _ = writeln!(
            o,
            "\nSafe now:       UNKNOWN — no live free-memory reading for every device.\n                Everything above is what is POSSIBLE (device totals). Pass --from-mesh\n                against a daemon that reports device_memory to also see what would\n                load right now."
        );
        return;
    };

    let _ = writeln!(
        o,
        "\nTwo capacities, because they answer different questions:"
    );
    let _ = writeln!(
        o,
        "  possible (device total) → can this mesh EVER run this model: pooled {:.1} GB → {}",
        gb(r.pooled),
        verdict(r.gate_pass, &r.overflows())
    );
    let _ = writeln!(
        o,
        "  safe now (live free)    → would a load RIGHT NOW succeed:     pooled {:.1} GB → {}",
        gb(sn.pooled),
        verdict(sn.gate_pass, &sn.overflows())
    );

    // Per-device gap, worst first. Only devices actually holding something can
    // overflow, so a device with no blocks is not interesting here.
    let mut gaps: Vec<(usize, u64, u64)> = r
        .rows
        .iter()
        .filter_map(|p| {
            let s = sn.rows.iter().find(|s| s.dev == p.dev)?;
            (p.vram() > s.vram()).then_some((p.dev, p.vram(), s.vram()))
        })
        .collect();
    gaps.sort_by_key(|&(_, total, free)| std::cmp::Reverse(total.saturating_sub(free)));
    if gaps.is_empty() {
        let _ = writeln!(
            o,
            "                No gap — every device is as free as it is large; nothing else is resident."
        );
    } else {
        for (dev, total, free) in &gaps {
            let _ = writeln!(
                o,
                "  dev {dev}: {:.1} GB total, {:.1} GB free → {:.1} GB held by other work right now",
                gb(*total),
                gb(*free),
                gb(total.saturating_sub(*free))
            );
        }
    }

    // The two bases cut the model differently — say so, because the split is what
    // a measurement is keyed on and what each worker caches.
    let cut = |a: &[DeviceRow]| -> String {
        let mut parts: Vec<String> = a
            .iter()
            .filter_map(|d| d.blocks.map(|(x, y)| (d.dev, y - x + 1)))
            .map(|(dev, n)| format!("{n}@dev{dev}"))
            .collect();
        parts.sort();
        parts.join(" + ")
    };
    let (pc, sc) = (cut(&r.rows), cut(&sn.rows));
    if pc != sc {
        let _ = writeln!(
            o,
            "                Different cut: possible would place {pc}, but a load now places {sc}.\n                The load executes the SAFE NOW cut — that is the one Speed refers to."
        );
    }

    if r.gate_pass && r.overflows().is_empty() && !sn.fits() {
        let _ = writeln!(
            o,
            "                → This model FITS this hardware but will NOT load right now. Free the\n                  memory (retire the resident model) rather than buying VRAM. Note that the\n                  live gate does not retry: it parks, so a refusal here outlives the transient."
        );
    }
}

/// `PASS` / `FAIL` for one basis, naming which gate failed.
fn verdict(gate_pass: bool, overflows: &[&DeviceRow]) -> String {
    match (gate_pass, overflows.is_empty()) {
        (true, true) => "PASS".to_string(),
        (false, _) => "FAIL (aggregate: cluster too small)".to_string(),
        (true, false) => {
            let ids: Vec<String> = overflows.iter().map(|d| d.dev.to_string()).collect();
            format!("FAIL (per-device overflow: dev {})", ids.join(", "))
        }
    }
}

/// The `Speed:` block.
///
/// The whole point of this section is that it is allowed to say nothing. A
/// number appears here only when a run produced it for this exact
/// configuration; otherwise the block names the command that would produce
/// one. It carries no estimate, no interpolation from a neighbouring split,
/// and — deliberately — no guess at how long measuring would take, since that
/// would itself be a fabricated number about a model we have never loaded.
fn render_speed_human(o: &mut String, r: &PlanReport) {
    use std::fmt::Write as _;

    match &r.speed {
        SpeedSection::Measured { summary: s } => {
            let _ = writeln!(
                o,
                "Speed:          {:.1} tok/s decode · TTFT {:.2} s — MEASURED on this exact split",
                s.decode_tok_s,
                s.ttft_ms / 1000.0
            );
            let _ = writeln!(
                o,
                "                {} · ctx {}{}",
                s.placement_human,
                s.n_ctx,
                match &s.backend {
                    Some(b) => format!(" · {b}"),
                    None => String::new(),
                }
            );
            // The headline above is the MEDIAN run (an operator call — see
            // MeasurementSummary); the observed range is run medians, so an
            // outlier run widens the band without setting the headline.
            if s.runs == 1 {
                let _ = writeln!(o, "                1 run · build {}", s.measured_build);
            } else {
                let _ = writeln!(
                    o,
                    "                median of {} runs · observed {:.1}–{:.1} tok/s · build {}",
                    s.runs, s.decode_tok_s_min, s.decode_tok_s_max, s.measured_build
                );
            }
            if s.stale {
                let _ = writeln!(
                    o,
                    "                (!) recorded on a different build than this binary. Re-run `svrn mesh bench`"
                );
                let _ = writeln!(o, "                    if the inference engine changed.");
            }
        }
        SpeedSection::NotMeasured { near } => {
            // Not "for this split" — the split may well be measured and the
            // link be what differs. Saying "split" there sent a reader looking
            // for a difference in the block apportionment that isn't present.
            let _ = writeln!(o, "Speed:          not measured for this configuration.");
            // An unclassifiable link is the one reason a reader cannot work out
            // for themselves from the rest of the output, so it is named.
            if r.speed_key
                .as_ref()
                .is_some_and(|k| k.link == sovereign_core::mesh_measurements::LinkClass::Unknown)
            {
                let _ = writeln!(
                    o,
                    "                No rpc-server is discovered for every machine in this plan,"
                );
                let _ = writeln!(
                    o,
                    "                so how the tensor stream would travel is unknown — and that"
                );
                let _ = writeln!(
                    o,
                    "                choice alone has moved decode by ~2.3x on this fleet."
                );
            }
            // A peer who measured *this* configuration outranks any near miss,
            // however recent — it is the only thing here that describes what was
            // actually asked about. Presentation order, chosen here rather than
            // in the sort, because the store's ranking is general-purpose and
            // this priority is a judgement about what a reader needs first.
            let (exact, differing): (Vec<_>, Vec<_>) = near.iter().partition(|n| n.is_exact());
            for n in exact.iter().take(2) {
                let who = n.taken_by.as_deref().unwrap_or("this machine");
                let _ = writeln!(
                    o,
                    "                {who} measured this configuration: {:.1} tok/s.",
                    n.decode_tok_s
                );
                let _ = writeln!(
                    o,
                    "                Same model, split, hardware fingerprint, link and context — but"
                );
                let _ = writeln!(
                    o,
                    "                their machine, so it is a report, not your measurement."
                );
                // An exact-key hit is the strongest thing on this surface, which
                // is exactly why the load it was taken under has to travel with
                // it. Same configuration on a busy box is not the same claim.
                if let Some(c) = &n.conditions {
                    let _ = writeln!(o, "                On their box at the time: {c}.");
                }
            }
            for n in differing.iter().take(2) {
                match n.taken_by.as_deref() {
                    Some(who) => {
                        let _ = writeln!(
                            o,
                            "                Measured by {who}: {} → {:.1} tok/s.",
                            n.placement_human, n.decode_tok_s
                        );
                    }
                    None => {
                        let _ = writeln!(
                            o,
                            "                Measured here: {} → {:.1} tok/s.",
                            n.placement_human, n.decode_tok_s
                        );
                    }
                }
                let _ = writeln!(
                    o,
                    "                That is a different configuration ({}), so its number does not apply here.",
                    n.differs_by.join(", ")
                );
                if let Some(c) = &n.conditions {
                    let _ = writeln!(o, "                Taken with: {c}.");
                }
                // Name each difference concretely where both sides could be
                // described. A facet that cannot be is still listed above: the
                // difference is real, and what is missing is an honest account
                // of it — which is not a licence to invent one.
                for d in n.detail.iter() {
                    if let (Some(theirs), Some(ours)) = (&d.theirs, &d.ours) {
                        let _ = writeln!(
                            o,
                            "                  {:<14} measured: {theirs}   yours: {ours}",
                            d.facet
                        );
                    }
                }
            }
            if near.is_empty() {
                let _ = writeln!(
                    o,
                    "                Sovereign does not quote throughput it has not measured."
                );
            }
            let _ = writeln!(o, "                Measure it:  svrn mesh bench");
        }
        SpeedSection::NotMeasurable(NotMeasurable::HypotheticalDevices) => {
            let _ = writeln!(
                o,
                "Speed:          not measurable — --devices describes hardware that isn't here."
            );
            let _ = writeln!(
                o,
                "                Run this with --from-mesh on the mesh itself, then `svrn mesh bench`."
            );
        }
        SpeedSection::NotMeasurable(NotMeasurable::HostUnidentified) => {
            let _ = writeln!(
                o,
                "Speed:          not measurable — this host advertises no hardware fingerprint,"
            );
            let _ = writeln!(
                o,
                "                so there is no key a measurement could be filed under. Upgrading"
            );
            let _ = writeln!(o, "                the daemon on this node fixes it.");
        }
        SpeedSection::NotMeasurable(NotMeasurable::PeerUnidentified { name }) => {
            let _ = writeln!(
                o,
                "Speed:          not measurable — {name} is holding part of this model but"
            );
            let _ = writeln!(
                o,
                "                advertises no hardware fingerprint, so a number measured on"
            );
            let _ = writeln!(
                o,
                "                this split could not say which machine produced it. Upgrading"
            );
            let _ = writeln!(o, "                the daemon on {name} fixes it.");
        }
    }
}

const HELP_MESH_PLAN: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn mesh plan",
    summary: "Dry-run a model's tensor split across a mesh — per-device fit, offline, no load.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn mesh plan <model.gguf | hf:owner/repo[/variant]> (--from-mesh | --devices <gb,..>) [--host <idx>] [--headroom <f>] [--json]",
        ),
        sovereign_cli_shared::help::HelpSection::Flags(&[
            (
                "--from-mesh",
                "Read the live mesh from the running daemon (online anchor workers + this host). Exclusive with --devices.",
            ),
            (
                "--devices <gb,...>",
                "Per-node usable VRAM in GB, in mesh order (e.g. 64,32,32). Plan a hypothetical mesh instead of --from-mesh.",
            ),
            (
                "--host <idx>",
                "0-based index of the host node (holds the output head). Default: last.",
            ),
            (
                "--headroom <f>",
                "Override the headroom factor (weight × f must fit each device). Defaults to `[shared_model] headroom` (else 1.2) — the value the load itself uses.",
            ),
            ("--json", "Emit the plan as a machine-readable JSON split manifest."),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Reuses the daemon's own plan_shards + quantize_vram, then overlays the real per-block\n\
             byte mass to show the BYTES each node holds and whether they fit — via the SAME\n\
             shard_fits decider the live load's per-device gate runs, so a plan that says OVERFLOW\n\
             names a cut the load would actually refuse. Reads only the GGUF header table: no model\n\
             load, no GPU, instant even on a 400 GB split. Also reports whether the model's per-block\n\
             mass is uniform (heterogeneous VRAM safe) or skewed (OOM risk).\n\
             \n\
             --from-mesh reports TWO capacities per device and does not choose between them:\n\
             POSSIBLE is the device total — what the silicon could hold if nothing else were\n\
             resident, i.e. whether this mesh can run the model at all. SAFE NOW is live free\n\
             memory as the loader reads it — whether a load started this second would succeed.\n\
             They differ whenever anything is loaded, and a large gap means 'busy', not 'too\n\
             small'. The exit code follows POSSIBLE: a device being momentarily busy does not\n\
             make the plan wrong. Read the SAFE NOW line for what would happen right now.\n\
             \n\
             The model can be a path or `hf:<owner>/<repo>[/<variant>]`, which plans a model you\n\
             have NOT downloaded. Only the GGUF headers are fetched — a few MB by HTTP range —\n\
             because the planner needs the block count and tensor table, never the weights: a\n\
             155 GB five-shard model sizes for ~17 MB. Name the quant directory as <variant>;\n\
             omit it and the command lists what the repo publishes rather than guessing, since\n\
             choosing a quant is choosing how much of your memory to spend. A header-only plan\n\
             is honest about tensor mass but has fetched no weights, so it cannot tell you the\n\
             download will succeed — only whether it would fit if it did.",
        ),
        sovereign_cli_shared::help::HelpSection::Examples(&[
            (
                "svrn mesh plan GLM-5.2.gguf --from-mesh",
                "Plan across your actual running mesh (reads each node's advertised VRAM)",
            ),
            (
                "svrn mesh plan hf:unsloth/DeepSeek-V4-Flash-0731-GGUF/UD-Q4_K_XL --from-mesh",
                "Will this 155 GB model fit my mesh? Answered before downloading it",
            ),
            (
                "svrn mesh plan hf:unsloth/DeepSeek-V4-Flash-0731-GGUF --devices 51,124",
                "No variant named: lists the quants the repo publishes, then pick one",
            ),
            (
                "svrn mesh plan Qwen3.5-122B.gguf --devices 64,32,32",
                "A hypothetical 64 GB host + two 32 GB workers",
            ),
            (
                "svrn mesh plan model.gguf --devices 128,128,128,128 --headroom 1.35",
                "Four nodes, conservative headroom",
            ),
        ]),
    ],
};

const HELP_MESH_CREATE: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn mesh create",
    summary: "Promote the solo mesh to a joinable mesh and print the shareable invite.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn mesh create [--name <name>]"),
        sovereign_cli_shared::help::HelpSection::Flags(&[(
            "--name <name>",
            "Human-readable mesh name (default: \"<host>'s Mesh\")",
        )]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Errors if a mesh already exists (e.g. from `svrn setup`'s silent solo mesh).\n\
             In that case, run `svrn mesh rotate` to generate a new shareable key instead.",
        ),
    ],
};

const HELP_MESH_JOIN: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn mesh join",
    summary: "Join an existing mesh using any of the three invite forms.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn mesh join <arg>"),
        sovereign_cli_shared::help::HelpSection::Examples(&[
            (
                "svrn mesh join cwth-a1b2-c3d4-e5f6",
                "Bare key typed from another user's terminal",
            ),
            (
                "svrn mesh join https://sovereign.dev/join/cwth-a1b2-c3d4-e5f6",
                "Clickable https link from an email",
            ),
            (
                "svrn mesh join sovereign://join/cwth-a1b2-c3d4-e5f6",
                "Native app deep link",
            ),
        ]),
    ],
};

const HELP_MESH_ROTATE: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn mesh rotate",
    summary: "Generate a new shareable join key (the previous key stops working for future joins).",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("svrn mesh rotate"),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Existing members keep their connections — rotation changes only who may JOIN.\n\
             Needs a RUNNING daemon: the rotation happens in-process, so no restart is\n\
             required and stopping the daemon first makes this refuse. (This note used to\n\
             say the opposite. It described the offline-disk-write era, when the daemon\n\
             re-persisted the old hash over the new one on its next gossip round and a\n\
             restart was the workaround.)",
        ),
    ],
};

async fn cmd_create(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_MESH_CREATE);
        return 0;
    }
    let mut name = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--name" {
            if let Some(n) = iter.next() {
                name = Some(n.clone());
            }
        }
    }

    // If a mesh already exists (e.g. the silent solo mesh created by
    // `svrn setup`), the join-key hash is stored but its plaintext
    // is gone — we can't re-show it. Direct the user to `mesh rotate`
    // instead of blindly attempting another create_mesh (which errors
    // with AlreadyRunning or leaves them confused).
    if sovereign_mesh::persist::load(&sovereign_root())
        .map(|opt| opt.is_some())
        .unwrap_or(false)
    {
        eprintln!("A mesh already exists (created during `svrn setup`).");
        eprintln!("To generate a new shareable join key, run:");
        eprintln!();
        eprintln!("  svrn mesh rotate");
        eprintln!();
        return 1;
    }

    let mesh_name = name.unwrap_or_else(|| {
        let host = hostname().unwrap_or_else(|| "sovereign".to_string());
        format!("{host}'s Mesh")
    });
    let node_name = hostname().unwrap_or_else(|| "sovereign-node".to_string());

    let daemon = EmbeddedDaemon::new(
        sovereign_root(),
        one_shot_setup_config(),
        mesh_admin_services(),
    );
    // Explicit create = serve remote peers → expose the client API
    // (bind non-loopback + require a bearer token).
    daemon.expose_client_api();
    match daemon.create_mesh(&mesh_name, &node_name).await {
        Ok(result) => {
            print_mesh_share(
                "Mesh created.",
                &result.mesh_name,
                &result.join_key,
                result.client_token.as_deref(),
                Some(&result.join_link),
            );
            0
        }
        Err(e) => {
            eprintln!("Failed to create mesh: {e}");
            1
        }
    }
}

/// Spec-format banner for a freshly-created or freshly-rotated mesh.
/// Prints both the https share URL and the CLI form so the inviter
/// can pick whichever suits the invitee's environment.
///
/// `join_link` is the daemon-built deep link (it carries the founder's
/// no-VPN dial string + TTL when the iroh endpoint is up); the https
/// form is derived from it so both printed forms share params. `None`
/// (the offline rotate path — no running daemon, no dial to read)
/// prints the bare form.
fn print_mesh_share(
    headline: &str,
    mesh_name: &str,
    join_key: &str,
    client_token: Option<&str>,
    join_link: Option<&str>,
) {
    let app_link = match join_link.and_then(parse_join_argument) {
        Some(sovereign_mesh::deep_link::DeepLink::Join {
            relay_hint,
            iroh_dial,
            encrypted,
            expires_at,
            ..
        }) => build_https_join_link(
            join_key,
            relay_hint.as_deref(),
            Some(mesh_name),
            iroh_dial.as_deref(),
            encrypted,
            expires_at,
        ),
        // `parse_join_argument` FILTERS to `Join`, so a guest link cannot
        // reach here — it is spelled out rather than folded into `_` so that a
        // third `DeepLink` variant breaks this build instead of silently
        // rendering an invite from something that is not one.
        Some(sovereign_mesh::deep_link::DeepLink::Guest { .. }) | None => {
            build_https_join_link(join_key, None, Some(mesh_name), None, false, None)
        }
    };
    println!();
    println!("{headline}");
    println!();
    println!("  Join key:  {join_key}");
    if let Some(token) = client_token {
        // Remote peers/clients authenticate to this node's API with
        // this bearer token (the daemon now binds non-loopback).
        println!("  API token: {token}");
    }
    println!();
    println!("Share with a friend:");
    println!("  App:  {app_link}");
    println!("  CLI:  svrn mesh join {join_key}");
    println!();
}

async fn cmd_join(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_MESH_JOIN);
        return 0;
    }
    let Some(arg) = args.first() else {
        eprintln!("Missing join key.");
        eprintln!("Usage: svrn mesh join <key-or-url>");
        eprintln!();
        eprintln!("Accepted forms:");
        eprintln!("  cwth-XXXX-XXXX-XXXX");
        eprintln!("  https://sovereign.dev/join/cwth-XXXX-XXXX-XXXX");
        eprintln!("  sovereign://join/cwth-XXXX-XXXX-XXXX");
        return 1;
    };

    // Validate the argument before either path so the user sees the
    // same error regardless of whether a daemon is up. `parse_join_argument`
    // accepts bare keys / https URLs / sovereign:// — same set as
    // /v1/mesh/join on the daemon side.
    if parse_join_argument(arg).is_none() {
        eprintln!("Invalid join argument: {arg}");
        eprintln!(
            "Expected a bare key (cwth-XXXX-XXXX-XXXX), an https URL, or a sovereign:// link."
        );
        return 1;
    }

    let node_name = hostname().unwrap_or_else(|| "sovereign-node".to_string());

    // Prefer the running daemon's HTTP endpoint when it's reachable.
    // Why: building a fresh `EmbeddedDaemon` from the CLI process
    // creates a SEPARATE in-memory `AppState`. Its `start_daemon`
    // fails silently to bind :9741/:9742 (the running daemon already
    // owns them, the bind error is swallowed by a `warn!` + `return`
    // inside an async block — not a hard failure), the handshake
    // still completes on the founder's side, but only the CLI
    // process's mesh state gets updated — never the long-running
    // daemon's. CLI exits, in-memory join state evaporates, and the
    // daemon keeps its solo-mesh `invite_key_hash`. Every subsequent
    // gossip from peers mismatches and gets rejected.
    //
    // Routing through `POST /v1/mesh/join` makes the running daemon
    // perform the join in-process, so the AppState that actually
    // serves gossip is the one that gets the adopted mesh.
    println!();
    println!("Joining mesh...");
    if daemon_listening_on(9741).await {
        return join_via_running_daemon(arg, &node_name).await;
    }

    eprintln!("(no daemon detected on :9741 — running the join in-process)");
    let daemon = EmbeddedDaemon::new(
        sovereign_root(),
        one_shot_setup_config(),
        mesh_admin_services(),
    );
    daemon.expose_client_api();
    let Some(link) = parse_join_argument(arg) else {
        // Pre-validated above, so this is unreachable. Bail
        // defensively rather than panic.
        eprintln!("Invalid join argument: {arg}");
        return 1;
    };
    match daemon.join_mesh(&link, &node_name).await {
        Ok(result) => {
            println!();
            println!("\u{2713} Connected to \"{}\"", result.mesh_name);
            println!("  Your node id: {}", result.node_id);
            println!();
            println!("Shared compute is now available.");
            println!();
            0
        }
        Err(e) => {
            eprintln!();
            eprintln!("Failed to join mesh: {e}");
            1
        }
    }
}

/// Probe `127.0.0.1:<port>` for an HTTP listener. Used to decide
/// between the in-process `EmbeddedDaemon` path and the
/// daemon-HTTP-endpoint path in `cmd_join`. We hit `/v1/models` rather
/// than `/` because the daemon's root route returns 405 for GET and
/// reqwest's `.send()` succeeds against 405 just as well as 200 — the
/// goal is "is anything listening", not "is the response 2xx".
pub(crate) async fn daemon_listening_on(port: u16) -> bool {
    let url = format!("http://127.0.0.1:{port}/v1/models");
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
    else {
        return false;
    };
    client.get(&url).send().await.is_ok()
}

/// POST `arg` to the running daemon's `/v1/mesh/join` endpoint and
/// surface the response. Mirrors the success / failure output of the
/// in-process path so the user can't tell which one ran (and shouldn't
/// have to).
async fn join_via_running_daemon(arg: &str, node_name: &str) -> i32 {
    let url = "http://127.0.0.1:9741/v1/mesh/join";
    let body = serde_json::json!({
        "key_or_url": arg,
        "node_name": node_name,
    });
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build HTTP client: {e}");
            return 1;
        }
    };
    let resp = match client.post(url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to reach running daemon at {url}: {e}");
            return 1;
        }
    };
    let status = resp.status();
    let payload: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Daemon returned non-JSON response (status={status}): {e}");
            return 1;
        }
    };
    if status.is_success() {
        let mesh_name = payload
            .get("mesh_name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let node_id = payload
            .get("node_id")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        println!();
        println!("\u{2713} Connected to \"{mesh_name}\"");
        println!("  Your node id: {node_id}");
        println!();
        println!("Shared compute is now available.");
        println!();
        0
    } else {
        let err = payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("(no error message)");
        eprintln!();
        eprintln!("Failed to join mesh (daemon returned {status}): {err}");
        1
    }
}

/// Rotate the join key on an existing mesh. Regenerates the plaintext
/// key + hash, writes the new hash back to `mesh.json`, and prints the
/// new shareable invite in the same format as `mesh create`.
async fn cmd_rotate(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_MESH_ROTATE);
        return 0;
    }
    let force = args.iter().any(|a| a == "--force");

    // Rotation MUST go through the running daemon. It used to be an offline
    // disk write, which is why it had to tell the user to restart: the daemon
    // held the old hash in memory and re-persisted it over the new one on its
    // next gossip round, silently reverting the rotation. There is no correct
    // offline rotation — the live mesh is the thing that has to change.
    if !daemon_listening_on(daemon_client_port()).await {
        eprintln!(
            "No daemon detected on :{} — rotation needs one.",
            daemon_client_port()
        );
        eprintln!("Start it with `svrn daemon start`, then re-run.");
        return 1;
    }
    rotate_via_running_daemon(force).await
}

/// The daemon's client port from `SetupConfig`, not a hardcoded 9741 — a
/// sandbox pointed at its own daemon must not rotate the operator's mesh.
pub(crate) fn daemon_client_port() -> u16 {
    sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.daemon.client_port)
        .unwrap_or(9741)
}

async fn rotate_via_running_daemon(force: bool) -> i32 {
    let port = daemon_client_port();
    let url = format!("http://127.0.0.1:{port}/v1/mesh/rotate?force={force}");
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build HTTP client: {e}");
            return 1;
        }
    };
    let resp = match client.post(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to reach running daemon at {url}: {e}");
            return 1;
        }
    };
    let status = resp.status();
    let payload: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Daemon returned non-JSON response (status={status}): {e}");
            return 1;
        }
    };
    if status.is_success() {
        let mesh_name = payload
            .get("mesh_name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        let join_key = payload
            .get("join_key")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)");
        eprintln!();
        eprintln!("Existing members stay connected — rotation changes only who may JOIN.");
        print_mesh_share("Join key rotated.", mesh_name, join_key, None, None);
        0
    } else {
        let err = payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("(no error message)");
        eprintln!();
        eprintln!("Failed to rotate (daemon returned {status}): {err}");
        1
    }
}

/// `svrn mesh status [--json] [--self] [--addr-only]`
///
/// Reads the running daemon's `/v1/mesh/status` endpoint and renders
/// the mesh view. Default output is human-readable; `--json` prints
/// the raw response from the daemon.
///
/// Designed to replace the toolbox-tailscale-socket dance in the
/// pod-deployment workflow. Two scripting modes:
///
///   `--self`           Restrict the rendering / JSON to the current
///                      node's row. Combine with `--addr-only` to
///                      capture this node's first advertised address
///                      for `SOVEREIGN_FOUNDER_ADDR`-style env
///                      assignments without parsing JSON elsewhere.
///
///   `--addr-only`      Print only the first advertised address of
///                      the matched row(s). One address per line.
///                      Exit code is 1 if no address is available
///                      yet (member exists but no gossip round has
///                      populated addresses).
///
/// Examples:
///   svrn mesh status
///   svrn mesh status --json
///   export SOVEREIGN_FOUNDER_ADDR=$(svrn mesh status --self --addr-only)
async fn cmd_status(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn mesh status [--json] [--self] [--addr-only]");
        eprintln!();
        eprintln!("Show mesh members, online status, and advertised addresses.");
        eprintln!("Reads /v1/mesh/status from the running daemon (default port 9741).");
        eprintln!();
        eprintln!("Flags:");
        eprintln!("  --json        Raw JSON pass-through from the daemon endpoint.");
        eprintln!("  --self        Restrict to the current node's row.");
        eprintln!("  --addr-only   Print only the first address of each matched row");
        eprintln!("                (one per line). Right for SOVEREIGN_FOUNDER_ADDR=$(...).");
        return 0;
    }

    let mut json_out = false;
    let mut self_only = false;
    let mut addr_only = false;
    for a in args {
        match a.as_str() {
            "--json" => json_out = true,
            "--self" => self_only = true,
            "--addr-only" => addr_only = true,
            other => {
                eprintln!("Unknown flag: {other}");
                eprintln!("Try `svrn mesh status --help` for usage.");
                return 2;
            }
        }
    }

    // Fetch from the daemon. Use SetupConfig for the port so a custom
    // client_port (set via `[daemon].client_port`) still works.
    let port = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.daemon.client_port)
        .unwrap_or(9741);
    let url = format!("http://127.0.0.1:{port}/v1/mesh/status");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest client builds");
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh status: daemon at {url} not reachable: {e}");
            eprintln!("hint: `svrn daemon status` to check, `svrn daemon start` to launch.");
            return 1;
        }
    };
    if !resp.status().is_success() {
        eprintln!(
            "mesh status: daemon returned HTTP {} from {url}",
            resp.status()
        );
        return 1;
    }
    let body = match resp.text().await {
        Ok(t) => t,
        Err(e) => {
            eprintln!("mesh status: read body: {e}");
            return 1;
        }
    };

    let status: sovereign_mesh::mesh_http::StatusResponse = match serde_json::from_str(&body) {
        Ok(s) => s,
        Err(e) => {
            // Daemon version drift — fall back to raw JSON pass-through
            // so the operator at least sees the data even when our
            // local DTO doesn't match.
            eprintln!("mesh status: response shape mismatch ({e}); printing raw JSON.");
            println!("{body}");
            return 1;
        }
    };

    // Filter to self when requested. Most addr-only / scripting uses
    // want exactly this node's address.
    let members: Vec<&sovereign_mesh::mesh_http::MemberDto> = if self_only {
        status.members.iter().filter(|m| m.is_self).collect()
    } else {
        status.members.iter().collect()
    };

    if addr_only {
        // Print first address per matched row, one per line. Right
        // shape for `export FOO=$(...)` (when --self matches one row)
        // and `for a in $(...)` (when matching multiple).
        let mut printed_any = false;
        for m in &members {
            if let Some(a) = m.addresses.first() {
                println!("{a}");
                printed_any = true;
            }
        }
        return if printed_any { 0 } else { 1 };
    }

    if json_out {
        // Re-serialize from our typed view so --self filtering takes
        // effect even in --json mode. Pretty-print so the output is
        // human-grokkable too.
        let filtered = serde_json::json!({
            "running": status.running,
            "mesh_name": status.mesh_name,
            "members_online": status.members_online,
            "members_total": status.members_total,
            "members": members,
            "join_key": status.join_key,
            "join_link": status.join_link,
            "node_class": status.node_class,
            "entry_node": status.entry_node,
        });
        match serde_json::to_string_pretty(&filtered) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("mesh status: serialize: {e}");
                return 1;
            }
        }
        return 0;
    }

    // Human-readable rendering. Designed to be skimmable in 2 seconds:
    // mesh name on top, online/total on the right, members table, and
    // the join key + link at the bottom (the operator typically needs
    // one of those for whatever workflow led them here).
    if !status.running {
        println!("mesh: daemon running, but no mesh active (`svrn mesh create` or `mesh join` to bootstrap)");
        return 0;
    }
    let mesh_name = status.mesh_name.as_deref().unwrap_or("<unnamed>");
    println!(
        "mesh: {mesh_name}    [{online}/{total} online]",
        online = status.members_online,
        total = status.members_total,
    );
    // This node's participant class, printed only when it is NOT the ordinary
    // `holder` — a fleet of holders should not pay a line of noise for the
    // common case, while the two classes that change what the node can do say
    // so. `terminal` is the one an operator most needs: its members table looks
    // identical to a broken holder's, because both advertise nothing.
    //
    // An older daemon sends no `node_class` at all and the field defaults to
    // empty, which prints nothing rather than guessing "holder" (§18.3).
    match status.node_class.as_str() {
        "terminal" => println!(
            "  this node: terminal — holds no models, routes every turn to {}",
            status.entry_node.as_deref().unwrap_or("<unset>"),
        ),
        "unconfigured" => {
            println!("  this node: unconfigured — no models and no entry node; run `svrn setup`")
        }
        _ => {}
    }
    println!();
    println!(
        "  {:<22} {:<12} {:<8} address(es)",
        "node_id", "name", "status"
    );
    println!("  {:-<22} {:-<12} {:-<8} {:-<25}", "", "", "", "");
    for m in &members {
        let self_tag = if m.is_self { " *" } else { "" };
        let addr_disp = if m.addresses.is_empty() {
            "<not advertised>".to_string()
        } else {
            m.addresses.join(", ")
        };
        // node_id is 22 chars including the "node-" prefix; truncate
        // gracefully if a future format grows it.
        let nid: String = m.node_id.chars().take(22).collect();
        let name: String = m.name.chars().take(12).collect();
        // A tombstoned row has no liveness worth reporting — it is not a
        // member. Rendering its last known `status` as "offline" made a
        // successful `forget-member` look like a no-op: the operator repairs
        // the roster, re-runs this, and sees the row exactly where it was.
        let status = if m.active {
            m.status.as_str()
        } else {
            "retired"
        };
        println!(
            "  {:<22} {:<12} {:<8} {}{}",
            nid, name, status, addr_disp, self_tag,
        );
    }
    crate::mesh_member_cmd::print_alias_warnings(&members);

    if !self_only {
        println!();
        if let Some(k) = status.join_key.as_deref() {
            println!("join key:  {k}");
        }
        if let Some(l) = status.join_link.as_deref() {
            println!("join link: {l}");
        }
    }
    0
}

/// `svrn mesh transport` — the operator's "is anyone actually on
/// iroh, and via a direct path or the relay?" surface (H2). Reads the
/// `iroh_transport` block of `/v1/mesh/status`.
async fn cmd_transport(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn mesh transport [--json]");
        eprintln!();
        eprintln!("Show each peer's live iroh connection path (direct / relayed / mixed / idle).");
        eprintln!(
            "Empty output means iroh isn't carrying mesh traffic (the mesh is on the IP path)."
        );
        return 0;
    }
    let json_out = args.iter().any(|a| a == "--json");

    let port = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.daemon.client_port)
        .unwrap_or(9741);
    let url = format!("http://127.0.0.1:{port}/v1/mesh/status");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest client builds");
    let body = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(t) => t,
            Err(e) => {
                eprintln!("mesh transport: read body: {e}");
                return 1;
            }
        },
        Ok(r) => {
            eprintln!(
                "mesh transport: daemon returned HTTP {} from {url}",
                r.status()
            );
            return 1;
        }
        Err(e) => {
            eprintln!("mesh transport: daemon at {url} not reachable: {e}");
            return 1;
        }
    };
    let status: sovereign_mesh::mesh_http::StatusResponse = match serde_json::from_str(&body) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mesh transport: response shape mismatch ({e}); printing raw JSON.");
            println!("{body}");
            return 1;
        }
    };

    if json_out {
        match serde_json::to_string_pretty(&status.iroh_transport) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("mesh transport: serialize: {e}");
                return 1;
            }
        }
        return 0;
    }

    if status.iroh_transport.is_empty() {
        println!("iroh transport: no peers on iroh (mesh is on the IP path, or iroh is disabled).");
        return 0;
    }
    println!("  {:<12} {:<9} relay / direct", "peer", "path");
    println!("  {:-<12} {:-<9} {:-<30}", "", "", "");
    for p in &status.iroh_transport {
        let name: String = p.name.chars().take(12).collect();
        let (path, detail) = match &p.path {
            Some(tp) => {
                let detail = match &tp.relay {
                    Some(r) => format!("relay={r}  direct={}", tp.active_direct_addrs),
                    None => format!("direct={}", tp.active_direct_addrs),
                };
                (tp.path.as_str(), detail)
            }
            None => ("unknown", "no endpoint record yet".to_string()),
        };
        println!("  {name:<12} {path:<9} {detail}");
    }
    0
}

async fn cmd_balance() -> i32 {
    println!("(contribution balance requires a running daemon)");
    0
}

/// `svrn mesh leave`
///
/// Was a success-shaped stub: it printed "(mesh leave requires a running
/// daemon)" and exited **0** having done nothing, while a daemon WAS running
/// and `POST /v1/mesh/leave` worked one hop away. That collapses ARCH §18.2's
/// *never-ran* into *passed* — the caller's script sees success and moves on.
/// No daemon is now a non-zero exit that says so.
async fn cmd_leave(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn mesh leave");
        eprintln!();
        eprintln!("Give up membership in the active mesh and return to a solo mesh.");
        eprintln!("Parked meshes are untouched — use `svrn mesh forget` to drop those.");
        return 0;
    }
    let port = daemon_client_port();
    if !daemon_listening_on(port).await {
        eprintln!("No daemon detected on :{port} — nothing to leave.");
        eprintln!("Start it with `svrn daemon start` if the mesh should be running.");
        return 1;
    }
    let url = format!("http://127.0.0.1:{port}/v1/mesh/leave");
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build HTTP client: {e}");
            return 1;
        }
    };
    match client.post(&url).send().await {
        Ok(r) if r.status().is_success() => {
            println!();
            println!("Left the mesh. This node is now its own solo mesh.");
            println!("The daemon restarts to rebind; give it ~10s.");
            0
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            eprintln!("Failed to leave (daemon returned {status}): {body}");
            1
        }
        Err(e) => {
            eprintln!("Failed to reach running daemon at {url}: {e}");
            1
        }
    }
}

/// `svrn mesh list` — every mesh this node has joined, active one marked.
///
/// Reads disk directly rather than the daemon: the answer is the same either
/// way, and an operator debugging a daemon that will not start still needs to
/// see what it is a member of.
async fn cmd_list(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn mesh list [--json]");
        eprintln!();
        eprintln!("Show every mesh this node has joined. The active one is marked '*'.");
        return 0;
    }
    let root = sovereign_root();
    let active = sovereign_mesh::persist::active_mesh_id(&root);
    let known = sovereign_mesh::persist::list_known(&root);

    if args.iter().any(|a| a == "--json") {
        let rows: Vec<serde_json::Value> = known
            .iter()
            .map(|m| {
                serde_json::json!({
                    "mesh_id": m.mesh_id.to_hex(),
                    "name": m.name,
                    "members_total": m.members.len(),
                    "is_active": active.as_ref() == Some(&m.mesh_id),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).unwrap_or_default()
        );
        return 0;
    }

    if known.is_empty() {
        println!(
            "No meshes joined yet. `svrn mesh create` or paste an invite with `svrn mesh join`."
        );
        return 0;
    }
    println!();
    for m in &known {
        let mark = if active.as_ref() == Some(&m.mesh_id) {
            "*"
        } else {
            " "
        };
        let state = if active.as_ref() == Some(&m.mesh_id) {
            "active"
        } else {
            "parked"
        };
        println!(
            " {mark} {:<28} {:>3} member(s)  {state}",
            m.name,
            m.members.len()
        );
    }
    println!();
    0
}

/// `svrn mesh switch <mesh>` — park the active mesh, bring another up.
async fn cmd_switch(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) || args.is_empty() {
        eprintln!("Usage: svrn mesh switch <mesh-name-or-id>");
        eprintln!();
        eprintln!("Park the active mesh and bring another joined mesh up in its place.");
        eprintln!("Peers on the parked mesh see this node go offline — NOT depart, so");
        eprintln!("switching back later is a resume and needs no invite.");
        eprintln!();
        eprintln!("`svrn mesh list` shows what is joined.");
        return if args.is_empty() { 1 } else { 0 };
    }
    let target = args[0].clone();
    let port = daemon_client_port();
    if !daemon_listening_on(port).await {
        eprintln!("No daemon detected on :{port} — switching needs one.");
        return 1;
    }
    let url = format!("http://127.0.0.1:{port}/v1/mesh/switch");
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build HTTP client: {e}");
            return 1;
        }
    };
    let resp = match client
        .post(&url)
        .json(&serde_json::json!({ "mesh": target }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to reach running daemon at {url}: {e}");
            return 1;
        }
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.is_success() {
        println!();
        println!("Switching to \"{target}\" — the daemon rebinds, give it ~10s.");
        println!("`svrn mesh status` will show the new roster.");
        0
    } else {
        eprintln!("Failed to switch (daemon returned {status}): {body}");
        1
    }
}

/// `svrn mesh forget <mesh>` — drop a PARKED mesh from this node.
async fn cmd_forget(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) || args.is_empty() {
        eprintln!("Usage: svrn mesh forget <mesh-name-or-id>");
        eprintln!();
        eprintln!("Delete a parked mesh's roster and invite key from this node.");
        eprintln!("Refuses on the ACTIVE mesh — switch or leave first.");
        eprintln!();
        eprintln!("Rejoining afterwards needs a fresh invite, since the roster is gone.");
        return if args.is_empty() { 1 } else { 0 };
    }
    let root = sovereign_root();
    let known = sovereign_mesh::persist::list_known(&root);
    let Some(found) = sovereign_mesh::persist::resolve_known(&known, &args[0]) else {
        eprintln!("Not a member of any mesh matching '{}'.", args[0]);
        eprintln!("`svrn mesh list` shows what is joined.");
        return 1;
    };
    match sovereign_mesh::persist::forget(&root, &found.mesh_id) {
        Ok(()) => {
            println!("Forgot \"{}\".", found.name);
            0
        }
        Err(e) => {
            eprintln!("Failed to forget \"{}\": {e}", found.name);
            1
        }
    }
}

async fn cmd_logs() -> i32 {
    println!("(mesh logs are written to stderr when the daemon runs)");
    0
}

/// `svrn mesh fetch-model <name> [--peer <peer-tailnet-addr>] [--out <dir>]`
///
/// Pulls a GGUF from a mesh peer over the tailnet. Used by the
/// friend-onboarding flow (WS5) so a new node doesn't need R2 /
/// S3 credentials of its own — it joins the mesh first, then
/// pulls model files from whoever already has them.
///
/// Discovery order:
///   1. If `--peer host:port` is given, use that directly.
///   2. Otherwise, read the local daemon's mesh.json to find peer
///      `addresses`, try each peer's `/internal/v1/models/list`
///      in turn, return the first peer that advertises `<name>`.
///
/// Destination: defaults to the parent dir of the local `[models]
/// .primary` path (the conventional models dir). `--out <dir>`
/// overrides.
///
/// Integrity: the peer's listing carries a SHA-256; the response
/// stream is hashed on the fly and the file is rejected on
/// mismatch. See `sovereign_mesh::model_fetch::fetch_model_to_dir`.
async fn cmd_fetch_model(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn mesh fetch-model <name> [--peer <host:port>] [--out <dir>]");
        eprintln!();
        eprintln!("Pulls a GGUF from a mesh peer over the tailnet. No R2 credentials required.");
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  svrn mesh fetch-model Darwin-9B-Opus.Q4_K_M.gguf");
        eprintln!("  svrn mesh fetch-model Qwen3-Embedding-0.6B-Q8_0.gguf --out ~/models");
        return 0;
    }

    let Some(name) = args.first().cloned() else {
        eprintln!("Missing model file name.");
        eprintln!("Usage: svrn mesh fetch-model <name> [--peer <host:port>] [--out <dir>]");
        return 1;
    };

    // Tiny manual flag parser — sticks with the existing CLI
    // convention here (no clap on this subcommand).
    let mut peer_override: Option<String> = None;
    let mut out_override: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--peer" => {
                i += 1;
                peer_override = args.get(i).cloned();
            }
            "--out" => {
                i += 1;
                out_override = args.get(i).map(PathBuf::from);
            }
            other => {
                eprintln!("Unknown flag: {other}");
                return 1;
            }
        }
        i += 1;
    }

    // Default dest is the dir holding `cfg.models.primary`. We
    // read SetupConfig from disk rather than the running daemon
    // so this command works even when the daemon's down — handy
    // during friend-onboarding where the daemon might be in its
    // first-boot loop.
    let dest_dir = match out_override {
        Some(p) => p,
        None => match sovereign_core::setup_config::SetupConfig::load() {
            Ok(cfg) => cfg
                .models
                .as_ref()
                .and_then(|m| m.primary.parent())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(".")),
            Err(e) => {
                eprintln!("error: could not load setup config to choose default --out dir: {e}");
                eprintln!(
                    "hint: pass --out <dir> explicitly, or run `svrn daemon --setup-only` first."
                );
                return 1;
            }
        },
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 60)) // 1h cap for very slow links
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: build http client: {e}");
            return 1;
        }
    };

    // Decide which peers to try. Explicit override wins.
    let peer_candidates: Vec<String> = if let Some(p) = peer_override {
        // Accept `host:port` or `http://host:port` — normalise to URL.
        let url = if p.starts_with("http://") || p.starts_with("https://") {
            p
        } else {
            format!("http://{p}")
        };
        vec![url]
    } else {
        match collect_peer_internal_urls().await {
            Ok(urls) if urls.is_empty() => {
                eprintln!("No mesh peers known. Run `svrn mesh join <link>` first,");
                eprintln!("or pass --peer <host:port> to target a specific node.");
                return 1;
            }
            Ok(urls) => urls,
            Err(e) => {
                eprintln!("error: discovering peers: {e}");
                return 1;
            }
        }
    };

    println!();
    println!(
        "Searching {} peer(s) for '{}'…",
        peer_candidates.len(),
        name
    );

    for peer_url in &peer_candidates {
        // Probe the peer's listing first so we can pick the one
        // that actually advertises `name` before committing to
        // the download.
        let listing = match sovereign_mesh::model_fetch::list_peer_files(&client, peer_url).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("  ✗ {peer_url}: list failed ({e})");
                continue;
            }
        };
        let Some(info) = listing.files.into_iter().find(|f| f.name == name) else {
            println!("  · {peer_url}: doesn't have it");
            continue;
        };
        println!(
            "  → {peer_url}: streaming {} ({} MiB, sha256={}…)",
            info.name,
            info.size_bytes / (1024 * 1024),
            &info.sha256[..16],
        );
        let started = std::time::Instant::now();
        let progress = |downloaded: u64, total: u64| {
            let pct = if total > 0 {
                100 * downloaded / total
            } else {
                0
            };
            eprint!(
                "\r  {} / {} MiB ({}%)…   ",
                downloaded / (1024 * 1024),
                total / (1024 * 1024),
                pct,
            );
        };
        match sovereign_mesh::model_fetch::fetch_model_to_dir(
            &client, peer_url, &info, &dest_dir, progress,
        )
        .await
        {
            Ok(path) => {
                let secs = started.elapsed().as_secs_f64();
                let mb_per_s = if secs > 0.0 {
                    (info.size_bytes as f64 / 1_048_576.0) / secs
                } else {
                    0.0
                };
                eprintln!();
                println!();
                println!("✓ saved to {}", path.display());
                println!(
                    "  {} MiB in {:.1}s ({:.1} MiB/s)",
                    info.size_bytes / (1024 * 1024),
                    secs,
                    mb_per_s,
                );
                return 0;
            }
            Err(e) => {
                eprintln!();
                eprintln!("  ✗ {peer_url}: fetch failed ({e})");
                // fall through to next peer
            }
        }
    }

    eprintln!();
    eprintln!("No peer could serve '{name}'.");
    1
}

/// Discover peer internal-port URLs by reading the local daemon's
/// persisted mesh.json. `MemberRecord.addresses` for each peer are
/// the gossip-port endpoints (`:9742`), which is exactly what we
/// want — the model-files routes live on the internal port.
async fn collect_peer_internal_urls() -> std::io::Result<Vec<String>> {
    let mesh_path = sovereign_root().join("mesh.json");
    let bytes = std::fs::read(&mesh_path)?;
    // Parse loosely — we only need the addresses array of each
    // non-self member. Using serde_json::Value avoids dragging in
    // the full Mesh deserialiser, which would force a tight
    // coupling on the on-disk schema this command only inspects.
    let v: serde_json::Value = serde_json::from_slice(&bytes)?;
    let self_id = v.get("self_node_id").and_then(|x| x.as_str()).unwrap_or("");
    let mut urls = Vec::new();
    if let Some(members) = v
        .get("mesh")
        .and_then(|m| m.get("members"))
        .and_then(|m| m.as_object())
    {
        for (nid, member) in members {
            if nid == self_id {
                continue;
            }
            if let Some(addrs) = member.get("addresses").and_then(|a| a.as_array()) {
                for a in addrs {
                    if let Some(s) = a.as_str() {
                        urls.push(format!("http://{}", s));
                    }
                }
            }
        }
    }
    Ok(urls)
}

// ── Corpus subcommand implementations ────────────────────

fn hostname() -> Option<String> {
    // `HOSTNAME` / `COMPUTERNAME` env vars aren't reliably set in
    // GUI-launched or systemd-spawned child processes — notably
    // macOS doesn't export HOSTNAME to `cargo tauri dev`. The
    // `hostname` crate wraps the real `gethostname(2)` syscall and
    // returns something useful on every platform we care about.
    // Strip the `.local` Bonjour suffix so "Alexs-MBP.local" renders
    // cleanly in the mesh roster.
    ::hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|s| s.strip_suffix(".local").map(|t| t.to_string()).unwrap_or(s))
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    /// A model with `n` uniform blocks, an output head, and a token embedding.
    fn model(n: u32, block_gb: u64, head_gb: u64, embd_gb: u64) -> Vec<(String, Option<u32>, u64)> {
        let mut v: Vec<(String, Option<u32>, u64)> = (0..n)
            .map(|i| (format!("blk.{i}.attn_q.weight"), Some(i), block_gb * GB))
            .collect();
        v.push(("output.weight".into(), None, head_gb * GB));
        v.push(("token_embd.weight".into(), None, embd_gb * GB));
        v
    }

    fn input(sizes: Vec<(String, Option<u32>, u64)>, n: u32, devices_gb: Vec<f64>) -> PlanInput {
        let host = devices_gb.len() - 1;
        PlanInput {
            model_name: "test.gguf".into(),
            n_layer: n,
            sizes,
            devices_gb,
            // Default to the single-basis shape: these tests assert on the
            // POSSIBLE allocation, which the top-level report fields carry
            // whether or not a live reading exists. The two-basis tests set this
            // explicitly.
            devices_free_gb: None,
            // No daemon in these tests, so no pin. The pin tests set it.
            block_split_pin: None,
            host,
            headroom: 1.2,
            headroom_from_flag: false,
            // These tests exercise the fit computation, not the speed lookup;
            // `mesh: None` is the `--devices` shape, which is barred from
            // matching a measurement by construction.
            mesh: None,
            n_ctx: 32_768,
            // No llama backend in these tests — weights-only fit, the same
            // fallback the live gate takes when the projection is unavailable.
            overheads: None,
        }
    }

    /// `build_report` against an empty measurement store — the week-1 state, and
    /// the state every fit assertion below cares about.
    fn report(i: PlanInput) -> PlanReport {
        build_report(
            i,
            &sovereign_core::mesh_measurements::MeasurementFile::new(),
            &[],
            "test-build",
        )
    }

    /// The index-space trap, pinned.
    ///
    /// `shard_fits` receives capacities in PLAN order — RPC workers first, host
    /// last — while a row is displayed under the index the operator typed in
    /// `--devices`. The two are different permutations whenever `--host` is not
    /// the last device, and mixing them up attributes every row to the wrong
    /// machine with no error and no visible symptom. This asserts the mapping
    /// end to end: whatever the plan order, each row's capacity is the VRAM of
    /// the device that row NAMES.
    #[test]
    fn each_rows_capacity_belongs_to_the_device_that_row_names() {
        let devices = vec![64.0, 32.0, 16.0];
        for host in 0..devices.len() {
            let mut i = input(model(48, 1, 2, 3), 48, devices.clone());
            i.host = host;
            let r = report(i);
            for row in &r.rows {
                assert_eq!(
                    row.vram(),
                    (devices[row.dev] * GIB) as u64,
                    "host={host}: row for device {} carries another device's capacity",
                    row.dev
                );
            }
            let head_holder = r.rows.iter().find(|d| d.holds_output).expect("a head");
            assert_eq!(
                head_holder.dev, host,
                "host={host}: the output head must land on the host"
            );
        }
    }

    /// The preview's verdict IS the live gate's verdict — same function, same
    /// numbers. Before 2026-07-28 this file had its own fold and its own
    /// comparison, so the two could drift apart silently; a preview that
    /// disagrees with the load it previews is worse than no preview.
    #[test]
    fn the_rows_come_from_the_shared_decider() {
        use sovereign_inference::embedded as inf;
        let sizes = model(48, 1, 2, 3);
        let devices = vec![64.0, 32.0, 32.0];
        let r = report(input(sizes.clone(), 48, devices.clone()));

        // Rebuild the same inputs the live load would hand `shard_fits`.
        let mass = inf::model_mass_from_sizes(&sizes, 48);
        let vram: Vec<u64> = devices.iter().map(|&g| (g * GIB) as u64).collect();
        let host = devices.len() - 1;
        let mut order: Vec<usize> = (0..vram.len()).filter(|&d| d != host).collect();
        order.push(host);
        let weights: Vec<f32> = order
            .iter()
            .map(|&d| inf::quantize_vram(vram[d]) as f32)
            .collect();
        let plan = inf::plan_shards_weighted(48, &weights, &mass.block_bytes, mass.head_bytes);
        let capacities: Vec<u64> = order.iter().map(|&d| vram[d]).collect();
        let fits = inf::shard_fits(&plan, &capacities, &mass, 1.2, None).expect("judgeable");

        for (pos, &d) in order.iter().enumerate() {
            let row = r
                .rows
                .iter()
                .find(|x| x.dev == d)
                .expect("a row per device");
            assert_eq!(row.fit, fits[pos], "device {d} disagrees with shard_fits");
        }
    }

    /// The defect the aggregate gate cannot see: pooled memory is ample, yet one
    /// device's own share does not fit it. The preview surfaced this first; as
    /// of 2026-07-28 the live load refuses on it too, through this same
    /// `shard_fits` call.
    ///
    /// The mechanism is `quantize_vram`'s 4 GiB bucket floor — a 2 GB device is
    /// weighted as though it had 4 GiB, so the split hands it roughly twice the
    /// mass it can hold. Aggregate arithmetic cannot see this, which is exactly
    /// why the per-device pass exists.
    #[test]
    fn a_small_device_overflows_while_the_pooled_gate_passes() {
        let r = report(input(model(300, 2, 0, 0), 300, vec![2.0, 1000.0]));
        assert!(
            r.gate_pass,
            "1002 GB pooled against a 600 GB model must clear the aggregate gate"
        );
        let overflows = r.overflows();
        assert_eq!(
            overflows.len(),
            1,
            "exactly the small device should overflow"
        );
        assert_eq!(overflows[0].dev, 0);
        assert!(overflows[0].need() > overflows[0].vram());
        assert_eq!(
            r.exit_code(),
            1,
            "a per-device overflow is not a passing plan"
        );
    }

    /// Every byte of the model is charged to exactly one device. If this drifts,
    /// the fit verdict is meaningless in a way no other assertion would catch.
    #[test]
    fn every_block_is_charged_exactly_once() {
        let r = report(input(model(48, 1, 2, 3), 48, vec![64.0, 32.0, 32.0]));
        let charged: u64 = r.rows.iter().map(|d| d.weight()).sum();
        assert_eq!(
            charged,
            48 * GB + 2 * GB,
            "block mass plus the output head, and nothing else"
        );
        // token_embd is host system RAM, never a device's share.
        assert_eq!(r.embd_bytes, 3 * GB);
        assert_eq!(r.total_weight, 48 * GB + 2 * GB + 3 * GB);
    }

    /// The output head rides the host, and the host is charged for it.
    #[test]
    fn the_host_is_charged_for_the_output_head() {
        let r = report(input(model(48, 1, 8, 0), 48, vec![64.0, 64.0]));
        let holder = r
            .rows
            .iter()
            .find(|d| d.holds_output)
            .expect("someone holds the head");
        assert!(holder.is_host, "the head belongs to the host");
        let blocks_only: u64 = holder
            .blocks
            .map(|(a, b)| (a..=b).count() as u64 * GB)
            .unwrap_or(0);
        assert_eq!(holder.weight(), blocks_only + 8 * GB);
    }

    /// `--host` moves both the head and the star in the table.
    #[test]
    fn the_host_index_selects_which_device_holds_the_head() {
        let mut i = input(model(48, 1, 4, 0), 48, vec![64.0, 64.0, 64.0]);
        i.host = 0;
        let r = report(i);
        assert!(r.rows[0].is_host && r.rows[0].holds_output);
        assert!(!r.rows[1].holds_output && !r.rows[2].holds_output);
        assert!(render_human(&r).contains("*   0  host"));
    }

    /// Need is `weight × headroom`, using the same truncating cast the table
    /// prints — so the gate and the displayed number agree at the boundary.
    #[test]
    fn need_is_weight_times_headroom_exactly() {
        let mut i = input(model(48, 1, 0, 0), 48, vec![512.0]);
        i.headroom = 1.35;
        let r = report(i);
        let d = &r.rows[0];
        assert_eq!(d.need(), (d.weight() as f64 * 1.35) as u64);
    }

    #[test]
    fn a_cluster_too_small_fails_the_aggregate_gate() {
        let r = report(input(model(48, 1, 0, 0), 48, vec![2.0, 2.0]));
        assert!(!r.gate_pass);
        assert_eq!(r.exit_code(), 1);
        assert!(render_human(&r).contains("FAIL — cluster too small"));
    }

    #[test]
    fn a_comfortable_fit_passes_both_gates() {
        let r = report(input(model(48, 1, 2, 1), 48, vec![256.0, 256.0]));
        assert!(r.gate_pass);
        assert!(r.overflows().is_empty());
        assert_eq!(r.exit_code(), 0);
        assert!(render_human(&r).contains("Per-device:     all devices fit ok"));
    }

    /// A dense model reports no MoE section; a routed-expert model does, and the
    /// expert mass is counted as cold rather than as per-token work.
    #[test]
    fn moe_is_reported_only_when_routed_experts_exist() {
        let dense = report(input(model(48, 1, 0, 0), 48, vec![256.0]));
        assert!(dense.moe.is_none());
        assert!(!render_human(&dense).contains("MoE:"));

        let mut sizes = model(48, 1, 0, 0);
        sizes.push(("blk.0.ffn_gate_exps.weight".into(), Some(0), 90 * GB));
        let moe = report(input(sizes, 48, vec![512.0]));
        let m = moe.moe.as_ref().expect("routed experts make this an MoE");
        assert_eq!(m.routed_expert_bytes, 90 * GB);
        assert_eq!(
            m.hot_bytes,
            (48 + 90) * GB - 90 * GB,
            "hot mass excludes the cold experts"
        );
        assert!(render_human(&moe).contains("MoE:"));
    }

    /// Uniform mass says heterogeneous VRAM is safe; skewed mass warns instead.
    #[test]
    fn block_mass_spread_drives_the_uniformity_verdict() {
        let uniform = report(input(model(48, 1, 0, 0), 48, vec![256.0]));
        assert!(uniform.block_mass.uniform);
        assert!(render_human(&uniform).contains("UNIFORM mass"));

        let mut skewed = model(48, 1, 0, 0);
        skewed[0].2 = 40 * GB;
        let r = report(input(skewed, 48, vec![256.0]));
        assert!(!r.block_mass.uniform);
        assert!(r.block_mass.spread > 1.15);
        assert!(render_human(&r).contains("NON-UNIFORM mass"));
    }

    /// Fewer nodes means fewer per-token hops — reported as a cost, never as a
    /// recommendation, because on a bandwidth-bound host offloading can still win.
    #[test]
    fn the_hop_count_follows_the_node_count_without_claiming_a_winner() {
        let r = report(input(model(48, 1, 0, 0), 48, vec![256.0, 256.0, 256.0]));
        assert_eq!(r.nodes.active_nodes, 3);
        assert_eq!(r.nodes.hops_now, 2);
        assert_eq!(r.nodes.min_nodes, 1, "one 256 GB node holds a 48 GB model");
        let out = render_human(&r);
        assert!(out.contains("3 holding blocks → 2 network hops per token"));
        assert!(
            out.contains("Measure both."),
            "the advisor must not claim fewer nodes is always faster"
        );
    }

    /// The headroom line distinguishes a what-if from the value the load will use.
    #[test]
    fn headroom_source_is_reported_honestly() {
        let mut i = input(model(48, 1, 0, 0), 48, vec![256.0]);
        i.headroom_from_flag = true;
        let flagged = report(i);
        assert!(render_human(&flagged).contains("WHAT-IF"));
        assert_eq!(render_json(&flagged)["headroom_source"], "flag");

        let configured = report(input(model(48, 1, 0, 0), 48, vec![256.0]));
        assert!(render_human(&configured).contains("matches the load's configured headroom"));
        assert_eq!(render_json(&configured)["headroom_source"], "config");
    }

    /// The JSON contract every scripted consumer reads.
    #[test]
    fn render_json_carries_the_whole_device_table() {
        let r = report(input(model(48, 1, 2, 1), 48, vec![64.0, 32.0, 32.0]));
        let j = render_json(&r);
        assert_eq!(j["blocks"], 48);
        assert!(j["aggregate_gate_pass"].as_bool().unwrap());
        assert_eq!(j["moe"], serde_json::Value::Null);
        let devices = j["devices"].as_array().expect("a row per device");
        assert_eq!(devices.len(), 3);
        for d in devices {
            for k in [
                "device",
                "role",
                "vram_gb",
                "blocks",
                "block_count",
                "holds_output",
                "weight_gb",
                "need_gb",
                "fits",
            ] {
                assert!(
                    !d[k].is_null() || k == "blocks",
                    "device row is missing {k}"
                );
            }
        }
        assert_eq!(j["devices"][2]["role"], "host");
    }

    /// A device that gets no blocks holds nothing and cannot manufacture a
    /// refusal.
    #[test]
    fn a_device_with_no_blocks_holds_nothing_and_fits() {
        let r = report(input(model(2, 1, 0, 0), 2, vec![64.0, 64.0, 64.0]));
        for d in r.rows.iter().filter(|d| d.blocks.is_none()) {
            assert_eq!(d.weight(), 0);
            assert!(d.fits());
        }
    }

    // --- the speed section ------------------------------------------------

    use sovereign_core::mesh_measurements as mm;

    fn mesh_devs(names: &[&str], fp: Option<u64>) -> Vec<MeshDevice> {
        mesh_devs_linked(names, fp, Some(mm::LinkClass::Direct))
    }

    /// `mesh_devs` with the link spelled out, for the tests that turn on it.
    fn mesh_devs_linked(
        names: &[&str],
        fp: Option<u64>,
        link: Option<mm::LinkClass>,
    ) -> Vec<MeshDevice> {
        names
            .iter()
            .map(|n| MeshDevice {
                name: (*n).into(),
                vram_gb: 64.0,
                // No live reading — the speed tests are about identity and link
                // class, not capacity. `two_capacity_*` covers the live basis.
                free_vram_gb: None,
                hw_fingerprint: fp,
                backend: Some("vulkan".into()),
                link,
            })
            .collect()
    }

    fn live(mesh: Vec<MeshDevice>) -> PlanInput {
        let devices_gb = mesh.iter().map(|d| d.vram_gb).collect::<Vec<_>>();
        let mut i = input(model(48, 1, 2, 0), 48, devices_gb);
        i.mesh = Some(mesh);
        i
    }

    // --- two capacities ----------------------------------------------------
    //
    // These pin the shape Alex asked for on 2026-07-29: report what is POSSIBLE
    // and what is SAFE NOW, name the gap, and never silently pick one. The
    // regression they exist to prevent is subtler than a wrong number — it is a
    // plan that predicts a cut the loader would not run, and therefore looks up a
    // measurement key nothing can ever be filed under.

    /// Devices with BOTH capacities spelled out: `(name, total_gb, free_gb)`.
    /// `None` free = no live reading for that device.
    fn devs_with_free(spec: &[(&str, f64, Option<f64>)]) -> Vec<MeshDevice> {
        spec.iter()
            .map(|(name, total, free)| MeshDevice {
                name: (*name).into(),
                vram_gb: *total,
                free_vram_gb: *free,
                hw_fingerprint: Some(7),
                backend: Some("vulkan".into()),
                link: Some(mm::LinkClass::Direct),
            })
            .collect()
    }

    /// The live two-basis shape: worker(s) first, host last.
    fn live_two(mesh: Vec<MeshDevice>) -> PlanInput {
        let devices_gb = mesh.iter().map(|d| d.vram_gb).collect::<Vec<_>>();
        let free = mesh
            .iter()
            .map(|d| d.free_vram_gb)
            .collect::<Option<Vec<_>>>();
        let mut i = input(model(48, 1, 2, 0), 48, devices_gb);
        i.devices_free_gb = free;
        i.mesh = Some(mesh);
        i
    }

    /// The measured mesh, as it actually stood on 2026-07-29: a 51 GB worker with
    /// most of its memory held by an outgoing generation, and a 124 GB host.
    fn the_real_mesh() -> Vec<MeshDevice> {
        devs_with_free(&[
            ("beefymac", 51.0, Some(19.5)),
            ("ruggedfox", 124.0, Some(110.0)),
        ])
    }

    /// The two bases produce DIFFERENT cuts, and the report says so rather than
    /// presenting one as the plan.
    #[test]
    fn two_capacities_cut_the_model_differently_and_the_gap_is_named() {
        let r = report(live_two(the_real_mesh()));
        let sn = r.safe_now.as_ref().expect("a live basis");

        let blocks = |a: &[DeviceRow]| -> Vec<u32> {
            let mut v: Vec<u32> = a
                .iter()
                .map(|d| d.blocks.map(|(x, y)| y - x + 1).unwrap_or(0))
                .collect();
            v.reverse(); // host last → worker share first
            v
        };
        assert_ne!(
            blocks(&r.rows),
            blocks(&sn.rows),
            "51 GB total vs 19.5 GB free must apportion the worker differently — \
             if these ever match, the test mesh stopped exercising the bug"
        );
        // The worker holds LESS on the live basis, because it has less room.
        let worker_possible = r.rows.iter().find(|d| !d.is_host).unwrap();
        let worker_safe = sn.rows.iter().find(|d| !d.is_host).unwrap();
        assert!(
            worker_safe.blocks.map(|(x, y)| y - x + 1).unwrap_or(0)
                < worker_possible.blocks.map(|(x, y)| y - x + 1).unwrap_or(0),
            "the busy worker must be given a smaller share, not a larger one"
        );

        let out = render_human(&r);
        assert!(out.contains("Two capacities"), "both bases must be shown");
        assert!(out.contains("possible (device total)"));
        assert!(out.contains("safe now (live free)"));
        assert!(
            out.contains("held by other work right now"),
            "the gap must be NAMED, not left for the operator to subtract: {out}"
        );
        assert!(
            out.contains("Different cut"),
            "a differing cut must be called out: {out}"
        );
    }

    /// THE regression this change exists for.
    ///
    /// A run is filed under the cut the loader EXECUTED (the live-free basis). The
    /// plan must find it. Before 2026-07-30 the plan keyed on the totals basis,
    /// predicted a different split, and reported "not measured" about a
    /// configuration it held a real number for.
    #[test]
    fn speed_is_looked_up_under_the_cut_the_loader_would_execute() {
        // The key the plan now queries.
        let probe = report(live_two(the_real_mesh()));
        let executed_key = probe.speed_key.clone().expect("key");

        // Sanity: that key is NOT the one the totals basis would have produced.
        // Without this, the test could pass while both bases agreed.
        let totals_only = {
            let mut i = live_two(the_real_mesh());
            i.devices_free_gb = None;
            report(i)
        };
        assert_ne!(
            executed_key.placement_digest,
            totals_only.speed_key.expect("key").placement_digest,
            "the two bases must key differently, or this test proves nothing"
        );

        let mut file = mm::MeasurementFile::new();
        mm::record(
            &mut file,
            mm::MeasurementRecord {
                witness: None,
                conditions: None,
                key: executed_key,
                decode_tok_s: 10.48,
                decode_tok_s_min: 10.27,
                decode_tok_s_max: 10.52,
                ttft_ms: 2444.0,
                itl_p50_ms: 72.9,
                itl_p95_ms: 158.1,
                prefill_tok_s: Some(13.0),
                cold_load_s: None,
                trials: 3,
                content_frames: 170,
                model_name: "test.gguf".into(),
                placement_human: "36 local + 12 @beefymac".into(),
                nodes: 2,
                hops: 1,
                measured_at: 1_753_500_000,
                build: "test-build".into(),
                backend: Some("vulkan".into()),
                link_rtt_ms: Some(0.4),
                verdict: mm::Verdict::Valid,
            },
        );

        let r = build_report(live_two(the_real_mesh()), &file, &[], "test-build");
        assert!(
            matches!(r.speed, SpeedSection::Measured { .. }),
            "a record filed under the executed cut must be FOUND, not reported as a near miss"
        );
        assert!(render_human(&r).contains("10.5 tok/s decode"));
    }

    /// A model the hardware can hold, that cannot load this second, is reported as
    /// exactly that — and does NOT fail the command. A busy device is not a wrong
    /// plan; conflating the two is how one number came to mix two defects.
    #[test]
    fn fits_the_hardware_but_not_right_now() {
        // 48 GB of blocks + a 2 GB head. Ample on totals; not against 12 GB free.
        let r = report(live_two(devs_with_free(&[
            ("beefymac", 51.0, Some(2.0)),
            ("ruggedfox", 124.0, Some(12.0)),
        ])));

        assert!(
            r.gate_pass && r.overflows().is_empty(),
            "possible basis fits"
        );
        let sn = r.safe_now.as_ref().expect("a live basis");
        assert!(!sn.fits(), "the live basis must refuse");
        assert_eq!(
            r.exit_code(),
            0,
            "exit code follows POSSIBLE — a transient residual does not make the plan wrong"
        );

        let out = render_human(&r);
        assert!(
            out.contains("FITS this hardware but will NOT load right now"),
            "the operator must be told which of the two problems they have: {out}"
        );
        assert!(
            out.contains("Free the"),
            "the repair is to free memory, not to buy VRAM: {out}"
        );

        let j = render_json(&r);
        assert_eq!(j["aggregate_gate_pass"], true);
        assert_eq!(j["safe_now"]["fits"], false);
    }

    /// One device without a live reading means there is no coherent live basis.
    /// Mixing free and total readings would invent a third cut matching nothing.
    #[test]
    fn a_partial_live_reading_yields_no_live_basis() {
        let r = report(live_two(devs_with_free(&[
            ("beefymac", 51.0, None),
            ("ruggedfox", 124.0, Some(110.0)),
        ])));
        assert!(
            r.safe_now.is_none(),
            "partial knowledge must not be averaged into a plausible-looking basis"
        );
        let out = render_human(&r);
        assert!(out.contains("Safe now:       UNKNOWN"), "{out}");
        assert!(render_json(&r)["safe_now"].is_null());
    }

    /// A pin overrides BOTH capacity bases, and the plan says so instead of
    /// presenting a VRAM-derived cut that will not load.
    ///
    /// This is the real 2026-07-29 configuration: `SOVEREIGN_RPC_BLOCK_SPLIT=12,36`
    /// pinned in a systemd drop-in since 2026-07-27, against a mesh whose
    /// capacities apportion 14/34.
    #[test]
    fn a_pinned_split_overrides_capacity_and_is_named() {
        let mut i = live_two(the_real_mesh());
        i.block_split_pin = Some("12,36".into());
        let r = report(i);

        let p = r.pinned.as_ref().expect("a valid pin applies");
        let n = |a: &[DeviceRow], dev: usize| -> u32 {
            a.iter()
                .find(|d| d.dev == dev)
                .and_then(|d| d.blocks.map(|(x, y)| y - x + 1))
                .unwrap_or(0)
        };
        assert_eq!(
            (n(&p.rows, 0), n(&p.rows, 1)),
            (12, 36),
            "the pin is obeyed"
        );
        // The derived cut depends on the model's mass profile (the real 122B gives
        // 14/34; this synthetic uniform model gives 15/33). What must hold on ANY
        // model is that capacity did NOT choose the pinned cut — otherwise the two
        // agree by luck and this test would pass while proving nothing.
        assert_ne!(
            (n(&r.rows, 0), n(&r.rows, 1)),
            (12, 36),
            "capacity must not have independently chosen the pinned cut"
        );

        let out = render_human(&r);
        assert!(out.contains("PINNED SPLIT"), "{out}");
        assert!(out.contains("SOVEREIGN_RPC_BLOCK_SPLIT=12,36"), "{out}");
        assert!(
            out.contains("NOT the VRAM-derived cut"),
            "the operator must be told the table above is not what loads: {out}"
        );

        let j = render_json(&r);
        assert_eq!(j["pinned"]["is_executed_cut"], true);
        assert_eq!(
            j["safe_now"]["is_executed_cut"], false,
            "a pin outranks the live-free basis"
        );
    }

    /// Speed is keyed on the PINNED cut, because that is what the loader runs.
    /// Without this the plan queries a key no run can ever file under — the exact
    /// failure that made the 10.48 tok/s two-node record unquotable.
    #[test]
    fn speed_is_keyed_on_the_pinned_cut() {
        let pinned_input = || {
            let mut i = live_two(the_real_mesh());
            i.block_split_pin = Some("12,36".into());
            i
        };
        let probe = report(pinned_input());
        let pinned_key = probe.speed_key.clone().expect("key");

        // The derived bases must key differently, or this proves nothing.
        assert_ne!(
            pinned_key.placement_digest,
            report(live_two(the_real_mesh()))
                .speed_key
                .expect("key")
                .placement_digest,
            "12/36 and 14/34 must hash differently"
        );

        let mut file = mm::MeasurementFile::new();
        mm::record(
            &mut file,
            mm::MeasurementRecord {
                witness: None,
                conditions: None,
                key: pinned_key,
                decode_tok_s: 10.48,
                decode_tok_s_min: 10.27,
                decode_tok_s_max: 10.52,
                ttft_ms: 2444.0,
                itl_p50_ms: 72.9,
                itl_p95_ms: 158.1,
                prefill_tok_s: Some(13.0),
                cold_load_s: None,
                trials: 3,
                content_frames: 170,
                model_name: "test.gguf".into(),
                placement_human: "36 local + 12 @beefymac".into(),
                nodes: 2,
                hops: 1,
                measured_at: 1_753_500_000,
                build: "test-build".into(),
                backend: Some("vulkan".into()),
                link_rtt_ms: Some(0.4),
                verdict: mm::Verdict::Valid,
            },
        );

        let r = build_report(pinned_input(), &file, &[], "test-build");
        assert!(
            matches!(r.speed, SpeedSection::Measured { .. }),
            "a record filed under the pinned cut must be FOUND"
        );
        assert!(render_human(&r).contains("10.5 tok/s decode"));
    }

    /// A pin the LOADER would reject must be rejected here identically, and named
    /// as having no effect — an operator who set it believes it is in force.
    #[test]
    fn an_invalid_pin_is_refused_not_repaired() {
        let mut i = live_two(the_real_mesh());
        i.block_split_pin = Some("10,10".into()); // sums to 20, not 48
        let r = report(i);

        assert!(r.pinned.is_none(), "a non-tiling pin must not be applied");
        let out = render_human(&r);
        assert!(out.contains("does NOT apply"), "{out}");
        assert!(
            out.contains("having no effect"),
            "the operator must learn the pin is inert: {out}"
        );
        assert!(render_json(&r)["pinned"].is_null());
        assert_eq!(render_json(&r)["block_split_pin"], "10,10");
    }

    /// An idle mesh has no gap, and the report says that plainly instead of
    /// printing a zero-width table nobody can read.
    #[test]
    fn an_idle_mesh_reports_no_gap() {
        let r = report(live_two(devs_with_free(&[
            ("beefymac", 51.0, Some(51.0)),
            ("ruggedfox", 124.0, Some(124.0)),
        ])));
        let sn = r.safe_now.as_ref().expect("a live basis");
        assert_eq!(sn.pooled, r.pooled, "identical capacities → identical pool");
        let out = render_human(&r);
        assert!(out.contains("No gap"), "{out}");
        assert!(
            !out.contains("Different cut"),
            "identical capacities cannot cut differently: {out}"
        );
    }

    /// A single-node plan has no link, and must key as `Local` rather than
    /// inheriting whatever the host's own row happens to say.
    #[test]
    fn a_single_node_plan_keys_as_local() {
        let mesh = mesh_devs(&["ruggedfox"], Some(7));
        let mut i = input(
            model(48, 1, 2, 0),
            48,
            mesh.iter().map(|d| d.vram_gb).collect(),
        );
        i.host = 0;
        i.mesh = Some(mesh);
        let r = report(i);
        assert_eq!(
            r.speed_key.expect("live mesh key").link,
            mm::LinkClass::Local,
            "no workers means no link to classify"
        );
    }

    /// A peer carrying blocks that discovery has NOT found a worker for cannot
    /// be attributed. The plan must not assume the good case: `Unknown` keys
    /// never match, so the reader is told "not measured" instead of being shown
    /// a direct-link number for a placement that might tunnel.
    #[test]
    fn a_peer_with_no_discovered_worker_makes_the_link_unknown() {
        let mesh = mesh_devs_linked(&["beefymac", "ruggedfox"], Some(7), None);
        let mut i = input(
            model(48, 1, 2, 0),
            48,
            mesh.iter().map(|d| d.vram_gb).collect(),
        );
        i.host = 1;
        i.mesh = Some(mesh);
        let r = report(i);
        let key = r.speed_key.expect("live mesh key");
        assert_eq!(key.link, mm::LinkClass::Unknown);
        // `lookup` refuses an Unknown link outright — proven against a stored
        // record in `mesh_measurements::an_unknown_link_never_matches_even_another_unknown`.
        assert!(mm::lookup(&mm::MeasurementFile::new(), &key, "0.0.0").is_none());
    }

    /// The same plan over a tunnel and over a direct link are different
    /// questions, and must not share an answer.
    #[test]
    fn the_link_is_the_only_difference_and_it_still_changes_the_key() {
        let plan_over = |link: mm::LinkClass| {
            let mesh = mesh_devs_linked(&["beefymac", "ruggedfox"], Some(7), Some(link));
            let mut i = input(
                model(48, 1, 2, 0),
                48,
                mesh.iter().map(|d| d.vram_gb).collect(),
            );
            i.host = 1;
            i.mesh = Some(mesh);
            report(i).speed_key.expect("live mesh key")
        };
        let direct = plan_over(mm::LinkClass::Direct);
        let tunnel = plan_over(mm::LinkClass::Tunnel);

        assert_eq!(
            direct.placement_digest, tunnel.placement_digest,
            "same machines, same split — the digest cannot tell these apart"
        );
        assert_eq!(direct.host_hw_fingerprint, tunnel.host_hw_fingerprint);
        assert_eq!(direct.n_ctx, tunnel.n_ctx);
        assert_ne!(direct, tunnel, "…but the key must, via the link");
    }

    /// An idle peer must not change the key.
    ///
    /// A machine apportioned no blocks is not part of the placement — it changes
    /// nothing about how the model decodes. If it entered the digest, a
    /// measurement taken today would stop matching the moment an unrelated peer
    /// came online, and `mesh bench` (which builds its shards from what the
    /// daemon reports is *loaded*, and so has no idle device to report) could
    /// never produce a key this side would look up.
    #[test]
    fn a_peer_holding_no_blocks_does_not_enter_the_digest() {
        // One block cannot be spread, so every device past the block-holder is
        // idle however much memory it advertises. (Shrinking a device does NOT
        // idle it: `quantize_vram` floors at one 4 GiB bucket, so a nominally
        // tiny peer still gets a share.)
        //
        // The device NAMES are ordered so that the same machine — beefymac —
        // ends up holding the block in both plans. That is deliberate and it is
        // the whole subtlety: adding a device changes the apportionment, so
        // "the same plan plus an idle peer" is not something you get by
        // appending a device. What is being asserted is narrower and true: two
        // plans in which the same machine holds the same blocks digest the same,
        // however many idle machines stand alongside.
        let plan_with = |names: &[&str], host: usize| {
            let mesh = mesh_devs(names, Some(7));
            let mut i = input(
                model(1, 4, 2, 0),
                1,
                mesh.iter().map(|d| d.vram_gb).collect(),
            );
            i.host = host;
            i.mesh = Some(mesh);
            report(i)
        };

        let two = plan_with(&["beefymac", "ruggedfox"], 1);
        let three = plan_with(&["idlepeer", "ruggedfox", "beefymac"], 1);

        let holder = |r: &PlanReport| {
            let row = r
                .rows
                .iter()
                .find(|d| d.blocks.is_some())
                .expect("a holder");
            (row.dev, row.blocks, row.holds_output)
        };
        assert_eq!(holder(&two).1, holder(&three).1, "same blocks…");
        assert_eq!(
            three.rows.iter().filter(|r| r.blocks.is_none()).count(),
            2,
            "…and the three-device plan must really have two idle devices, or \
             this proves nothing"
        );
        assert_eq!(
            two.speed_key.expect("two-device key").placement_digest,
            three.speed_key.expect("three-device key").placement_digest,
        );
    }

    /// Why the filter above is needed at all: an idle shard, if it reached the
    /// digest, would change it — and `mesh bench` builds its shards from what
    /// the daemon reports is LOADED, so it has no idle device to contribute and
    /// could never reproduce such a key.
    #[test]
    fn an_idle_shard_would_change_the_digest_if_it_reached_it() {
        let held = mm::PlacementShard {
            node_key: "beefymac".into(),
            hw: Some(0xF0F),
            blocks: Some((0, 47)),
            holds_output: true,
        };
        let idle = mm::PlacementShard {
            node_key: "idlepeer".into(),
            hw: Some(0xF0F),
            blocks: None,
            holds_output: false,
        };
        assert_ne!(
            mm::placement_digest("local", 48, &[held.clone()]),
            mm::placement_digest("local", 48, &[held, idle]),
        );
    }

    /// Any tokens-per-second figure carrying an actual number.
    ///
    /// Deliberately not a bare `contains("tok/s")`: the hops advisor legitimately
    /// says "Net tok/s depends on the host", which is prose about a tradeoff, not
    /// a claim about this mesh. What must never appear unmeasured is a *number*.
    fn quotes_a_rate(s: &str) -> bool {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|w| w[1].starts_with("tok/s") && w[0].chars().any(|c| c.is_ascii_digit()))
    }

    /// `--devices` describes hardware that is not here, so no measurement can
    /// apply to it. Barred by construction, not by a runtime check.
    #[test]
    fn a_hypothetical_mesh_is_not_measurable() {
        let r = report(input(model(48, 1, 2, 0), 48, vec![64.0, 64.0]));
        assert!(matches!(
            r.speed,
            SpeedSection::NotMeasurable(NotMeasurable::HypotheticalDevices)
        ));
        assert!(r.speed_key.is_none(), "no key exists for absent hardware");

        let j = render_json(&r);
        assert_eq!(j["speed"]["status"], "not_measurable");
        assert_eq!(j["speed"]["reason"], "hypothetical-devices");
        assert!(j["speed"]["key"].is_null());
        assert!(render_human(&r).contains("not measurable"));
    }

    /// A host that advertises no fingerprint gets an honest refusal rather than
    /// a placeholder key that would collide every unidentified machine.
    #[test]
    fn an_unidentified_host_is_not_measurable() {
        let r = report(live(mesh_devs(&["beefymac", "ruggedfox"], None)));
        assert!(matches!(
            r.speed,
            SpeedSection::NotMeasurable(NotMeasurable::HostUnidentified)
        ));
        assert_eq!(render_json(&r)["speed"]["reason"], "host-unidentified");
    }

    /// The host knows what it is; the peer holding half the model does not.
    ///
    /// Keying on the peer's *name* alone would be the blind spot this field
    /// closed: swap that machine's GPU, keep its name, and every number it ever
    /// filed keeps answering. The refusal names the machine to go upgrade,
    /// because the repair is on a different box than the one being asked.
    #[test]
    fn a_peer_without_a_fingerprint_is_not_measurable_and_is_named() {
        let mut devs = mesh_devs(&["beefymac", "ruggedfox"], Some(7));
        devs[0].hw_fingerprint = None;
        let r = report(live(devs));
        assert!(
            matches!(
                &r.speed,
                SpeedSection::NotMeasurable(NotMeasurable::PeerUnidentified { name })
                    if name == "beefymac"
            ),
            "an unidentifiable peer must not be keyed on its name alone"
        );
        assert_eq!(
            render_json(&r)["speed"]["reason"],
            "peer-unidentified:beefymac"
        );
        let human = render_human(&r);
        assert!(human.contains("beefymac"), "name the machine: {human}");
        assert!(
            !quotes_a_rate(&human),
            "nothing may quote a rate for a placement it cannot attribute: {human}"
        );
    }

    /// The consumer half of the agreement, with a peer's hardware in play:
    /// `mesh plan` must build the digest `mesh bench` files under, or every
    /// record is written to a key nothing ever looks up.
    ///
    /// This is the distributed counterpart to
    /// `a_solo_bench_and_a_solo_plan_agree_on_the_digest` in `mesh_bench`.
    #[test]
    fn a_peers_hardware_reaches_the_digest_the_bench_would_file_under() {
        let with_beefy = mm::placement_digest(
            "distributed",
            48,
            &[
                mm::PlacementShard {
                    node_key: "beefymac".into(),
                    hw: Some(7),
                    blocks: Some((0, 11)),
                    holds_output: false,
                },
                mm::PlacementShard {
                    node_key: "ruggedfox".into(),
                    hw: Some(7),
                    blocks: Some((12, 47)),
                    holds_output: true,
                },
            ],
        );
        let after_gpu_swap = mm::placement_digest(
            "distributed",
            48,
            &[
                mm::PlacementShard {
                    node_key: "beefymac".into(),
                    hw: Some(8),
                    blocks: Some((0, 11)),
                    holds_output: false,
                },
                mm::PlacementShard {
                    node_key: "ruggedfox".into(),
                    hw: Some(7),
                    blocks: Some((12, 47)),
                    holds_output: true,
                },
            ],
        );
        assert_ne!(
            with_beefy, after_gpu_swap,
            "same name, same split, different silicon — a different measurement"
        );
    }

    /// An identified mesh with nothing recorded says so, and offers the command.
    #[test]
    fn an_identified_mesh_with_no_record_says_not_measured() {
        let r = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))));
        let SpeedSection::NotMeasured { near } = &r.speed else {
            panic!("expected NotMeasured");
        };
        assert!(near.is_empty(), "an empty store has no near misses");

        let k = r.speed_key.as_ref().expect("an identified mesh has a key");
        assert_eq!(k.n_ctx, 32_768);
        assert_eq!(k.host_hw_fingerprint, 7);
        assert!(k.model_fingerprint.starts_with("mf1:"));
        // pd2 since 2026-07-29: the shard hashes each machine's hardware, not
        // just its name. This assertion is why the label and the construction
        // cannot drift apart unnoticed — it caught exactly that during the bump.
        assert!(k.placement_digest.starts_with("pd2:"));

        let out = render_human(&r);
        assert!(out.contains("not measured for this configuration"));
        assert!(out.contains("svrn mesh bench"));
        assert_eq!(render_json(&r)["speed"]["status"], "not_measured");
    }

    /// THE guard for week 1: with no measurement, no rate is quoted anywhere,
    /// and every numeric field is null rather than zero. Zero is a number a
    /// consumer will divide by; null is an absence it has to handle.
    #[test]
    fn no_rate_is_quoted_and_no_numeric_is_zero_when_unmeasured() {
        for r in [
            report(input(model(48, 1, 2, 0), 48, vec![64.0, 64.0])),
            report(live(mesh_devs(&["a", "b"], Some(7)))),
            report(live(mesh_devs(&["a", "b"], None))),
        ] {
            let out = render_human(&r);
            assert!(
                !quotes_a_rate(&out),
                "an unmeasured plan quoted a rate:\n{out}"
            );
            let s = &render_json(&r)["speed"];
            for k in [
                "decode_tok_s",
                "decode_tok_s_min",
                "decode_tok_s_max",
                "ttft_ms",
                "itl_p50_ms",
                "itl_p95_ms",
                "prefill_tok_s",
                "runs",
                "measured_at",
                "measured_build",
                "stale",
            ] {
                assert!(s[k].is_null(), "speed.{k} must be null, not a value");
            }
        }
    }

    /// A record filed under this exact configuration is served, and the whole
    /// block is populated.
    #[test]
    fn a_matching_record_is_served_back() {
        let probe = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))));
        let key = probe.speed_key.clone().expect("key");

        let mut file = mm::MeasurementFile::new();
        mm::record(
            &mut file,
            mm::MeasurementRecord {
                witness: None,
                conditions: None,
                key,
                decode_tok_s: 14.1,
                decode_tok_s_min: 13.9,
                decode_tok_s_max: 14.2,
                ttft_ms: 910.0,
                itl_p50_ms: 71.0,
                itl_p95_ms: 79.0,
                prefill_tok_s: None,
                cold_load_s: Some(112.3),
                trials: 3,
                content_frames: 256,
                model_name: "test.gguf".into(),
                placement_human: "36 local + 12 @beefymac".into(),
                nodes: 2,
                hops: 1,
                measured_at: 1_753_500_000,
                build: "test-build".into(),
                backend: Some("vulkan".into()),
                link_rtt_ms: Some(0.4),
                verdict: mm::Verdict::Valid,
            },
        );

        let r = build_report(
            live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
            &file,
            &[],
            "test-build",
        );
        assert!(matches!(r.speed, SpeedSection::Measured { .. }));
        let out = render_human(&r);
        assert!(out.contains("14.1 tok/s decode"));
        assert!(out.contains("MEASURED on this exact split"));
        assert!(quotes_a_rate(&out), "a measured plan SHOULD quote a rate");

        let s = &render_json(&r)["speed"];
        assert_eq!(s["status"], "measured");
        assert_eq!(s["decode_tok_s"], 14.1);
        assert_eq!(s["runs"], 1);
        assert_eq!(s["stale"], false);
        assert!(
            s["prefill_tok_s"].is_null(),
            "unmeasured prefill stays null"
        );
    }

    /// The near miss says *how* the measured configuration differed, not merely
    /// that it did.
    ///
    /// This is the surface that has to carry the weight once a record can come
    /// from a machine the reader has never seen: the key pins the exact split
    /// and the exact silicon, so an exact hit is vanishingly unlikely, and
    /// `differs by: split` gives a stranger nothing to judge with.
    #[test]
    fn a_near_miss_names_both_splits_when_the_record_kept_a_witness() {
        let mut key = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))))
            .speed_key
            .expect("key");

        // The same model on the same two machines, cut 12/36 instead of evenly.
        let measured = mm::PlacementWitness {
            mode: "distributed".into(),
            total_blocks: 48,
            shards: vec![
                mm::PlacementShard {
                    node_key: "beefymac".into(),
                    hw: Some(7),
                    blocks: Some((0, 11)),
                    holds_output: false,
                },
                mm::PlacementShard {
                    node_key: "ruggedfox".into(),
                    hw: Some(7),
                    blocks: Some((12, 47)),
                    holds_output: true,
                },
            ],
            machines: vec![
                mm::MachineWitness {
                    node_key: "beefymac".into(),
                    vram_gb: 64,
                    backend: Some("vulkan".into()),
                },
                mm::MachineWitness {
                    node_key: "ruggedfox".into(),
                    vram_gb: 64,
                    backend: Some("vulkan".into()),
                },
            ],
        };
        key.placement_digest = measured.digest();

        let mut file = mm::MeasurementFile::new();
        mm::record(
            &mut file,
            mm::MeasurementRecord {
                witness: Some(measured),
                conditions: None,
                key,
                decode_tok_s: 11.7,
                decode_tok_s_min: 11.7,
                decode_tok_s_max: 11.7,
                ttft_ms: 800.0,
                itl_p50_ms: 80.0,
                itl_p95_ms: 90.0,
                prefill_tok_s: None,
                cold_load_s: None,
                trials: 3,
                content_frames: 128,
                model_name: "test.gguf".into(),
                placement_human: "36 local + 12 @beefymac".into(),
                nodes: 2,
                hops: 1,
                measured_at: 1_753_400_000,
                build: "test-build".into(),
                backend: Some("vulkan".into()),
                link_rtt_ms: None,
                verdict: mm::Verdict::Valid,
            },
        );

        let r = build_report(
            live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
            &file,
            &[],
            "test-build",
        );
        let SpeedSection::NotMeasured { near } = &r.speed else {
            panic!("a different split is not a hit");
        };
        assert_eq!(near[0].differs_by, vec!["split"]);
        assert_eq!(
            near[0].detail[0].theirs.as_deref(),
            Some("beefymac 12 · ruggedfox 36 +head")
        );
        let ours = near[0].detail[0]
            .ours
            .clone()
            .expect("the plan describes its own split");

        let out = render_human(&r);
        assert!(
            out.contains("measured: beefymac 12 · ruggedfox 36 +head"),
            "the human output must name the measured split, not just the facet:\n{out}"
        );
        assert!(
            out.contains(&format!("yours: {ours}")),
            "and the one being planned, to compare against:\n{out}"
        );

        let d = &render_json(&r)["speed"]["near_misses"][0]["differences"][0];
        assert_eq!(d["facet"], "split");
        assert_eq!(d["measured"], "beefymac 12 · ruggedfox 36 +head");
        assert_eq!(d["yours"], ours);
    }

    /// A record taken on a different split is named as context but never
    /// becomes this plan's number.
    #[test]
    fn a_record_for_another_split_is_a_near_miss_not_an_answer() {
        let other = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))));
        let mut key = other.speed_key.clone().expect("key");
        key.placement_digest = "pd1:0000000000000000".into();

        let mut file = mm::MeasurementFile::new();
        mm::record(
            &mut file,
            mm::MeasurementRecord {
                witness: None,
                conditions: None,
                key,
                decode_tok_s: 11.7,
                decode_tok_s_min: 11.7,
                decode_tok_s_max: 11.7,
                ttft_ms: 800.0,
                itl_p50_ms: 80.0,
                itl_p95_ms: 90.0,
                prefill_tok_s: None,
                cold_load_s: None,
                trials: 3,
                content_frames: 128,
                model_name: "test.gguf".into(),
                placement_human: "48 local (solo)".into(),
                nodes: 1,
                hops: 0,
                measured_at: 1_753_400_000,
                build: "test-build".into(),
                backend: Some("vulkan".into()),
                link_rtt_ms: None,
                verdict: mm::Verdict::Valid,
            },
        );

        let r = build_report(
            live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
            &file,
            &[],
            "test-build",
        );
        let SpeedSection::NotMeasured { near } = &r.speed else {
            panic!("a different split is not a hit");
        };
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].differs_by, vec!["split"]);

        let out = render_human(&r);
        assert!(out.contains("not measured for this configuration"));
        assert!(out.contains("48 local (solo)"));
        assert!(
            out.contains("does not apply here"),
            "the other number must be explicitly disclaimed"
        );

        let s = &render_json(&r)["speed"];
        assert_eq!(s["status"], "not_measured");
        assert!(
            s["decode_tok_s"].is_null(),
            "a near miss must NEVER populate this plan's rate"
        );
        assert_eq!(s["near_misses"][0]["decode_tok_s"], 11.7);
        assert!(
            s["near_misses"][0]["taken_by"].is_null(),
            "null is this machine's own run"
        );
    }

    // -- Travel -------------------------------------------------------------

    /// A peer's record, as `GET /v1/mesh/measurements` would deliver it.
    fn peer_record(key: mm::MeasurementKey, tok_s: f64, placement: &str) -> mm::ForeignRecord {
        mm::ForeignRecord {
            origin_node: "b88252e4325bc3771122334455667788".into(),
            origin_name: Some("BeefyMac".into()),
            record: mm::MeasurementRecord {
                witness: None,
                conditions: None,
                key,
                decode_tok_s: tok_s,
                decode_tok_s_min: tok_s - 0.1,
                decode_tok_s_max: tok_s + 0.1,
                ttft_ms: 2203.0,
                itl_p50_ms: 90.0,
                itl_p95_ms: 98.0,
                prefill_tok_s: None,
                cold_load_s: None,
                trials: 3,
                content_frames: 256,
                model_name: "test.gguf".into(),
                placement_human: placement.into(),
                nodes: 2,
                hops: 1,
                measured_at: 1_785_000_000,
                build: "test-build".into(),
                backend: Some("metal".into()),
                link_rtt_ms: None,
                verdict: mm::Verdict::Valid,
            },
        }
    }

    /// The whole point of travel: an empty local store still answers, because a
    /// peer measured the thing being asked about.
    #[test]
    fn a_peer_measurement_reaches_the_plan_and_is_attributed() {
        let probe = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))));
        let key = probe.speed_key.clone().expect("key");
        let file = mm::MeasurementFile::new();
        let peers = [peer_record(key, 11.08, "36 local + 12 @beefymac")];

        let r = build_report(
            live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
            &file,
            &peers,
            "test-build",
        );

        // Still "not measured" — `lookup` reads local records only, so a peer's
        // number never becomes this machine's measurement.
        let SpeedSection::NotMeasured { near } = &r.speed else {
            panic!("a peer's record must not be served as a local hit");
        };
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].taken_by.as_deref(), Some("BeefyMac"));
        assert!(near[0].is_exact(), "same key, so nothing differs");

        let out = render_human(&r);
        assert!(out.contains("not measured for this configuration"));
        assert!(
            out.contains("BeefyMac measured this configuration: 11.1 tok/s"),
            "the peer's number must be named as theirs: {out}"
        );
        assert!(
            out.contains("their machine, so it is a report, not your measurement"),
            "and disclaimed as not the reader's own: {out}"
        );

        let s = &render_json(&r)["speed"];
        assert_eq!(s["status"], "not_measured");
        assert!(
            s["decode_tok_s"].is_null(),
            "a peer's number must NEVER populate this plan's rate"
        );
        assert_eq!(s["near_misses"][0]["taken_by"], "BeefyMac");
        assert_eq!(s["near_misses"][0]["exact"], true);
    }

    /// A local measurement wins the headline even when a peer also has one: the
    /// reader's own hardware is the fact, the peer's is a report about it.
    #[test]
    fn a_local_hit_still_beats_a_peer_with_the_same_key() {
        let probe = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))));
        let key = probe.speed_key.clone().expect("key");
        let mut file = mm::MeasurementFile::new();
        mm::record(&mut file, peer_record(key.clone(), 7.75, "mine").record);
        let peers = [peer_record(key, 11.08, "theirs")];

        let r = build_report(
            live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
            &file,
            &peers,
            "test-build",
        );
        let SpeedSection::Measured { summary } = &r.speed else {
            panic!("a local record under the asked-for key is a hit");
        };
        assert_eq!(summary.decode_tok_s, 7.75);
        let out = render_human(&r);
        assert!(
            !out.contains("11.1"),
            "a peer's faster number must not appear beside a local hit as if it \
             were an alternative reading of the same machine: {out}"
        );
    }

    /// A peer on a machine that differs is a near miss like any other, and the
    /// facets that differ are named.
    #[test]
    fn a_peer_on_different_hardware_is_a_named_near_miss() {
        let probe = report(live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))));
        let mut other = probe.speed_key.clone().expect("key");
        other.host_hw_fingerprint = 0xdead_beef;
        let file = mm::MeasurementFile::new();
        let peers = [peer_record(other, 22.4, "24 local + 24 @othermac")];

        let r = build_report(
            live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
            &file,
            &peers,
            "test-build",
        );
        let SpeedSection::NotMeasured { near } = &r.speed else {
            panic!("different host hardware is not a hit");
        };
        assert_eq!(near[0].differs_by, vec!["host-hardware"]);
        assert!(!near[0].is_exact());

        let out = render_human(&r);
        assert!(
            out.contains("Measured by BeefyMac: 24 local + 24 @othermac → 22.4 tok/s"),
            "{out}"
        );
        assert!(out.contains("does not apply here"));
    }

    /// With no daemon there are no peers, and that must read exactly as it did
    /// before travel existed.
    #[test]
    fn no_peers_is_the_pre_travel_behaviour_unchanged() {
        let file = mm::MeasurementFile::new();
        let r = build_report(
            live(mesh_devs(&["beefymac", "ruggedfox"], Some(7))),
            &file,
            &[],
            "test-build",
        );
        let out = render_human(&r);
        assert!(out.contains("Sovereign does not quote throughput it has not measured."));
        assert!(!out.contains("Measured by"));
    }

    /// A record from a different build is still shown — with a warning. Hiding
    /// it would cost a re-measurement for nothing.
    #[test]
    fn a_record_from_another_build_is_shown_and_flagged() {
        let probe = report(live(mesh_devs(&["a", "b"], Some(7))));
        let key = probe.speed_key.clone().expect("key");
        let mut file = mm::MeasurementFile::new();
        mm::record(
            &mut file,
            mm::MeasurementRecord {
                witness: None,
                conditions: None,
                key,
                decode_tok_s: 14.1,
                decode_tok_s_min: 14.0,
                decode_tok_s_max: 14.2,
                ttft_ms: 900.0,
                itl_p50_ms: 70.0,
                itl_p95_ms: 78.0,
                prefill_tok_s: None,
                cold_load_s: None,
                trials: 3,
                content_frames: 256,
                model_name: "test.gguf".into(),
                placement_human: "36/12".into(),
                nodes: 2,
                hops: 1,
                measured_at: 1_753_000_000,
                build: "0.9.1".into(),
                backend: Some("vulkan".into()),
                link_rtt_ms: None,
                verdict: mm::Verdict::Valid,
            },
        );
        let r = build_report(live(mesh_devs(&["a", "b"], Some(7))), &file, &[], "0.10.0");
        assert!(render_human(&r).contains("(!) recorded on a different build"));
        assert_eq!(render_json(&r)["speed"]["stale"], true);
    }
}
