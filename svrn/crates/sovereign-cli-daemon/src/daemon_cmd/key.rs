// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn daemon key` — the one verb that adds, revokes and lists the daemon's
//! named credentials: one record `{name, token, groups}` that resolves to
//! `Principal::Asserted { sub: name, groups }` (ADDRESSED_TEXT §5.5 rule 2).
//!
//! It replaced `svrn mesh token` as well as its own old self. A credential
//! belongs to the daemon's client API, not to the mesh, and this verb works
//! with no daemon running, which an on-prem install needs: it writes the
//! first keys before the unit starts, and under `loopback = "none"` an
//! HTTP-only verb could not mint the first key.
//!
//! Two paths, one store:
//!
//! - **A daemon listens** on this config's client port: the verb asks it, on
//!   127.0.0.1, through the owner-only routes. The change is live on the next
//!   request, revocation included. Under `loopback = "none"` the routes need
//!   an admin key, which the verb presents from `SOVEREIGN_API_KEY`.
//! - **None listens**: the verb edits `<data_dir>/client-tokens` directly, and
//!   the daemon reads it when it starts.
//!
//! It never falls back from the first to the second: a running daemon that
//! refused would otherwise keep admitting a credential this verb reported as
//! revoked.

use std::time::Duration;

use sovereign_cli_shared::help::{Help, HelpSection};
use sovereign_core::setup_config::SetupConfig;
use sovereign_daemon::client_tokens::{
    client_tokens_dir, ClientTokenStore, LoopbackPosture, KEY_ADMIN_GROUP,
};

const HELP: Help = Help {
    command: "svrn daemon key",
    summary: "Add, revoke or list the named credentials this daemon admits.",
    sections: &[
        HelpSection::Usage(
            "svrn daemon key --add <name> [--group <group>]...\n\
             svrn daemon key --revoke <name>\n\
             svrn daemon key --list",
        ),
        HelpSection::Flags(&[
            ("--add <name>", "Mint a credential named <name> and print it once. Calls made with it are logged by <name>, and conversations made with it are owned by it."),
            ("--group <group>", "A group the credential asserts. `admin` reaches ingest, the admin routes, and minting."),
            ("--revoke <name>", "Stop admitting <name>. Live on the next request when a daemon is running."),
            ("--list", "Which credentials exist, by name and groups. Never the secrets."),
        ]),
        HelpSection::Notes(
            "Credentials live at <data_dir>/client-tokens/<name>.key, mode 0600. A client sends\n\
             one as `Authorization: Bearer <key>`; every key starts with `svrn_`.\n\n\
             Minting needs `[daemon] loopback` declared in config.toml: \"owner\" (a local\n\
             process presenting nothing is the owner, a desktop) or \"none\" (every caller\n\
             presents a key, an on-prem box). With a daemon running, the verb asks it on\n\
             127.0.0.1 and presents SOVEREIGN_API_KEY when set; with none, it edits the store.",
        ),
    ],
};

/// What was asked for.
enum Action {
    Add { name: String, groups: Vec<String> },
    Revoke(String),
    List,
}

pub(super) async fn cmd_key(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) || args.is_empty() {
        sovereign_cli_shared::help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }
    let action = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let config = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "Could not read the daemon config, so the key store's directory is unknown: {e}"
            );
            return 1;
        }
    };
    let port = config.daemon.client_port;
    if daemon_listening(port).await {
        tracing::debug!(port, "daemon key: a daemon listens; asking it");
        return online(port, action).await;
    }
    tracing::debug!(port, "daemon key: no daemon listens; editing the store");
    offline(&config, action)
}

fn parse(args: &[String]) -> Result<Action, String> {
    let mut add: Option<String> = None;
    let mut revoke: Option<String> = None;
    let mut groups: Vec<String> = Vec::new();
    let mut list = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| match it.next() {
            Some(v) => Ok(v.clone()),
            None => Err(format!("{flag} needs a value — `svrn daemon key --help`")),
        };
        match a.as_str() {
            "--add" => add = Some(value("--add")?),
            "--revoke" => revoke = Some(value("--revoke")?),
            "--group" => groups.push(value("--group")?),
            "--list" => list = true,
            other => return Err(format!("unknown flag for `svrn daemon key`: {other}")),
        }
    }
    match (add, revoke, list) {
        (Some(name), None, false) => Ok(Action::Add { name, groups }),
        (None, Some(name), false) if groups.is_empty() => Ok(Action::Revoke(name)),
        (None, None, true) if groups.is_empty() => Ok(Action::List),
        _ => Err("Pick one: --add, --revoke or --list (--group goes with --add).".into()),
    }
}

async fn daemon_listening(port: u16) -> bool {
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .is_ok_and(|r| r.is_ok())
}

// ── a daemon listens: the owner-only routes ─────────────────────────────────

async fn online(port: u16, action: Action) -> i32 {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Could not build an HTTP client: {e}");
            return 1;
        }
    };
    let base = format!("http://127.0.0.1:{port}/internal/client/token");
    let request = match &action {
        Action::Add { name, groups } => client
            .post(&base)
            .json(&serde_json::json!({ "name": name, "groups": groups })),
        Action::Revoke(name) => client
            .post(format!("{base}/revoke"))
            .json(&serde_json::json!({ "name": name })),
        Action::List => client.get(format!("{base}/list")),
    };
    let request = match sovereign_core::setup_config::client_credential() {
        Some(key) => request.bearer_auth(key),
        None => request,
    };
    let resp = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("The daemon on 127.0.0.1:{port} did not answer: {e}");
            return 1;
        }
    };
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        let why = body
            .get("error")
            .map(|e| e.to_string())
            .unwrap_or_else(|| body.to_string());
        eprintln!("The running daemon refused ({status}): {why}");
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            eprintln!(
                "Minting and revoking are the owner's: run this as a local process presenting \
                 nothing (loopback = \"owner\"), or export an admin key as SOVEREIGN_API_KEY. \
                 Stopping the daemon lets this verb edit the store directly."
            );
        }
        return 1;
    }
    match action {
        Action::Add { name, groups } => match body.get("token").and_then(|t| t.as_str()) {
            Some(token) => {
                print_minted(&name, &groups, token, true);
                0
            }
            None => {
                eprintln!("The daemon minted nothing it would name a key: {body}");
                1
            }
        },
        Action::Revoke(name) => {
            if body.get("revoked").and_then(|v| v.as_bool()) == Some(true) {
                println!("Revoked '{name}'. The next request presenting it is refused.");
                0
            } else {
                eprintln!("No credential named '{name}' — nothing was revoked.");
                1
            }
        }
        Action::List => {
            let rows: Vec<Row> = body
                .as_array()
                .map(|a| a.iter().map(Row::from_json).collect())
                .unwrap_or_default();
            print_rows(&rows);
            0
        }
    }
}

// ── no daemon: the store on disk ─────────────────────────────────────────────

fn offline(config: &SetupConfig, action: Action) -> i32 {
    let dir = client_tokens_dir(&config.data.dir);
    let posture = match LoopbackPosture::resolve(config.daemon.loopback.as_deref(), Some(&dir)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let store = ClientTokenStore::load(Some(dir.clone()), posture);
    match action {
        Action::Add { name, groups } => {
            let token = match sovereign_daemon::client_auth::generate_bearer_token() {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("Could not mint a key: {e}");
                    return 1;
                }
            };
            match store.mint(&name, &groups, token.clone()) {
                Ok(row) => {
                    print_minted(&row.name, &row.groups, &token, false);
                    println!(
                        "Kept at {}, mode 0600.",
                        dir.join(format!("{}.key", row.name)).display()
                    );
                    0
                }
                Err(e) => {
                    eprintln!("Could not add the key: {e}");
                    1
                }
            }
        }
        Action::Revoke(name) => {
            if store.revoke(&name) {
                println!("Revoked '{name}'. The daemon reads the store when it starts.");
                0
            } else {
                eprintln!("No credential named '{name}' under {}.", dir.display());
                1
            }
        }
        Action::List => {
            let rows: Vec<Row> = store
                .list()
                .into_iter()
                .map(|r| Row {
                    name: r.name,
                    groups: r.groups,
                    fingerprint: r.fingerprint,
                })
                .collect();
            print_rows(&rows);
            0
        }
    }
}

// ── rendering ────────────────────────────────────────────────────────────────

struct Row {
    name: String,
    groups: Vec<String>,
    fingerprint: String,
}

impl Row {
    fn from_json(v: &serde_json::Value) -> Self {
        let text = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        Self {
            name: text("name"),
            groups: v
                .get("groups")
                .and_then(|g| g.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|g| g.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            fingerprint: text("fingerprint"),
        }
    }
}

/// The one moment a credential is printed. The indented line is the key and
/// nothing else: `distributions/deploy/onprem/install.sh` reads it by that
/// shape.
fn print_minted(name: &str, groups: &[String], token: &str, live: bool) {
    println!("Key for '{name}' (groups: {}):", groups_text(groups));
    println!();
    println!("  {token}");
    println!();
    println!("Send it as `Authorization: Bearer <key>`. It is printed once here.");
    if live {
        println!("The running daemon admits it from the next request.");
    } else {
        println!("The daemon reads it when it starts.");
    }
}

fn print_rows(rows: &[Row]) {
    if rows.is_empty() {
        println!("No named credentials.");
        return;
    }
    for row in rows {
        println!(
            "{}  groups: {}  fingerprint: {}",
            row.name,
            groups_text(&row.groups),
            row.fingerprint
        );
    }
}

fn groups_text(groups: &[String]) -> String {
    if groups.is_empty() {
        format!("none (not {KEY_ADMIN_GROUP})")
    } else {
        groups.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn one_action_at_a_time_and_groups_only_with_add() {
        assert!(matches!(
            parse(&args(&["--add", "claude-code"])),
            Ok(Action::Add { ref name, ref groups }) if name == "claude-code" && groups.is_empty()
        ));
        assert!(matches!(
            parse(&args(&["--add", "it", "--group", "admin"])),
            Ok(Action::Add { ref groups, .. }) if groups == &["admin".to_string()]
        ));
        assert!(matches!(parse(&args(&["--list"])), Ok(Action::List)));
        assert!(matches!(
            parse(&args(&["--revoke", "x"])),
            Ok(Action::Revoke(ref n)) if n == "x"
        ));
        for bad in [
            &["--add", "a", "--list"][..],
            &["--list", "--group", "admin"],
            &["--revoke"],
            &["--new", "laptop"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }
}
