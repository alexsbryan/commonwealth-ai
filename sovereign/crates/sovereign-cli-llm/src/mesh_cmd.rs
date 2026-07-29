// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh` subcommand handlers (the `corpus` half moved to
//! `corpus_cmd` in the §3.2 split that fixed the dispatch naming lie).
//!
//! These are lightweight commands that don't require loading a full model
//! or database — they manage the embedded Commonwealth daemon.

use std::path::PathBuf;

use sovereign_cli_shared::dirs::mesh_data_dir;
use sovereign_mesh::deep_link::{build_https_join_link, parse_join_argument};
use sovereign_mesh::EmbeddedDaemon;

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
        "rotate" => cmd_rotate(&args[1..]).await,
        "status" => cmd_status(&args[1..]).await,
        "transport" => cmd_transport(&args[1..]).await,
        "balance" => cmd_balance().await,
        "leave" => cmd_leave().await,
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
                eprintln!("  ~/.sovereign/rpc-cache (matches the in-process worker).");
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
            eprintln!("could not resolve a cache dir (pass --cache-dir or set HOME)");
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
                "status",
                "Show mesh members, hosted knowledge, loaded models",
            ),
            (
                "transport",
                "Show each peer's live iroh path (direct / relayed / mixed)",
            ),
            ("balance", "Show your contribution to the mesh"),
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
    pub(crate) vram_gb: f64,
    pub(crate) hw_fingerprint: Option<u64>,
    pub(crate) backend: Option<String>,
}

/// Read the live mesh from the running daemon's `/v1/mesh/status` and build the
/// per-device vector for `mesh plan --from-mesh`: online anchor workers first,
/// this host (`is_self`) last so the output head lands on it. Returns
/// `(devices, host index)`. Prints the resolved mesh to stderr (so `--json`
/// stays clean on stdout).
async fn devices_from_live_mesh() -> Result<(Vec<MeshDevice>, usize), String> {
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

    let mut workers: Vec<MeshDevice> = Vec::new();
    let mut host: Option<MeshDevice> = None;
    for m in &members {
        let dev = MeshDevice {
            name: m
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string(),
            vram_gb: m.get("vram_gb").and_then(|v| v.as_f64()).unwrap_or(0.0),
            hw_fingerprint: m.get("hw_fingerprint").and_then(|v| v.as_u64()),
            backend: m
                .get("backend")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        };
        let is_self = m.get("is_self").and_then(|b| b.as_bool()).unwrap_or(false);
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

    eprintln!(
        "Resolved live mesh: {} online anchor worker(s) + this host",
        workers.len()
    );
    for w in &workers {
        eprintln!("  worker  {}: {:.0} GB VRAM", w.name, w.vram_gb);
    }
    eprintln!(
        "  host    {}: {:.0} GB VRAM  (holds the output head)",
        host.name, host.vram_gb
    );
    if workers.is_empty() {
        eprintln!(
            "  note: no online anchor workers — the plan will show a single-node (local) load."
        );
    }
    eprintln!();

    let mut devices = workers;
    devices.push(host);
    let host_idx = devices.len() - 1;
    Ok((devices, host_idx))
}

/// `svrn mesh plan` — dry-run a model's tensor split across a mesh, offline. Reuses
/// the daemon's own `plan_shards` + `quantize_vram` (so the dry run matches the live
/// load), then overlays the REAL per-block byte mass — which the live planner ignores
/// — to show the bytes each device holds and whether they fit. Surfaces the
/// per-device check the live load lacks (it gates only on aggregate pooled memory),
/// with operator-set `--headroom` instead of the hardcoded 1.2×.
async fn cmd_plan(args: &[String]) -> i32 {
    use sovereign_inference::embedded as inf;
    let mut model: Option<PathBuf> = None;
    let mut devices_gb: Vec<f64> = Vec::new();
    let mut host_idx: Option<usize> = None;
    // Default headroom mirrors the daemon's OWN resolution order exactly, so a
    // previewed plan uses the SAME factor the load executes with: an explicit
    // `SOVEREIGN_RPC_HEADROOM` env wins (the daemon reads it directly), else the
    // `[shared_model] headroom` config (bootstrap bridges config→env), else 1.2.
    // `--headroom` overrides this for what-if planning.
    let mut headroom: f64 = std::env::var("SOVEREIGN_RPC_HEADROOM")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .or_else(|| {
            sovereign_core::setup_config::SetupConfig::load()
                .ok()
                .and_then(|c| c.shared_model.headroom)
        })
        .filter(|&h| h >= 1.0)
        .unwrap_or(1.2);
    let mut headroom_from_flag = false;
    let mut json = false;
    let mut from_mesh = false;
    // `Some` only under `--from-mesh`. A `--devices` plan describes hardware
    // that is not here, so it has no identity and can never match a
    // measurement — see `SpeedSection::NotMeasurable`.
    let mut mesh_devices: Option<Vec<MeshDevice>> = None;
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
            s if model.is_none() && !s.starts_with('-') => model = Some(PathBuf::from(s)),
            other => {
                eprintln!("Unknown arg: {other}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(model) = model else {
        sovereign_cli_shared::help::print(&HELP_MESH_PLAN);
        return 2;
    };
    if from_mesh {
        if !devices_gb.is_empty() {
            eprintln!("--from-mesh and --devices are mutually exclusive");
            return 2;
        }
        match devices_from_live_mesh().await {
            Ok((devs, h)) => {
                devices_gb = devs.iter().map(|d| d.vram_gb).collect();
                host_idx = Some(h);
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
        .map(|c| c.models.effective_context_size())
        .unwrap_or(16384);

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
            host,
            headroom,
            headroom_from_flag,
            mesh: mesh_devices,
            n_ctx,
        },
        &sovereign_core::mesh_measurements::load(),
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
    /// Per-device usable VRAM in GB, in caller order.
    pub(crate) devices_gb: Vec<f64>,
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotMeasurable {
    /// `--devices` describes hardware that is not present.
    HypotheticalDevices,
    /// The host advertises no hardware fingerprint (an older daemon), so there
    /// is no key under which a measurement could have been filed.
    HostUnidentified,
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
    /// What we can honestly say about how fast this configuration runs.
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
    current_build: &str,
) -> PlanReport {
    use sovereign_inference::embedded as inf;

    let PlanInput {
        model_name,
        n_layer,
        sizes,
        devices_gb,
        host,
        headroom,
        headroom_from_flag,
        mesh,
        n_ctx,
    } = input;

    // Per-block byte mass + global tensors (output head → last block-holder;
    // token_embd → host system RAM; other globals lumped as host overhead).
    // Routed-expert (`_exps`) mass is the COLD part of an MoE model — only the
    // router's top-k experts are read per token, so it can be ~90% of the bytes
    // yet a small fraction of the per-token work. `model_mass_from_sizes` is the
    // same decomposition the live load's planner uses.
    let mass = inf::model_mass_from_sizes(&sizes, n_layer);
    let total_weight: u64 = mass.total_bytes();

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
    let plan = inf::plan_shards_weighted(n_layer, &weights, &mass.block_bytes, mass.head_bytes);

    // The per-device verdict, from the decider the live gate runs. Capacities go
    // in PLAN order (`order[pos]`), and the display maps back through `order`
    // below — the two index spaces look interchangeable and are not.
    let capacities: Vec<u64> = order.iter().map(|&d| vram[d]).collect();
    let fits = inf::shard_fits(&plan, &capacities, &mass, headroom);

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
                    need_bytes: 0,
                    capacity_bytes: capacities[pos],
                }),
        });
    }
    rows.sort_by_key(|r| r.dev);

    // Aggregate gate (the live daemon's model×1.2, with YOUR headroom).
    let pooled: u64 = vram.iter().sum();
    let gate_need = (total_weight as f64 * headroom) as u64;
    let gate_pass = pooled >= gate_need;

    // Block-mass uniformity → the "does heterogeneity stay safe" verdict.
    let nz: Vec<u64> = mass.block_bytes.iter().copied().filter(|&b| b > 0).collect();
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

    let (speed, speed_key) = resolve_speed(
        &rows,
        mesh.as_deref(),
        &sizes,
        n_layer,
        active_nodes,
        n_ctx,
        measurements,
        current_build,
    );

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
        nodes: NodesReport {
            active_nodes,
            hops_now: active_nodes - 1,
            min_nodes,
            hops_min: min_nodes.saturating_sub(1),
        },
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
    let shards: Vec<mm::PlacementShard> = rows
        .iter()
        .filter(|r| r.blocks.is_some() || r.holds_output)
        .map(|r| mm::PlacementShard {
            node_key: mesh
                .get(r.dev)
                .map(|d| d.name.clone())
                .unwrap_or_else(|| format!("dev{}", r.dev)),
            blocks: r.blocks,
            holds_output: r.holds_output,
        })
        .collect();
    let mode = if active_nodes <= 1 {
        "local"
    } else {
        "distributed"
    };

    let key = mm::MeasurementKey::for_plan(
        host,
        mm::model_fingerprint(sizes, n_layer),
        mm::placement_digest(mode, n_layer, &shards),
        n_ctx,
    );

    let section = match mm::lookup(measurements, &key, current_build) {
        Some(summary) => SpeedSection::Measured {
            summary: Box::new(summary),
        },
        None => SpeedSection::NotMeasured {
            near: mm::near_misses(measurements, &key),
        },
    };
    (section, Some(key))
}

/// The machine-readable plan.
pub(crate) fn render_json(r: &PlanReport) -> serde_json::Value {
    let devices_json: Vec<serde_json::Value> = r
        .rows
        .iter()
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
        .collect();
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
            "Per-device:     {} device(s) OVERFLOW [{}] -> the LIVE load would OOM here (it gates only on the aggregate, not per-device).",
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

    render_speed_human(&mut o, r);
    o
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
            let _ = writeln!(
                o,
                "                {} run{}, {:.1}–{:.1} tok/s · build {}",
                s.runs,
                if s.runs == 1 { "" } else { "s" },
                s.decode_tok_s_min,
                s.decode_tok_s_max,
                s.measured_build
            );
            if s.stale {
                let _ = writeln!(
                    o,
                    "                (!) recorded on a different build than this binary. Re-run `svrn mesh bench`"
                );
                let _ = writeln!(o, "                    if the inference engine changed.");
            }
        }
        SpeedSection::NotMeasured { near } => {
            let _ = writeln!(o, "Speed:          not measured for this split.");
            for n in near.iter().take(2) {
                let _ = writeln!(
                    o,
                    "                Measured on this mesh: {} → {:.1} tok/s.",
                    n.placement_human, n.decode_tok_s
                );
                let _ = writeln!(
                    o,
                    "                That is a different configuration ({}), so its number does not apply here.",
                    n.differs_by.join(", ")
                );
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
    }
}

const HELP_MESH_PLAN: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "svrn mesh plan",
    summary: "Dry-run a model's tensor split across a mesh — per-device fit, offline, no load.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage(
            "svrn mesh plan <model.gguf> (--from-mesh | --devices <gb,..>) [--host <idx>] [--headroom <f>] [--json]",
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
             byte mass to show the BYTES each node holds and whether they fit — the per-device check\n\
             the live load does NOT do (it gates only on aggregate pooled memory). Reads only the GGUF\n\
             header table: no model load, no GPU, instant even on a 400 GB split. Also reports whether\n\
             the model's per-block mass is uniform (heterogeneous VRAM safe) or skewed (OOM risk).",
        ),
        sovereign_cli_shared::help::HelpSection::Examples(&[
            (
                "svrn mesh plan GLM-5.2.gguf --from-mesh",
                "Plan across your actual running mesh (reads each node's advertised VRAM)",
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
            "Existing members keep their connections. If the daemon is running, restart it\n\
             so the new key is active in-memory (the persisted mesh.json is updated on disk).",
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
    if sovereign_mesh::persist::load(&mesh_data_dir())
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

    let daemon = EmbeddedDaemon::new(mesh_data_dir());
    // Explicit create = serve remote peers → expose the client API
    // (bind non-loopback + require a bearer token).
    daemon.expose_client_api();
    match daemon.create_mesh(&mesh_name, &node_name).await {
        Ok(result) => {
            print_mesh_share(
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
        None => build_https_join_link(join_key, None, Some(mesh_name), None, false, None),
    };
    println!();
    println!("Mesh created.");
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
    // daemon keeps its solo-mesh `join_key_hash`. Every subsequent
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
    let daemon = EmbeddedDaemon::new(mesh_data_dir());
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
async fn daemon_listening_on(port: u16) -> bool {
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
    match sovereign_mesh::persist::rotate_join_key(&mesh_data_dir()) {
        Ok(Some(rotated)) => {
            eprintln!();
            eprintln!("Note: existing members stay connected. Only future joins need the new key.");
            eprintln!("If the daemon is currently running, restart it to load the new key.");
            // Rotation is an offline persist op — the API token is
            // unchanged, so it isn't reprinted here (shown on create /
            // `mesh status`).
            // Offline persist op — no running daemon to read a dial
            // string from; the daemon's status poll (`current_invite`)
            // serves the dial-bearing link once it's back up.
            print_mesh_share(&rotated.mesh_name, &rotated.join_key, None, None);
            0
        }
        Ok(None) => {
            eprintln!("No mesh to rotate — run `svrn setup` or `svrn mesh create` first.");
            1
        }
        Err(e) => {
            eprintln!("Failed to rotate join key: {e}");
            1
        }
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
        println!(
            "  {:<22} {:<12} {:<8} {}{}",
            nid, name, m.status, addr_disp, self_tag,
        );
    }
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

async fn cmd_leave() -> i32 {
    println!("(mesh leave requires a running daemon)");
    0
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
                .primary
                .parent()
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
    let mesh_path = sovereign_cli_shared::dirs::mesh_data_dir().join("mesh.json");
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
            host,
            headroom: 1.2,
            headroom_from_flag: false,
            // These tests exercise the fit computation, not the speed lookup;
            // `mesh: None` is the `--devices` shape, which is barred from
            // matching a measurement by construction.
            mesh: None,
            n_ctx: 32_768,
        }
    }

    /// `build_report` against an empty measurement store — the week-1 state, and
    /// the state every fit assertion below cares about.
    fn report(i: PlanInput) -> PlanReport {
        build_report(
            i,
            &sovereign_core::mesh_measurements::MeasurementFile::new(),
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
        let fits = inf::shard_fits(&plan, &capacities, &mass, 1.2).expect("judgeable");

        for (pos, &d) in order.iter().enumerate() {
            let row = r.rows.iter().find(|x| x.dev == d).expect("a row per device");
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
        names
            .iter()
            .map(|n| MeshDevice {
                name: (*n).into(),
                vram_gb: 64.0,
                hw_fingerprint: fp,
                backend: Some("vulkan".into()),
            })
            .collect()
    }

    fn live(mesh: Vec<MeshDevice>) -> PlanInput {
        let devices_gb = mesh.iter().map(|d| d.vram_gb).collect::<Vec<_>>();
        let mut i = input(model(48, 1, 2, 0), 48, devices_gb);
        i.mesh = Some(mesh);
        i
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
            let mut i = input(model(1, 4, 2, 0), 1, mesh.iter().map(|d| d.vram_gb).collect());
            i.host = host;
            i.mesh = Some(mesh);
            report(i)
        };

        let two = plan_with(&["beefymac", "ruggedfox"], 1);
        let three = plan_with(&["idlepeer", "ruggedfox", "beefymac"], 1);

        let holder = |r: &PlanReport| {
            let row = r.rows.iter().find(|d| d.blocks.is_some()).expect("a holder");
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
            blocks: Some((0, 47)),
            holds_output: true,
        };
        let idle = mm::PlacementShard {
            node_key: "idlepeer".into(),
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
        assert!(k.placement_digest.starts_with("pd1:"));

        let out = render_human(&r);
        assert!(out.contains("not measured for this split"));
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
            "test-build",
        );
        let SpeedSection::NotMeasured { near } = &r.speed else {
            panic!("a different split is not a hit");
        };
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].differs_by, vec!["split"]);

        let out = render_human(&r);
        assert!(out.contains("not measured for this split"));
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
        let r = build_report(live(mesh_devs(&["a", "b"], Some(7))), &file, "0.10.0");
        assert!(render_human(&r).contains("(!) recorded on a different build"));
        assert_eq!(render_json(&r)["speed"]["stale"], true);
    }
}
