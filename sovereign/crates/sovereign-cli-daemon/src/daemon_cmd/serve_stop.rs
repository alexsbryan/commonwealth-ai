// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stop the `serve` the daemon brought up (pb-svrn-dials-serve).
//!
//! The daemon starts serve detached at boot (`serve_client::ensure_serve`), so
//! a stop that left it would keep the models in memory, and a restart would
//! reuse serve with the models it loaded before, not the config's. Before the
//! switch both released and reloaded the models, because the daemon held them.
//! Only the default base is this daemon's to stop: a serve at an operator-named
//! `[node] entry` is someone else's process.

use sovereign_core::setup_config::SetupConfig;
use sovereign_daemon::serve_client::{resolve_serve_base, ServeBase, ServeBaseSource};

/// After the daemon's own stop: when that succeeded, stop serve too and
/// report the combined result; when it failed, report that and leave serve.
pub(super) async fn after(daemon_stop: i32) -> i32 {
    if daemon_stop != 0 {
        return daemon_stop;
    }
    stop_serve().await
}

/// `svrn daemon status`'s second process: where the daemon serves from, as it
/// decided at boot (`/status`'s `serving`), and, when that is serve, whether
/// serve is listening on its port.
pub(super) async fn print_serving(client: &reqwest::Client, base: &str) {
    let serving = match client.get(format!("{base}/status")).send().await {
        Ok(r) if r.status().is_success() => {
            r.json::<serde_json::Value>().await.ok().and_then(|v| {
                v.get("serving")
                    .and_then(|s| s.as_str())
                    .map(str::to_string)
            })
        }
        _ => None,
    };
    let Some(serving) = serving else {
        println!("  serving: not reported by this daemon");
        return;
    };
    println!("  serving: {serving}");
    if serving != "serve" {
        return;
    }
    let port = sovereign_contracts::venue::DEFAULT_SERVE_PORT;
    #[cfg(unix)]
    match super::lifecycle::find_daemon_pid_by_port(port) {
        Some(pid) => println!("  serve: running (pid {pid}, :{port})"),
        None => println!("  serve: nothing listening on :{port}"),
    }
}

#[cfg(unix)]
async fn stop_serve() -> i32 {
    let serve = match SetupConfig::load() {
        Ok(config) => resolve_serve_base(&config.node),
        Err(e) => {
            eprintln!("  serve: not stopped, the config is unreadable ({e})");
            return 1;
        }
    };
    stop_serve_at(&serve, sovereign_contracts::venue::DEFAULT_SERVE_PORT).await
}

/// The stop itself, over the resolved base and the port to look serve up on.
#[cfg(unix)]
async fn stop_serve_at(serve: &ServeBase, port: u16) -> i32 {
    if serve.source != ServeBaseSource::Default {
        eprintln!(
            "  serve at {} is [node] entry's, not this daemon's: left running",
            serve.base
        );
        return 0;
    }
    let Some(pid) = super::lifecycle::find_daemon_pid_by_port(port) else {
        tracing::debug!(port, "serve stop: nothing listens on serve's port");
        eprintln!("  serve: not running (nothing on :{port})");
        return 0;
    };
    eprintln!("stopping serve (pid {pid}, :{port}) …");
    // SAFETY: POSIX kill; `pid` is the process listening on serve's port.
    if unsafe {
        super::lifecycle::libc_kill(pid, 15 /* SIGTERM */)
    } != 0
    {
        eprintln!("✗ serve: SIGTERM to pid {pid} failed");
        return 1;
    }
    super::lifecycle::await_exit_or_sigkill(pid, ", serve").await
}

#[cfg(not(unix))]
async fn stop_serve() -> i32 {
    eprintln!("  serve: stop it by hand on this platform (no port lookup)");
    0
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A process listening on a free port, standing in for serve. Detached
    /// through a shell, as serve is from the CLI (never its child), so its
    /// exit is judged by the port and no zombie outlives the kill.
    fn listener() -> u16 {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port();
        let script = format!(
            "python3 -c 'import socket,time\ns=socket.socket()\ns.bind((\"127.0.0.1\",{port}))\ns.listen()\ntime.sleep(120)' >/dev/null 2>&1 &"
        );
        std::process::Command::new("sh")
            .args(["-c", &script])
            .status()
            .expect("sh");
        let up = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            listening(port)
        });
        assert!(up, "the stand-in never listened on :{port}");
        port
    }

    fn listening(port: u16) -> bool {
        super::super::lifecycle::find_daemon_pid_by_port(port).is_some()
    }

    fn base(source: ServeBaseSource) -> ServeBase {
        ServeBase {
            base: "http://127.0.0.1:1".to_string(),
            source,
        }
    }

    #[tokio::test]
    async fn the_default_serve_is_stopped_and_an_entry_is_left_running() {
        let entry = listener();
        assert_eq!(
            stop_serve_at(&base(ServeBaseSource::NodeEntry), entry).await,
            0
        );
        assert!(
            listening(entry),
            "a serve at [node] entry is not this daemon's to stop"
        );
        assert_eq!(
            stop_serve_at(&base(ServeBaseSource::Default), entry).await,
            0
        );

        let ours = listener();
        assert_eq!(
            stop_serve_at(&base(ServeBaseSource::Default), ours).await,
            0
        );
        assert!(!listening(ours), "the default serve must be stopped");
    }
}
