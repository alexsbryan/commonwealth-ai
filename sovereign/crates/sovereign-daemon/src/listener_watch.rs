// SPDX-License-Identifier: AGPL-3.0-or-later
//! Client-listener watchdog (DAEMON_RESILIENCE.md P0.5).
//!
//! The client API bind lives inside the mesh daemon's spawned serve
//! task and is deliberately BEST-EFFORT — making it fatal would strand
//! the sovereign-mesh integration tests that bind the default ports
//! under full-workspace parallelism (the documented landmine from the
//! `leave_to_solo` work). The cost of best-effort was a reachable
//! **phantom-Running** state: models loaded, process alive, no client
//! listener — every caller sees connection-refused while the service
//! manager sees a healthy process. Observed live 2026-07-18: a mesh
//! leave dropped `:9741` and the process idled for days.
//!
//! This watchdog closes the hole from OUTSIDE the bind path, so the
//! test posture is untouched: TCP-probe the client port once a minute;
//! after [`FAILURES_TO_EXIT`] consecutive refusals declare the
//! listener lost and self-SIGTERM. The exit site maps the request to
//! **exit code 104** so launchd/systemd relaunch a clean process —
//! same shape as `memory_watch`'s RSS hard-limit exit 102. Transient
//! gaps (the ~1s `leave_to_solo` rebind) can never accumulate three
//! minute-spaced consecutive failures.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Set when the watchdog triggered the shutdown — the exit site in
/// `daemon_cmd` reads this to exit 104 (relaunch) instead of 0.
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

const PROBE_INTERVAL: Duration = Duration::from_secs(60);
const FAILURES_TO_EXIT: u32 = 3;
/// Grace before the first probe. The listener binds during mesh start,
/// long before this elapses even on a cold boot — the grace exists so
/// a pathologically slow boot can't be mistaken for a lost listener.
const STARTUP_GRACE: Duration = Duration::from_secs(120);

pub fn exit_requested() -> bool {
    EXIT_REQUESTED.load(Ordering::Relaxed)
}

/// Destination for the probe. `0.0.0.0`/`::` are bind-side wildcards,
/// not dialable — those (and unparseable binds) probe loopback, which
/// every wildcard bind serves. A specific non-loopback `client_bind`
/// is probed at its own address.
fn probe_addr(client_bind: &str, port: u16) -> std::net::SocketAddr {
    let ip: std::net::IpAddr = match client_bind.trim().parse() {
        Ok(ip) if !std::net::IpAddr::is_unspecified(&ip) => ip,
        _ => std::net::IpAddr::from([127, 0, 0, 1]),
    };
    std::net::SocketAddr::from((ip, port))
}

fn probe(addr: std::net::SocketAddr) -> bool {
    std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok()
}

/// The watchdog loop. Run under `supervise::spawn_supervised` so a
/// panic in the probe path can't silently disarm the watchdog.
pub async fn watch_loop(client_bind: String, client_port: u16) {
    let addr = probe_addr(&client_bind, client_port);
    tracing::info!(%addr, "listener-watch: armed");
    tokio::time::sleep(STARTUP_GRACE).await;
    let mut consecutive: u32 = 0;
    loop {
        let alive = tokio::task::spawn_blocking(move || probe(addr))
            .await
            .unwrap_or(false);
        if alive {
            if consecutive > 0 {
                tracing::info!(
                    %addr,
                    failed_probes = consecutive,
                    "listener-watch: listener is back"
                );
            }
            consecutive = 0;
        } else {
            consecutive += 1;
            tracing::warn!(
                %addr,
                consecutive,
                threshold = FAILURES_TO_EXIT,
                "listener-watch: client port not accepting connections"
            );
            if consecutive >= FAILURES_TO_EXIT {
                tracing::error!(
                    %addr,
                    "listener-watch: client listener LOST (phantom-Running) — initiating \
                     graceful restart (exit 104; service manager relaunches)"
                );
                EXIT_REQUESTED.store(true, Ordering::SeqCst);
                // Funnel through the existing graceful path, exactly
                // like memory_watch's hard-limit exit.
                #[cfg(unix)]
                // SAFETY: raise(SIGTERM) on our own pid is handled by
                // the daemon's signal listener.
                unsafe {
                    libc::raise(libc::SIGTERM);
                }
                return;
            }
        }
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_addr_resolves_wildcard_and_specific_binds() {
        assert_eq!(
            probe_addr("0.0.0.0", 9741),
            std::net::SocketAddr::from(([127, 0, 0, 1], 9741))
        );
        assert_eq!(
            probe_addr("127.0.0.1", 9741),
            std::net::SocketAddr::from(([127, 0, 0, 1], 9741))
        );
        assert_eq!(
            probe_addr("192.168.1.5", 9700),
            std::net::SocketAddr::from(([192, 168, 1, 5], 9700))
        );
        // Garbage bind falls back to loopback rather than disarming.
        assert_eq!(
            probe_addr("not-an-ip", 9741),
            std::net::SocketAddr::from(([127, 0, 0, 1], 9741))
        );
    }

    #[test]
    fn probe_detects_live_listener() {
        // Only the live case is asserted: probing the port after
        // dropping the listener is RACY under the full-workspace test
        // runner — another suite can bind the just-freed ephemeral
        // port between drop and probe (observed 2026-07-18, one flake
        // in 7,731). The dead case is std's connect_timeout returning
        // Err, not logic this module owns.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let addr = listener.local_addr().unwrap();
        assert!(probe(addr), "live listener must probe true");
    }
}
