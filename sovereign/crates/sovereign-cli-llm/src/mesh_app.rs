// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh app` — the viewer half of published apps.
//!
//! `svrn mesh media <peer>` hands back a URL that reaches ONE origin, because
//! a node could only declare one. A node publishes many now, each named, so
//! this verb hands back a base URL and the app name is the first path segment
//! under it — `<base>/chores/tasks` is the chore app's `/tasks`.
//!
//! Given a name it prints the app's own URL directly, because that is what a
//! person pastes into a browser, and it does ONE real request through the
//! bridge so they see an HTTP status rather than a port number. A bridge
//! accepts whether or not the far end answers, so without the probe "here is
//! your URL" would be a claim nobody watched — the same reason
//! `mesh_media.rs` probes.

use std::time::Duration;

use crate::mesh_cmd::daemon_client_port;

pub(crate) async fn cmd_app(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn mesh app [<peer>] [<app>] [--json] [--no-probe]");
        eprintln!();
        eprintln!("With no <peer>: list the members that publish apps, as gossip knows them —");
        eprintln!("nothing is dialed. With a <peer>: print the base URL that reaches that");
        eprintln!("member's apps over the mesh, dialed by its key, no VPN and no port");
        eprintln!("forwarded. With a <peer> and an <app>: print that app's own URL, ready to");
        eprintln!("paste into a browser.");
        eprintln!();
        eprintln!("<peer> is a member name or a node-id prefix of at least 4 chars;");
        eprintln!("`svrn mesh status` lists both. The peer must publish something:");
        eprintln!("  svrn publish chores 5000");
        eprintln!();
        eprintln!("Your requests arrive at the app with your verified identity attached");
        eprintln!("(X-Mesh-Member, X-Mesh-Node, X-Mesh-Pubkey) — the app never writes a");
        eprintln!("login page, and a client cannot forge those headers.");
        eprintln!();
        eprintln!("Flags:");
        eprintln!("  --json       Raw JSON from the daemon (peer, node_id, url, via, path).");
        eprintln!("  --no-probe   Skip the GET through the bridge; print the URL only.");
        return 0;
    }
    let json_out = args.iter().any(|a| a == "--json");
    let probe = !args.iter().any(|a| a == "--no-probe");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let peer = positional.first().map(|s| s.as_str());
    let app = positional.get(1).map(|s| s.as_str());

    let port = daemon_client_port();
    let url = format!("http://127.0.0.1:{port}/v1/mesh/app");
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
        return list_publishers(&client, &url, port, json_out).await;
    };

    let resp = match client.get(&url).query(&[("peer", peer)]).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh app: daemon at {url} not reachable: {e}");
            eprintln!("The bridge lives in the running daemon — `svrn daemon start`.");
            return 1;
        }
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        eprint!(
            "{}",
            crate::mesh_skew::render_failure(&client, port, "GET /v1/mesh/app", status, body).await
        );
        return 1;
    }
    let reach: sovereign_mesh::media_reach::MediaReach = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh app: response shape mismatch ({e}): {body}");
            return 1;
        }
    };
    if json_out {
        println!("{body}");
        return 0;
    }

    // Without an app name there is nothing to probe: the base URL names no
    // app, and the acceptor answers a nameless request with the 404 that
    // lists what IS published. That refusal is the useful output, so the
    // no-name form fetches it rather than skipping the request.
    let Some(app) = app else {
        println!("{}  (via {})", reach.url, reach.via);
        println!();
        match probe_published(&client, &reach.url).await {
            Some(list) => println!("{} publishes: {list}", reach.peer),
            None => println!(
                "Could not read what {} publishes — the peer may be asleep, or may publish \
                 nothing. `svrn mesh app {}` again once it is up.",
                reach.peer, reach.peer
            ),
        }
        println!();
        println!("Reach one with:  svrn mesh app {} <app>", reach.peer);
        return 0;
    };

    let app_url = format!("{}/{app}", reach.url.trim_end_matches('/'));
    println!("{app_url}");
    if !probe {
        return 0;
    }
    let started = std::time::Instant::now();
    match client
        .get(&app_url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) if r.status().as_u16() == 404 => {
            // The acceptor's own 404 carries the published list; the app's
            // would not. Either way the person needs the body, not the code —
            // and a body that could not be READ is said so rather than
            // printed as a blank line, which is the failure `mesh_skew` was
            // written for (ARCH §6).
            eprintln!();
            match r.text().await {
                Ok(text) if !text.trim().is_empty() => eprint!("{text}"),
                Ok(_) => eprintln!(
                    "  {app_url} → 404 with an empty body. That is not the acceptor's \
                     refusal (it names what is published), so the app itself answered 404 \
                     — check the path under /{app}."
                ),
                Err(e) => eprintln!("  {app_url} → 404, and its body could not be read ({e})."),
            }
            1
        }
        Ok(r) => {
            println!(
                "  GET /{app} → {} in {} ms — paste that URL into a browser.",
                r.status(),
                started.elapsed().as_millis()
            );
            0
        }
        Err(e) => {
            eprintln!(
                "  GET /{app} failed after {} ms: {e}",
                started.elapsed().as_millis()
            );
            eprintln!(
                "  The peer refuses this dial or is not reachable — it may not be a member, \
                 may publish no apps, or may have you outside its `[iroh] app_allow`."
            );
            1
        }
    }
}

/// Ask the bridge with no app named. The acceptor answers 404 with the list
/// of published names, which is the only way to learn them without the peer
/// gossiping its registry — and gossiping it would publish every housemate's
/// app names to the whole mesh whether or not they may reach them.
async fn probe_published(client: &reqwest::Client, base: &str) -> Option<String> {
    let resp = client
        .get(format!("{}/", base.trim_end_matches('/')))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    let body = resp.text().await.ok()?;
    body.lines()
        .find_map(|l| l.strip_prefix("this node publishes: "))
        .map(str::to_string)
}

/// `svrn mesh app` with no peer: the members that publish apps, from gossip.
async fn list_publishers(client: &reqwest::Client, url: &str, port: u16, json_out: bool) -> i32 {
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh app: daemon at {url} not reachable: {e}");
            eprintln!("The roster lives in the running daemon — `svrn daemon start`.");
            return 1;
        }
    };
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        eprint!(
            "{}",
            crate::mesh_skew::render_failure(client, port, "GET /v1/mesh/app", status, body).await
        );
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
            eprintln!("mesh app: response shape mismatch ({e}): {body}");
            return 1;
        }
    };
    if offers.offering.is_empty() {
        println!(
            "No member publishes an app yet. Anyone can, in one line:\n  \
             svrn publish chores 5000\nIt shows here within one gossip round."
        );
        return 0;
    }
    println!("Members publishing apps:");
    for o in &offers.offering {
        let status = format!("{:?}", o.status).to_ascii_lowercase();
        let path = o
            .path
            .as_ref()
            .map(|p| match &p.relay {
                Some(r) => format!("{} via {r}", p.path),
                None => p.path.clone(),
            })
            .unwrap_or_else(|| "no path yet".to_string());
        println!("  {:<16} {:<10} {}", o.peer, status, path);
    }
    println!();
    println!("What each publishes:  svrn mesh app <peer>");
    0
}
