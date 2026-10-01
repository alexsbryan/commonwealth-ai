// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn daemon key` — the one verb that adds, revokes and lists on-prem API
//! keys (decision phase-b-86).
//!
//! It writes the daemon's key store directly, under the configured data
//! directory, and needs no running daemon: install writes the first keys
//! before the unit starts, and on a keyed daemon loopback grants nothing, so
//! an HTTP route would need a key to add the first key. The daemon reads the
//! store when it starts; a change takes effect at the next start.

use sovereign_cli_shared::help::{Help, HelpSection};
use sovereign_core::setup_config::SetupConfig;
use sovereign_daemon::client_tokens::{client_tokens_dir, keys, KEY_ADMIN_GROUP};

const HELP: Help = Help {
    command: "svrn daemon key",
    summary: "Add, revoke or list the API keys an on-prem daemon identifies callers by.",
    sections: &[
        HelpSection::Usage(
            "svrn daemon key --add <sub> [--group <group>]...\n\
             svrn daemon key --revoke <sub>\n\
             svrn daemon key --list",
        ),
        HelpSection::Flags(&[
            ("--add <sub>", "Mint a key for <sub> and print it once. <sub> owns the conversations made with it."),
            ("--group <group>", "A group the key asserts. `admin` reaches ingest and the admin routes."),
            ("--revoke <sub>", "Delete <sub>'s key."),
            ("--list", "Which keys exist, by sub and groups. Never the secrets."),
        ]),
        HelpSection::Notes(
            "Keys live at <data_dir>/client-tokens/<sub>.key, mode 0600. A daemon holding\n\
             any key identifies EVERY caller by key: loopback grants nothing. It reads the\n\
             keys when it starts, so restart it after a change.",
        ),
    ],
};

pub(super) fn cmd_key(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) || args.is_empty() {
        sovereign_cli_shared::help::print(&HELP);
        return if args.is_empty() { 2 } else { 0 };
    }
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
        let parsed = match a.as_str() {
            "--add" => value("--add").map(|v| add = Some(v)),
            "--revoke" => value("--revoke").map(|v| revoke = Some(v)),
            "--group" => value("--group").map(|v| groups.push(v)),
            "--list" => {
                list = true;
                Ok(())
            }
            other => Err(format!("unknown flag for `svrn daemon key`: {other}")),
        };
        if let Err(e) = parsed {
            eprintln!("{e}");
            return 2;
        }
    }
    if [add.is_some(), revoke.is_some(), list]
        .iter()
        .filter(|x| **x)
        .count()
        != 1
    {
        eprintln!("Pick one: --add, --revoke or --list.");
        return 2;
    }
    let config = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "Could not read the daemon config, so the key store's directory is unknown: {e}"
            );
            return 1;
        }
    };
    let dir = client_tokens_dir(&config.data.dir);
    if let Some(sub) = add {
        let token = match sovereign_daemon::client_auth::generate_bearer_token() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Could not mint a key: {e}");
                return 1;
            }
        };
        return match keys::add_key(&dir, &sub, &groups, &token) {
            Ok(row) => {
                println!(
                    "Key for '{}' (groups: {}):",
                    row.sub,
                    groups_text(&row.groups)
                );
                println!();
                println!("  {token}");
                println!();
                println!("Send it as `Authorization: Bearer <key>`. It is printed once here and");
                println!(
                    "kept at {}, mode 0600.",
                    dir.join(format!("{}.key", row.sub)).display()
                );
                println!("Restart the daemon for it to take effect.");
                0
            }
            Err(e) => {
                eprintln!("Could not add the key: {e}");
                1
            }
        };
    }
    if let Some(sub) = revoke {
        return match keys::revoke_key(&dir, &sub) {
            Ok(true) => {
                println!("Revoked '{sub}'. Restart the daemon for it to take effect.");
                0
            }
            Ok(false) => {
                eprintln!("No key named '{sub}' under {}.", dir.display());
                1
            }
            Err(e) => {
                eprintln!("Could not revoke '{sub}': {e}");
                1
            }
        };
    }
    let rows = keys::list_keys(&dir);
    if rows.is_empty() {
        println!(
            "No API keys under {} — this daemon is unkeyed.",
            dir.display()
        );
        return 0;
    }
    for row in rows {
        println!(
            "{}  groups: {}  fingerprint: {}",
            row.sub,
            groups_text(&row.groups),
            row.fingerprint
        );
    }
    0
}

fn groups_text(groups: &[String]) -> String {
    if groups.is_empty() {
        format!("none (not {KEY_ADMIN_GROUP})")
    } else {
        groups.join(", ")
    }
}
