// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh media origin` — declare where this machine's media server answers.
//!
//! The viewer half (`svrn mesh media <peer>`) dials a member's media origin,
//! which that member's cw-rails serves from `[media] origin` in its
//! rails.toml. This verb is that edit with two guarantees: the document is
//! edited as TOML (comments survive, no default is materialised), and what
//! was written is PARSED BACK by cw-rails' own reader before the command
//! reports success. `offer` writes the same key and also narrows and
//! provisions a viewer account.
//!
//! The origin may be any address this machine can dial. Unlike `svrn publish`
//! — which binds a local port and must stay loopback so the mesh is the only
//! way in — this is a DIAL TARGET the owner already runs: Jellyfin in a
//! container beside the daemon (`127.0.0.1:8096`), or the same server on
//! another box of theirs. The mesh is still the only way in from outside; the
//! address is the owner's to choose.

use std::net::SocketAddr;

use toml_edit::{DocumentMut, Item, Value};

use super::rails_media::{self, load, write};

pub(crate) async fn cmd_media_origin(args: &[String]) -> i32 {
    if sovereign_cli_base::help::wants_help(args) {
        help();
        return 0;
    }
    if args.is_empty() {
        return show();
    }
    if args.iter().any(|a| a == "--clear") {
        return clear().await;
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
    let (path, mut doc) = match load() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("media origin: {e}");
            return 1;
        }
    };
    let prev = match set_origin(&mut doc, &addr.to_string()) {
        Ok(prev) => prev,
        Err(e) => {
            eprintln!("media origin: {e}");
            return 1;
        }
    };
    match write(&path, &doc) {
        Ok(()) => {
            match prev {
                Some(old) if old == addr.to_string() => {
                    println!("Media origin was already {addr}.")
                }
                Some(old) => println!("Media origin: {old} → {addr}"),
                None => println!("Media origin declared: {addr}"),
            }
            println!("  ({})", path.display());
            println!("Housemates reach it with:  svrn mesh media <your-name>");
            rails_media::reload("origin").await
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

fn set_origin(doc: &mut DocumentMut, addr: &str) -> Result<Option<String>, String> {
    let media = rails_media::media_table(doc)?;
    let prev = media
        .get("origin")
        .and_then(Item::as_str)
        .map(str::to_string);
    media.insert("origin", Value::from(addr).into());
    Ok(prev)
}

fn clear_origin(doc: &mut DocumentMut) -> bool {
    doc.get_mut("media")
        .and_then(Item::as_table_mut)
        .and_then(|t| t.remove("origin"))
        .is_some()
}

fn show() -> i32 {
    let stored = load().and_then(|(_, doc)| super::offer::stored_origin(&doc));
    match stored {
        Err(e) => {
            eprintln!("media origin: {e}");
            1
        }
        Ok(Some(o)) => {
            println!("Media origin: {o}");
            println!("  Housemates reach it with:  svrn mesh media <your-name>");
            0
        }
        Ok(None) => {
            println!("This node serves no media origin.");
            println!();
            println!("  svrn mesh media origin 8096        # Jellyfin on this machine");
            println!("  svrn mesh media origin <addr:port> # somewhere else of yours");
            0
        }
    }
}

async fn clear() -> i32 {
    let (path, mut doc) = match load() {
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
    match write(&path, &doc) {
        Ok(()) => {
            println!("Media origin cleared.");
            println!("  ({})", path.display());
            rails_media::reload("origin").await
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
    println!("<header>` stores your key locally and cw-rails adds it on the way in.");
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINAL: &str = "\
# narrowed during the knockout, restore later
name = \"holder\"
";

    #[test]
    fn declaring_preserves_comments_and_adds_one_line() {
        let mut doc = ORIGINAL.parse::<DocumentMut>().unwrap();
        set_origin(&mut doc, "127.0.0.1:8096").unwrap();
        let out = doc.to_string();
        assert!(out.contains("narrowed during the knockout"), "{out}");
        assert!(out.contains("name = \"holder\""), "{out}");
        let read: commonwealth_rails::config::Config = toml::from_str(&out).unwrap();
        assert_eq!(read.media.origin, Some("127.0.0.1:8096".parse().unwrap()));
        let code = |t: &str| -> Vec<String> {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string)
                .collect()
        };
        assert_eq!(
            code(&out).len(),
            code(ORIGINAL).len() + 2,
            "[media] and origin: {out}"
        );
    }

    #[test]
    fn declaring_creates_the_section_when_absent_and_never_duplicates_it() {
        let mut bare = "[relay]\nurls = []\n".parse::<DocumentMut>().unwrap();
        set_origin(&mut bare, "127.0.0.1:8096").unwrap();
        assert_eq!(bare.to_string().matches("[media]").count(), 1);
        set_origin(&mut bare, "127.0.0.1:8097").unwrap();
        let out = bare.to_string();
        assert_eq!(out.matches("[media]").count(), 1, "{out}");
        assert!(out.contains("origin = \"127.0.0.1:8097\""), "{out}");
    }

    #[test]
    fn clearing_reports_whether_anything_was_declared() {
        let mut doc = ORIGINAL.parse::<DocumentMut>().unwrap();
        assert!(!clear_origin(&mut doc));
        set_origin(&mut doc, "127.0.0.1:8096").unwrap();
        assert!(clear_origin(&mut doc));
        assert!(!doc.to_string().contains("origin ="));
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
