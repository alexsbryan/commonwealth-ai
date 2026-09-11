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
    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn mesh media [<peer>] [--json] [--no-probe]");
        eprintln!();
        eprintln!("With no <peer>: list the members that offer a media origin, as gossip");
        eprintln!("knows them — nothing is dialed. With a <peer>:");
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
