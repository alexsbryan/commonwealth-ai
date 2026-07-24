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

/// Read the live mesh from the running daemon's `/v1/mesh/status` and build the
/// per-device VRAM vector for `mesh plan --from-mesh`: online anchor workers first,
/// this host (`is_self`) last so the output head lands on it. Returns
/// `(vram_gb per device, host index)`. Prints the resolved mesh to stderr (so
/// `--json` stays clean on stdout).
async fn devices_from_live_mesh() -> Result<(Vec<f64>, usize), String> {
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

    let mut workers: Vec<(String, f64)> = Vec::new();
    let mut host: Option<(String, f64)> = None;
    for m in &members {
        let name = m
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("?")
            .to_string();
        let is_self = m.get("is_self").and_then(|b| b.as_bool()).unwrap_or(false);
        let online = m.get("status").and_then(|s| s.as_str()) == Some("online");
        let can_anchor = m
            .get("can_anchor")
            .and_then(|b| b.as_bool())
            .unwrap_or(false);
        let vram = m.get("vram_gb").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if is_self {
            host = Some((name, vram));
        } else if online && can_anchor {
            workers.push((name, vram));
        }
    }
    let (host_name, host_vram) =
        host.ok_or_else(|| "could not find this node (is_self) in the mesh status".to_string())?;

    eprintln!(
        "Resolved live mesh: {} online anchor worker(s) + this host",
        workers.len()
    );
    for (n, v) in &workers {
        eprintln!("  worker  {n}: {v:.0} GB VRAM");
    }
    eprintln!("  host    {host_name}: {host_vram:.0} GB VRAM  (holds the output head)");
    if workers.is_empty() {
        eprintln!(
            "  note: no online anchor workers — the plan will show a single-node (local) load."
        );
    }
    eprintln!();

    let mut devices: Vec<f64> = workers.iter().map(|(_, v)| *v).collect();
    devices.push(host_vram);
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
            Ok((gb, h)) => {
                devices_gb = gb;
                host_idx = Some(h);
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

    // Per-block byte mass + global tensors (output head → last block-holder;
    // token_embd → host system RAM; other globals lumped as host overhead).
    let mut block_bytes = vec![0u64; n_layer as usize];
    let (mut output_bytes, mut embd_bytes, mut other_global) = (0u64, 0u64, 0u64);
    // Routed-expert (`_exps`) mass is the COLD part of an MoE model — only the
    // router's top-k experts are read per token, so it can be ~90% of the bytes
    // yet a small fraction of the per-token work. Tallied for the hot/cold report.
    let mut routed_expert_bytes = 0u64;
    for (name, layer, nbytes) in &sizes {
        if inf::is_routed_expert_tensor(name) {
            routed_expert_bytes += *nbytes;
        }
        match layer {
            Some(l) if (*l as usize) < block_bytes.len() => block_bytes[*l as usize] += *nbytes,
            Some(_) => other_global += *nbytes,
            None if inf::is_output_tensor(name) => output_bytes += *nbytes,
            None if name.contains("token_embd") => embd_bytes += *nbytes,
            None => other_global += *nbytes,
        }
    }
    let total_weight: u64 =
        block_bytes.iter().sum::<u64>() + output_bytes + embd_bytes + other_global;

    let gib = 1024.0_f64 * 1024.0 * 1024.0;
    let vram: Vec<u64> = devices_gb.iter().map(|&g| (g * gib) as u64).collect();
    let host = host_idx.unwrap_or(vram.len() - 1);
    if host >= vram.len() {
        eprintln!("--host {host} out of range (valid 0..{})", vram.len() - 1);
        return 2;
    }

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
    let plan = inf::plan_shards_weighted(n_layer, &weights, &block_bytes, output_bytes);

    let gb = |b: u64| b as f64 / gib;
    struct Row {
        dev: usize,
        is_host: bool,
        vram: u64,
        blocks: Option<(u32, u32)>,
        holds_output: bool,
        weight: u64,
    }
    let mut rows: Vec<Row> = Vec::with_capacity(vram.len());
    for (pos, &d) in order.iter().enumerate() {
        let shard = &plan[pos];
        let mut w = 0u64;
        if let Some((a, b)) = shard.blocks {
            for blk in a..=b {
                w += block_bytes[blk as usize];
            }
        }
        if shard.holds_output {
            w += output_bytes;
        }
        rows.push(Row {
            dev: d,
            is_host: d == host,
            vram: vram[d],
            blocks: shard.blocks,
            holds_output: shard.holds_output,
            weight: w,
        });
    }
    rows.sort_by_key(|r| r.dev);

    // Aggregate gate (the live daemon's model×1.2, with YOUR headroom) + per-device fit.
    let pooled: u64 = vram.iter().sum();
    let gate_need = (total_weight as f64 * headroom) as u64;
    let gate_pass = pooled >= gate_need;
    let overflows: Vec<&Row> = rows
        .iter()
        .filter(|r| (r.weight as f64 * headroom) as u64 > r.vram)
        .collect();

    // Block-mass uniformity → the "does heterogeneity stay safe" verdict.
    let nz: Vec<u64> = block_bytes.iter().copied().filter(|&b| b > 0).collect();
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
    let uniform = spread <= 1.15;

    // MoE hot/cold split + node-count/hop advisor.
    let is_moe = routed_expert_bytes > 0;
    // Hot = resident mass touched every token: all block bytes minus the cold
    // routed experts, plus the output head (token_embd lives in host RAM).
    let hot_bytes = block_bytes
        .iter()
        .sum::<u64>()
        .saturating_sub(routed_expert_bytes)
        + output_bytes;
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
    let hops_now = active_nodes - 1;
    let hops_min = min_nodes.saturating_sub(1);

    if json {
        let devices_json: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "device": r.dev,
                    "role": if r.is_host { "host" } else { "worker" },
                    "vram_gb": gb(r.vram),
                    "blocks": r.blocks.map(|(a, b)| [a, b]),
                    "block_count": r.blocks.map(|(a, b)| b - a + 1).unwrap_or(0),
                    "holds_output": r.holds_output,
                    "weight_gb": gb(r.weight),
                    "need_gb": gb((r.weight as f64 * headroom) as u64),
                    "fits": (r.weight as f64 * headroom) as u64 <= r.vram,
                })
            })
            .collect();
        let out = serde_json::json!({
            "model": model.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
            "blocks": n_layer,
            "weights_gb": gb(total_weight),
            "output_head_gb": gb(output_bytes),
            "token_embd_host_ram_gb": gb(embd_bytes),
            "block_mass_gb": {
                "min": gb(bmin), "max": gb(bmax), "mean": gb(bmean),
                "spread": spread, "uniform": uniform
            },
            "headroom": headroom,
            "headroom_source": if headroom_from_flag { "flag" } else { "config" },
            "pooled_gb": gb(pooled),
            "aggregate_gate_need_gb": gb(gate_need),
            "aggregate_gate_pass": gate_pass,
            "per_device_overflow_devices": overflows.iter().map(|r| r.dev).collect::<Vec<_>>(),
            "moe": if is_moe {
                serde_json::json!({
                    "routed_expert_gb": gb(routed_expert_bytes),
                    "routed_expert_pct": 100.0 * routed_expert_bytes as f64 / total_weight as f64,
                    "hot_gb": gb(hot_bytes),
                    "hot_pct": 100.0 * hot_bytes as f64 / total_weight as f64,
                })
            } else {
                serde_json::Value::Null
            },
            "nodes_used": active_nodes,
            "hops": hops_now,
            "min_nodes": min_nodes,
            "min_hops": hops_min,
            "devices": devices_json,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return if gate_pass && overflows.is_empty() {
            0
        } else {
            1
        };
    }

    // Human report.
    let name = model.file_name().and_then(|n| n.to_str()).unwrap_or("?");
    println!("svrn mesh plan — dry run (no load, no GPU)\n");
    println!("Model:  {name}");
    println!(
        "        {n_layer} blocks · {:.1} GB weights  (output head {:.1} GB · token_embd {:.1} GB on host RAM)",
        gb(total_weight),
        gb(output_bytes),
        gb(embd_bytes)
    );
    if uniform {
        println!(
            "Blocks: {:.2}–{:.2} GB (mean {:.2}) · {spread:.2}× spread → UNIFORM mass",
            gb(bmin),
            gb(bmax),
            gb(bmean)
        );
        println!("        VRAM-proportional block count ≈ byte-proportional, so heterogeneous VRAM is safe.");
    } else {
        println!(
            "Blocks: {:.2}–{:.2} GB (mean {:.2}) · {spread:.2}× spread → NON-UNIFORM mass  (!)",
            gb(bmin),
            gb(bmax),
            gb(bmean)
        );
        println!("        Split apportions by byte MASS (not count), so heterogeneous VRAM stays balanced — but a single block heavier than a small node's whole share still can't be split contiguously. Watch per-device fit.");
    }
    if is_moe {
        println!(
            "MoE:    {:.1} GB routed experts ({:.0}% — COLD, only top-k read per token) · {:.1} GB hot skeleton ({:.0}% — every token)",
            gb(routed_expert_bytes),
            100.0 * routed_expert_bytes as f64 / total_weight as f64,
            gb(hot_bytes),
            100.0 * hot_bytes as f64 / total_weight as f64,
        );
        println!("        Whole blocks (experts included) stay on one node, so decode keeps its {hops_now}-hop path — a layer's experts are never scattered across nodes.");
    }
    let hr_note = if headroom_from_flag {
        "--headroom override — WHAT-IF; the load executes with the [shared_model] headroom"
    } else {
        "matches the load's configured headroom"
    };
    println!("Headroom: {headroom:.2}× ({hr_note}) — weight × {headroom:.2} must fit each device (covers KV + buffers)\n");

    println!("  dev  role    VRAM       blocks     n   weight     need       fit");
    for r in &rows {
        let (blocks_s, n_s) = match r.blocks {
            Some((a, b)) => (format!("{a}-{b}"), format!("{}", b - a + 1)),
            None => ("—".to_string(), "0".to_string()),
        };
        let need = (r.weight as f64 * headroom) as u64;
        let fit = if need <= r.vram {
            format!("ok  +{:.1} GB", gb(r.vram - need))
        } else {
            format!("OVERFLOW -{:.1} GB", gb(need - r.vram))
        };
        let star = if r.is_host { "*" } else { " " };
        let role = if r.is_host { "host" } else { "worker" };
        println!(
            "{star} {:>3}  {:<6} {:>6.1} GB  {:<8}  {:>2}  {:>6.1} GB  {:>6.1} GB  {fit}",
            r.dev,
            role,
            gb(r.vram),
            blocks_s,
            n_s,
            gb(r.weight),
            gb(need)
        );
        if r.is_host && embd_bytes > 0 {
            println!(
                "       (+ token_embd {:.1} GB in host system RAM, not VRAM)",
                gb(embd_bytes)
            );
        }
    }

    println!();
    println!(
        "Aggregate gate: pooled {:.1} GB {} model×{headroom:.2} ({:.1} GB) → {}",
        gb(pooled),
        if gate_pass { ">=" } else { "<" },
        gb(gate_need),
        if gate_pass {
            "PASS".to_string()
        } else {
            "FAIL — cluster too small; the host reports \"forming\" and does not load".to_string()
        }
    );
    if overflows.is_empty() {
        println!("Per-device:     all devices fit ok");
    } else {
        let ids: Vec<String> = overflows.iter().map(|r| r.dev.to_string()).collect();
        println!(
            "Per-device:     {} device(s) OVERFLOW [{}] -> the LIVE load would OOM here (it gates only on the aggregate, not per-device).",
            overflows.len(),
            ids.join(", ")
        );
        println!("\nOptions:");
        println!("   • move the host role to your largest node (--host <idx>) — the host also holds the output head");
        println!("   • lower --headroom for a tighter pack (less KV room), or give the overflowing node more free VRAM");
        if !uniform {
            println!("   • this model is skewed enough that one block's mass exceeds a small node's share — the split is already mass-aware, so the fix is more VRAM on that node or a different --host, not a smarter split");
        }
    }

    // Nodes & hops advisor — single-stream pipeline decode costs (nodes-1) hops
    // per token, so fewer nodes = fewer hops = lower hop LATENCY. That is a
    // tradeoff, NOT a win button: on a memory-bandwidth-bound host (e.g. a
    // unified-memory APU) offloading layers frees host weight-read bandwidth and
    // can raise THROUGHPUT despite the extra hop — the measured 122B ran ~20%
    // faster distributed (36/12) than solo. So report the hop cost; don't claim
    // fewer nodes is always faster.
    println!(
        "Nodes:          {active_nodes} holding blocks → {hops_now} network hop{} per token",
        if hops_now == 1 { "" } else { "s" }
    );
    if min_nodes < active_nodes {
        println!(
            "                mass alone fits {min_nodes} node{} ({hops_min} hop{}) — {} fewer node(s) would cut {} per-token hop(s) of latency. Net tok/s depends on the host: if it's memory-bandwidth-bound, keeping layers offloaded can still win. Measure both.",
            if min_nodes == 1 { "" } else { "s" },
            if hops_min == 1 { "" } else { "s" },
            active_nodes - min_nodes,
            hops_now - hops_min,
        );
    }

    if gate_pass && overflows.is_empty() {
        0
    } else {
        1
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
