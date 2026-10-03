// SPDX-License-Identifier: AGPL-3.0-or-later
//! The desktop's ENTIRE daemon interaction: one call, once, at startup.
//!
//! sv-surface's `sv-no-daemon-management` bar, as the operator put it on
//! 2026-09-11:
//!
//! > "The daemon is the daemon and the desktop client is the client. […] The
//! > app still does nothing daemon related. It invokes something and moves
//! > on."
//!
//! That is this module. It asks `sovereign_turn_client`'s [`ServingHost`]
//! whether a host is serving and — in a build carrying the `bundled-backend`
//! feature, which the desktop's `Cargo.toml` declares — names the sidecar it
//! may bring one up from. Then it moves on. Nothing here holds a `Child`, a
//! restart policy, a health loop or a shutdown budget; the daemon owns its
//! own lifetime (ARCH principle 12), and a client holding any of those four
//! would be managing a daemon again.
//!
//! # Where it sits, and why that is the whole wiring
//!
//! `main`'s Tauri setup closure calls this immediately BEFORE
//! `bootstrap::detect`. `detect` already probes the client port first and
//! returns `BootstrapMode::Attach` for any daemon that answers, whoever
//! started it — so a host this call reached is a host `detect` attaches to,
//! with no change to `bootstrap` at all.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sovereign_contracts::setup_config::SetupConfig;
use sovereign_turn_client::{BundledBackend, Reached, ServingHost, CAN_BRING_UP_A_BACKEND};

/// Whole budget when this call has a backend to bring up: the probe, the
/// spawn and the readiness wait together.
///
/// 60 s, which is `supervisor_setup`'s `healthy_deadline` and was chosen for
/// the same reason — a cold GGUF load on CPU takes tens of seconds. Using
/// that number rather than a new one means the flip from a supervised child
/// to a brought-up daemon changes no wait a user can feel. It is a CAP, not
/// a sleep: `ensure_reachable` polls at 250 ms and returns on the first
/// answer.
const BRING_UP_WINDOW: Duration = Duration::from_secs(60);

/// Whole budget when there is nothing to bring up: two probes, no polling.
///
/// A window here would make a machine with no daemon and no binary sit
/// staring at a dead port before the UI appeared. `ensure_reachable` still
/// probes once up front and once more at the deadline, which is what the
/// bootstrap probe this precedes would have cost anyway.
const PROBE_ONLY_WINDOW: Duration = Duration::ZERO;

/// Ensure a serving host is reachable, and report what happened.
///
/// `Some(Reached)` names which of the two occurred — a host we did not start
/// was answering, or this build brought one up. `None` means no host is
/// reachable, or that this launch deliberately does not want one; every
/// branch says which on the trace, because an unreachable backend reported
/// as a default is the failure ARCH principle 6 exists to stop.
///
/// The return value is deliberately not a handle. `Reached::BroughtUp`
/// carries a pid as a FACT for the log, and there is nothing in this crate
/// that can signal, wait on or restart it.
pub(crate) async fn ensure_reachable() -> Option<Reached> {
    let host = crate::launch_mode::daemon_host();
    let cfg = match SetupConfig::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            // Not an error: it is what a machine looks like before the setup
            // wizard has run. There is no port to probe and no models for a
            // daemon to load, and `bootstrap::detect` is about to route this
            // launch to the wizard.
            tracing::info!(
                reason = %e,
                "reach: no setup config yet — the wizard owns this launch, nothing to reach"
            );
            return None;
        }
    };

    let base = format!("http://127.0.0.1:{}", cfg.daemon.client_port);
    let mut serving = ServingHost::at(&base);
    let mut within = PROBE_ONLY_WINDOW;

    // A launch that asked THIS process to run the weights must not also get a
    // separate daemon on the same port. `DaemonHost` is the one place that
    // decision is made (`launch_mode::publish`, from `main`); asking it here
    // rather than re-reading the environment is why the two cannot disagree.
    if host.is_supervised() {
        match crate::daemon_binary::stable_daemon_binary() {
            Some(binary) => {
                tracing::info!(
                    path = %binary.display(),
                    "reach: this build ships a backend it can bring up"
                );
                serving = serving.bringing_up(backend_at(binary, &log_dir()));
                within = BRING_UP_WINDOW;
            }
            None => tracing::info!(
                can_bring_up = CAN_BRING_UP_A_BACKEND,
                "reach: no daemon binary beside the app, under the override, or on the \
                 usual install paths — this launch can only probe"
            ),
        }
    } else {
        tracing::info!(
            host = host.as_str(),
            "reach: not bringing a backend up — this process was asked to host its own"
        );
    }

    match serving.ensure_reachable(within).await {
        Ok(reached) => {
            tracing::info!(%base, ?reached, "reach: a serving host is reachable");
            Some(reached)
        }
        Err(e) => {
            tracing::warn!(
                %base,
                error = %e,
                "reach: no serving host — bootstrap will decide this launch without one"
            );
            None
        }
    }
}

/// How this desktop spells "be a daemon".
///
/// `daemon run` is the identical argv for all three resolvable names: `svrn`
/// and `sovereign-cli` are dispatchers that `exec` the daemon sibling on the
/// `daemon` verb, and the sidecar IS that sibling and parses the same
/// `Launch::Daemon`. One arg vector, not one per name.
///
/// `log_to` is not optional in practice: a detached process's output goes
/// nowhere, so a daemon that dies at startup would be invisible — the
/// failure ARCH principle 1 calls a decision you cannot see. The path is the
/// one `svrn daemon start` already appends to
/// (`daemon_cmd/lifecycle.rs:654`), so a bring-up's output lands where every
/// reader and every runbook already looks.
fn backend_at(binary: PathBuf, log_dir: &Path) -> BundledBackend {
    let backend = BundledBackend::at(binary).arg("daemon").arg("run");
    match std::fs::create_dir_all(log_dir) {
        Ok(()) => backend.log_to(log_dir.join("daemon.err")),
        Err(e) => {
            tracing::warn!(
                dir = %log_dir.display(),
                error = %e,
                "reach: cannot create the log dir — the brought-up daemon's output will \
                 be DISCARDED, so a startup failure will show only as an unanswered probe"
            );
            backend
        }
    }
}

/// `<branded root>/logs` — the directory `svrn daemon start` already appends
/// its `daemon.err` into. One spelling, so a bring-up's output and a manual
/// start's land in the same file.
fn log_dir() -> PathBuf {
    sovereign_contracts::rebrand::svrnmesh_root().join("logs")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing to bring up must cost roughly nothing.
    ///
    /// The regression it pins is real and easy to reintroduce:
    /// `ensure_reachable` spends its WHOLE window polling when no backend is
    /// configured, so a non-zero [`PROBE_ONLY_WINDOW`] would make every
    /// launch on a machine with no daemon and no binary sit on a dead port
    /// for that long before the window appeared.
    #[tokio::test]
    async fn a_launch_with_nothing_to_bring_up_does_not_poll() {
        // A port nothing is listening on: bind, learn the number, release.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);

        let started = std::time::Instant::now();
        let outcome = ServingHost::at(format!("http://127.0.0.1:{port}"))
            .ensure_reachable(PROBE_ONLY_WINDOW)
            .await;
        let elapsed = started.elapsed();

        // Absence is REPORTED, never defaulted (ARCH principle 6).
        let err = outcome.expect_err("nothing is serving on a released port");
        assert!(
            err.to_string().contains("no serving host answered"),
            "unexpected report: {err}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "a probe-only reach took {elapsed:?} — the window is being spent polling, \
             which is the wait every daemonless launch would pay before the UI appeared"
        );
    }

    #[test]
    fn the_backend_is_named_daemon_run_and_its_output_is_kept() {
        let dir = std::env::temp_dir().join(format!("svt1-logs-{}", std::process::id()));
        let b = backend_at(PathBuf::from("/nonexistent/svrn"), &dir);
        // `BundledBackend` is PartialEq, so the whole spec is one comparison
        // against an independently-spelled expectation.
        let expected = BundledBackend::at("/nonexistent/svrn")
            .arg("daemon")
            .arg("run")
            .log_to(dir.join("daemon.err"));
        assert_eq!(b, expected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unwritable_log_dir_discards_output_rather_than_failing_the_launch() {
        // `/dev/null/logs` cannot be created on any unix. The bring-up must
        // still be spelled — a daemon the user can reach beats a daemon whose
        // stderr we could file — and the warning above is what says so.
        let b = backend_at(
            PathBuf::from("/nonexistent/svrn"),
            Path::new("/dev/null/logs"),
        );
        assert_eq!(
            b,
            BundledBackend::at("/nonexistent/svrn")
                .arg("daemon")
                .arg("run")
        );
    }
}
