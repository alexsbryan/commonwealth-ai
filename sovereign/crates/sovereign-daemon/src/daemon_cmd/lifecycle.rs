// SPDX-License-Identifier: AGPL-3.0-or-later
//! The run path's signal handling — the ONE piece of cli-daemon's
//! `daemon_cmd/lifecycle.rs` the assembled-host process needs. The
//! lifecycle verbs (start / stop / restart / reload / status), the
//! port probes and the pidfile readers are OUT-of-process verb
//! infrastructure and stay with the `svrn` CLI tree, which keeps its
//! own originals.

/// Wait for SIGINT (Ctrl-C) or SIGTERM (systemd/launchd shutdown) — the
/// only triggers that end the daemon's run loop. A user-initiated mesh
/// leave no longer exits the process: `POST /v1/mesh/leave` re-creates a
/// solo mesh in-process (`EmbeddedDaemon::leave_to_solo`, rebinding `:9741`)
/// so there's nothing to relaunch. Both signals are a deliberate stop →
/// exit 0; the RSS-hard-limit self-exit in `shutdown_daemon` is the only
/// path that asks the service manager for a relaunch.
pub(super) async fn wait_for_shutdown() {
    // Glassbox: shutdown forensics. A 2026-05-20 incident left the
    // daemon abort-crashing in ggml-metal's `__cxa_finalize_ranges`
    // path with no breadcrumb naming the trigger — was it SIGINT
    // from a stray Ctrl-C, SIGTERM from a peer `svrn daemon
    // stop`, launchd OOM, or something else? Without a log, the
    // post-mortem stalls. Emit the signal source + process context
    // (PID, PPID, peak RSS, jetsam hint) so the next incident
    // surfaces the trigger immediately.
    //
    // We can't get the sender PID — `tokio::signal::unix::signal`
    // abstracts away `SA_SIGINFO`, so siginfo.si_pid is unreachable
    // without dropping to raw `sigaction()`. Logging local context
    // is the practical second-best: if RSS is in the jetsam danger
    // zone (>~24 GiB on a 64 GiB host) and the signal was SIGTERM,
    // the operator gets a strong hint to inspect Console for a
    // "low memory" jetsam event.
    #[cfg(unix)]
    {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "sigterm handler install failed — falling back to SIGINT-only"
                    );
                    tokio::signal::ctrl_c().await.ok();
                    log_shutdown_context("SIGINT", "fallback");
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => log_shutdown_context("SIGINT", "primary"),
            _ = sigterm.recv() => log_shutdown_context("SIGTERM", "primary"),
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!(signal = "ctrl_c", "daemon: shutdown signal received");
    }
}

/// One-line shutdown-receipt log with forensic context. Field names
/// are stable so `grep "daemon: shutdown signal received"` walks the
/// trail across runs.
#[cfg(unix)]
fn log_shutdown_context(signal: &'static str, path: &'static str) {
    let pid = std::process::id();
    let ppid: i64 = unsafe { libc::getppid() } as i64;
    let rss_mb = peak_rss_mb();
    let jetsam_risk = rss_mb.map(|mb| mb >= 24 * 1024).unwrap_or(false);
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if jetsam_risk && signal == "SIGTERM" {
        // Platform-correct forensics pointer: during the 2026-07-27
        // post-mortem this line said "inspect Console.app" on a Linux
        // box — misdirecting exactly the person doing memory forensics.
        let where_to_look = if cfg!(target_os = "macos") {
            "inspect Console.app for 'low memory' or 'memorystatus'"
        } else {
            "check `journalctl -k` / `dmesg` for oom-kill and `coredumpctl list`"
        };
        tracing::warn!(
            signal,
            path,
            pid,
            ppid,
            rss_mb = rss_mb.unwrap_or(0),
            at_unix = now_unix,
            "daemon: shutdown signal received — peak RSS suggests possible jetsam/OOM trigger; {} around this timestamp",
            where_to_look
        );
    } else {
        tracing::info!(
            signal,
            path,
            pid,
            ppid,
            rss_mb = rss_mb.unwrap_or(0),
            at_unix = now_unix,
            "daemon: shutdown signal received"
        );
    }
}

/// Peak resident-set size in MiB via `getrusage(RUSAGE_SELF)`.
/// On macOS, `ru_maxrss` is in *bytes*; on Linux it's in *kilobytes*.
/// `None` on platforms / failures.
#[cfg(unix)]
fn peak_rss_mb() -> Option<u64> {
    // SAFETY: getrusage with a properly-zeroed `rusage` struct is safe.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    if rc != 0 {
        return None;
    }
    let raw = ru.ru_maxrss as u64;
    #[cfg(target_os = "macos")]
    {
        Some(raw / (1024 * 1024)) // bytes → MiB
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(raw / 1024) // kilobytes → MiB
    }
}
