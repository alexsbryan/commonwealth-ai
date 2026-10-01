// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh` subcommand handlers (the `corpus` half moved to
//! `corpus_cmd` in the §3.2 split that fixed the dispatch naming lie).
//!
//! These are lightweight commands that don't require loading a full model
//! or database — they drive the RUNNING Commonwealth daemon over HTTP
//! (`sovereign-turn-client`), never an in-process one (FIVE_PROGRAMS §11
//! step 10: a mesh verb that embedded a daemon left its state in a
//! process that exits when the command does).

use mesh_join_vocab::deep_link::{build_https_join_link, parse_join_argument};
use sovereign_contracts::setup_config::client_daemon_base_for;
use sovereign_turn_client::reach::ServingHost;
use sovereign_turn_client::TurnClient;

/// Run a mesh subcommand. Returns the exit code.
pub async fn run_mesh(args: &[String]) -> i32 {
    if args.is_empty() {
        sovereign_cli_base::help::print(&HELP_MESH);
        return 1;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        sovereign_cli_base::help::print(&HELP_MESH);
        return 0;
    }

    match args[0].as_str() {
        "up" => crate::rails_up::cmd_up(&args[1..]).await,
        "create" => cmd_create(&args[1..]).await,
        "join" => cmd_join(&args[1..]).await,
        "list" => cmd_list(&args[1..]).await,
        "switch" => cmd_switch(&args[1..]).await,
        "forget" => cmd_forget(&args[1..]).await,
        "forget-member" => crate::mesh_member_cmd::cmd_forget_member(&args[1..]).await,
        "media" => crate::mesh_media::cmd_media(&args[1..]).await,
        "app" => crate::mesh_app::cmd_app(&args[1..]).await,
        "offers" => crate::mesh_offers::cmd_offers(&args[1..]).await,
        "pod" => crate::mesh_pod::cmd_pod(&args[1..]).await,
        "rotate" => cmd_rotate(&args[1..]).await,
        "grant" => crate::mesh_guest::cmd_grant(&args[1..]).await,
        "use" => crate::mesh_guest::cmd_use(&args[1..]).await,
        "token" => crate::mesh_token::cmd_token(&args[1..]).await,
        "status" => cmd_status(&args[1..]).await,
        "transport" => cmd_transport(&args[1..]).await,
        "balance" => cmd_balance().await,
        "leave" => cmd_leave(&args[1..]).await,
        "logs" => cmd_logs().await,
        // serve's weight verbs (phase-b-22) and placement measurement
        // (pb-serve-placement): the dispatcher execs sovereign-serve for these
        // spellings, so only a direct call lands here.
        verb @ ("fetch-model" | "warm-cache" | "fetch-ner" | "plan" | "bench") => {
            eprintln!(
                "mesh {verb}: owned by serve. Run `svrn mesh {verb}` (the dispatcher \
                 routes it to sovereign-serve) or `sovereign-serve {verb}`."
            );
            2
        }
        "check-invariants" => cmd_check_invariants(&args[1..]).await,
        "soak-gate" => cmd_soak_gate(&args[1..]).await,
        other => {
            eprintln!("Unknown mesh subcommand: {other}");
            sovereign_cli_base::help::print(&HELP_MESH);
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
                eprintln!("  Polls GET /v1/mesh/status on each node's cw-rails (its rails_base");
                eprintln!("  host:port) and asserts the mesh");
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

#[path = "mesh_cmd_help.rs"]
mod mesh_help;
use mesh_help::HELP_MESH;

const HELP_MESH_CREATE: sovereign_cli_base::help::Help = sovereign_cli_base::help::Help {
    command: "svrn mesh create",
    summary: "Promote the solo mesh to a joinable mesh and print the shareable invite.",
    sections: &[
        sovereign_cli_base::help::HelpSection::Usage(
            "svrn mesh create [--name <name>] [--encrypt]",
        ),
        sovereign_cli_base::help::HelpSection::Flags(&[
            (
                "--name <name>",
                "Human-readable mesh name (default: \"<host>'s Mesh\")",
            ),
            (
                "--encrypt",
                "Require encryption mesh-wide: every traffic class rides iroh with no \
                 plaintext fallback, and the plaintext client API is closed to the network",
            ),
        ]),
        sovereign_cli_base::help::HelpSection::Notes(
            "Needs a RUNNING daemon (`svrn daemon start`): the create happens in the\n\
             daemon that keeps serving after this command exits, not in this process.\n\
             \n\
             Errors if a mesh already exists (e.g. from `svrn setup`'s silent solo mesh).\n\
             In that case, run `svrn mesh rotate` to generate a new shareable key instead.\n\
             \n\
             `--encrypt` is inherited: every joiner picks it up from the join snapshot and \
             gossip, and a mesh cannot be relaxed back to plaintext afterwards. Invites \
             become short-lived, and peers reach each other only over key-authenticated \
             iroh — which is why an encrypted mesh's `:9741` binds loopback-only.",
        ),
    ],
};

const HELP_MESH_JOIN: sovereign_cli_base::help::Help = sovereign_cli_base::help::Help {
    command: "svrn mesh join",
    summary: "Join an existing mesh using any of the three invite forms.",
    sections: &[
        sovereign_cli_base::help::HelpSection::Usage("svrn mesh join <arg>"),
        sovereign_cli_base::help::HelpSection::Examples(&[
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
        sovereign_cli_base::help::HelpSection::Notes(
            "Needs a RUNNING daemon (`svrn daemon start`). The daemon performs the join;\n\
             a join run in this CLI process would evaporate with it.",
        ),
    ],
};

const HELP_MESH_ROTATE: sovereign_cli_base::help::Help = sovereign_cli_base::help::Help {
    command: "svrn mesh rotate",
    summary: "Generate a new shareable join key (the previous key stops working for future joins).",
    sections: &[
        sovereign_cli_base::help::HelpSection::Usage("svrn mesh rotate"),
        sovereign_cli_base::help::HelpSection::Notes(
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
    if sovereign_cli_base::help::wants_help(args) {
        sovereign_cli_base::help::print(&HELP_MESH_CREATE);
        return 0;
    }
    let mut name = None;
    let mut require_encryption = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--name" => {
                if let Some(n) = iter.next() {
                    name = Some(n.clone());
                }
            }
            "--encrypt" => require_encryption = true,
            // REFUSED, not ignored. `--encrypt` is the one flag here whose
            // absence cannot be corrected afterwards — the help says the
            // posture is fixed for the life of the mesh — so a typo that fell
            // into a catch-all arm founded a PLAINTEXT mesh and exited 0
            // reporting success. Absence is reported, never defaulted (§18.3).
            other => {
                eprintln!("error: unknown argument for `mesh create`: {other}");
                eprintln!();
                eprintln!(
                    "hint: a mesh's encryption posture is fixed when it is created, so a \
                     misspelled flag is refused rather than ignored. Run `svrn mesh create \
                     --help` for the accepted flags."
                );
                return 2;
            }
        }
    }

    let mesh_name = name.unwrap_or_else(|| {
        let host = hostname().unwrap_or_else(|| "sovereign".to_string());
        format!("{host}'s Mesh")
    });
    let node_name = hostname().unwrap_or_else(|| "sovereign-node".to_string());

    // The mesh, its key and its founding are cw-rails' (pb-mesh-exit-
    // transport): bring it up the way `svrn mesh up` does, then ask its
    // create door. A mesh this node already holds is cw-rails' to refuse,
    // in its own words.
    let base = match crate::rails_up::up().await {
        Ok(base) => base,
        Err(why) => {
            eprintln!("Cannot create a mesh: {why}");
            return 1;
        }
    };
    // `mesh_create`, not a bare POST: the door defaults `name`/`node_name`
    // on the HOST side and carries `encrypt` — the one flag here whose
    // absence cannot be corrected afterwards (the help says the posture is
    // fixed for the life of the mesh), so it is passed through verbatim.
    match TurnClient::new(&base)
        .mesh_create::<CreatedMesh>(Some(&mesh_name), Some(&node_name), require_encryption)
        .await
    {
        Ok(result) => {
            print_mesh_share(
                if require_encryption {
                    "Mesh created (encrypted)."
                } else {
                    "Mesh created."
                },
                &result.mesh_name,
                &result.join_key,
                result.client_token.as_deref(),
                result.join_link.as_deref(),
            );
            if require_encryption {
                println!(
                    "  Encryption is mesh-wide and inherited: joiners cannot opt out, \n\
                     \x20 the plaintext client API is closed to the network, and invites expire."
                );
            }
            0
        }
        Err(e) => {
            eprintln!("Failed to create mesh: {e}");
            1
        }
    }
}

/// `POST /v1/mesh/create`'s answer, as this CLIENT parses it. cw-rails'
/// answer is a JSON object (commonwealth-rails membership.rs `create`); the
/// parse shape is the client's to own, and [`TurnClient::mesh_create`] is
/// generic over it by design.
#[derive(Debug, serde::Deserialize)]
struct CreatedMesh {
    mesh_name: String,
    join_key: String,
    /// `None` when cw-rails could not build a link (`join_link_absent`
    /// names why); the bare-key form prints instead.
    #[serde(default)]
    join_link: Option<String>,
    /// `None` when the daemon stayed loopback-only. Skipped on the wire
    /// rather than nulled, so the default must be here too.
    #[serde(default)]
    client_token: Option<String>,
}

/// Spec-format banner for a freshly-created or freshly-rotated mesh.
/// Prints both the https share URL and the CLI form so the inviter
/// can pick whichever suits the invitee's environment.
///
/// `join_link` is the daemon-built deep link (it carries the founder's
/// no-VPN dial string + TTL when the iroh endpoint is up); the https
/// form is derived from it so both printed forms share params. `None`
/// (a daemon that answered with no link) prints the bare-key form.
fn print_mesh_share(
    headline: &str,
    mesh_name: &str,
    join_key: &str,
    client_token: Option<&str>,
    join_link: Option<&str>,
) {
    let app_link = match join_link.and_then(parse_join_argument) {
        Some(mesh_join_vocab::deep_link::DeepLink::Join {
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
        Some(mesh_join_vocab::deep_link::DeepLink::Guest { .. }) | None => {
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
    // The CLI line carries the deep link (with its dial) when there is one: a
    // bare key only reaches a founder that LAN discovery can see. Quoted, since
    // the link's `&` is a shell operator — zsh refuses it, bash cuts it there.
    let cli_arg = shell_quote(join_link.unwrap_or(join_key));
    println!("  CLI:  svrn mesh join {cli_arg}");
    if join_link.is_none() {
        println!("        (the daemon returned no dial link — this bare key only reaches");
        println!("         a joiner on the same LAN; `svrn mesh status` shows the full link)");
    }
    println!();
}

/// Single-quote `s` for a POSIX shell (and zsh): `'` becomes `'\''`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

async fn cmd_join(args: &[String]) -> i32 {
    if sovereign_cli_base::help::wants_help(args) {
        sovereign_cli_base::help::print(&HELP_MESH_JOIN);
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

    // The join goes to cw-rails' join door over HTTP, always: cw-rails is
    // the node's one mesh endpoint and holds its key (pb-mesh-exit-
    // transport), so the process that must adopt the mesh is the one that
    // gossips. The base is `[daemon] rails_base` through its one reader,
    // never a literal port (ARCH §10.6: a hardcoded port once POSTed a
    // join to a different process than the one that gossips).
    println!();
    println!("Joining mesh...");
    let base = match crate::rails_up::up().await {
        Ok(base) => base,
        Err(why) => {
            eprintln!("Cannot join: {why}");
            return 1;
        }
    };
    join_via_running_daemon(&TurnClient::new(&base), arg, &node_name).await
}

/// `POST /v1/mesh/join`'s answer, as this CLIENT parses it — see
/// [`CreatedMesh`] for why the parse shape lives on the client side.
#[derive(Debug, serde::Deserialize)]
struct JoinedMesh {
    mesh_name: String,
    node_id: String,
}

/// Is a daemon serving on `<port>`? Delegates to
/// `sovereign_turn_client::reach` — the one decider for "is a serving
/// host answering" (its module doc counts the thirteen probe copies it
/// retired, and this used to be one of them: a private reqwest loop
/// over `/v1/models`). The ready door is still `/v1/models` — the
/// daemon's root route 405s a GET — but the verdict is now "2xx on the
/// ready door", TIGHTER than this helper's old "any response counts".
/// A listener that answers 404 there is not this daemon, and callers
/// that dial on a `true` would have failed one request later anyway.
pub(crate) async fn daemon_listening_on(port: u16) -> bool {
    ServingHost::at(client_daemon_base_for(port))
        .is_serving()
        .await
}

/// Ask cw-rails to join (`TurnClient::mesh_join` → `POST /v1/mesh/join`)
/// and surface the response. The HOST parses `key_or_url` — any of the
/// three invite forms — and performs the join in the process that gossips;
/// this client holds no mesh state of its own.
async fn join_via_running_daemon(client: &TurnClient, arg: &str, node_name: &str) -> i32 {
    match client.mesh_join::<JoinedMesh>(arg, Some(node_name)).await {
        Ok(result) => {
            println!();
            println!("\u{2713} Connected to \"{}\"", result.mesh_name);
            println!("  Your node id: {}", result.node_id);
            println!();
            println!("Shared compute is now available.");
            println!();
            0
        }
        // The error text carries the host's own words for a refusal —
        // an invite it cannot read, a mesh that requires encryption —
        // which is the sentence the operator needs, not a status code.
        Err(e) => {
            eprintln!();
            eprintln!("Failed to join mesh: {e}");
            1
        }
    }
}

/// Rotate the join key on an existing mesh. Regenerates the plaintext
/// key + hash, writes the new hash back to `mesh.json`, and prints the
/// new shareable invite in the same format as `mesh create`.
async fn cmd_rotate(args: &[String]) -> i32 {
    if sovereign_cli_base::help::wants_help(args) {
        sovereign_cli_base::help::print(&HELP_MESH_ROTATE);
        return 0;
    }
    let force = args.iter().any(|a| a == "--force");

    // Rotation MUST go through the running mesh endpoint, cw-rails. It used
    // to be an offline disk write, which the live process re-persisted over
    // on its next gossip round, silently reverting the rotation. There is no
    // correct offline rotation — the live mesh is the thing that has to
    // change. An absent cw-rails is the request's error, naming the base.
    rotate_via_running_daemon(force).await
}

/// The daemon's client port from `SetupConfig`, not a hardcoded 9741 — a
/// sandbox pointed at its own daemon must not rotate the operator's mesh.
pub(crate) fn daemon_client_port() -> u16 {
    sovereign_contracts::setup_config::SetupConfig::load()
        .map(|c| c.daemon.client_port)
        .unwrap_or(9741)
}

/// cw-rails' base from `SetupConfig` through its one reader
/// (`rails_kv::resolve_rails_base`): the membership doors and
/// `/v1/mesh/status` are cw-rails' since pb-mesh-exit-transport, and svrn's
/// copies answer 410 naming this base.
pub(crate) fn rails_base() -> String {
    let config = sovereign_contracts::setup_config::SetupConfig::load()
        .unwrap_or_else(|_| sovereign_contracts::setup_config::SetupConfig::unconfigured());
    sovereign_turn_client::rails_kv::resolve_rails_base(&config.daemon)
}

async fn rotate_via_running_daemon(force: bool) -> i32 {
    let url = format!("{}/v1/mesh/rotate?force={force}", rails_base());
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
            eprintln!("Failed to reach cw-rails at {url}: {e}");
            return 1;
        }
    };
    let status = resp.status();
    let payload: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("cw-rails returned a non-JSON response (status={status}): {e}");
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
        let join_link = payload.get("join_link").and_then(|v| v.as_str());
        eprintln!();
        eprintln!("Existing members stay connected — rotation changes only who may JOIN.");
        print_mesh_share("Join key rotated.", mesh_name, join_key, None, join_link);
        0
    } else {
        let err = payload
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("(no error message)");
        eprintln!();
        eprintln!("Failed to rotate (cw-rails returned {status}): {err}");
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
    if sovereign_cli_base::help::wants_help(args) {
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

    // Fetch from cw-rails, the mesh endpoint, at `[daemon] rails_base`.
    let url = format!("{}/v1/mesh/status", rails_base());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest client builds");
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh status: cw-rails at {url} not reachable: {e}");
            eprintln!(
                "hint: `{}` brings it up.",
                sovereign_turn_client::rails_kv::RAILS_BRING_UP_VERB
            );
            return 1;
        }
    };
    if !resp.status().is_success() {
        eprintln!(
            "mesh status: cw-rails returned HTTP {} from {url}",
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

    let status: sovereign_contracts::daemon_wire::MeshStatusSummary =
        match serde_json::from_str::<sovereign_contracts::daemon_wire::RailsMeshStatus>(&body) {
            Ok(s) => s.into(),
            Err(e) => {
                // Version drift — fall back to raw JSON pass-through so the
                // operator at least sees the data even when our local DTO
                // doesn't match.
                eprintln!("mesh status: response shape mismatch ({e}); printing raw JSON.");
                println!("{body}");
                return 1;
            }
        };

    // Filter to self when requested. Most addr-only / scripting uses
    // want exactly this node's address.
    let members: Vec<&sovereign_contracts::daemon_wire::MemberDto> = if self_only {
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
        // Min-width, never a cap: this name is what a person types into
        // `svrn mesh app <peer>` / `svrn mesh media <peer>`, and a truncated
        // one does not resolve (measured 2026-09-23: the table printed
        // `Alexs-MacBoo` for `Alexs-MacBook-Pro-2`, and `mesh app` answered
        // "no member matching"). Long names push the address column right;
        // that is the honest trade. `{:<12}` still aligns the common short
        // case.
        let name = m.name.as_str();
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
            println!("join with: svrn mesh join {}", shell_quote(l));
        }
    }
    0
}

/// `svrn mesh transport` — the operator's "is anyone actually on
/// iroh, and via a direct path or the relay?" surface (H2). Reads the
/// `iroh_transport` block of `/v1/mesh/status`.
async fn cmd_transport(args: &[String]) -> i32 {
    if sovereign_cli_base::help::wants_help(args) {
        eprintln!("Usage: svrn mesh transport [--json]");
        eprintln!();
        eprintln!("Show each peer's live iroh connection path (direct / relayed / mixed / idle).");
        eprintln!(
            "Empty output means iroh isn't carrying mesh traffic (the mesh is on the IP path)."
        );
        return 0;
    }
    let json_out = args.iter().any(|a| a == "--json");

    let url = format!("{}/v1/mesh/status", rails_base());
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
                "mesh transport: cw-rails returned HTTP {} from {url}",
                r.status()
            );
            return 1;
        }
        Err(e) => {
            eprintln!("mesh transport: daemon at {url} not reachable: {e}");
            return 1;
        }
    };
    let status: sovereign_contracts::daemon_wire::MeshStatusSummary =
        match serde_json::from_str::<sovereign_contracts::daemon_wire::RailsMeshStatus>(&body) {
            Ok(s) => s.into(),
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
        // Min-width, never a cap — same rule as the status table's name
        // column: a name a person cannot read is a name they cannot type.
        let name = p.name.as_str();
        let (path, detail) = match &p.path {
            Some(tp) => {
                // The addresses, not just how many: a count cannot say WHICH
                // wire a path is riding, and that is the whole question when
                // a peer stays reachable through a cut link.
                let addrs = match tp.active_direct_socket_addrs.as_slice() {
                    [] => String::new(),
                    a => format!(" [{}]", a.join(" ")),
                };
                let detail = match &tp.relay {
                    Some(r) => format!("relay={r}  direct={}{addrs}", tp.active_direct_addrs),
                    None => format!("direct={}{addrs}", tp.active_direct_addrs),
                };
                (tp.path.as_str(), detail)
            }
            None => ("unknown", "no endpoint record yet".to_string()),
        };
        println!("  {name:<12} {path:<9} {detail}");
    }
    // The watchdog's verdict on the SAME question, printed next to the table
    // it judges. On 2026-09-09 this table read "no endpoint record yet" for
    // every peer while `self_reachability` reported healthy, and nothing on
    // this surface connected the two — so the green light won.
    if let Some(r) = status.self_reachability.as_ref() {
        let h = &r.health;
        println!();
        if h.peer_paths_wedged {
            println!(
                "  watchdog: peer paths WEDGED — {}/{} active; this endpoint held a path and \
                 lost it. Self-heal is escalating (rebuilds so far: {}).",
                h.peer_paths_active, h.peer_paths_total, h.rebuilds
            );
        } else {
            println!(
                "  watchdog: {}/{} peer paths active · relay_homed={} · discovery_ok={} · \
                 rebuilds={}",
                h.peer_paths_active,
                h.peer_paths_total,
                h.relay_homed,
                match h.discovery_ok {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "not-run",
                },
                h.rebuilds
            );
        }
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
    if sovereign_cli_base::help::wants_help(args) {
        eprintln!("Usage: svrn mesh leave");
        eprintln!();
        eprintln!("Give up membership in the active mesh and return to a solo mesh.");
        eprintln!("Parked meshes are untouched — use `svrn mesh forget` to drop those.");
        return 0;
    }
    // cw-rails' leave door; an absent cw-rails is the request's error below,
    // a non-zero exit naming the base.
    let url = format!("{}/v1/mesh/leave", rails_base());
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
            println!("cw-rails re-solos in place; `svrn mesh status` shows the new roster.");
            0
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            eprintln!("Failed to leave (cw-rails returned {status}): {body}");
            1
        }
        Err(e) => {
            eprintln!("Failed to reach cw-rails at {url}: {e}");
            1
        }
    }
}

/// `svrn mesh list` — every mesh this node has joined, active one marked.
///
/// Reads cw-rails' `/v1/mesh/status` `meshes`: the meshes, active and
/// parked, are cw-rails' store since pb-mesh-exit-transport.
async fn cmd_list(args: &[String]) -> i32 {
    if sovereign_cli_base::help::wants_help(args) {
        eprintln!("Usage: svrn mesh list [--json]");
        eprintln!();
        eprintln!("Show every mesh this node has joined. The active one is marked '*'.");
        return 0;
    }
    let base = rails_base();
    let status = match TurnClient::new(&base).mesh_status().await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mesh list: cw-rails at {base} did not answer: {e}");
            eprintln!(
                "hint: `{}` brings it up.",
                sovereign_turn_client::rails_kv::RAILS_BRING_UP_VERB
            );
            return 1;
        }
    };
    let known = status.meshes;

    if args.iter().any(|a| a == "--json") {
        println!(
            "{}",
            serde_json::to_string_pretty(&known).unwrap_or_default()
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
        let (mark, state) = if m.is_active {
            ("*", "active")
        } else {
            (" ", "parked")
        };
        println!(
            " {mark} {:<28} {:>3} member(s)  {state}",
            m.name, m.members_total
        );
    }
    println!();
    0
}

/// `svrn mesh switch <mesh>` — park the active mesh, bring another up.
async fn cmd_switch(args: &[String]) -> i32 {
    if sovereign_cli_base::help::wants_help(args) || args.is_empty() {
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
    // cw-rails' switch door; an absent cw-rails is the request's error.
    let url = format!("{}/v1/mesh/switch", rails_base());
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
            eprintln!("Failed to reach cw-rails at {url}: {e}");
            return 1;
        }
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if status.is_success() {
        println!();
        println!("Switched to \"{target}\".");
        println!("`svrn mesh status` will show the new roster.");
        0
    } else {
        eprintln!("Failed to switch (cw-rails returned {status}): {body}");
        1
    }
}

/// `svrn mesh forget <mesh>` — drop a PARKED mesh from this node.
async fn cmd_forget(args: &[String]) -> i32 {
    if sovereign_cli_base::help::wants_help(args) || args.is_empty() {
        eprintln!("Usage: svrn mesh forget <mesh-name-or-id>");
        eprintln!();
        eprintln!("Delete a parked mesh's roster and invite key from this node.");
        eprintln!("Refuses on the ACTIVE mesh — switch or leave first.");
        eprintln!();
        eprintln!("Rejoining afterwards needs a fresh invite, since the roster is gone.");
        return if args.is_empty() { 1 } else { 0 };
    }
    // cw-rails' forget door resolves the name or id against its own store
    // and refuses the active mesh in its own words.
    let base = rails_base();
    match TurnClient::new(&base).mesh_forget(&args[0]).await {
        Ok(()) => {
            println!("Forgot \"{}\".", args[0]);
            0
        }
        Err(e) => {
            eprintln!("Failed to forget \"{}\": {e}", args[0]);
            eprintln!("`svrn mesh list` shows what is joined.");
            1
        }
    }
}

async fn cmd_logs() -> i32 {
    println!("(mesh logs are written to stderr when the daemon runs)");
    0
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
#[path = "mesh_cmd_quote_tests.rs"]
mod quote_tests;
