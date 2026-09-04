// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn ring` — the mesh as a deployment target whose unit is a group.
//!
//! # What this verb is for
//!
//! A builder who finishes something on their laptop and wants it to exist for
//! exactly their trust ring has had nowhere to put it. A VPS makes it public
//! infrastructure with accounts to manage; Tailscale gives reachability but no
//! in-app identity; a Discord bot puts the data at Discord; the local-first
//! stack syncs data but gives you nowhere to run anything and no idea who is
//! asking. `ring` is the missing verb: **deploy to the people, not to a host.**
//!
//! # The three lines a housemate actually types
//!
//! ```text
//! svrn ring roster add alex --self          # bind my name to my node key
//! svrn ring dev house-expenses              # serve the app, open the tab
//! svrn ring log house-expenses              # what is on the journal, and what is missing
//! ```
//!
//! Everything else is scaffolding for the app itself.
//!
//! # Where the authority lives
//!
//! `ring dev` mints a [`Scope::Rails`] grant against the local daemon and holds
//! the token itself, so the browser tab never sees a credential and the app
//! reaches exactly one namespace's journal and nothing else on the daemon.
//! The grant is minted per run and dies with it.
//!
//! # Why there is no `svrn ring balances`
//!
//! There was, and it could not survive the rail carrying an opaque payload.
//! A balance is an *expense app's* reading of a journal; the rail — and
//! therefore this CLI — does not know what a payload means, and a terminal
//! that printed balances for one tenant would be the money rules living in a
//! second place (ARCH §10.6). `ring log` shows what the rail can honestly
//! say: who wrote what, in the order every node applies it, and what could
//! not be accounted for. The balances are rendered by the app, which is the
//! only thing that knows what one is.
//!
//! **The roster is written from here and is not reachable from the rail at
//! all.** There is no roster route, so a deployed app cannot add a key to the
//! ring — including its own. That is a property of the route set rather than a
//! check, which is the same move the rail itself makes (ARCH §7.1).
//!
//! [`Scope::Rails`]: commonwealth_knowledge::Scope::Rails

use std::collections::BTreeMap;

/// A dev grant lives as long as the dev server, and a housemate leaves one
/// running all evening. Long enough not to expire mid-session, short enough
/// that a forgotten one is not a standing key.
const DEV_GRANT_TTL_SECS: u64 = 12 * 3600;

pub async fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("new") => run_new(&args[1..]),
        Some("roster") => run_roster(&args[1..]).await,
        Some("dev") => run_dev(&args[1..]).await,
        Some("log") => run_log(&args[1..]).await,
        _ => {
            eprintln!(
                "usage:\n\
                 \x20 svrn ring new <dir> [--name <title>]\n\
                 \x20 svrn ring roster add <person> (--key <node-pubkey-hex> | --self) --ring <ns>\n\
                 \x20 svrn ring roster list --ring <ns>\n\
                 \x20 svrn ring dev <ns> [--dir <bundle-dir>] [--port <n>]\n\
                 \x20 svrn ring log <ns> [--json]\n\n\
                 new     scaffold a ring app (index.html, app.js, its reducer and its tests).\n\
                 roster  bind a person's name to the node key they sign with.\n\
                 dev     mint a rail grant and serve the app at http://127.0.0.1:4318/.\n\
                 log     the acts on this journal, in the order every node applies them,\n\
                 \x20       and everything the rail could not account for.\n\n\
                 A ring namespace is created by its first write — there is nothing to\n\
                 provision. Start with `roster add`, because an op signed by a key no\n\
                 roster claims is a gap rather than an act.\n\n\
                 What an act MEANS — a balance, a borrowed drill — is the app's, not\n\
                 this CLI's. Open the app with `ring dev` to see it rendered."
            );
            2
        }
    }
}

mod dev;
mod scaffold;

use dev::run_dev;
use scaffold::run_new;

// ── shared plumbing ──────────────────────────────────────────

/// This node's ring journal for one namespace.
///
/// Opening one is free — [`RingJournal::open`] touches no disk — and it is
/// how the CLI and the daemon stay on ONE path and ONE roster serialisation.
/// Until cw-lift 1c this module joined `rings/<ns>/roster.json` itself and
/// wrote it with its own `to_string_pretty`, which was a second answer to a
/// question a ring cannot afford two answers to (ARCH §10.6). `sovereign_root()`
/// IS the daemon's data dir, which is what makes the two agree.
///
/// It also gains the namespace check for free: the daemon refuses to open a
/// namespace that is not a plain directory name, so a roster written under
/// one was a file nothing would ever read.
fn ring_journal(namespace: &str) -> Result<commonwealth_rail::RingJournal, String> {
    commonwealth_rail::RingJournal::open(&sovereign_cli_shared::dirs::sovereign_root(), namespace)
        .map_err(|e| e.to_string())
}

/// Read the daemon's client port from config rather than hardcoding 9741 —
/// a sandbox pointed at its own daemon must not act on the operator's.
fn daemon_client_port() -> u16 {
    sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.daemon.client_port)
        .unwrap_or(9741)
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("http client: {e}"))
}

async fn error_text(resp: reqwest::Response) -> String {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
        .unwrap_or(body);
    if detail.is_empty() {
        status.to_string()
    } else {
        format!("{status}: {detail}")
    }
}

/// One operator-side read of a namespace: the admitted acts, the gaps, and
/// the roster the DAEMON actually loaded.
///
/// Loopback with no grant, so the daemon trusts the listener rather than a
/// token — which is exactly why the rail routes are mounted on the operator
/// surface as well as the rail one.
async fn rail_log(namespace: &str) -> Result<serde_json::Value, String> {
    let port = daemon_client_port();
    let url = format!("http://127.0.0.1:{port}/v1/rail/log?namespace={namespace}");
    let resp = http_client()?
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("cannot reach the daemon at {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(error_text(resp).await);
    }
    resp.json().await.map_err(|e| format!("bad response: {e}"))
}

/// Mint a grant that reaches exactly one namespace's rail and nothing else.
async fn mint_rail_grant(namespace: &str) -> Result<String, String> {
    let port = daemon_client_port();
    let url = format!("http://127.0.0.1:{port}/internal/guest/grant");
    let body = serde_json::json!({
        "scopes": { "rail": namespace },
        "ttl_secs": DEV_GRANT_TTL_SECS,
        "label": format!("ring dev: {namespace}"),
    });
    let resp = http_client()?
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("cannot reach the daemon at {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(error_text(resp).await);
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| format!("bad grant: {e}"))?;
    v.get("token")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .ok_or_else(|| "the daemon's grant response carried no token".to_string())
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// The leading bare word, if there is one.
///
/// Only the FIRST argument counts: a bare word later in the line is the value
/// of the flag before it, and treating it as positional is how
/// `--ring house` quietly becomes "the person is called house".
fn positional(args: &[String]) -> Option<&str> {
    args.first()
        .map(String::as_str)
        .filter(|a| !a.starts_with("--"))
}

// ── roster ───────────────────────────────────────────────────

async fn run_roster(args: &[String]) -> i32 {
    // Subcommand first, then the ring: a bare `svrn ring roster` should print
    // the shape of the command, not a complaint about one flag of it.
    let sub = args.first().map(String::as_str);
    if !matches!(sub, Some("add") | Some("list")) {
        eprintln!(
            "usage:\n\
             \x20 svrn ring roster add <person> (--key <hex> | --self) --ring <ns>\n\
             \x20 svrn ring roster list --ring <ns>"
        );
        return 2;
    }
    let Some(namespace) = flag(args, "--ring") else {
        eprintln!("ring roster: which ring? pass --ring <namespace>");
        return 2;
    };
    match sub {
        Some("add") => roster_add(namespace, &args[1..]).await,
        _ => roster_list(namespace).await,
    }
}

/// This node's own signing key, as the rail names it: hex of the Ed25519
/// public key. The same value `Op.actor` carries.
fn self_actor() -> Result<String, String> {
    let data_dir = sovereign_cli_shared::dirs::sovereign_root();
    let key = commonwealth_transport::identity::load_or_generate_node_key(&data_dir);
    Ok(commonwealth_rail::actor_of(&key))
}

async fn roster_add(namespace: &str, args: &[String]) -> i32 {
    let Some(person) = positional(args) else {
        eprintln!("ring roster add: which person? `svrn ring roster add alex --self --ring <ns>`");
        return 2;
    };
    let key = if args.iter().any(|a| a == "--self") {
        match self_actor() {
            Ok(k) => k,
            Err(e) => {
                eprintln!("ring roster add: {e}");
                return 1;
            }
        }
    } else if let Some(k) = flag(args, "--key") {
        k.to_string()
    } else {
        eprintln!(
            "ring roster add: name the key — `--self` for this workstation, or\n\
             `--key <hex>` with what the other person's `svrn ring roster add … --self` printed."
        );
        return 2;
    };
    if hex::decode(&key).map(|b| b.len()) != Ok(32) {
        eprintln!(
            "ring roster add: `{key}` is not a node public key — expected 64 hex characters.\n\
             The person joining runs `svrn ring roster add <their-name> --self --ring {namespace}`\n\
             and reads their key off that output."
        );
        return 2;
    }

    let journal = match ring_journal(namespace) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("ring roster add: {e}");
            return 1;
        }
    };
    let path = journal.roster_path();
    // A MISSING roster is an empty ring; an UNREADABLE one is an error and
    // says so. The hand-rolled read this replaced defaulted on any read
    // failure at all, so a permission problem silently became "nobody is in
    // this ring" and the next write dropped every key already in it
    // (ARCH §18.3).
    let mut roster = match journal.roster() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ring roster add: {} is not readable: {e}", path.display());
            return 1;
        }
    };
    let entry = roster
        .members
        .entry(commonwealth_rail::Person::from(person))
        .or_default();
    if entry.iter().any(|k| k == &key) {
        println!("{person} already signs with that key in `{namespace}`.");
        return 0;
    }
    entry.push(key.clone());

    if let Err(e) = journal.set_roster(&roster) {
        eprintln!("ring roster add: write {}: {e}", path.display());
        return 1;
    }
    println!("{person} → {key}");
    println!("  ring: {namespace}");
    println!("  file: {}", path.display());

    // Read it back through the DAEMON, not off the disk we just wrote. A
    // roster the daemon does not load is a roster that does nothing, and the
    // two would differ silently if this CLI and that daemon ever disagreed
    // about where a ring lives (ARCH §18.1 — assert on what the subject
    // cannot echo back).
    match rail_log(namespace).await {
        Ok(v) => {
            let loaded = v
                .get("roster")
                .and_then(|r| r.get("members"))
                .and_then(|m| m.get(person))
                .and_then(|k| k.as_array())
                .map(|k| k.iter().any(|v| v.as_str() == Some(key.as_str())))
                .unwrap_or(false);
            if loaded {
                println!("  the daemon has it.");
            } else {
                eprintln!(
                    "\nWARNING: the daemon does not report this entry. The roster was written\n\
                     to {} but the running daemon is reading a different one — check that it\n\
                     was started against this data directory.",
                    path.display()
                );
                return 1;
            }
        }
        Err(e) => {
            println!("  (daemon not reachable to confirm: {e})");
        }
    }
    0
}

async fn roster_list(namespace: &str) -> i32 {
    match rail_log(namespace).await {
        Ok(v) => {
            let members = v
                .get("roster")
                .and_then(|r| r.get("members"))
                .and_then(|m| m.as_object())
                .cloned()
                .unwrap_or_default();
            if members.is_empty() {
                println!(
                    "`{namespace}` has no roster yet — every op will fold to an unknown-signer gap.\n\
                     Add yourself: svrn ring roster add <you> --self --ring {namespace}"
                );
                return 0;
            }
            for (person, keys) in members {
                let keys: Vec<String> = keys
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|k| k.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                println!("{person}");
                for k in keys {
                    println!("  {k}");
                }
            }
            0
        }
        Err(e) => {
            eprintln!("ring roster list: {e}");
            1
        }
    }
}

// ── log ──────────────────────────────────────────────────────

/// `svrn ring log <ns>` — what this node holds, as the rail sees it.
///
/// Deliberately NOT an app view. The payload column is the app's own JSON,
/// printed as it was signed, because the rail has no way to render it and
/// guessing at one for the tenant that happens to be in front of us is how a
/// second expense implementation gets born.
async fn run_log(args: &[String]) -> i32 {
    let Some(namespace) = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
    else {
        eprintln!("ring log: which ring? `svrn ring log <namespace>`");
        return 2;
    };
    let v = match rail_log(namespace).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ring log: {e}");
            return 1;
        }
    };
    if args.iter().any(|a| a == "--json") {
        println!("{v}");
        return 0;
    }

    let empty = Vec::new();
    let ops = v.get("ops").and_then(|o| o.as_array()).unwrap_or(&empty);
    let gaps = v.get("gaps").and_then(|g| g.as_array()).unwrap_or(&empty);
    let held = v.get("held").and_then(|h| h.as_u64()).unwrap_or(0);

    println!(
        "{namespace} — {} act(s) admitted from {held} line(s) held",
        ops.len()
    );
    println!();
    if ops.is_empty() {
        println!("  nothing recorded yet.");
    }
    for op in ops {
        println!("  {}", op_line(op));
    }

    // The gaps are not a footnote. Acts printed without them are the exact
    // failure this rail exists to avoid: a confident answer over a subset.
    println!();
    if gaps.is_empty() {
        println!("  complete — every op this node holds is accounted for.");
        return 0;
    }
    println!("  INCOMPLETE — this is what could be read, and:");
    for gap in gaps {
        // The sentence comes from the RAIL, on the wire. Rendering it here
        // would be a second wording of the same condition, and the terminal
        // and the app's page would drift (ARCH §10.6). A gap without one is
        // still printed — dropping it would turn "your peer is newer than
        // you" into silence, which is the failure the gap list prevents.
        match gap.get("message").and_then(|m| m.as_str()) {
            Some(sentence) => println!("    {sentence}"),
            None => println!("    {gap}"),
        }
    }
    println!();
    println!("  A gap does not make the acts above wrong, it makes them PARTIAL.");
    println!("  Sequence holes usually close on their own — peers re-send within a minute.");
    0
}

/// One admitted op as one line: when, who, and what they said.
///
/// Split out and pure so the shape is testable without a daemon — the render
/// carries three facts that are easy to drop by accident (that an op was
/// voided, that a correction names a target, that a correction may state no
/// replacement) and each of those silently changes what a reader concludes.
fn op_line(op: &serde_json::Value) -> String {
    let who = op.get("person").and_then(|p| p.as_str()).unwrap_or("?");
    let when = op
        .get("ts_unix")
        .and_then(|t| t.as_i64())
        .map(short_stamp)
        .unwrap_or_else(|| "?".into());
    let mut what = match op.get("payload") {
        Some(p) => p.to_string(),
        // A correction that states no replacement is not an empty act, it is
        // a withdrawal — saying "voids X" and nothing else is the truth.
        None => "(no replacement)".to_string(),
    };
    if let Some(target) = op.get("corrects").and_then(|c| c.as_str()) {
        what = format!("corrects {} → {what}", short_id(target));
    }
    let mark = if op.get("voided").and_then(|v| v.as_bool()).unwrap_or(false) {
        // Not hidden. A voided act is part of the history, and an app that
        // shows the correction without the thing corrected leaves a reader
        // unable to check the change.
        " [voided]"
    } else {
        ""
    };
    format!("{when}  {who:<12}{mark} {what}")
}

fn short_id(id: &str) -> String {
    if id.len() > 12 {
        format!("{}…", &id[..12])
    } else {
        id.to_string()
    }
}

/// `YYYY-MM-DD HH:MM` in UTC. Enough to order a conversation about the
/// journal, without a date library.
fn short_stamp(ts: i64) -> String {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant's algorithm), shifted to the 0000-03-01
    // era so leap years fall at the end of the cycle.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_reads_as_a_date_a_person_can_match_to_a_conversation() {
        assert_eq!(short_stamp(0), "1970-01-01 00:00");
        assert_eq!(short_stamp(1_788_048_000), "2026-08-30 00:00");
        // A leap day, because the civil-from-days arithmetic is where this
        // kind of hand-rolled conversion goes wrong.
        assert_eq!(short_stamp(1_709_164_800), "2024-02-29 00:00");
    }

    /// The three facts this line must not silently drop.
    #[test]
    fn a_log_line_says_who_wrote_it_what_it_voids_and_whether_it_was_voided() {
        let plain = serde_json::json!({
            "person": "alex", "ts_unix": 1_756_512_000,
            "payload": { "kind": "expense" }, "voided": false,
        });
        let line = op_line(&plain);
        assert!(line.contains("alex"), "{line}");
        assert!(line.contains("expense"), "{line}");
        assert!(!line.contains("voided"), "{line}");

        let voided = serde_json::json!({
            "person": "bo", "ts_unix": 1_756_512_000,
            "payload": { "kind": "expense" }, "voided": true,
        });
        assert!(
            op_line(&voided).contains("[voided]"),
            "a voided act must say so"
        );

        let withdrawal = serde_json::json!({
            "person": "cy", "ts_unix": 1_756_512_000,
            "corrects": "ring-0123456789abcdef", "voided": false,
        });
        let line = op_line(&withdrawal);
        assert!(line.contains("corrects ring-0123456…"), "{line}");
        assert!(
            line.contains("no replacement"),
            "a correction that states nothing must not read as an empty act: {line}"
        );
    }
}
