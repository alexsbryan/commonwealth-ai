// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where is the daemon binary this build can use?
//!
//! One question, one answer, and the only reason this module exists: a client
//! needs a path before it can ask `sovereign_turn_client`'s `ServingHost` to
//! bring a backend up (the `bundled-backend` capability). It resolves a path
//! and nothing else — it does not start, register, supervise or stop
//! anything, which is the whole point of the campaign it serves
//! (`sv-surface`, `sv-no-daemon-management`).
//!
//! For one commit this is called by nothing; svt-1 wires it. The service
//! registration it was extracted from was CUT at svt-0: a second story about
//! who starts a daemon, resting on a Windows backend nobody has run on
//! Windows, with a branch that had the app calling `stop_service`.
//! Registration stays where it already lived, `svrn install-service`.

use std::path::PathBuf;
use std::time::Duration;

use sovereign_turn_client::ServingHost;

/// The daemon binary this build can bring up, or `None`.
///
/// The order is the order a real install produces, and each entry is a place
/// a binary genuinely lands rather than a guess:
///
/// 1. `SVRNMESH_DAEMON_BINARY` / `SOVEREIGN_DAEMON_BINARY` — the operator's
///    override, read through `rebrand::svrnmesh_env` so both spellings work.
/// 2. Beside this executable. Nothing puts one there today; a Tauri sidecar
///    (`externalBin`, currently unset — `RELEASING.md` records the config
///    split that keeps it out of dev builds) lands exactly here, so that
///    work flips this on with no change to this function.
/// 3. `~/.local/bin`, which is where `landing/install.sh` puts `svrn` by
///    default, then `/usr/local/bin`.
///
/// `svrn` first and `sovereign-cli` second at every location: `svrn` is the
/// symlink the installer creates and the name a user will have; the
/// dispatcher it points at is the fallback for an install that placed one
/// without the other.
#[allow(dead_code)] // wired by svt-1; see the module doc
pub(crate) fn daemon_binary() -> Option<PathBuf> {
    // `svrnmesh_env` is the one reader of the two prefixes (ARCH principle
    // 8); spelling `var_os` here would honour one of them by accident.
    if let Some(raw) = sovereign_contracts::rebrand::svrnmesh_env("DAEMON_BINARY") {
        let p = PathBuf::from(raw);
        if p.is_file() {
            tracing::info!(path = %p.display(), "service-adoption: using the DAEMON_BINARY override");
            return Some(p);
        }
        tracing::warn!(
            path = %p.display(),
            "service-adoption: DAEMON_BINARY is set but is not a file — ignoring it"
        );
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            dirs.push(parent.to_path_buf());
        }
    }
    // `~/.local/bin` and `/usr/local/bin` are unix conventions, and
    // `landing/install.sh` — a bash script — is what puts `svrn` in the
    // first of them. `$HOME` rather than `dirs::home_dir`, which
    // `clippy.toml` denies so that per-user SOVEREIGN paths go through
    // `sovereign_contracts::rebrand`; this is not one of those, it is the
    // installer's own directory.
    #[cfg(unix)]
    {
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join(".local").join("bin"));
        }
        dirs.push(PathBuf::from("/usr/local/bin"));
    }

    first_binary_in(&dirs)
}

/// The search itself, over directories a caller supplies — so the order can
/// be tested without moving this host's real binaries around.
fn first_binary_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        for name in ["svrn", "sovereign-cli"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &std::path::Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\n").unwrap();
        p
    }

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "svc-adopt-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn nothing_installed_anywhere_is_no_binary_not_a_guess() {
        let empty = tmp("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(first_binary_in(&[empty]), None);
    }

    #[test]
    fn the_first_directory_wins_so_a_sidecar_beats_an_installed_cli() {
        let beside = tmp("beside");
        let installed = tmp("installed");
        let sidecar = touch(&beside, "svrn");
        touch(&installed, "svrn");
        assert_eq!(
            first_binary_in(&[beside, installed]),
            Some(sidecar),
            "a binary shipped with the app must win over whatever else is on the box"
        );
    }

    #[test]
    fn svrn_wins_over_the_dispatcher_within_one_directory() {
        let dir = tmp("both");
        let svrn = touch(&dir, "svrn");
        touch(&dir, "sovereign-cli");
        assert_eq!(first_binary_in(&[dir]), Some(svrn));
    }

    #[test]
    fn the_dispatcher_is_found_when_the_symlink_is_absent() {
        let dir = tmp("dispatcher-only");
        let cli = touch(&dir, "sovereign-cli");
        assert_eq!(first_binary_in(&[dir.clone()]), Some(cli));
    }

    #[test]
    fn a_directory_is_not_a_binary() {
        let dir = tmp("dir-named-svrn");
        std::fs::create_dir_all(dir.join("svrn")).unwrap();
        assert_eq!(
            first_binary_in(&[dir]),
            None,
            "is_file() is the test — a directory called svrn resolves to a path \
             nothing can execute, and the caller finds out at spawn time"
        );
    }
}
