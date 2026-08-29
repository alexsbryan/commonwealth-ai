// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh forget-member`, and the collision warning that points at it.
//!
//! The operator's two ends of the endpoint-key loop: `print_alias_warnings`
//! is the diagnosis `svrn mesh status` prints, `cmd_forget_member` is the
//! repair it names. Split out of `mesh_cmd.rs` rather than added to it —
//! that file is 5,200 lines, well past ARCH §3.1's ceiling, and this is one
//! concern with a seam of its own.
//!
//! The rule itself lives once, in
//! [`commonwealth_core::mesh::aliased_endpoint_keys`]; nothing here compares
//! keys (§10.6).

use sovereign_mesh::mesh_http::MemberDto;

use crate::mesh_cmd::{daemon_client_port, daemon_listening_on};

/// Print a warning for every endpoint key claimed by two ACTIVE members.
///
/// Named here because `svrn mesh status` is where the operator already is,
/// and because a collision is invisible on that screen without it: both rows
/// read `offline` while the endpoint behind them answers on demand, which
/// looks like two dead peers rather than one aliased node.
pub(crate) fn print_alias_warnings(members: &[&MemberDto]) {
    // The diagnosis half of the endpoint-key rule. `merge_from_authenticated`
    // refuses to create a collision and `forget-member` repairs one, but a
    // roster that already holds one shows nothing wrong on this screen: both
    // rows read `offline` while the endpoint behind them answers on demand,
    // which looks like two dead peers rather than one aliased node. Name it
    // where the operator is already looking, and name the repair with it.
    //
    // Asks the same predicate the daemon's admission guard and the DST
    // invariant ask — this does not compare keys itself (ARCH §10.6).
    let claims = members.iter().enumerate().filter_map(|(i, m)| {
        let key = m.node_pubkey.as_deref()?;
        let bytes: [u8; 32] = hex::decode(key).ok()?.try_into().ok()?;
        Some(commonwealth_core::mesh::EndpointClaim {
            // Positional stand-in: the DTO carries node_id as a string with no
            // parse back to `NodeId`, and the index is what we resolve through
            // below. Never displayed.
            node_id: commonwealth_core::ids::NodeId::from_u128(i as u128),
            name: format!("{} ({})", m.name, m.node_id),
            node_pubkey: Some(commonwealth_core::ids::NodePubkey(bytes)),
            active: m.active,
        })
    });
    for alias in commonwealth_core::mesh::aliased_endpoint_keys(claims) {
        println!();
        println!(
            "  WARNING: {} active members share endpoint key {}:",
            alias.members.len(),
            &alias.node_pubkey.to_string()[..16]
        );
        for (_, who) in &alias.members {
            println!("    - {who}");
        }
        println!("  They are one machine. Peers dial the key, so both rows read offline");
        println!("  while the endpoint answers. Retire the stale one:");
        println!("    svrn mesh forget-member <node-id-or-name>");
    }
}

/// `svrn mesh forget-member <node>` — retire ONE member row.
///
/// The repair for an endpoint-key collision, and the missing third of that
/// loop: the rule could be checked (`svrn mesh check-invariants`) and enforced
/// (the gossip admission guard) but not fixed, so a roster that already held a
/// collision stayed broken and every read through it stayed wrong.
pub(crate) async fn cmd_forget_member(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) || args.is_empty() {
        eprintln!("Usage: svrn mesh forget-member <node-id-or-name> [--force]");
        eprintln!();
        eprintln!("Tombstone one member row in the ACTIVE mesh and let gossip carry");
        eprintln!("the removal. `<node>` is an exact member name or a node_id prefix");
        eprintln!("of at least 4 characters; `svrn mesh status` lists both.");
        eprintln!();
        eprintln!("This is the repair for the collision `svrn mesh status` warns about:");
        eprintln!("two active members sharing one endpoint key. Retire the stale row.");
        eprintln!();
        eprintln!("Refuses on this node (use `svrn mesh leave`) and on a member that is");
        eprintln!("online and NOT aliased — that would be an eviction, and the member's");
        eprintln!("next gossip round undoes it anyway. --force overrides the second.");
        return if args.is_empty() { 1 } else { 0 };
    }
    let force = args.iter().any(|a| a == "--force");
    let Some(member) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("Which member? `svrn mesh forget-member <node-id-or-name>`");
        return 1;
    };

    let port = daemon_client_port();
    if !daemon_listening_on(port).await {
        eprintln!("No daemon detected on :{port} — the roster lives in the running daemon.");
        eprintln!("Start it with `svrn daemon start`.");
        return 1;
    }
    let url = format!("http://127.0.0.1:{port}/v1/mesh/forget-member");
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
        .json(&serde_json::json!({ "member": member, "force": force }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mesh forget-member: daemon at {url} not reachable: {e}");
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
    let out: sovereign_mesh::roster_repair::ForgottenMember = match serde_json::from_str(&body) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("mesh forget-member: response shape mismatch ({e}): {body}");
            return 1;
        }
    };
    if out.already_retired {
        println!("\"{}\" was already retired — nothing to do.", out.name);
        return 0;
    }
    if out.was_aliased {
        println!(
            "Retired \"{}\" — it was one of two active rows on one endpoint key.",
            out.name
        );
        println!("Re-run `svrn mesh status`; the survivor should come back online as");
        println!("peers merge the tombstone.");
    } else {
        println!("Retired \"{}\".", out.name);
    }
    println!("The removal travels on the ordinary gossip round.");
    0
}
