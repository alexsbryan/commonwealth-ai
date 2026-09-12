// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh media <peer>` — the viewer half of federated media.
//!
//! Asks the running daemon for the loopback URL that reaches `<peer>`'s
//! `[iroh] media_origin` (`GET /v1/mesh/media`), then does ONE real request
//! through it so the person sees an HTTP status rather than a port number.
//! A bridge accepts whether or not the far end answers, so without the probe
//! "here is your URL" would be a claim nobody watched. Split out of
//! `mesh_cmd.rs` for the reason `mesh_member_cmd.rs` was.

use std::time::{Duration, Instant};

use crate::mesh_cmd::daemon_client_port;

pub(crate) async fn cmd_media(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("fanout") {
        return cmd_media_fanout(&args[1..]).await;
    }
    if args.first().map(String::as_str) == Some("declare") {
        return cmd_media_declare(&args[1..]);
    }
    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn mesh media [<peer>] [--json] [--no-probe]");
        eprintln!("       svrn mesh media fanout <path> [--peers a,b] [--method M] [--timeout-ms N] [--json]");
        eprintln!(
            "       svrn mesh media declare <header-name>   # value on stdin; --list / --clear"
        );
        eprintln!();
        eprintln!("With no <peer>: list the members that offer a media origin, as gossip");
        eprintln!("knows them — nothing is dialed. `fanout <path>` asks every offering member");
        eprintln!("the same request and prints one row each. With a <peer>:");
        eprintln!("Print a localhost URL that reaches <peer>'s media origin over the mesh —");
        eprintln!("dialed by its key, no VPN, no port forwarded. Point a player or a browser");
        eprintln!("at it. <peer> is a member name or a node-id prefix of at least 4 chars;");
        eprintln!("`svrn mesh status` lists both.");
        eprintln!();
        eprintln!(
            "The peer must declare what it serves:  [iroh] media_origin = \"127.0.0.1:8096\""
        );
        eprintln!("(Jellyfin's default). A peer that declares nothing closes the dial.");
        eprintln!();
        eprintln!("If your origin authenticates its own clients (Jellyfin wants an");
        eprintln!("X-Emby-Token), `declare` stores YOUR key on YOUR machine and your daemon");
        eprintln!("adds it on the way in — so housemates reach your library holding no key");
        eprintln!("of yours. See `svrn mesh media declare --help`.");
        eprintln!();
        eprintln!("Flags:");
        eprintln!("  --json       Raw JSON from the daemon (peer, node_id, url, via, path).");
        eprintln!("  --no-probe   Skip the GET / through the bridge; print the URL only.");
        return 0;
    }
    let json_out = args.iter().any(|a| a == "--json");
    let probe = !args.iter().any(|a| a == "--no-probe");
    let peer = args.iter().find(|a| !a.starts_with("--"));

    let port = daemon_client_port();
    let url = format!("http://127.0.0.1:{port}/v1/mesh/media");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build HTTP client: {e}");
            return 1;
        }
    };
    let Some(peer) = peer else {
        return list_offers(&client, &url, json_out).await;
    };
    let resp = match client.get(&url).query(&[("peer", peer)]).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh media: daemon at {url} not reachable: {e}");
            eprintln!("The bridge lives in the running daemon — `svrn daemon start`.");
            return 1;
        }
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let msg = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or(body);
        eprintln!("{msg}");
        return 1;
    }
    let reach: sovereign_mesh::media_reach::MediaReach = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh media: response shape mismatch ({e}): {body}");
            return 1;
        }
    };

    // One real request through the bridge. This is the only line that can
    // tell a working splice from a port that merely accepts: an ALPN the
    // peer does not serve, a peer with no `media_origin`, or an origin that
    // is down all surface HERE as a transport error, not as a URL.
    let probed = if probe {
        let started = Instant::now();
        let outcome = client
            .get(format!("{}/", reach.url))
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        Some(match outcome {
            Ok(r) => ProbeOutcome {
                ok: true,
                detail: format!(
                    "GET / → {} in {} ms",
                    r.status(),
                    started.elapsed().as_millis()
                ),
            },
            Err(e) => ProbeOutcome {
                ok: false,
                detail: format!(
                    "GET / failed after {} ms: {e} — the peer refuses this dial (not a member, \
                     or it declares no [iroh] media_origin) or its origin is down",
                    started.elapsed().as_millis()
                ),
            },
        })
    } else {
        None
    };

    if json_out {
        let mut v = serde_json::json!(reach);
        if let Some(p) = &probed {
            v["probe"] = serde_json::json!({ "ok": p.ok, "detail": p.detail });
        }
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        // The bar is stated on the RELAYED path, and on one LAN every path
        // reads `mixed` — a direct leg and a relay both live, bytes on the
        // direct leg. Say which kind of number a play here would be, so a
        // LAN demo is never written up as the relayed reading.
        let path = reach
            .path
            .as_ref()
            .map(|p| {
                let mut line = match &p.relay {
                    Some(r) => format!("{} via {r}", p.path),
                    None => p.path.clone(),
                };
                if p.relayed_reading {
                    line.push_str("  — a RELAYED reading (the bar's kind)");
                } else if p.path == "mixed" {
                    line.push_str(&format!(
                        "  — {} direct addr + relay both live; bytes ride the direct leg, so this \
                         is NOT a relayed reading (pin both ends with SOVEREIGN_IROH_RELAY_ONLY=1)",
                        p.active_direct_addrs
                    ));
                } else if p.path == "direct" {
                    line.push_str("  — a direct reading, not the bar's");
                }
                line
            })
            .unwrap_or_else(|| "not yet dialed".into());
        println!("{}", reach.url);
        println!("  peer  {} ({})", reach.peer, reach.node_id);
        println!("  path  {path}");
        if let Some(p) = &probed {
            println!("  probe {}", p.detail);
        }
    }
    match probed {
        Some(p) if !p.ok => 2,
        _ => 0,
    }
}

struct ProbeOutcome {
    ok: bool,
    detail: String,
}

/// `svrn mesh media` with no peer: the members offering a media origin, from
/// gossip. One line per member — name, node id, status, path — so a person
/// can pick one and run the verb again with it.
async fn list_offers(client: &reqwest::Client, url: &str, json_out: bool) -> i32 {
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh media: daemon at {url} not reachable: {e}");
            eprintln!("The roster lives in the running daemon — `svrn daemon start`.");
            return 1;
        }
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let msg = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or(body);
        eprintln!("{msg}");
        return 1;
    }
    if json_out {
        println!("{body}");
        return 0;
    }
    #[derive(serde::Deserialize)]
    struct Offers {
        offering: Vec<sovereign_mesh::media_reach::MediaOffer>,
    }
    let offers: Offers = match serde_json::from_str(&body) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("mesh media: response shape mismatch ({e}): {body}");
            return 1;
        }
    };
    if offers.offering.is_empty() {
        println!(
            "No member offers a media origin. A holder declares one with \
             `[iroh] media_origin = \"127.0.0.1:8096\"` and restarts its daemon; it shows \
             here within one gossip round."
        );
        return 0;
    }
    println!("Members offering a media origin:");
    for o in &offers.offering {
        // The roster's word, lowercased — total, so a row never renders blank.
        let status = format!("{:?}", o.status).to_ascii_lowercase();
        let path = o
            .path
            .as_ref()
            .map(|p| match &p.relay {
                Some(r) => format!("{} via {r}", p.path),
                None => p.path.clone(),
            })
            .unwrap_or_else(|| "no path yet (nothing dialed)".to_string());
        println!("  {:<16} {}  {:<8} {}", o.peer, o.node_id, status, path);
    }
    println!();
    println!("Play one:  svrn mesh media <peer>");
    0
}

/// `svrn mesh media fanout <path>` — the same request to every member that
/// offers a media origin, one attributed row each. The catalogue half of
/// federated media; a title is still played through the per-member URL.
async fn cmd_media_fanout(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) || args.is_empty() {
        eprintln!("Usage: svrn mesh media fanout <path> [--peers a,b] [--method M] [--timeout-ms N] [--json]");
        eprintln!();
        eprintln!("Ask every member that offers a media origin the same request — <path> is");
        eprintln!("origin-relative, e.g. /System/Info/Public or '/Items?Recursive=true' — through");
        eprintln!("each member's own mesh bridge, concurrently, and print one row per member:");
        eprintln!("what its origin answered, or why it was not asked. Bodies are capped (4 MiB)");
        eprintln!("and never merged: item semantics are the origin's. Play a title through");
        eprintln!("`svrn mesh media <peer>`.");
        eprintln!();
        eprintln!("Flags:");
        eprintln!("  --peers a,b     Only these members (name or ≥4-char id prefix). A member the");
        eprintln!("                  roster refuses is still a row, carrying the refusal.");
        eprintln!("  --method M      HTTP method (default GET).");
        eprintln!(
            "  --timeout-ms N  Per-member cap (default 10000); a slower member is a failed row."
        );
        eprintln!("  --json          The daemon's document: path, asked, rows[].");
        return if args.is_empty() { 1 } else { 0 };
    }
    let json_out = args.iter().any(|a| a == "--json");
    let mut path: Option<&str> = None;
    let mut peers: Option<Vec<String>> = None;
    let mut method: Option<String> = None;
    let mut timeout_ms: Option<u64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {}
            "--peers" => {
                i += 1;
                peers = args.get(i).map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                });
            }
            "--method" => {
                i += 1;
                method = args.get(i).cloned();
            }
            "--timeout-ms" => {
                i += 1;
                timeout_ms = args.get(i).and_then(|v| v.parse().ok());
            }
            other if other.starts_with("--") => {
                eprintln!("unknown flag {other}");
                return 1;
            }
            other => path = Some(other),
        }
        i += 1;
    }
    let Some(path) = path else {
        eprintln!("Which path? `svrn mesh media fanout /System/Info/Public`");
        return 1;
    };
    let port = daemon_client_port();
    let url = format!("http://127.0.0.1:{port}/v1/mesh/media/fanout");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build HTTP client: {e}");
            return 1;
        }
    };
    let mut body = serde_json::json!({ "path": path });
    if let Some(p) = &peers {
        body["peers"] = serde_json::json!(p);
    }
    if let Some(m) = &method {
        body["method"] = serde_json::json!(m);
    }
    if let Some(t) = timeout_ms {
        body["timeout_ms"] = serde_json::json!(t);
    }
    let resp = match client.post(&url).json(&body).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh media fanout: daemon at {url} not reachable: {e}");
            eprintln!("The bridges live in the running daemon — `svrn daemon start`.");
            return 1;
        }
    };
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or(text);
        eprintln!("{msg}");
        return 1;
    }
    if json_out {
        println!("{text}");
        return 0;
    }
    let doc: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("mesh media fanout: response shape mismatch ({e}): {text}");
            return 1;
        }
    };
    let rows = doc["rows"].as_array().cloned().unwrap_or_default();
    println!(
        "{} → {} member(s) asked",
        doc["path"].as_str().unwrap_or(path),
        doc["asked"].as_u64().unwrap_or(rows.len() as u64)
    );
    for r in &rows {
        let name = r["name"].as_str().unwrap_or("?");
        let node = r["node_id"].as_str().unwrap_or("?");
        let ms = r["elapsed_ms"].as_u64().unwrap_or(0);
        let line = match r["verdict"].as_str().unwrap_or("?") {
            "served" => format!(
                "{} {}B{}{}",
                r["status"].as_u64().unwrap_or(0),
                r["bytes"].as_u64().unwrap_or(0),
                if r["truncated"].as_bool().unwrap_or(false) {
                    " (truncated)"
                } else {
                    ""
                },
                r["content_type"]
                    .as_str()
                    .map(|c| format!("  {c}"))
                    .unwrap_or_default()
            ),
            "failed" => format!("failed — {}", r["reason"].as_str().unwrap_or("")),
            "never_asked" => format!("not asked — {}", r["reason"].as_str().unwrap_or("")),
            other => other.to_string(),
        };
        println!("  {name:<16} {node}  {ms:>5} ms  {line}");
    }
    0
}

// ─── declare: this node's own credential for its own origin ─────────────────

/// `svrn mesh media declare <header>` — store the credential THIS node adds to
/// requests reaching ITS OWN media origin.
///
/// # Why the value comes from stdin and not from an argument
///
/// An argument is in `ps` while the process runs and in `~/.zsh_history`
/// forever. Piping it is the difference between a secret the operator chose to
/// store and a secret their shell also kept. The README this serves previously
/// said `printf '%s' key > file`, which has the same defect — this verb exists
/// partly to retire that instruction.
///
/// The value is never echoed, never logged, and never printed by `--list`;
/// whether one is SET is operational and is printed. Same discipline as
/// `PUT /v1/mcp/servers/{name}/token`.
pub(crate) fn cmd_media_declare(args: &[String]) -> i32 {
    use std::io::Read;

    let dir = commonwealth_media::dir_under(&sovereign_contracts::rebrand::svrnmesh_root());

    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn mesh media declare <header-name>     # value on stdin");
        eprintln!("       svrn mesh media declare <header-name> --clear");
        eprintln!("       svrn mesh media declare --list");
        eprintln!();
        eprintln!("Store a credential this node adds to requests reaching ITS OWN media");
        eprintln!("origin — so housemates reach your Jellyfin without holding your API key.");
        eprintln!("The value is added on YOUR machine, after the caller is admitted as a");
        eprintln!("member, and displaces any copy of that header the caller sent.");
        eprintln!();
        eprintln!("  printf '%%s' \"$KEY\" | svrn mesh media declare x-emby-token");
        eprintln!();
        eprintln!("The value is read from stdin so it never lands in your shell history,");
        eprintln!("stored 0600 under the secrets dir, and never printed back. It is NOT in");
        eprintln!("config.toml on purpose: a secret there rides along with anything that is");
        eprintln!("shared, synced, backed up, or gossiped to a peer.");
        eprintln!();
        eprintln!("Run `svrn daemon reload` after changing a declaration.");
        return 0;
    }

    if args.iter().any(|a| a == "--list") {
        let declared = commonwealth_media::read_declared_in(&dir);
        if declared.is_empty() {
            println!("No declarations. This node adds no credential of its own to requests");
            println!("reaching its media origin. ({})", dir.display());
            return 0;
        }
        println!("Headers this node adds to requests reaching its own origin:");
        for (name, _) in &declared {
            // Names only. The value is the thing this store exists to keep.
            println!("  {name}  <set>");
        }
        return 0;
    }

    let Some(name) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("svrn mesh media declare: name a header, e.g. `x-emby-token`.");
        eprintln!("Run with --help for the whole shape.");
        return 2;
    };

    if !commonwealth_media::valid_header_name(&name.to_ascii_lowercase()) {
        eprintln!(
            "svrn mesh media declare: {name:?} is not a usable header name \
             (letters, digits, - and _ only, 64 chars max)."
        );
        return 2;
    }

    if args.iter().any(|a| a == "--clear") {
        return match commonwealth_media::write_declared_in(&dir, name, "") {
            Ok(()) => {
                println!(
                    "Cleared {}. Run `svrn daemon reload` to apply.",
                    name.to_ascii_lowercase()
                );
                0
            }
            Err(e) => {
                eprintln!("Could not clear {name}: {e}");
                1
            }
        };
    }

    if atty_stdin() {
        eprintln!("svrn mesh media declare: the value is read from STDIN, so it stays out of");
        eprintln!("your shell history. Pipe it:");
        eprintln!();
        eprintln!("  printf '%%s' \"$KEY\" | svrn mesh media declare {name}");
        return 2;
    }

    let mut value = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut value) {
        eprintln!("Could not read the value from stdin: {e}");
        return 1;
    }
    if value.trim().is_empty() {
        eprintln!("svrn mesh media declare: stdin was empty — nothing stored.");
        eprintln!("To remove a declaration, use --clear.");
        return 2;
    }

    match commonwealth_media::write_declared_in(&dir, name, &value) {
        Ok(()) => {
            // What is SET, never what it is.
            println!(
                "Stored {} (0600, {}). Run `svrn daemon reload` to apply.",
                name.to_ascii_lowercase(),
                dir.display()
            );
            0
        }
        Err(e) => {
            eprintln!("Could not store {name}: {e}");
            1
        }
    }
}

#[cfg(unix)]
fn atty_stdin() -> bool {
    // SAFETY: `isatty` reads a descriptor's mode and has no side effects.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

#[cfg(not(unix))]
fn atty_stdin() -> bool {
    false
}
