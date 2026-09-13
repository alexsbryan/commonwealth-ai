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
//! `ring dev` mints a rail-scoped grant against the local daemon — `Scope::Rails`,
//! which `commonwealth-knowledge` owns beside the rest of the guest grants — and
//! holds the token itself, so the browser tab never sees a credential and the
//! app reaches exactly one namespace's journal and nothing else on the daemon.
//! The grant is minted per run and dies with it. It is minted over HTTP, so
//! this crate names the scope and does not link the crate that declares it.
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
//! One namespace has a roster nobody writes at all. `mesh-measurements` is the
//! daemon's own ring (cw-lift 2d), and its roster is the mesh's membership,
//! re-derived from local state on every read. That is still not a route —
//! nothing is accepted from a peer, and a key is in it only because a member
//! row already carried it — but it does mean `roster add` and `roster list`
//! have nothing to do there, and both refuse rather than write or read a file
//! the daemon ignores. See [`refuse_derived_roster`]. `ring log` and
//! `ring seal` go over HTTP, and the daemon's rail has ONE roster reader that
//! knows this namespace derives (cw-lift 4a follow-up), so both work on it.
//! When the door refuses a write there it is because this node is in no mesh,
//! and it says exactly that — `svrn mesh create`, or join one — rather than
//! naming the `roster add` this namespace refuses.

use std::collections::BTreeMap;

/// A dev grant lives as long as the dev server, and a housemate leaves one
/// running all evening. Long enough not to expire mid-session, short enough
/// that a forgotten one is not a standing key.
const DEV_GRANT_TTL_SECS: u64 = 12 * 3600;

pub async fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("new") => run_new(&args[1..]),
        Some("roster") => run_roster(&args[1..]).await,
        Some("introduce") => run_introduce(&args[1..]).await,
        Some("dev") => run_dev(&args[1..]).await,
        Some("log") => run_log(&args[1..]).await,
        Some("seal") => run_seal(&args[1..]).await,
        _ => {
            eprintln!(
                "usage:\n\
                 \x20 svrn ring new <dir> [--name <title>]\n\
                 \x20 svrn ring roster add <person> (--key <node-pubkey-hex> | --self) [--on <op-id>] --ring <ns>\n\
                 \x20 svrn ring roster show --ring <ns>\n\
                 \x20 svrn ring introduce <person> --key <node-pubkey-hex> --reason <why> --ring <ns>\n\
                 \x20 svrn ring dev <ns> [--dir <bundle-dir>] [--port <n>]\n\
                 \x20 svrn ring log <ns> [--json]\n\
                 \x20 svrn ring seal <ns>\n\n\
                 new     scaffold a ring app (index.html, app.js, its reducer and its tests).\n\
                 roster  bind a person's name to the node key they sign with, and show why\n\
                 \x20       each key is here.\n\
                 introduce\n\
                 \x20       vouch for a key on the journal, so the row that admits it can name\n\
                 \x20       the act instead of somebody's memory. It admits NOBODY by itself.\n\
                 dev     mint a rail grant and serve the app at http://127.0.0.1:4318/.\n\
                 log     the acts on this journal, in the order every node applies them,\n\
                 \x20       and everything the rail could not account for.\n\
                 seal    retire everything this node wrote before now, and delete it.\n\n\
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

/// Refuse a namespace whose roster is DERIVED rather than written here.
///
/// `mesh-measurements` is the daemon's own ring (cw-lift 2d). Its roster is
/// the mesh's membership, re-derived from state the node already holds on
/// every read (`sovereign_mesh::ring_roster`) — which is what lets an
/// unplaceable signer HEAL when that node finally advertises a key, and why
/// nothing writes it down.
///
/// Every command in this module that touches `roster.json` therefore has to
/// stop here. A file written by `roster add` would be a second answer to who
/// is in that ring (ARCH §10.6): the daemon's rail reads its ONE roster
/// source for this namespace and ignores the file, so the file would be
/// believed by nothing and mislead whoever found it.
///
/// Only the file's readers and writer stop here. `ring log` and `ring seal`
/// ask the daemon, whose rail answers with the derived roster, and both work
/// on this namespace.
fn refuse_derived_roster(namespace: &str) -> Option<String> {
    if namespace != sovereign_core::mesh_measurements::MEASUREMENTS_APP_ID {
        return None;
    }
    Some(format!(
        "`{namespace}` is the daemon's own ring — its roster IS the mesh's membership, \
         derived fresh on every read and never written down.\n\
         Who is in it: svrn mesh status\n\
         What is on it: svrn ring log {namespace}, or svrn mesh plan (peers' runs, attributed)"
    ))
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

/// The rail's two routes and its two operator-side clients — **re-exported,
/// not defined here** (cw-lift 5e).
///
/// They were this module's while `ring` and `job` were the only callers. `svrn
/// quality check --distribute` is a third and it lives in `sovereign-cli`, so
/// the pair moved to `sovereign_cli_shared::rail`, the crate both already
/// link. Re-exported rather than re-imported at each call site because
/// [`dev`](super::ring_cmd::dev) needs the CONSTANTS (it proxies opaque bytes
/// under a grant token and cannot use the functions) and every other caller
/// here needs the functions, so one `use` line serves both and a route renamed
/// on the daemon still breaks the build at every caller.
pub(crate) use sovereign_cli_shared::rail::{
    error_text, rail_append, rail_log, RAIL_APPEND_PATH, RAIL_LOG_PATH,
};

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
    // `show` and `list` are ONE command under two spellings, not two views.
    // The demo asks for `show`, the ring's first users learned `list`, and a
    // second renderer of the same roster is how the terminal and the campaign
    // end up disagreeing about what a row says (ARCH §10.6).
    if !matches!(sub, Some("add") | Some("list") | Some("show")) {
        eprintln!(
            "usage:\n\
             \x20 svrn ring roster add <person> (--key <hex> | --self) [--on <op-id>] --ring <ns>\n\
             \x20 svrn ring roster show --ring <ns>\n\n\
             --on names the `svrn ring introduce` op this key is being admitted on,\n\
             so the row can say WHY it is there. Without it the row reads as\n\
             warrant-unknown, which is what every row written before today says."
        );
        return 2;
    }
    let Some(namespace) = flag(args, "--ring") else {
        eprintln!("ring roster: which ring? pass --ring <namespace>");
        return 2;
    };
    if let Some(why) = refuse_derived_roster(namespace) {
        eprintln!("ring roster: {why}");
        return 2;
    }
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

    // The warrant, BEFORE the file is touched: a row that names an op which
    // does not resolve is precisely the failure this rung exists to prevent,
    // and it must be refused rather than written and explained later.
    let person_name = commonwealth_rail::Person::from(person);
    let vouch = match flag(args, "--on") {
        None => None,
        Some(op_id) => match resolve_warrant(namespace, &person_name, &key, op_id).await {
            Ok(v) => Some(v),
            Err(refusal) => {
                eprintln!("ring roster add: {refusal}");
                return 2;
            }
        },
    };

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
    // (ARCH §18.3). The FILE, by name: this is its writer, and the one
    // caller besides the rail's own door that may read it directly.
    let mut roster = match journal.roster_file() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ring roster add: {} is not readable: {e}", path.display());
            return 1;
        }
    };
    // ONE writer of both halves of a row, so a warrant naming a key no row
    // carries cannot be written. A key already in the ring is left exactly as
    // it is — re-running an add must not quietly restate why somebody is here.
    if !roster.bind_key(person_name, key.clone(), vouch) {
        println!("{person} already signs with that key in `{namespace}`, warrant and all.");
        return 0;
    }

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

/// The roster the DAEMON loaded, and the admission it computed over the same
/// journal, in one read.
///
/// Typed rather than walked as JSON: the roster is a `Roster`, so a row's
/// warrant reaches this command by being part of the type instead of by a
/// second hand-written reader of the wire shape. And the two halves come from
/// ONE answer — a roster read at one moment and an admission read at another
/// could disagree about who was a member when.
async fn roster_and_admission(
    namespace: &str,
) -> Result<(commonwealth_rail::Roster, commonwealth_rail::Admission), String> {
    let v = rail_log(namespace).await?;
    let roster: commonwealth_rail::Roster = serde_json::from_value(
        v.get("roster").cloned().ok_or_else(|| {
            format!("the daemon's log answer carried no `roster` — this build and that daemon do not agree on the shape of `{RAIL_LOG_PATH}`")
        })?,
    )
    .map_err(|e| format!("the daemon's roster is a shape this build cannot read: {e}"))?;
    let admission = sovereign_cli_shared::rail::admission_from_wire(&v)?;
    Ok((roster, admission))
}

/// Resolve the op a `roster add --on` is acting on, or refuse in a sentence.
///
/// Resolution goes through the DAEMON for the same reason the read-back below
/// does: the roster that decides which acts are readable is the one the
/// daemon loaded, and folding the file here would resolve a warrant against a
/// membership the running node does not have.
///
/// The [`Vouch`](commonwealth_rail::Vouch) is MINTED from the op — the
/// operator names an op id and nothing else, so the signer and the date on
/// the row are read off the signed act rather than typed by the person the
/// row is about (ARCH §18.1).
async fn resolve_warrant(
    namespace: &str,
    person: &commonwealth_rail::Person,
    key: &str,
    op_id: &str,
) -> Result<commonwealth_rail::Vouch, String> {
    let (roster, admission) = roster_and_admission(namespace).await.map_err(|e| {
        format!(
            "--on names an op that has to be resolved before a warrant can be written, \n\
             and the daemon did not answer: {e}"
        )
    })?;
    let op = commonwealth_rail::OpId::from_raw(op_id);
    match commonwealth_rail::trace_op(&roster, &admission.ops, person, key, &op) {
        commonwealth_rail::VouchStatus::Traced { by_actor, at, .. } => {
            Ok(commonwealth_rail::Vouch {
                op,
                by: by_actor,
                at,
            })
        }
        // The refusal is the RAIL's sentence. A row is written with a warrant
        // that resolves or it is not written at all.
        other => Err(format!(
            "{other}\n\
             Write the introduction first: svrn ring introduce {person} --key {key} \
             --reason <why> --ring {namespace}"
        )),
    }
}

/// `svrn ring roster show|list <ns>` — who is in this ring, and why.
async fn roster_list(namespace: &str) -> i32 {
    let (roster, admission) = match roster_and_admission(namespace).await {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("ring roster show: {e}");
            return 1;
        }
    };
    if roster.members.is_empty() {
        println!(
            "`{namespace}` has no roster yet — every op will fold to an unknown-signer gap.\n\
             Add yourself: svrn ring roster add <you> --self --ring {namespace}"
        );
        return 0;
    }
    // A daemon older than this CLI has no `vouches` field, and serde DROPS an
    // unknown field silently — so it reads the file, re-serializes it without
    // the warrants, and every row below reads "warrant unknown" when the
    // reason is sitting on disk. That is this repo's characteristic failure:
    // a well-formed answer that is wrong. Detected against the file this
    // command's own writer maintains, and NAMED rather than papered over by
    // rendering the file's warrants against the daemon's members (ARCH §18.3).
    let on_disk = ring_journal(namespace).and_then(|j| {
        let path = j.roster_path();
        j.roster_file()
            .map(|r| (path, r))
            .map_err(|e| e.to_string())
    });
    let skew = match &on_disk {
        Ok((path, file)) if roster.vouches.is_empty() && !file.vouches.is_empty() => {
            eprintln!(
                "WARNING: {} carries warrants and the running daemon reported none, so every\n\
                 row below will read as warrant-unknown. That daemon is older than this CLI —\n\
                 restart it (svrn daemon stop && svrn daemon start) and run this again.\n",
                path.display()
            );
            true
        }
        Ok(_) => false,
        // COULD-NOT-JUDGE is not "no skew" (ARCH §18.3). The rows still
        // print — they are the daemon's answer and they are worth reading —
        // but a reader is told the cross-check did not run.
        Err(e) => {
            eprintln!(
                "NOTE: `{namespace}`'s roster file could not be read, so this command cannot\n\
                 tell whether the daemon is reporting every warrant on disk: {e}\n"
            );
            false
        }
    };
    for (person, keys) in &roster.members {
        println!("{person}");
        for k in keys {
            println!("  {k}");
            // The warrant sentence is composed by the rail, for the same
            // reason the gap sentences are: this line and the app's page must
            // say the same thing about the same row (ARCH §10.6). A row with
            // no warrant says so and is not an error — it is every row
            // written before warrants existed.
            println!(
                "    {}",
                commonwealth_rail::trace(&roster, &admission.ops, person, k)
            );
        }
    }
    // The rows printed either way — a roster is worth reading even when the
    // reasons are missing — but a caller must not read exit 0 as "these rows
    // have no warrants".
    i32::from(skew)
}

// ── introduce ────────────────────────────────────────────────

/// The act, or the sentence saying why there is not one.
///
/// Pure and separate from [`run_introduce`] so the refusals have a test: the
/// command's other half is one POST, and a test that reached it would write
/// to whichever daemon the operator happens to be running.
fn introduce_act(
    namespace: &str,
    person: &str,
    key: &str,
    reason: &str,
) -> Result<commonwealth_rail::RailAct, String> {
    if key.is_empty() {
        return Err(format!(
            "--key <hex> is required — an introduction is about a KEY, and a name with\n\
             no key vouches for nobody. Use what their\n\
             `svrn ring roster add <them> --self --ring {namespace}` printed."
        ));
    }
    if hex::decode(key).map(|b| b.len()) != Ok(32) {
        return Err(format!(
            "`{key}` is not a node public key — expected 64 hex characters.\n\
             Use what their `svrn ring roster add <them> --self --ring {namespace}` printed."
        ));
    }
    // A reason is required and may not be blank. An introduction whose reason
    // is empty is the PGP failure mode arriving early: a signature nobody can
    // weigh, which is worth as much as no signature at all.
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(
            "--reason <why> is required. The ring reads it to decide whether to admit\n\
             this key — \"she fixed the boiler\", \"bought the drill off me\"."
                .to_string(),
        );
    }
    let payload =
        commonwealth_rail::Introduce::new(commonwealth_rail::Person::from(person), key, reason)
            .payload()
            .map_err(|e| e.to_string())?;
    Ok(commonwealth_rail::RailAct::Record { payload })
}

/// `svrn ring introduce <person> --key <hex> --reason <why> --ring <ns>`
///
/// Writes ONE act to the journal and touches no roster. That is the whole
/// point: an introduction is evidence a person acts on, and the operator
/// running `roster add --on <op>` is the one who admits anybody. An
/// introduction arriving from a peer moves nothing at all — there is no
/// roster route (`sovereign-mesh/src/ring_roster.rs`).
async fn run_introduce(args: &[String]) -> i32 {
    let Some(person) = positional(args) else {
        eprintln!(
            "ring introduce: who? `svrn ring introduce dee --key <hex> --reason <why> --ring <ns>`"
        );
        return 2;
    };
    let Some(namespace) = flag(args, "--ring") else {
        eprintln!("ring introduce: which ring? pass --ring <namespace>");
        return 2;
    };
    // The daemon's own ring derives its roster from mesh membership, so an
    // introduction there is evidence for a decision nobody makes — the same
    // reason `roster add` refuses it.
    if let Some(why) = refuse_derived_roster(namespace) {
        eprintln!("ring introduce: {why}");
        return 2;
    }
    let key = flag(args, "--key").unwrap_or_default();
    let reason = flag(args, "--reason").unwrap_or_default();
    let act = match introduce_act(namespace, person, key, reason) {
        Ok(act) => act,
        Err(refusal) => {
            eprintln!("ring introduce: {refusal}");
            return 2;
        }
    };
    let v = match rail_append(namespace, &act).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ring introduce: {e}");
            return 1;
        }
    };
    let Some(id) = v.get("id").and_then(|i| i.as_str()) else {
        eprintln!(
            "ring introduce: the daemon's answer carried no `id` — this build and that daemon \n\
             do not agree on the shape of `{RAIL_APPEND_PATH}`"
        );
        return 1;
    };
    println!("{person} ← {key}");
    println!("  ring:   {namespace}");
    println!("  op:     {id}");
    println!("  reason: {reason}");
    println!();
    println!("  Nobody is in the ring yet — an introduction is evidence, not admission.");
    println!("  Admit them on it: svrn ring roster add {person} --key {key} --on {id} --ring {namespace}");
    0
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
    let corrects = op.get("corrects").and_then(|c| c.as_str());
    // `null` and absent are the same fact here and the wire has used both.
    let payload = op.get("payload").filter(|p| !p.is_null());
    let mut what = match (payload, corrects) {
        (Some(p), _) => p.to_string(),
        // A correction that states no replacement is not an empty act, it is
        // a withdrawal — saying "voids X" and nothing else is the truth.
        (None, Some(_)) => "(no replacement)".to_string(),
        // A seal carries nothing BY DESIGN — it is delivery, not meaning, so
        // `AdmittedOp::applies` is false and no reducer sees it. It reached
        // the terminal wearing the correction's fallback and read as
        // "(no replacement)", which says a withdrawal happened: the opposite
        // of what a seal does, on the one act that DELETES things.
        //
        // The two absences identify it without a second field on the wire: a
        // `Record` always carries a payload and a `Correct` always names what
        // it corrects, so nothing else can present as neither.
        (None, None) => "(sealed — everything this key wrote before is retired)".to_string(),
    };
    if let Some(target) = corrects {
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

/// The two renderings a person reads — **the rail's**, not this module's.
///
/// Both were defined here until the roster warrant needed the same two. A
/// warrant sentence is composed in `commonwealth-rail-core` (the gap
/// sentences already are, for the same reason), and a date spelled one way in
/// that sentence and another way in the log line printed above it would be
/// two answers to one question (ARCH §10.6).
pub(crate) use commonwealth_rail::{short_id, short_stamp};

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "4242424242424242424242424242424242424242424242424242424242424242";

    /// **An introduction the ring cannot weigh is refused at the terminal.**
    /// Each of these is an input that makes this check fail, which is what
    /// makes it a check (ARCH §18.1).
    #[test]
    fn an_introduction_without_a_key_or_a_reason_is_refused() {
        for (key, reason, expect) in [
            ("", "she fixed the boiler", "--key <hex> is required"),
            ("nothex", "she fixed the boiler", "not a node public key"),
            (&KEY[..60], "she fixed the boiler", "not a node public key"),
            (KEY, "", "--reason <why> is required"),
            // Blank is not a reason. An empty attestation is the PGP failure
            // mode: a signature nobody can weigh.
            (KEY, "   ", "--reason <why> is required"),
        ] {
            let Err(refusal) = introduce_act("house", "dee", key, reason) else {
                panic!("key {key:?} with reason {reason:?} was accepted");
            };
            assert!(
                refusal.contains(expect),
                "{key:?}/{reason:?} refused with the wrong sentence: {refusal}"
            );
        }
    }

    /// And the act it does build is a `Record` carrying the introduction —
    /// never a `RailAct` variant of its own, because a variant is a branch
    /// inside `admit` and the rail does not know what an act means.
    #[test]
    fn an_introduction_is_an_ordinary_record_the_rail_does_not_interpret() {
        let act = introduce_act("house", "dee", KEY, "  she fixed the boiler  ").unwrap();
        let commonwealth_rail::RailAct::Record { payload } = &act else {
            panic!("an introduction must ride as a Record: {act:?}");
        };
        assert_eq!(
            commonwealth_rail::Introduce::from_payload(payload),
            Some(commonwealth_rail::Introduce::new(
                commonwealth_rail::Person::from("dee"),
                KEY,
                "she fixed the boiler"
            ))
        );
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

        // A seal carries neither, and it is not a withdrawal of anything. It
        // rendered as "(no replacement)" until 4a made seals reachable from
        // the terminal, which said the opposite of the truth on the one act
        // that deletes.
        let seal = serde_json::json!({
            "person": "alex", "ts_unix": 1_756_512_000, "voided": false,
        });
        let line = op_line(&seal);
        assert!(line.contains("sealed"), "{line}");
        assert!(
            !line.contains("no replacement"),
            "a seal is not a withdrawal: {line}"
        );
        // Both spellings of "no payload" reach the same reading.
        let explicit_null = serde_json::json!({
            "person": "alex", "ts_unix": 1_756_512_000,
            "payload": serde_json::Value::Null, "voided": false,
        });
        assert_eq!(op_line(&explicit_null), line);
    }

    /// The one namespace whose roster is derived is refused by the roster
    /// commands — the only ones that touch `roster.json` — and every other
    /// namespace is untouched. The refusal names where to look instead — a
    /// "no" with no next step is how an operator ends up hand-writing the
    /// file anyway — and what it names is `ring log`, which reads through the
    /// daemon and works on this namespace.
    #[test]
    fn the_daemons_own_ring_refuses_a_hand_written_roster() {
        let why = refuse_derived_roster("mesh-measurements")
            .expect("the namespace whose roster is the mesh's membership");
        assert!(why.contains("svrn mesh status"), "{why}");
        assert!(why.contains("svrn ring log mesh-measurements"), "{why}");
        assert!(refuse_derived_roster("house-expenses").is_none());
        assert!(refuse_derived_roster("mesh-measurement").is_none());
        assert_eq!(
            refuse_derived_roster(sovereign_core::mesh_measurements::MEASUREMENTS_APP_ID).is_some(),
            true,
            "the guard reads the ONE constant, not a second spelling of it"
        );
    }
}

// ── seal ─────────────────────────────────────────────────────

/// `svrn ring seal <ns>` — retire everything this node has written, and delete
/// it from the journal.
///
/// # Why the terminal has this and an app does not need it
///
/// Sealing is an ordinary act: it takes the next `seq`, it is signed by the
/// node key, and it travels the same total order as every record. So an app
/// posts `{"op":"seal"}` to the append door and needs nothing from here. What
/// the terminal adds is the *operator's* reason to do it — a journal is bounded
/// only by somebody deciding it has kept enough, and there is no cadence the
/// rail could pick for you.
///
/// **That is deliberate, and it is the same refusal `sync.rs` makes about a
/// truncation setting.** A local rule for when to seal would put a
/// disagreement one layer up: two nodes on different cadences, each re-sending
/// what the other has retired. A seal is a signed act in one order, so peers
/// agree about it by the mechanism they already have.
///
/// # Who seals is not a policy question
///
/// It looks like one, and it is not. [`RailAct::Seal`](commonwealth_rail::RailAct::Seal)
/// carries no actor, so sealing somebody else's history is unwritable — every
/// alternative to self-sealing is already ruled out by the type. The worry that
/// remains is that a node which goes quiet never seals and its history grows
/// without bound, and it does not survive contact: an actor's history grows
/// only when that actor WRITES, so a node that is offline contributes nothing
/// further, and a node that is writing is by definition able to seal. Growth is
/// bounded per actor by what that actor writes between its own seals, and
/// nobody needs to seal on anyone else's behalf.
///
/// This goes over HTTP rather than opening the journal directly, and the reason
/// is `seq`. The daemon serialises appends behind one writer lock per
/// namespace; a second process picking the next sequence number from its own
/// read would race it and both would land on the same `seq` — a fork, reported
/// by every node forever. `roster add` writes a different file and has no such
/// hazard, which is why it does open the directory.
async fn run_seal(args: &[String]) -> i32 {
    let Some(namespace) = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
    else {
        eprintln!("ring seal: which ring? `svrn ring seal <namespace>`");
        return 2;
    };
    let v = match rail_append(namespace, &commonwealth_rail::RailAct::Seal).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ring seal: {e}");
            return 1;
        }
    };

    let seq = v.get("seq").and_then(|s| s.as_u64()).unwrap_or(0);
    println!("{namespace} — sealed at seq {seq}.");
    // The seal landed either way; what follows is whether the deletion it
    // authorises actually happened. Printing only the seal would leave an
    // operator believing the journal shrank when it may not have.
    match v.get("retired") {
        Some(r) if r.get("refused").is_some() => {
            let why = r
                .get("refused")
                .and_then(|w| w.as_str())
                .unwrap_or("no reason given");
            println!();
            println!("  The seal is written. Nothing was deleted:");
            println!("    {why}");
            return 1;
        }
        Some(r) => {
            let removed = r.get("removed").and_then(|n| n.as_u64()).unwrap_or(0);
            let kept = r.get("kept").and_then(|n| n.as_u64()).unwrap_or(0);
            let cleared = r.get("gaps_cleared").and_then(|n| n.as_u64()).unwrap_or(0);
            println!("  retired {removed} line(s) from disk; {kept} remain.");
            if cleared > 0 {
                println!(
                    "  {cleared} gap(s) went with them — lines the rail had refused, \
                     below a floor their author has now retired."
                );
            }
            if removed == 0 {
                println!("  Nothing was below the floor yet. Peers keep what they hold.");
            }
        }
        // An older daemon: the seal is real, the prune is not something this
        // build of the daemon does. Say so rather than print a silent success.
        None => println!("  This daemon did not report a prune — it predates one."),
    }
    println!();
    println!("  Peers delete their copy when this seal reaches them, not before.");
    0
}
