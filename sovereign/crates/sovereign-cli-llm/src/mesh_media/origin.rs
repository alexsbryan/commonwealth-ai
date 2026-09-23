// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh media origin` — declare where this machine's media server answers.
//!
//! The viewer half (`svrn mesh media <peer>`) already dials a member's
//! `[iroh] media_origin`. Until this verb the declaring half was a hand edit:
//! find `[iroh]`, add the key, do not duplicate the header, save, restart —
//! and `publish_cmd`'s header records what the obvious `printf` does to a
//! config that already has an `[iroh]` section. This is the same edit with the
//! same two guarantees: the document is edited as TOML (comments survive, no
//! default is materialised), and what was written is PARSED BACK before the
//! command reports success.
//!
//! The origin may be any address this machine can dial. Unlike `svrn publish`
//! — which binds a local port and must stay loopback so the mesh is the only
//! way in — this is a DIAL TARGET the owner already runs: Jellyfin in a
//! container beside the daemon (`127.0.0.1:8096`), or the same server on
//! another box of theirs. The mesh is still the only way in from outside; the
//! address is the owner's to choose.

use std::net::SocketAddr;

use sovereign_contracts::setup_config::SetupConfig;
use toml_edit::{DocumentMut, Item, Value};

use crate::publish_cmd::{load_doc, write_doc};

pub(crate) fn cmd_media_origin(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        help();
        return 0;
    }
    if args.is_empty() {
        return show();
    }
    if args.iter().any(|a| a == "--clear") {
        return clear();
    }
    let Some(raw) = args.iter().find(|a| !a.starts_with('-')) else {
        help();
        return 2;
    };
    let addr = match resolve_origin(raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("media origin: {e}");
            return 2;
        }
    };
    let (path, mut doc) = match load_doc() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("media origin: {e}");
            return 1;
        }
    };
    let prev = set_origin(&mut doc, &addr.to_string());
    match write_doc(&path, &doc) {
        Ok(()) => {
            match prev {
                Some(old) if old == addr.to_string() => {
                    println!("Media origin was already {addr}.")
                }
                Some(old) => println!("Media origin: {old} → {addr}"),
                None => println!("Media origin declared: {addr}"),
            }
            println!("  ({})", path.display());
            println!("  Restart to serve it:  svrn daemon restart");
            println!();
            println!("Housemates reach it with:  svrn mesh media <your-name>");
            0
        }
        Err(e) => {
            eprintln!("media origin: could not write the config — {e}");
            1
        }
    }
}

/// A bare port means loopback, like `svrn publish` — `8096` is Jellyfin's
/// default and wants no more typing than that. Any other address is taken as
/// given and must parse as a socket address.
fn resolve_origin(raw: &str) -> Result<SocketAddr, String> {
    if let Ok(p) = raw.parse::<u16>() {
        if p == 0 {
            return Err("port 0 is not a port a server listens on".to_string());
        }
        return Ok(([127, 0, 0, 1], p).into());
    }
    raw.parse::<SocketAddr>()
        .map_err(|_| format!("{raw:?} is neither a port nor a host:port"))
}

fn set_origin(doc: &mut DocumentMut, addr: &str) -> Option<String> {
    let iroh = doc
        .entry("iroh")
        .or_insert_with(|| Item::Table(toml_edit::Table::new()))
        .as_table_mut()?;
    let prev = iroh
        .get("media_origin")
        .and_then(Item::as_str)
        .map(str::to_string);
    iroh.insert("media_origin", Value::from(addr).into());
    prev
}

fn clear_origin(doc: &mut DocumentMut) -> bool {
    doc.get_mut("iroh")
        .and_then(Item::as_table_mut)
        .and_then(|t| t.remove("media_origin"))
        .is_some()
}

fn show() -> i32 {
    let cfg = match SetupConfig::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("media origin: could not read this node's config — {e}");
            return 1;
        }
    };
    match &cfg.iroh.media_origin {
        Some(o) => {
            println!("Media origin: {o}");
            println!("  Housemates reach it with:  svrn mesh media <your-name>");
            0
        }
        None => {
            println!("This node serves no media origin.");
            println!();
            println!("  svrn mesh media origin 8096        # Jellyfin on this machine");
            println!("  svrn mesh media origin <addr:port> # somewhere else of yours");
            0
        }
    }
}

fn clear() -> i32 {
    let (path, mut doc) = match load_doc() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("media origin: {e}");
            return 1;
        }
    };
    if !clear_origin(&mut doc) {
        eprintln!("media origin: nothing was declared — there is nothing to clear.");
        return 1;
    }
    match write_doc(&path, &doc) {
        Ok(()) => {
            println!("Media origin cleared.");
            println!("  ({})", path.display());
            println!("  Restart to withdraw it:  svrn daemon restart");
            0
        }
        Err(e) => {
            eprintln!("media origin: could not write the config — {e}");
            1
        }
    }
}

fn help() {
    println!("Usage: svrn mesh media origin [<addr:port>]");
    println!("       svrn mesh media origin --clear");
    println!();
    println!("Declare where THIS machine's media server answers, so a member holding the");
    println!("mesh key reaches it with `svrn mesh media <you>` — no VPN, no port forwarded.");
    println!();
    println!("  svrn mesh media origin            what this node declares");
    println!("  svrn mesh media origin 8096       Jellyfin on this machine (loopback)");
    println!("  svrn mesh media origin --clear    stop offering it");
    println!();
    println!("If your server authenticates its own clients, `svrn mesh media declare");
    println!("<header>` stores your key locally and your daemon adds it on the way in.");
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "\
# narrowed during the knockout, restore later
[iroh]
enabled = true
";

    #[test]
    fn declaring_preserves_comments_and_adds_one_line() {
        let mut doc = ORIGINAL.parse::<DocumentMut>().unwrap();
        set_origin(&mut doc, "127.0.0.1:8096");
        let out = doc.to_string();
        assert!(out.contains("narrowed during the knockout"), "{out}");
        assert!(out.contains("enabled = true"), "{out}");
        assert!(out.contains("media_origin = \"127.0.0.1:8096\""), "{out}");
        let code = |t: &str| -> Vec<String> {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string)
                .collect()
        };
        assert_eq!(code(&out).len(), code(ORIGINAL).len() + 1, "{out}");
    }

    #[test]
    fn declaring_creates_the_section_when_absent_and_never_duplicates_it() {
        let mut bare = "[models]\nprimary = \"/a\"\n"
            .parse::<DocumentMut>()
            .unwrap();
        set_origin(&mut bare, "127.0.0.1:8096");
        assert_eq!(bare.to_string().matches("[iroh]").count(), 1);
        set_origin(&mut bare, "127.0.0.1:8097");
        let out = bare.to_string();
        assert_eq!(out.matches("[iroh]").count(), 1, "{out}");
        assert!(out.contains("media_origin = \"127.0.0.1:8097\""), "{out}");
    }

    #[test]
    fn clearing_reports_whether_anything_was_declared() {
        let mut doc = ORIGINAL.parse::<DocumentMut>().unwrap();
        assert!(!clear_origin(&mut doc));
        set_origin(&mut doc, "127.0.0.1:8096");
        assert!(clear_origin(&mut doc));
        assert!(!doc.to_string().contains("media_origin"));
    }

    /// A bare port is loopback, and nonsense is refused by name — the same
    /// failing inputs `resolve_target` names for `svrn publish`.
    #[test]
    fn a_bare_port_is_loopback_and_nonsense_is_refused() {
        assert_eq!(
            resolve_origin("8096").unwrap(),
            "127.0.0.1:8096".parse().unwrap()
        );
        for bad in ["0", "not-a-port", "127.0.0.1"] {
            assert!(!resolve_origin(bad).unwrap_err().is_empty(), "{bad}");
        }
    }
}
