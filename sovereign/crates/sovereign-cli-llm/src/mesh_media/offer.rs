// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn mesh media offer | withdraw | admit` — this node's origin, and who
//! it is offered to.
//!
//! Its own file because together with the probe/list half it put
//! `mesh_media.rs` into the 800-1200 approach band (ARCH §3.1). A sibling of
//! `declare` and `viewer`, which is how this module was already organised.

use super::*;

/// Set `[iroh] media_origin` and `[iroh] media_allow` in the config document.
/// No `--admit` REMOVES `media_allow`, so re-offering without names widens back
/// to every member rather than keeping a narrowing nobody typed this time.
pub(super) fn set_offer(
    doc: &mut toml_edit::DocumentMut,
    origin: std::net::SocketAddr,
    admit: &[String],
    viewer_user: Option<&str>,
) -> Result<(), String> {
    let iroh = doc
        .entry("iroh")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .ok_or("`iroh` in config.toml is not a table")?;
    iroh.insert("media_origin", toml_edit::value(origin.to_string()));
    if admit.is_empty() {
        iroh.remove("media_allow");
    } else {
        let names: toml_edit::Array = admit.iter().map(String::as_str).collect();
        iroh.insert("media_allow", toml_edit::value(names));
    }
    // `None` LEAVES the recorded viewer alone rather than clearing it: only
    // `offer` mints a viewer, and `admit` — which shares this writer — must
    // not silently un-record the account the presence poll reads.
    if let Some(id) = viewer_user {
        iroh.insert("media_viewer_user", toml_edit::value(id));
    }
    Ok(())
}

/// Remove every key `offer` wrote. The inverse, and the whole of it: origin,
/// admit list and viewer account go together, because each one describes an
/// offer that no longer exists.
pub(super) fn clear_offer(doc: &mut toml_edit::DocumentMut) -> bool {
    let Some(iroh) = doc.get_mut("iroh").and_then(|i| i.as_table_mut()) else {
        return false;
    };
    let had = iroh.contains_key("media_origin");
    iroh.remove("media_origin");
    iroh.remove("media_allow");
    iroh.remove("media_viewer_user");
    had
}

/// Where a media server listens when nobody said: Jellyfin's HTTP port, then
/// its HTTPS port, on this machine's loopback. The address is the tool's to
/// know, not the person's to type (ring-room census, 99ca7e4cb).
pub(super) const WELL_KNOWN_ORIGINS: [&str; 2] = ["127.0.0.1:8096", "127.0.0.1:8920"];

/// The first candidate something accepts a TCP connection on.
pub(super) fn probe_origin(candidates: &[&str]) -> Option<std::net::SocketAddr> {
    candidates
        .iter()
        .filter_map(|c| c.parse::<std::net::SocketAddr>().ok())
        .find(|addr| {
            let up = std::net::TcpStream::connect_timeout(addr, Duration::from_millis(500)).is_ok();
            tracing::debug!(%addr, up, "media offer: probed a well-known origin");
            up
        })
}

/// The origin a previous `offer` stored in `[iroh] media_origin`.
/// `Ok(None)` when nothing was offered; a value that does not parse is an
/// error, not "nothing offered".
pub(super) fn stored_origin(
    doc: &toml_edit::DocumentMut,
) -> Result<Option<std::net::SocketAddr>, String> {
    let Some(item) = doc.get("iroh").and_then(|i| i.get("media_origin")) else {
        return Ok(None);
    };
    let text = item
        .as_str()
        .ok_or_else(|| format!("[iroh] media_origin is not a string: {item}"))?;
    text.parse()
        .map(Some)
        .map_err(|e| format!("[iroh] media_origin = {text:?} does not parse: {e}"))
}

/// `svrn mesh media offer [<origin>] [--admit <member>...]` — offer this
/// node's media origin to the mesh, then `svrn daemon reload` so the running
/// acceptor serves it. With no origin it probes [`WELL_KNOWN_ORIGINS`] and
/// says what it found. Both keys reload live (`MediaRoute`), so no restart:
/// a restart left peers dialing the holder's endpoint for 120 s (ring-room
/// 99ca7e4cb leg 3).
pub(super) async fn cmd_media_offer(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn mesh media offer [<origin>] [--admit <member>...]");
        eprintln!();
        eprintln!("Offer a media server on this machine to the members of this mesh.");
        eprintln!("<origin> is a port (8096) or a loopback host:port; with none, the");
        eprintln!("well-known local media ports (Jellyfin's 8096, then 8920) are tried.");
        eprintln!("With no --admit every member may reach it; --admit names the only");
        eprintln!("members who may, by member name or a node-id prefix, as `svrn mesh");
        eprintln!("status` shows them (`svrn mesh media admit` narrows a standing offer).");
        eprintln!("The running daemon is reloaded to publish the offer.");
        return 0;
    }
    let mut origin: Option<&str> = None;
    let mut admit: Vec<String> = Vec::new();
    let mut in_admit = false;
    for a in args {
        match a.as_str() {
            "--admit" => in_admit = true,
            other if other.starts_with("--") => {
                eprintln!("mesh media offer: unknown flag {other}");
                return 1;
            }
            other if in_admit => admit.push(other.to_string()),
            other if origin.is_none() => origin = Some(other),
            other => {
                eprintln!("mesh media offer: one origin only (got {other:?} as well)");
                return 1;
            }
        }
    }
    if in_admit && admit.is_empty() {
        eprintln!("mesh media offer: --admit names at least one member");
        return 1;
    }
    let origin = match origin {
        Some(origin) => match crate::publish_cmd::resolve_target(origin) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("mesh media offer: {e}");
                return 1;
            }
        },
        None => match probe_origin(&WELL_KNOWN_ORIGINS) {
            Some(found) => {
                tracing::info!(%found, "media offer: no origin given, found a listener");
                println!("No origin given; found a media server listening on {found}.");
                found
            }
            None => {
                tracing::warn!(tried = ?WELL_KNOWN_ORIGINS, "media offer: no origin given, none found");
                eprintln!(
                    "mesh media offer: no origin given, and nothing listens on {}. \
                     Name it: `svrn mesh media offer <port>`.",
                    WELL_KNOWN_ORIGINS.join(" or ")
                );
                return 1;
            }
        },
    };
    let (path, doc) = match crate::publish_cmd::load_doc() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("mesh media offer: {e}");
            return 1;
        }
    };
    // The read-only account viewers reach this library as, minted with the
    // credential the install stage declared and REPLACING it — the admin-
    // equivalent key that was declared until now is the thing this closes
    // (see `viewer`). A failure here is named and does NOT stop the offer: a
    // library served by the old credential is what the holder already had,
    // and refusing to offer at all would be a worse answer than a loud one.
    let root = sovereign_contracts::rebrand::svrnmesh_root();
    let dir = commonwealth_media::dir_under(&root);
    let house_dir = commonwealth_media::house_dir_under(&root);
    let before = commonwealth_media::read_declared_in(&dir);
    let viewer = match viewer::provision(origin, &before).await {
        Ok(v) => {
            // The credential being replaced is the HOUSE's — the one account
            // on this origin that can see the holder's own sessions. Kept
            // here, on this machine only, so the presence poll still has an
            // asker after the declaration becomes the viewer's read-only
            // token. Skipped when there is nothing to keep: on a second offer
            // `provision` hands back the token already declared, and storing
            // that as the house credential would lose the elevated one.
            if let Some((_, install)) = before
                .iter()
                .find(|(n, val)| n == "authorization" && *val != v.credential)
            {
                if let Err(e) =
                    commonwealth_media::write_declared_in(&house_dir, "authorization", install)
                {
                    eprintln!(
                        "mesh media offer: the house credential could not be kept — {e}; this \
                         node will publish no \"in use\" signal"
                    );
                }
            }
            if let Err(e) =
                commonwealth_media::write_declared_in(&dir, "authorization", &v.credential)
            {
                eprintln!(
                    "mesh media offer: the viewer was created but could not be declared — {e}"
                );
                None
            } else {
                println!("Viewers reach this library as a read-only account (policy read back).");
                Some(v)
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "media offer: no read-only viewer account");
            eprintln!("mesh media offer: no read-only viewer account — {e}");
            eprintln!("  The offer stands with whatever credential is declared, and this node");
            eprintln!("  will publish no \"in use\" signal until a viewer account exists.");
            None
        }
    };
    write_offer(
        path,
        doc,
        origin,
        &admit,
        viewer.as_ref().map(|v| v.id.as_str()),
    )
}

/// `svrn mesh media withdraw` — stop offering this machine's library. The
/// inverse of `offer` and the whole of it: origin, admit list and viewer
/// account are removed together and the daemon reloaded, so the offer is gone
/// from every member's rail within one gossip round rather than at the next
/// restart.
pub(super) fn cmd_media_withdraw(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) {
        eprintln!("Usage: svrn mesh media withdraw");
        eprintln!();
        eprintln!("Stop offering this machine's media server to the mesh. Removes the");
        eprintln!("origin, the admit list and the read-only viewer account this node");
        eprintln!("recorded, then reloads the daemon so members see it go within one");
        eprintln!("gossip round. The library, the server and the declared credential are");
        eprintln!("untouched — `svrn mesh media offer` puts it back.");
        return 0;
    }
    if let Some(extra) = args.iter().next() {
        eprintln!("mesh media withdraw: takes no arguments (got {extra:?})");
        return 1;
    }
    let (path, mut doc) = match crate::publish_cmd::load_doc() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("mesh media withdraw: {e}");
            return 1;
        }
    };
    let had = clear_offer(&mut doc);
    if let Err(e) = crate::publish_cmd::write_doc(&path, &doc) {
        eprintln!("mesh media withdraw: could not write the config — {e}");
        return 1;
    }
    tracing::info!(config = %path.display(), had, "media offer withdrawn");
    if had {
        println!("Withdrawn. ({})", path.display());
    } else {
        // Not an error — the end state is the one asked for — but never
        // reported as if a live offer had just been taken down.
        println!(
            "This machine was not offering media; nothing to withdraw. ({})",
            path.display()
        );
    }
    println!("reloading the daemon to publish the withdrawal…");
    reload_daemon()
}

/// `svrn mesh media admit <member>...` — narrow a standing offer to the named
/// members, reading the origin `offer` stored rather than asking for it again.
pub(super) fn cmd_media_admit(args: &[String]) -> i32 {
    if sovereign_cli_shared::help::wants_help(args) || args.is_empty() {
        eprintln!("Usage: svrn mesh media admit <member>...");
        eprintln!();
        eprintln!("Narrow this machine's media offer to the named members (member name or");
        eprintln!("node-id prefix, as `svrn mesh status` shows them). The origin is the one");
        eprintln!("`svrn mesh media offer` stored; `offer` with no --admit widens back.");
        return if args.is_empty() { 1 } else { 0 };
    }
    if let Some(flag) = args.iter().find(|a| a.starts_with("--")) {
        eprintln!("mesh media admit: unknown flag {flag}");
        return 1;
    }
    let (path, doc) = match crate::publish_cmd::load_doc() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("mesh media admit: {e}");
            return 1;
        }
    };
    let origin = match stored_origin(&doc) {
        Ok(Some(origin)) => origin,
        Ok(None) => {
            tracing::warn!(config = %path.display(), "media admit: no stored media_origin");
            eprintln!(
                "mesh media admit: this machine offers no media yet — `svrn mesh media offer` first."
            );
            return 1;
        }
        Err(e) => {
            tracing::warn!(config = %path.display(), error = %e, "media admit: stored media_origin unreadable");
            eprintln!("mesh media admit: {e} ({})", path.display());
            return 1;
        }
    };
    write_offer(path, doc, origin, args, None)
}

/// Write the offer into the config and reload the daemon — the one tail
/// `offer` and `admit` share.
fn write_offer(
    path: std::path::PathBuf,
    mut doc: toml_edit::DocumentMut,
    origin: std::net::SocketAddr,
    admit: &[String],
    viewer_user: Option<&str>,
) -> i32 {
    if let Err(e) = set_offer(&mut doc, origin, admit, viewer_user)
        .and_then(|()| crate::publish_cmd::write_doc(&path, &doc))
    {
        eprintln!("mesh media offer: could not write the config — {e}");
        return 1;
    }
    tracing::info!(%origin, admit = ?admit, config = %path.display(), "media offer written");
    println!("Offering {origin}. ({})", path.display());
    println!("  {}", offered_to_line(&admit));
    println!("reloading the daemon to publish the offer…");
    reload_daemon()
}

/// `daemon reload` through the dispatcher — this binary does not own the
/// `daemon` verb. A reload that fails is the verb's failure.
fn reload_daemon() -> i32 {
    let dispatcher = match std::env::current_exe()
        .map_err(|e| e.to_string())
        .and_then(|exe| crate::bench_cmd::ablate::dispatcher_exe(&exe))
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!("mesh media offer: {e}");
            eprintln!("The offer is written; run `svrn daemon reload`.");
            return 1;
        }
    };
    match std::process::Command::new(&dispatcher)
        .args(["daemon", "reload"])
        .status()
    {
        Ok(s) if s.success() => 0,
        other => {
            tracing::warn!(?other, "media offer: daemon reload failed");
            eprintln!("mesh media offer: the reload did not apply — `svrn daemon reload`.");
            1
        }
    }
}
