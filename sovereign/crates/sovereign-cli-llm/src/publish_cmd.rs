// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn publish` — put a localhost port in front of the house, by name.
//!
//! The friction this removes is the whole first impression: open
//! `config.toml`, find `[iroh]`, add a line under the right table, do not
//! duplicate the header, save, restart. Five steps and two ways to get it
//! wrong — this dev host already HAS an `[iroh]` section, so the obvious
//! `printf '[iroh]\nmedia_origin=…' >> config.toml` produces invalid TOML and
//! a daemon that will not boot.
//!
//! So the config is edited as a TOML DOCUMENT (`toml_edit`): the `[iroh.apps]`
//! table is created if absent and one key is set. There is no text append and
//! no section to find, so a duplicated table is structurally absent rather
//! than warned about (ARCH §10).
//!
//! # Why not load-mutate-save through serde
//!
//! That was the first version, and it was wrong in a way only a real config
//! showed. `SetupConfig::save` is `toml::to_string_pretty` over the parsed
//! value, which rewrites the whole document. Run against this dev host's own
//! `config.toml` it destroyed ELEVEN operator comments — the model-choice
//! history, the note recording why `context_size` was raised to 131072 and by
//! whom, the warning not to run a full cargo build at that setting, the
//! commented-out `role = "consumer"` line documenting how to revert the
//! shared-model experiment — and it MATERIALISED defaults that were not in the
//! file: `fast_idle_secs = 900`, `embed_idle_secs = 900`, `local_only =
//! false`. Pinning a code-derived default into a person's config, silently, is
//! the substitution shape this workspace keeps paying for (ARCH §6): the node
//! now ignores any future change to that default and nothing said so.
//!
//! A verb whose entire pitch is "easier than editing the TOML yourself" cannot
//! be the thing that eats your comments.

use std::net::SocketAddr;
use std::path::PathBuf;

use sovereign_contracts::setup_config::SetupConfig;
use toml_edit::{DocumentMut, Item, Table};

pub async fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some(a) if sovereign_cli_shared::help::wants_help(args) => {
            let _ = a;
            help();
            0
        }
        None => list(),
        _ => publish(args),
    }
}

/// `svrn unpublish <name>` — the other half, dispatched separately so the
/// verb reads as its own action rather than as a flag on this one.
pub async fn run_unpublish(args: &[String]) -> i32 {
    if args.is_empty() || sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn unpublish <name>");
        eprintln!();
        eprintln!("Stop publishing an app to the house. `svrn publish` lists what is published.");
        return if args.is_empty() { 2 } else { 0 };
    }
    let name = &args[0];
    let (path, mut doc) = match load_doc() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("publish: {e}");
            return 1;
        }
    };
    let removed = apps_table(&mut doc).and_then(|t| t.remove(name)).is_some();
    if !removed {
        // Absence is reported, not treated as success: "already gone" and
        // "you typed the wrong name" are different facts and the second is
        // the likely one (ARCH §6).
        eprintln!("publish: nothing named {name:?} is published.");
        let published = published_names(&doc);
        if !published.is_empty() {
            eprintln!("  published: {}", published.join(", "));
        }
        return 1;
    }
    // An emptied table is REMOVED, not left as a bare `[iroh.apps]` header.
    // An empty table still deserialises fine, but it reads to the next person
    // as "apps were configured here and something is wrong", and it is the
    // kind of husk that accumulates.
    if apps_table(&mut doc).map(|t| t.is_empty()).unwrap_or(false) {
        if let Some(iroh) = doc.get_mut("iroh").and_then(Item::as_table_mut) {
            iroh.remove("apps");
        }
    }
    match write_doc(&path, &doc) {
        Ok(()) => {
            println!("Unpublished {name}. ({})", path.display());
            restart_note();
            0
        }
        Err(e) => {
            eprintln!("publish: could not write the config — {e}");
            1
        }
    }
}

/// Read `config.toml` as a format-preserving document, beside the parse that
/// VALIDATES it.
///
/// Both, not either: `toml_edit` will happily edit a document whose shape
/// `SetupConfig` would reject, and writing a key into a config that does not
/// load leaves a person with two problems. The parse is the gate; the document
/// is what gets edited.
fn load_doc() -> Result<(PathBuf, DocumentMut), String> {
    let path = SetupConfig::default_path();
    SetupConfig::load().map_err(|e| {
        format!(
            "could not read this node's config — {e}\n  Run `svrn setup` first, or \
             `svrn setup --terminal <entry>` on a node that holds no models."
        )
    })?;
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let doc = text
        .parse::<DocumentMut>()
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok((path, doc))
}

/// `[iroh.apps]`, created if absent. `None` only when `[iroh]` exists as
/// something that is not a table, which `SetupConfig::load` would already
/// have refused.
///
/// A newly-created table is positioned at the END of the document rather than
/// immediately under `[iroh]`, and that is not cosmetic. `toml_edit` attaches
/// a trailing comment to whatever follows it, so inserting a sub-table in
/// place pushes `[iroh]`'s last comment BELOW the new header — where a note
/// about `media_allow` now reads as a note about apps. The comment survived
/// either way; it stopped meaning what it said. TOML does not care where
/// `[iroh.apps]` sits, so the honest place is after everything that was
/// already written.
fn apps_table(doc: &mut DocumentMut) -> Option<&mut Table> {
    let end = doc
        .iter()
        .filter_map(|(_, v)| v.as_table().and_then(Table::position))
        .max()
        .map(|p| p + 1)
        .unwrap_or(0);
    let iroh = doc
        .entry("iroh")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()?;
    let fresh = !iroh.contains_key("apps");
    let apps = iroh
        .entry("apps")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()?;
    if fresh {
        apps.set_position(end);
    }
    Some(apps)
}

fn published_names(doc: &DocumentMut) -> Vec<String> {
    doc.get("iroh")
        .and_then(Item::as_table)
        .and_then(|t| t.get("apps"))
        .and_then(Item::as_table)
        .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
        .unwrap_or_default()
}

/// Write the edited document back, then PARSE WHAT WAS WRITTEN.
///
/// The re-parse is the gate that makes this verb safe to run on a config a
/// person hand-maintains: if the edit produced something `SetupConfig` cannot
/// load, the original is restored and the daemon still boots. A config verb
/// that can brick a node is worse than editing the file yourself, which is the
/// thing it replaces (ARCH §5 — the failing input is any edit that does not
/// round-trip).
fn write_doc(path: &std::path::Path, doc: &DocumentMut) -> Result<(), String> {
    let original = std::fs::read_to_string(path).ok();
    std::fs::write(path, doc.to_string()).map_err(|e| format!("write {}: {e}", path.display()))?;
    if let Err(e) = SetupConfig::load_from(path) {
        if let Some(original) = original {
            let _ = std::fs::write(path, original);
            return Err(format!(
                "the edit produced a config this node cannot load ({e}) — your \
                 config.toml is unchanged"
            ));
        }
        return Err(format!(
            "the edit produced a config this node cannot load ({e})"
        ));
    }
    Ok(())
}

fn help() {
    eprintln!("Usage: svrn publish [<name> <port>]");
    eprintln!();
    eprintln!("Put a localhost port in front of the house, under a name. Housemates reach");
    eprintln!("it by mesh key — no port forwarded, no VPN, no login page — and your app");
    eprintln!("is handed the caller's verified identity on every request:");
    eprintln!();
    eprintln!("    X-Mesh-Member: LittleMac");
    eprintln!("    X-Mesh-Node:   node-44ae7614");
    eprintln!("    X-Mesh-Pubkey: 8f2c…");
    eprintln!();
    eprintln!("A client cannot forge those; your daemon strips any copy it was sent and");
    eprintln!("adds the ones it verified in the handshake.");
    eprintln!();
    eprintln!("  svrn publish                 what am I publishing");
    eprintln!("  svrn publish chores 5000     publish, durably");
    eprintln!("  svrn unpublish chores        stop");
    eprintln!();
    eprintln!("A name is letters, digits, `_` and `-`. Housemates reach it at");
    eprintln!("`svrn mesh app <you> <name>`; their requests arrive at your app with the");
    eprintln!("name stripped from the path, so your app serves `/tasks`, not");
    eprintln!("`/chores/tasks`.");
    eprintln!();
    eprintln!("Who may reach your apps is `[iroh] app_allow` — empty (the default) is every");
    eprintln!("member, and it is SEPARATE from `media_allow`, so publishing a print queue");
    eprintln!("to the house does not open your film library.");
}

fn list() -> i32 {
    let cfg = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("publish: could not read this node's config — {e}");
            return 1;
        }
    };
    if cfg.iroh.apps.is_empty() && cfg.iroh.media_origin.is_none() {
        println!("This node publishes nothing.");
        println!();
        println!("  svrn publish chores 5000     put a localhost port in front of the house");
        return 0;
    }
    if let Some(origin) = &cfg.iroh.media_origin {
        println!("Media origin (reached with `svrn mesh media <you>`):");
        println!("  {origin}");
        println!();
    }
    if !cfg.iroh.apps.is_empty() {
        println!("Apps (reached with `svrn mesh app <you> <name>`):");
        for (name, target) in &cfg.iroh.apps {
            println!("  {name:<16} {target}");
        }
        println!();
    }
    let allow = |label: &str, list: &[String]| {
        if list.is_empty() {
            println!("{label}: every member");
        } else {
            println!("{label}: {}", list.join(", "));
        }
    };
    allow("Who may reach the apps ", &cfg.iroh.app_allow);
    allow("Who may reach the media", &cfg.iroh.media_allow);
    0
}

fn publish(args: &[String]) -> i32 {
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let (Some(name), Some(port)) = (positional.first(), positional.get(1)) else {
        eprintln!("Usage: svrn publish <name> <port>   (e.g. `svrn publish chores 5000`)");
        eprintln!("Run `svrn publish --help` for the whole shape.");
        return 2;
    };
    // The same rule the acceptor enforces, checked HERE too so a bad name is
    // refused when it is typed rather than silently never matching a request
    // later (`split_app_name` cannot produce a name outside this set, so an
    // entry outside it is unreachable config — the worst kind).
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        eprintln!(
            "publish: {name:?} is not a usable app name — letters, digits, `_` and `-` only."
        );
        eprintln!("  The name becomes the first path segment housemates reach it at, so it");
        eprintln!("  cannot contain `/`, spaces, or anything that would need escaping.");
        return 2;
    }
    let target = match resolve_target(port) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("publish: {e}");
            return 2;
        }
    };
    let (path, mut doc) = match load_doc() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("publish: {e}");
            return 1;
        }
    };
    // Publishing implies serving: a node with `[iroh] enabled = false` would
    // write the entry and advertise nothing, which reads as a broken daemon
    // rather than as a config that says no. Say so instead of fixing it
    // silently — flipping someone's transport off their own config is not
    // this verb's call.
    let iroh_off = doc
        .get("iroh")
        .and_then(Item::as_table)
        .and_then(|t| t.get("enabled"))
        .and_then(|i| i.as_bool())
        == Some(false);
    let Some(apps) = apps_table(&mut doc) else {
        eprintln!("publish: `[iroh]` in your config is not a table — fix it by hand first.");
        return 1;
    };
    let replacing = apps
        .get(name.as_str())
        .and_then(|i| i.as_str())
        .map(str::to_string);
    apps.insert(name, toml_edit::value(target.to_string()));
    match write_doc(&path, &doc) {
        Ok(()) => {
            match replacing {
                Some(ref old) if *old == target.to_string() => {
                    println!("{name} was already published on {target}.");
                    return 0;
                }
                Some(old) => println!("Republished {name}: {old} → {target}"),
                None => println!("Published {name} on {target}."),
            }
            println!("  ({})", path.display());
            if iroh_off {
                println!();
                println!("WARNING: `[iroh] enabled = false` on this node, so nothing is actually");
                println!("served to the mesh. Set it true and restart to publish for real.");
                return 0;
            }
            restart_note();
            println!();
            println!("Housemates reach it with:  svrn mesh app <your-name> {name}");
            0
        }
        Err(e) => {
            eprintln!("publish: could not write the config — {e}");
            1
        }
    }
}

/// `5000` → `127.0.0.1:5000`; a full `host:port` is taken as given and must be
/// loopback.
///
/// Loopback-only is not a limitation to relax later: the point of publishing
/// is that the app stays bound to this machine and is reached by mesh key. A
/// `0.0.0.0` target would already be on the LAN to anyone, with no identity
/// attached, which is the thing the mesh replaces.
fn resolve_target(port: &str) -> Result<SocketAddr, String> {
    if let Ok(p) = port.parse::<u16>() {
        if p == 0 {
            return Err("port 0 is not a port a server listens on".to_string());
        }
        return Ok(([127, 0, 0, 1], p).into());
    }
    let addr: SocketAddr = port
        .parse()
        .map_err(|_| format!("{port:?} is neither a port nor a host:port"))?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "{addr} is not loopback — publish binds a port ON THIS MACHINE to the mesh. \
             An app on a routable address is already reachable without us, and without \
             any identity attached to the caller."
        ));
    }
    Ok(addr)
}

fn restart_note() {
    println!("  Restart to serve it:  svrn daemon restart");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression this module was rewritten for, as a failing input a
    /// test can name: a real config carries comments and omits every field it
    /// is happy to take from code. Editing it must add three lines and touch
    /// nothing else.
    ///
    /// The `fast_idle_secs`-shaped assertion is the load-bearing one. A serde
    /// round-trip writes every default into the file, which silently pins a
    /// value nobody chose and detaches the node from future changes to it
    /// (ARCH §6) — and no test that only checked "the app is published" would
    /// have caught it.
    #[test]
    fn editing_a_config_preserves_its_comments_and_invents_no_defaults() {
        const ORIGINAL: &str = "\
# The primary was raised to 131072 by the operator on 2026-08-18.
# Do NOT run a full cargo build against the daemon at this setting.
[models]
primary = \"/models/a.gguf\"
context_size = 131072

[iroh]
enabled = true
# media_allow = [\"LittleMac\"]   # narrowed during the knockout, restore later
";
        let mut doc = ORIGINAL.parse::<DocumentMut>().unwrap();
        apps_table(&mut doc)
            .unwrap()
            .insert("chores", toml_edit::value("127.0.0.1:5000"));
        let out = doc.to_string();

        for comment in [
            "raised to 131072 by the operator",
            "Do NOT run a full cargo build",
            "restore later",
        ] {
            assert!(out.contains(comment), "comment lost: {comment}\n{out}");
        }
        for invented in ["fast_idle_secs", "embed_idle_secs", "local_only"] {
            assert!(
                !out.contains(invented),
                "a default nobody wrote appeared: {invented}\n{out}"
            );
        }
        assert!(out.contains("[iroh.apps]"), "{out}");
        assert!(out.contains("chores = \"127.0.0.1:5000\""), "{out}");

        // The guarantee, stated exactly: every line that was not a comment
        // and not blank survives unchanged and in order, and the only lines
        // ADDED are the table header and its key. What is NOT guaranteed is
        // where a file-trailing comment lands — `toml_edit` renders an
        // appended table before the document's trailing trivia, so the
        // `media_allow` note above ends up after `[iroh.apps]`. It is still
        // in the file and still says what it said; it is one table further
        // from the `[iroh]` it annotates. Asserting a prefix instead would
        // claim a promise the format cannot make.
        let code = |t: &str| -> Vec<String> {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string)
                .collect()
        };
        let (before, after) = (code(ORIGINAL), code(&out));
        assert_eq!(
            after.len(),
            before.len() + 2,
            "exactly two lines added\n{out}"
        );
        for line in &before {
            assert!(after.contains(line), "line lost: {line}\n{out}");
        }
    }

    /// A config with no `[iroh]` at all gets one, and a config that already
    /// has `[iroh.apps]` gains a key rather than a second table — the
    /// duplicate-header failure a `>>` append produces.
    #[test]
    fn the_apps_table_is_created_once_and_never_duplicated() {
        let mut bare = "[models]\nprimary = \"/a\"\n"
            .parse::<DocumentMut>()
            .unwrap();
        apps_table(&mut bare)
            .unwrap()
            .insert("chores", toml_edit::value("127.0.0.1:5000"));
        assert_eq!(bare.to_string().matches("[iroh.apps]").count(), 1);

        let mut existing = bare.clone();
        apps_table(&mut existing)
            .unwrap()
            .insert("printer", toml_edit::value("127.0.0.1:8080"));
        let out = existing.to_string();
        assert_eq!(out.matches("[iroh.apps]").count(), 1, "{out}");
        assert_eq!(published_names(&existing), vec!["chores", "printer"]);
    }

    /// Unpublishing the last app removes the table rather than leaving a bare
    /// `[iroh.apps]` header behind — the husk that reads as "something is
    /// misconfigured here" to the next person.
    #[test]
    fn removing_the_last_app_takes_the_empty_table_with_it() {
        let mut doc = "[iroh]\nenabled = true\n".parse::<DocumentMut>().unwrap();
        apps_table(&mut doc)
            .unwrap()
            .insert("chores", toml_edit::value("127.0.0.1:5000"));
        assert!(apps_table(&mut doc).unwrap().remove("chores").is_some());
        if apps_table(&mut doc).map(|t| t.is_empty()).unwrap_or(false) {
            doc.get_mut("iroh")
                .and_then(Item::as_table_mut)
                .unwrap()
                .remove("apps");
        }
        let out = doc.to_string();
        assert!(!out.contains("apps"), "{out}");
        assert!(out.contains("enabled = true"), "{out}");
    }

    /// A bare port is the common form and must mean loopback, never the
    /// wildcard — the difference between "the house can reach it" and "the
    /// coffee shop wifi can".
    #[test]
    fn a_bare_port_resolves_to_loopback() {
        assert_eq!(
            resolve_target("5000").unwrap(),
            "127.0.0.1:5000".parse().unwrap()
        );
    }

    /// The failing inputs. A routable target is refused rather than accepted
    /// and quietly published, because accepting it would put somebody's app
    /// on the LAN with no identity on the caller.
    #[test]
    fn a_routable_or_nonsense_target_is_refused_by_name() {
        for bad in ["0.0.0.0:5000", "192.168.1.5:80", "not-a-port", "0"] {
            let e = resolve_target(bad).unwrap_err();
            assert!(!e.is_empty(), "{bad} must be refused with a reason");
        }
        assert!(resolve_target("0.0.0.0:5000")
            .unwrap_err()
            .contains("not loopback"));
    }

    #[test]
    fn an_explicit_loopback_host_port_is_taken_as_given() {
        assert_eq!(
            resolve_target("127.0.0.1:8096").unwrap(),
            "127.0.0.1:8096".parse().unwrap()
        );
        assert!(resolve_target("[::1]:8096").is_ok());
    }
}
