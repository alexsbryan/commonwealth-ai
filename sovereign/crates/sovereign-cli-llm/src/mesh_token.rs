// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh token` — a client-API credential per device, revocable alone.
//!
//! An invite makes someone a MEMBER. A grant makes them a GUEST, scoped and
//! short-lived (`svrn mesh grant`, [`crate::mesh_guest`]). A named token is
//! neither: it is the full client API, for a machine you own, held until you
//! take it back. Lending the API used to mean handing over `[daemon]
//! client_token` — the one everything else already holds — so taking it back
//! meant rotating it for the desktop, the editor and the script on the NAS as
//! well. `docs/THREAT_MODEL.md` §Known gaps entry 3.
//!
//! This lives outside `mesh_cmd.rs` for the reason [`crate::mesh_guest`] does:
//! that file is long past the §3.1 split threshold. The dispatch there is one
//! arm; everything else is here. The three routes are on the daemon's OPERATOR
//! listener, so this drives `127.0.0.1` and no other address — minting a
//! credential is not something a peer may ask for.

use sovereign_cli_shared::help::{Help, HelpSection};

use crate::mesh_guest::{daemon_client_port, error_text, http_client};

pub(crate) const HELP_MESH_TOKEN: Help = Help {
    command: "svrn mesh token",
    summary: "Give one machine its own client-API token, revocable without touching the others.",
    sections: &[
        HelpSection::Usage(
            "svrn mesh token --new <label>\n\
             svrn mesh token --list\n\
             svrn mesh token --revoke <label>",
        ),
        HelpSection::Flags(&[
            (
                "--new <label>",
                "Mint a token and print it once. The label is how you revoke it later:\n                    letters, digits, '-' and '_'.",
            ),
            ("--list", "Which machines this node admits, by label. Never the tokens."),
            (
                "--revoke <label>",
                "Stop admitting that one. Takes effect on the next request — no restart,\n                    and every other token keeps working.",
            ),
        ]),
        HelpSection::Notes(
            "A named token is the WHOLE client API, not a scoped slice of it — that is\n\
             `svrn mesh grant`, which is what to reach for when lending a model to someone\n\
             you do not own the machine of.\n\n\
             The daemon-wide `[daemon] client_token` keeps working alongside these. Set\n\
             `[daemon] client_tokens = \"named-only\"` in ~/.svrnmesh/config.toml once every\n\
             machine has its own, and the shared one is refused with a sentence saying so.",
        ),
    ],
};

pub(crate) async fn cmd_token(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        sovereign_cli_shared::help::print(&HELP_MESH_TOKEN);
        return 0;
    }

    let mut new_label: Option<String> = None;
    let mut revoke_label: Option<String> = None;
    let mut list = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--new" => {
                i += 1;
                match args.get(i) {
                    Some(l) => new_label = Some(l.clone()),
                    None => {
                        eprintln!("--new needs a label: `svrn mesh token --new laptop`");
                        return 2;
                    }
                }
            }
            "--revoke" => {
                i += 1;
                match args.get(i) {
                    Some(l) => revoke_label = Some(l.clone()),
                    None => {
                        eprintln!("--revoke needs the label from `svrn mesh token --list`");
                        return 2;
                    }
                }
            }
            "--list" => list = true,
            other => {
                eprintln!("Unknown flag for `svrn mesh token`: {other}");
                eprintln!("`svrn mesh token --help` lists what it takes.");
                return 2;
            }
        }
        i += 1;
    }

    // Three verbs, one at a time. Asking for two is a contradiction rather
    // than a sequence — the same refusal `mesh grant` makes for `--wall
    // --rail`, and for the same reason: doing one of them silently leaves the
    // operator believing something untrue about what they just did.
    if [new_label.is_some(), revoke_label.is_some(), list]
        .iter()
        .filter(|x| **x)
        .count()
        > 1
    {
        eprintln!("Pick one: --new, --list or --revoke.");
        return 2;
    }

    let port = daemon_client_port();
    if !crate::mesh_cmd::daemon_listening_on(port).await {
        eprintln!("No daemon detected on :{port} — client tokens live in its state.");
        eprintln!("Start it with `svrn daemon start`, then re-run.");
        return 1;
    }

    if let Some(label) = new_label {
        return token_new(port, &label).await;
    }
    if let Some(label) = revoke_label {
        return token_revoke(port, &label).await;
    }
    if list {
        return token_list(port).await;
    }
    eprintln!("Nothing asked for. `svrn mesh token --help`.");
    2
}

async fn token_new(port: u16, label: &str) -> i32 {
    let client = match http_client(10) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let url = format!("http://127.0.0.1:{port}/internal/client/token");
    let resp = match client
        .post(&url)
        .json(&serde_json::json!({ "label": label }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to reach the daemon at {url}: {e}");
            return 1;
        }
    };
    if !resp.status().is_success() {
        eprintln!("Could not mint a token: {}", error_text(resp).await);
        return 1;
    }
    let payload: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Daemon returned a non-JSON token: {e}");
            return 1;
        }
    };
    let Some(token) = payload.get("token").and_then(|v| v.as_str()) else {
        eprintln!("Daemon minted nothing it would name a token.");
        return 1;
    };
    println!();
    println!("Token for '{label}':");
    println!();
    println!("  {token}");
    println!();
    println!("Send it as `Authorization: Bearer <token>`. It is printed once here and");
    println!("kept at <data_dir>/client-tokens/{label}.token, mode 0600.");
    println!();
    println!("  svrn mesh token --revoke {label}");
    println!();
    0
}

async fn token_list(port: u16) -> i32 {
    let client = match http_client(10) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let url = format!("http://127.0.0.1:{port}/internal/client/token/list");
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to reach the daemon at {url}: {e}");
            return 1;
        }
    };
    if !resp.status().is_success() {
        eprintln!("Could not list client tokens: {}", error_text(resp).await);
        return 1;
    }
    let rows: Vec<serde_json::Value> = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Daemon returned a non-JSON token list: {e}");
            return 1;
        }
    };
    if rows.is_empty() {
        println!("No named client tokens. The shared [daemon] client_token is the only one.");
        return 0;
    }
    println!();
    println!("{:<24}  {}", "LABEL", "FINGERPRINT");
    for row in &rows {
        let label = row.get("label").and_then(|v| v.as_str()).unwrap_or("?");
        let fp = row
            .get("fingerprint")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        println!("{label:<24}  {fp}");
    }
    println!();
    0
}

async fn token_revoke(port: u16, label: &str) -> i32 {
    let client = match http_client(10) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let url = format!("http://127.0.0.1:{port}/internal/client/token/revoke");
    let resp = match client
        .post(&url)
        .json(&serde_json::json!({ "label": label }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Failed to reach the daemon at {url}: {e}");
            return 1;
        }
    };
    if !resp.status().is_success() {
        eprintln!("Could not revoke: {}", error_text(resp).await);
        return 1;
    }
    let payload: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    // Revoking something that was never there is reported as such rather than
    // as success — an operator who mistyped a label must not walk away
    // believing a live credential is dead.
    if payload.get("revoked").and_then(|v| v.as_bool()) == Some(true) {
        println!("Revoked '{label}'. The next request bearing that token is refused.");
        println!("Every other named token, and the shared one, are untouched.");
        0
    } else {
        eprintln!("No token named '{label}' — nothing was revoked.");
        eprintln!("`svrn mesh token --list` shows the labels this node admits.");
        1
    }
}
