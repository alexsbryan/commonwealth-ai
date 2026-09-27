// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stop the `serve` the daemon brought up (pb-svrn-dials-serve).
//!
//! The daemon starts serve detached at boot (`serve_client::ensure_serve`), so
//! a stop that left it would keep the models in memory, and a restart would
//! reuse serve with the models it loaded before, not the config's. Before the
//! switch both released and reloaded the models, because the daemon held them.
//! Only the serve this daemon brought up is its to stop: the bring-up records
//! its pid (`serve_client::ServeRecord`), and the stop signals that pid alone,
//! while it still listens where it was brought up. A serve at an
//! operator-named `[node] entry`, or one found already serving (a developer's
//! own `sovereign-serve`), is someone else's process and is left running.

use sovereign_core::setup_config::SetupConfig;
#[cfg(unix)]
use sovereign_daemon::serve_client::ServeRecord;
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
    let port = sovereign_contracts::venue::serve_port();
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
    stop_serve_at(&serve, &sovereign_daemon::startup::serve_pid_path()).await
}

/// The stop itself, over the resolved base and the bring-up's record.
#[cfg(unix)]
async fn stop_serve_at(serve: &ServeBase, record_path: &std::path::Path) -> i32 {
    if serve.source != ServeBaseSource::Default {
        eprintln!(
            "  serve at {} is [node] entry's, not this daemon's: left running",
            serve.base
        );
        return 0;
    }
    let record = match ServeRecord::read_from(record_path) {
        Ok(Some(record)) => record,
        Ok(None) => {
            let port = sovereign_contracts::venue::serve_port();
            tracing::debug!(port, "serve stop: no bring-up record");
            match super::lifecycle::find_daemon_pid_by_port(port) {
                Some(pid) => eprintln!(
                    "  serve (pid {pid}, :{port}) was not brought up by this daemon: left running"
                ),
                None => eprintln!("  serve: not running (nothing on :{port})"),
            }
            return 0;
        }
        Err(e) => {
            eprintln!("✗ serve: not stopped, its bring-up record is unreadable ({e})");
            return 1;
        }
    };
    let ServeRecord { pid, port } = record;
    let listener = super::lifecycle::find_daemon_pid_by_port(port);
    tracing::debug!(pid, port, listener = ?listener, "serve stop: bring-up record read");
    if listener != Some(pid as i32) {
        let _ = std::fs::remove_file(record_path);
        match listener {
            Some(other) => eprintln!(
                "  serve: pid {pid} no longer listens on :{port}; pid {other} there is not this daemon's: left running"
            ),
            None => eprintln!("  serve: not running (pid {pid} no longer on :{port})"),
        }
        return 0;
    }
    eprintln!("stopping serve (pid {pid}, :{port}) …");
    let pid = pid as i32;
    // SAFETY: POSIX kill; `pid` is the serve this daemon brought up, and it
    // still listens on the port it was brought up on.
    if unsafe {
        super::lifecycle::libc_kill(pid, 15 /* SIGTERM */)
    } != 0
    {
        eprintln!("✗ serve: SIGTERM to pid {pid} failed");
        return 1;
    }
    let code = super::lifecycle::await_exit_or_sigkill(pid, ", serve").await;
    let _ = std::fs::remove_file(record_path);
    code
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

    /// Record `port`'s listener as this daemon's bring-up.
    fn record(dir: &tempfile::TempDir, pid: u32, port: u16) -> std::path::PathBuf {
        let path = dir.path().join("serve.pid");
        ServeRecord { pid, port }.write_to(&path).expect("record");
        path
    }

    fn pid_on(port: u16) -> u32 {
        super::super::lifecycle::find_daemon_pid_by_port(port).expect("listening") as u32
    }

    /// The serve this daemon brought up is stopped, and its record cleared.
    #[tokio::test]
    async fn the_recorded_serve_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let ours = listener();
        let path = record(&dir, pid_on(ours), ours);
        assert_eq!(
            stop_serve_at(&base(ServeBaseSource::Default), &path).await,
            0
        );
        assert!(!listening(ours), "the recorded serve must be stopped");
        assert!(!path.exists(), "the record is cleared with it");
    }

    /// A listener on serve's port that this daemon did not bring up (no
    /// record) survives the stop: a developer's own serve, found already
    /// serving at boot.
    #[tokio::test]
    async fn a_serve_this_daemon_did_not_bring_up_survives_the_stop() {
        let dir = tempfile::tempdir().unwrap();
        let theirs = listener();
        let path = dir.path().join("serve.pid");
        assert_eq!(
            stop_serve_at(&base(ServeBaseSource::Default), &path).await,
            0
        );
        assert!(
            listening(theirs),
            "an unrecorded serve is not this daemon's to stop"
        );
        let pid = pid_on(theirs) as i32;
        // SAFETY: the stand-in this test started.
        unsafe { super::super::lifecycle::libc_kill(pid, 9) };
    }

    /// A record whose pid no longer listens on its port names a process the
    /// stop must not touch (a reused pid, or another serve on the port).
    #[tokio::test]
    async fn a_stale_record_signals_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let theirs = listener();
        let path = record(&dir, std::process::id(), theirs);
        assert_eq!(
            stop_serve_at(&base(ServeBaseSource::Default), &path).await,
            0
        );
        assert!(listening(theirs), "the listener is not the recorded pid");
        assert!(!path.exists(), "the stale record is cleared");
        let pid = pid_on(theirs) as i32;
        // SAFETY: the stand-in this test started.
        unsafe { super::super::lifecycle::libc_kill(pid, 9) };
    }

    /// A serve at `[node] entry` is left running even when a record names it.
    #[tokio::test]
    async fn an_entry_serve_is_left_running() {
        let dir = tempfile::tempdir().unwrap();
        let entry = listener();
        let path = record(&dir, pid_on(entry), entry);
        assert_eq!(
            stop_serve_at(&base(ServeBaseSource::NodeEntry), &path).await,
            0
        );
        assert!(
            listening(entry),
            "a serve at [node] entry is not this daemon's to stop"
        );
        let pid = pid_on(entry) as i32;
        // SAFETY: the stand-in this test started.
        unsafe { super::super::lifecycle::libc_kill(pid, 9) };
    }
}
