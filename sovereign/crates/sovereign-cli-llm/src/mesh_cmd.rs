// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign mesh` subcommand handlers (the `corpus` half moved to
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
        "balance" => cmd_balance().await,
        "leave" => cmd_leave().await,
        "logs" => cmd_logs().await,
        "fetch-model" => cmd_fetch_model(&args[1..]).await,
        "warm-cache" => cmd_warm_cache(&args[1..]).await,
        "check-invariants" => cmd_check_invariants(&args[1..]).await,
        "soak-gate" => cmd_soak_gate(&args[1..]).await,
        other => {
            eprintln!("Unknown mesh subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP_MESH);
            1
        }
    }
}

/// `sovereign mesh warm-cache <gguf> [--cache-dir <dir>]`
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
                eprintln!("Usage: sovereign mesh warm-cache <model.gguf> [--cache-dir <dir>]");
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
        eprintln!("Usage: sovereign mesh warm-cache <model.gguf> [--cache-dir <dir>]");
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

/// `sovereign mesh check-invariants --nodes <a:port,b:port,...> [--expect-live <id,...>] [--json]`
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
                eprintln!("Usage: sovereign mesh check-invariants --nodes <a:port,b:port,...> [--expect-live <id,...>] [--json]");
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
        let line = serde_json::json!({
            "nodes": nodes,
            "unreachable": unreachable,
            "violations": violations
                .iter()
                .map(|v| serde_json::json!({ "invariant": v.invariant, "detail": v.detail }))
                .collect::<Vec<_>>(),
            "ok": violations.is_empty(),
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

/// `sovereign mesh soak-gate <findings.jsonl> [--baseline <file>] [--update-baseline]`
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
                eprintln!("Usage: sovereign mesh soak-gate <findings.jsonl> [--baseline <file>] [--update-baseline]");
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
    command: "sovereign mesh",
    summary: "Manage the local Commonwealth mesh (create / join / rotate / status).",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("sovereign mesh <subcommand> [args]"),
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
                "check-invariants --nodes <a,b,..>",
                "Poll /v1/mesh/status across nodes and assert convergence/no-ghost/liveness (soak harness)",
            ),
            (
                "soak-gate <findings.jsonl>",
                "Gate mesh-soak SLIs (violation rate, load latency) against a committed baseline",
            ),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Run `sovereign mesh <subcommand> --help` for subcommand-specific flags.",
        ),
    ],
};

const HELP_MESH_CREATE: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "sovereign mesh create",
    summary: "Promote the solo mesh to a joinable mesh and print the shareable invite.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("sovereign mesh create [--name <name>]"),
        sovereign_cli_shared::help::HelpSection::Flags(&[(
            "--name <name>",
            "Human-readable mesh name (default: \"<host>'s Mesh\")",
        )]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "Errors if a mesh already exists (e.g. from `sovereign setup`'s silent solo mesh).\n\
             In that case, run `sovereign mesh rotate` to generate a new shareable key instead.",
        ),
    ],
};

const HELP_MESH_JOIN: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "sovereign mesh join",
    summary: "Join an existing mesh using any of the three invite forms.",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("sovereign mesh join <arg>"),
        sovereign_cli_shared::help::HelpSection::Examples(&[
            (
                "sovereign mesh join cwth-a1b2-c3d4-e5f6",
                "Bare key typed from another user's terminal",
            ),
            (
                "sovereign mesh join https://sovereign.dev/join/cwth-a1b2-c3d4-e5f6",
                "Clickable https link from an email",
            ),
            (
                "sovereign mesh join sovereign://join/cwth-a1b2-c3d4-e5f6",
                "Native app deep link",
            ),
        ]),
    ],
};

const HELP_MESH_ROTATE: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "sovereign mesh rotate",
    summary: "Generate a new shareable join key (the previous key stops working for future joins).",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("sovereign mesh rotate"),
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
    // `sovereign setup`), the join-key hash is stored but its plaintext
    // is gone — we can't re-show it. Direct the user to `mesh rotate`
    // instead of blindly attempting another create_mesh (which errors
    // with AlreadyRunning or leaves them confused).
    if sovereign_mesh::persist::load(&mesh_data_dir())
        .map(|opt| opt.is_some())
        .unwrap_or(false)
    {
        eprintln!("A mesh already exists (created during `sovereign setup`).");
        eprintln!("To generate a new shareable join key, run:");
        eprintln!();
        eprintln!("  sovereign mesh rotate");
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
    println!("  CLI:  sovereign mesh join {join_key}");
    println!();
}

async fn cmd_join(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_MESH_JOIN);
        return 0;
    }
    let Some(arg) = args.first() else {
        eprintln!("Missing join key.");
        eprintln!("Usage: sovereign mesh join <key-or-url>");
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
            eprintln!(
                "No mesh to rotate — run `sovereign setup` or `sovereign mesh create` first."
            );
            1
        }
        Err(e) => {
            eprintln!("Failed to rotate join key: {e}");
            1
        }
    }
}

/// `sovereign mesh status [--json] [--self] [--addr-only]`
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
///   sovereign mesh status
///   sovereign mesh status --json
///   export SOVEREIGN_FOUNDER_ADDR=$(sovereign mesh status --self --addr-only)
async fn cmd_status(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: sovereign mesh status [--json] [--self] [--addr-only]");
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
                eprintln!("Try `sovereign mesh status --help` for usage.");
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
            eprintln!(
                "hint: `sovereign daemon status` to check, `sovereign daemon start` to launch."
            );
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
        println!("mesh: daemon running, but no mesh active (`sovereign mesh create` or `mesh join` to bootstrap)");
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

/// `sovereign mesh fetch-model <name> [--peer <peer-tailnet-addr>] [--out <dir>]`
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
        eprintln!("Usage: sovereign mesh fetch-model <name> [--peer <host:port>] [--out <dir>]");
        eprintln!();
        eprintln!("Pulls a GGUF from a mesh peer over the tailnet. No R2 credentials required.");
        eprintln!();
        eprintln!("Examples:");
        eprintln!("  sovereign mesh fetch-model Darwin-9B-Opus.Q4_K_M.gguf");
        eprintln!("  sovereign mesh fetch-model Qwen3-Embedding-0.6B-Q8_0.gguf --out ~/models");
        return 0;
    }

    let Some(name) = args.first().cloned() else {
        eprintln!("Missing model file name.");
        eprintln!("Usage: sovereign mesh fetch-model <name> [--peer <host:port>] [--out <dir>]");
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
                eprintln!("hint: pass --out <dir> explicitly, or run `sovereign daemon --setup-only` first.");
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
                eprintln!("No mesh peers known. Run `sovereign mesh join <link>` first,");
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
