// SPDX-License-Identifier: AGPL-3.0-or-later
//! Hand the daemon to the operating system at the end of setup, so the node
//! is available to the mesh when this app is closed.
//!
//! # Why setup does this at all
//!
//! Operator direction, 2026-09-11 (`quality/campaigns/sv-surface.toml`,
//! "AMENDED 2026-09-11"): "even when you're not interacting with the app you
//! ought to still be available to contribute to the mesh". Availability is a
//! property of the NODE, not a convenience for the app — a daemon that exists
//! only while a window is open is not a mesh member. So the service is the
//! desktop's path too, and setup installs it by default.
//!
//! Until this module, nothing in the desktop ever called
//! `sovereign_service::install_service`: only `svrn setup`'s finish step and
//! the explicit `svrn install-service` verb did. A desktop user who never
//! touched the CLI had a daemon that died with the window, every time.
//!
//! # Why it changes no boot path
//!
//! `bootstrap::decide` already probes `:9741` FIRST and returns
//! [`crate::bootstrap::BootstrapMode::Attach`] when anything answers. So the
//! post-wizard relaunch that already exists
//! ([`crate::supervisor_setup::maybe_restart_into_supervised`]) needs no new
//! branch: with a service registered and serving, the fresh instance probes,
//! finds a daemon it does not own, and attaches. Without one it spawns the
//! supervised child exactly as before. What changes is what the fresh
//! instance FINDS, not how it decides.
//!
//! # The one thing that must not race
//!
//! If the app relaunches before the service's daemon has bound its port, the
//! fresh instance probes, sees nothing, goes Local, and binds `:9741` itself
//! — and then the service's daemon crash-loops against a port it cannot have.
//! That is the exact failure `bootstrap.rs` was written to end ("silently
//! failed and the user got a blank chat surface"). So registration is
//! followed by a readiness wait sized against the daemon's OWN budget, and
//! if it still has not answered we STOP the service rather than relaunch
//! into a port fight (ARCH principle 6 — the degradation is named and
//! logged, never silent). A stopped service starts again at next login, by
//! which time the app will attach to it.

use std::path::PathBuf;
use std::time::Duration;

use sovereign_turn_client::ServingHost;

/// How long to wait for the freshly registered daemon to answer.
///
/// The daemon's own readiness budget is 120s
/// (`sovereign-cli-daemon/src/daemon_cmd/lifecycle.rs`,
/// `parse_ready_timeout`), and matching it is deliberate: a shorter window
/// here would declare a healthy cold model load a failure and tear down a
/// service that was seconds from serving.
const READY_WITHIN: Duration = Duration::from_secs(120);

/// What setup did about the service. Every variant is a log line and a
/// value, because "we did not register one" and "we registered one that
/// never answered" call for different things next (ARCH principle 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Adoption {
    /// The OS owns a daemon and it is answering. The relaunch will attach.
    Serving { binary: PathBuf },
    /// No daemon binary is installed beside the app or in the places the
    /// installer puts one, so there is nothing to register. The app keeps
    /// the supervised child it has always had.
    NoBinary,
    /// Registration itself failed — a managed host, a refused write, an
    /// unsupported platform.
    InstallFailed { reason: String },
    /// Registered, but nothing answered inside [`READY_WITHIN`]. The
    /// service was stopped again so it cannot fight the supervised child
    /// for the port.
    SilentAndStopped { binary: PathBuf },
}

/// The daemon binary this app can hand to the service manager, or `None`.
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

/// Register the daemon with the OS and wait for it to answer.
///
/// Best-effort by construction: every failure leaves the app on the path it
/// took before this module existed, and says which one in the log.
pub(crate) async fn adopt_service(client_port: u16) -> Adoption {
    let Some(binary) = daemon_binary() else {
        tracing::info!(
            "service-adoption: no daemon binary found beside the app or in the install \
             locations — keeping the supervised child; this node will not serve peers \
             while the app is closed"
        );
        return Adoption::NoBinary;
    };

    if let Err(reason) = sovereign_service::install_service(&binary) {
        tracing::warn!(
            binary = %binary.display(),
            %reason,
            "service-adoption: could not register the daemon with the OS — keeping the \
             supervised child"
        );
        return Adoption::InstallFailed { reason };
    }

    tracing::info!(
        binary = %binary.display(),
        client_port,
        "service-adoption: registered the daemon with the OS — waiting for it to answer"
    );

    let host = ServingHost::at(format!("http://127.0.0.1:{client_port}"));
    if host.wait_until_serving(READY_WITHIN).await {
        tracing::info!(
            binary = %binary.display(),
            "service-adoption: the OS-owned daemon is serving — this node stays available \
             to the mesh when the app closes"
        );
        return Adoption::Serving { binary };
    }

    // Committed to neither topology: a registered-but-silent daemon would
    // race the supervised child for the port the moment it woke up.
    let stop = sovereign_service::stop_service();
    tracing::warn!(
        binary = %binary.display(),
        waited_secs = READY_WITHIN.as_secs(),
        stop_result = ?stop,
        "service-adoption: the registered daemon never answered — stopped the service so it \
         cannot fight the supervised child for the port. It starts again at next login."
    );
    Adoption::SilentAndStopped { binary }
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
            "is_file() is the test — a directory called svrn would be handed to \
             install_service as an ExecStart that can never run"
        );
    }
}
