// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn ring serve` — declare one ring app to the room, and bind the door.
//!
//! `svrn ring dev` is the DEVELOPMENT half: it serves a bundle on loopback
//! and holds a grant for as long as the process runs. This is the DURABLE
//! half, the one the room reaches: it writes `[daemon] guest_bind` (the
//! room-facing address the door listens on) and `[daemon.guest_pages]` (the
//! apps the door will admit guests to), then the daemon restarts and serves.
//!
//! Before this verb the walk required hand-editing two config keys, and the
//! failure mode was the one `publish`'s header describes: a `printf` appended
//! under an existing `[daemon]` is invalid TOML, or a duplicated header, or a
//! key in the wrong table — five steps and two ways to get it wrong. The
//! grammar of the room is a verb (ARCH §10): the config is edited as a TOML
//! document, never rewritten through a serde round-trip, so a person's
//! comments survive.
//!
//! What this module is NOT: it does not decide who may be a guest (that is
//! the grant) and it does not serve the page (that is the daemon's door,
//! `sovereign_daemon::guest_door`). It declares and binds, nothing else.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sovereign_contracts::guest_pages::GuestAccess;
use sovereign_contracts::setup_config::SetupConfig;
use toml_edit::{DocumentMut, Item, Table, Value};

use crate::publish_cmd::{load_doc, write_doc};

/// `svrn ring serve <ns> --dir <bundle> --bind <addr:port> [--read]`
/// `svrn ring serve --clear <ns>`
/// `svrn ring serve` — what this door declares.
pub(super) fn run_serve(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        help();
        return 0;
    }
    if args.is_empty() {
        return list();
    }
    let clear = args.iter().any(|a| a == "--clear");
    let ns = match args.iter().find(|a| !a.starts_with('-')) {
        Some(n) => n.clone(),
        None => {
            eprintln!("Usage: svrn ring serve <namespace> --dir <bundle-dir> [--bind <addr:port>] [--read]");
            eprintln!("       svrn ring serve --clear <namespace>");
            return 2;
        }
    };
    // The same name rule the scaffold and the grant use, checked here so a
    // namespace that could never match a page is refused when typed.
    if !commonwealth_media::valid_app_name(ns.as_bytes()) {
        eprintln!("ring serve: {ns:?} is not a usable namespace — letters, digits, `_` and `-`.");
        return 2;
    }
    // A namespace the daemon writes itself is NEVER open to guests (it is one
    // of the daemon's own rings, whatever any config says). Refuse it here
    // rather than write an entry the door drops at load: the config failure
    // would read as "the room cannot reach it", not as "this was never
    // allowed" (ARCH §6). The decision lives in one place — the mesh crate's
    // roster — and this verb asks it (ARCH §10.6).
    if !clear && sovereign_mesh::ring_roster::is_daemon_owned(&ns) {
        eprintln!(
            "ring serve: `{ns}` is one of this daemon's own rings and is never open to \
             guests, whatever a page registry says."
        );
        return 2;
    }

    let (path, mut doc) = match load_doc() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ring serve: {e}");
            return 1;
        }
    };

    if clear {
        let removed = clear_guest_page(&mut doc, &ns);
        if !removed {
            eprintln!("ring serve: `{ns}` is not declared to guests here.");
            let declared = declared_names(&doc);
            if !declared.is_empty() {
                eprintln!("  declared: {}", declared.join(", "));
            }
            return 1;
        }
        return match write_doc(&path, &doc) {
            Ok(()) => {
                println!("`{ns}` is no longer served to guests.");
                println!("  ({})", path.display());
                restart_note();
                0
            }
            Err(e) => {
                eprintln!("ring serve: could not write the config — {e}");
                1
            }
        };
    }

    let dir = match flag(args, "--dir").map(PathBuf::from) {
        Some(d) => d,
        None => {
            eprintln!("ring serve: which bundle? `svrn ring serve {ns} --dir <bundle-dir>`");
            return 2;
        }
    };
    // Fail before writing: a declared bundle with no index.html is a page the
    // room opens onto a 404, and the person finds out from a phone rather than
    // from the command that promised it.
    if !dir.join("index.html").is_file() {
        eprintln!(
            "ring serve: no index.html in {} — scaffold one with `svrn ring new <dir>`, \
             or pass --dir for the right bundle.",
            dir.display()
        );
        return 1;
    }
    let access = if args.iter().any(|a| a == "--read") {
        GuestAccess::Read
    } else {
        GuestAccess::Write
    };

    let bind = flag(args, "--bind");
    let bind_prev = bind.and_then(|b| set_bind(&mut doc, b));

    let prev = set_guest_page(&mut doc, &ns, &dir, access);
    match write_doc(&path, &doc) {
        Ok(()) => {
            match (bind, bind_prev) {
                (Some(b), Some(old)) if old != b => {
                    println!("Guest door was on {old}; it is now {b}.")
                }
                (Some(b), None) => println!("Guest door bound to {b}."),
                (Some(b), Some(_)) => println!("Guest door is on {b}."),
                _ => {}
            }
            match prev {
                Some(p) if p == dir.to_string_lossy() => {
                    println!("`{ns}` was already served from {}.", dir.display());
                }
                Some(old) => println!("`{ns}` now served from {old} → {}.", dir.display()),
                None => println!("`{ns}` is now served to guests from {}.", dir.display()),
            }
            println!("  guests: {}   ({})", access.as_str(), path.display());
            if access == GuestAccess::Read {
                println!("  (read-only: a guest's append to `{ns}` is refused by name)");
            }
            restart_note();
            if !has_bind(&doc) {
                println!();
                println!("  NOTE: no `guest_bind` — the room has no address to reach the door on.");
                println!("  Pass `--bind <this machine's room address:port>`.");
            }
            println!();
            println!("Mint the room's one QR for every app so declared:");
            println!("  svrn mesh grant --wall --ttl 2h --url http://<addr:port>/ring/ --qr-svg wall-qr.svg");
            0
        }
        Err(e) => {
            eprintln!("ring serve: could not write the config — {e}");
            1
        }
    }
}

fn help() {
    eprintln!(
        "Usage: svrn ring serve <namespace> --dir <bundle-dir> [--bind <addr:port>] [--read]"
    );
    eprintln!("       svrn ring serve --clear <namespace>");
    eprintln!("       svrn ring serve");
    eprintln!();
    eprintln!("Declare a ring app to the ROOM and bind the guest door. Writes");
    eprintln!("`[daemon] guest_bind` and `[daemon.guest_pages]`, so one `svrn mesh grant");
    eprintln!("--wall` link reaches every app declared here — the owner widens the wall by");
    eprintln!("declaring an app, not by minting another link.");
    eprintln!();
    eprintln!("  --dir <bundle-dir>  the app to serve (must hold index.html)");
    eprintln!("  --bind <addr:port>  the room-facing address the door listens on");
    eprintln!("  --read              guests may read this app and not write to it");
    eprintln!("  --clear <namespace> stop serving one app to the room");
    eprintln!();
    eprintln!("`svrn ring dev` is the other half: it serves a bundle on loopback for the");
    eprintln!("wall's own screen, holding its grant only while it runs.");
    eprintln!();
    eprintln!("Restart the daemon to serve what you declared:  svrn daemon restart");
}

fn restart_note() {
    println!("  Restart to serve it:  svrn daemon restart");
}

/// The `[daemon.guest_pages]` table, created if absent — positioned at the END
/// of the document for the reason `publish_cmd` documents: `toml_edit` hangs a
/// trailing comment on whatever follows it, so inserting a sub-table in place
/// pushes the `[daemon]` section's last comment under the new header, where it
/// stops meaning what it said.
fn guest_pages_table(doc: &mut DocumentMut) -> Option<&mut Table> {
    let end = doc
        .iter()
        .filter_map(|(_, v)| v.as_table().and_then(Table::position))
        .max()
        .map(|p| p + 1)
        .unwrap_or(0);
    let daemon = doc
        .entry("daemon")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()?;
    let fresh = !daemon.contains_key("guest_pages");
    let pages = daemon
        .entry("guest_pages")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()?;
    if fresh {
        pages.set_position(end);
    }
    Some(pages)
}

/// `[daemon] guest_bind = "<addr>"`. Returns the previous value.
fn set_bind(doc: &mut DocumentMut, addr: &str) -> Option<String> {
    let daemon = doc
        .entry("daemon")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()?;
    let prev = daemon
        .get("guest_bind")
        .and_then(Item::as_str)
        .map(str::to_string);
    daemon.insert("guest_bind", Value::from(addr).into());
    prev
}

fn has_bind(doc: &DocumentMut) -> bool {
    doc.get("daemon")
        .and_then(Item::as_table)
        .and_then(|t| t.get("guest_bind"))
        .is_some()
}

/// Set `[daemon.guest_pages].<ns>`. The bare path is the one-line form for an
/// app guests read and write; the narrowed form exists only for `--read`.
/// Returns the previous bundle dir.
fn set_guest_page(
    doc: &mut DocumentMut,
    ns: &str,
    dir: &Path,
    access: GuestAccess,
) -> Option<String> {
    let pages = guest_pages_table(doc)?;
    let prev = pages.get(ns).and_then(Item::as_str).map(str::to_string);
    let item = match access {
        GuestAccess::Write => Value::from(dir.to_string_lossy().as_ref()).into(),
        GuestAccess::Read => {
            let mut t = toml_edit::InlineTable::new();
            t.insert("dir", dir.to_string_lossy().as_ref().into());
            t.insert("guests", access.as_str().into());
            let mut v = Value::InlineTable(t);
            v.decor_mut().set_prefix(" ");
            Item::Value(v)
        }
    };
    pages.insert(ns, item);
    prev
}

/// Remove one entry; drop the whole table when it empties, rather than leave a
/// bare `[daemon.guest_pages]` header that reads as "something is wrong here".
fn clear_guest_page(doc: &mut DocumentMut, ns: &str) -> bool {
    let removed = guest_pages_table(doc).and_then(|t| t.remove(ns)).is_some();
    if removed
        && guest_pages_table(doc)
            .map(|t| t.is_empty())
            .unwrap_or(false)
    {
        if let Some(daemon) = doc.get_mut("daemon").and_then(Item::as_table_mut) {
            daemon.remove("guest_pages");
        }
    }
    removed
}

fn declared_names(doc: &DocumentMut) -> Vec<String> {
    doc.get("daemon")
        .and_then(Item::as_table)
        .and_then(|t| t.get("guest_pages"))
        .and_then(Item::as_table)
        .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
        .unwrap_or_default()
}

/// What this door declares, asked of the config (the daemon is the thing that
/// serves it; `svrn ring serve` without arguments is the declaration's own
/// view, and there is no live listing to ask for beyond it).
fn list() -> i32 {
    let cfg = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ring serve: could not read this node's config — {e}");
            return 1;
        }
    };
    let bind = cfg.daemon.guest_bind.clone();
    let pages: BTreeMap<String, _> = cfg.daemon.guest_pages.clone();
    if bind.is_none() && pages.is_empty() {
        println!("This node serves no ring app to a room.");
        println!();
        println!("  svrn ring serve <ns> --dir <bundle> --bind <room address:port>");
        return 0;
    }
    match &bind {
        Some(b) => println!("Guest door: {b}"),
        None => println!("Guest door: NOT BOUND — the room has no address to reach it on"),
    }
    if !pages.is_empty() {
        println!();
        println!("Apps the room may open (one `svrn mesh grant --wall` reaches all of them):");
        for (ns, page) in &pages {
            println!(
                "  {ns:<20} {:<6} {}",
                page.guests().as_str(),
                page.dir().display()
            );
        }
    }
    println!();
    println!("Restart to serve it:  svrn daemon restart");
    0
}

/// The leading bare word after the namespace, if any — kept local rather than
/// shared because `ring_cmd`'s own helper is the namespace extractor and this
/// verb reads flags around one positional.
fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "\
# The primary was raised to 131072 by the operator on 2026-08-18.
[daemon]
# the door is off until a room exists
[models]
primary = \"/models/a.gguf\"
";

    #[test]
    fn serving_an_app_adds_one_line_and_preserves_comments() {
        let mut doc = ORIGINAL.parse::<DocumentMut>().unwrap();
        set_bind(&mut doc, "192.168.1.20:19947");
        set_guest_page(
            &mut doc,
            "house-expenses",
            Path::new("/srv/house"),
            GuestAccess::Write,
        );
        let out = doc.to_string();
        assert!(out.contains("raised to 131072 by the operator"), "{out}");
        assert!(out.contains("the door is off until a room exists"), "{out}");
        assert!(out.contains("guest_bind = \"192.168.1.20:19947\""), "{out}");
        assert!(out.contains("[daemon.guest_pages]"), "{out}");
        assert!(out.contains("house-expenses = \"/srv/house\""), "{out}");
        // A bare path means read AND write — no invented `guests` key.
        assert!(!out.contains("guests"), "{out}");
    }

    #[test]
    fn read_only_is_the_narrowed_form_and_says_so() {
        let mut doc = "[daemon]\n".parse::<DocumentMut>().unwrap();
        set_guest_page(
            &mut doc,
            "ledger",
            Path::new("/srv/ledger"),
            GuestAccess::Read,
        );
        let out = doc.to_string();
        assert!(
            out.contains("ledger = { dir = \"/srv/ledger\", guests = \"read\" }"),
            "{out}"
        );
    }

    #[test]
    fn the_table_is_created_once_and_never_duplicated() {
        let mut doc = ORIGINAL.parse::<DocumentMut>().unwrap();
        set_guest_page(&mut doc, "a", Path::new("/srv/a"), GuestAccess::Write);
        set_guest_page(&mut doc, "b", Path::new("/srv/b"), GuestAccess::Read);
        let out = doc.to_string();
        assert_eq!(out.matches("[daemon.guest_pages]").count(), 1, "{out}");
        assert_eq!(declared_names(&doc), vec!["a", "b"]);
    }

    #[test]
    fn clearing_the_last_app_takes_the_empty_table_with_it() {
        let mut doc = ORIGINAL.parse::<DocumentMut>().unwrap();
        set_guest_page(&mut doc, "a", Path::new("/srv/a"), GuestAccess::Write);
        assert!(clear_guest_page(&mut doc, "a"));
        let out = doc.to_string();
        assert!(!out.contains("guest_pages"), "{out}");
        assert!(out.contains("[daemon]"), "{out}");
    }

    #[test]
    fn clearing_a_namespace_that_was_never_declared_reports_absence() {
        let mut doc = ORIGINAL.parse::<DocumentMut>().unwrap();
        assert!(!clear_guest_page(&mut doc, "nobody"));
    }

    #[test]
    fn a_daemon_owned_namespace_is_never_servable() {
        // The one decider is the mesh crate's roster, asked here — this test
        // names the refusal, not a second copy of the list.
        assert!(sovereign_mesh::ring_roster::is_daemon_owned(
            "mesh-measurements"
        ));
    }
}
