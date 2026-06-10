// SPDX-License-Identifier: AGPL-3.0-or-later
//! Proactive RSS watch for the daemon process.
//!
//! The daemon pins multi-GB model weights; on a 64 GB host macOS
//! jetsam SIGKILLs it somewhere past ~44 GB RSS with no drain and no
//! breadcrumb (2026-06-10 incident: the daemon died under memory
//! pressure during a workspace build and stayed down). Shutdown-time
//! *forensics* exist (`lifecycle::log_shutdown_context` flags peak RSS
//! on the way out) — this module is the **runtime** half: a 60s
//! sampler that
//!
//! 1. publishes the latest RSS to a process-global (read by `/status`
//!    consumers in this process and by future doctor checks),
//! 2. `warn!`s on upward crossing of a soft limit
//!    (`SOVEREIGN_RSS_SOFT_LIMIT_MB`, default 20480 — comfortably
//!    under the 24 GiB forensic flag line), re-warning at most every
//!    15 minutes while above, and
//! 3. optionally (`SOVEREIGN_RSS_HARD_LIMIT_MB`, **unset = disabled**)
//!    triggers a graceful self-SIGTERM with a **non-zero exit code**
//!    on breach, so a service-managed daemon (launchd
//!    `KeepAlive.SuccessfulExit=false` / systemd `Restart=on-failure`)
//!    is relaunched clean *before* jetsam can SIGKILL it mid-write.
//!    In-process beats unit-file MemoryMax: cgroup limits deliver
//!    SIGKILL with no drain — exactly the corruption being avoided.
//!
//! Loop shape cloned from `log_rotation::spawn_rotation_loop`
//! (interval ticker, skip the first tick, `spawn_blocking` for the
//! sample).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Latest sampled RSS in MiB. 0 = not yet sampled.
static LATEST_RSS_MB: AtomicU64 = AtomicU64::new(0);

/// Set when the hard limit triggered the shutdown — the exit site in
/// `daemon_cmd` reads this to turn the otherwise-clean SIGTERM exit
/// into a non-zero one (required for the service manager to restart).
static HARD_EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn latest_rss_mb() -> Option<u64> {
    match LATEST_RSS_MB.load(Ordering::Relaxed) {
        0 => None,
        v => Some(v),
    }
}

pub fn hard_exit_requested() -> bool {
    HARD_EXIT_REQUESTED.load(Ordering::Relaxed)
}

/// Soft warn threshold. Env-overridable; default 20 GiB.
pub fn soft_limit_mb() -> u64 {
    parse_limit_mb(
        std::env::var("SOVEREIGN_RSS_SOFT_LIMIT_MB").ok().as_deref(),
        Some(20_480),
    )
    .unwrap_or(20_480)
}

/// Hard restart threshold. `None` (the default — env unset/invalid)
/// disables the behavior entirely.
pub fn hard_limit_mb() -> Option<u64> {
    parse_limit_mb(
        std::env::var("SOVEREIGN_RSS_HARD_LIMIT_MB").ok().as_deref(),
        None,
    )
}

fn parse_limit_mb(raw: Option<&str>, default: Option<u64>) -> Option<u64> {
    match raw {
        None => default,
        Some(v) => v.trim().parse::<u64>().ok().filter(|&n| n > 0).or(default),
    }
}

/// Warn-on-upward-crossing with a re-warn floor: one warning the
/// moment RSS crosses the soft limit, then at most one every
/// `REWARN_EVERY` while it stays above. Pure state machine so the
/// policy is testable with injected clocks.
const REWARN_EVERY: Duration = Duration::from_secs(15 * 60);

struct WarnGate {
    above: bool,
    last_warn: Option<Instant>,
}

impl WarnGate {
    fn new() -> Self {
        Self {
            above: false,
            last_warn: None,
        }
    }

    /// Returns true when this observation should emit a warning.
    fn observe(&mut self, rss_mb: u64, soft_limit_mb: u64, now: Instant) -> bool {
        let above = rss_mb > soft_limit_mb;
        let warn = if !above {
            self.above = false;
            false
        } else if !self.above {
            // Upward crossing.
            self.above = true;
            true
        } else {
            // Still above: re-warn only past the floor.
            self.last_warn
                .map(|t| now.duration_since(t) >= REWARN_EVERY)
                .unwrap_or(true)
        };
        if warn {
            self.last_warn = Some(now);
        }
        warn
    }
}

/// Spawn the watch loop. Limits are read once at spawn (a daemon
/// restart picks up env changes — same posture as every other
/// `SOVEREIGN_*` knob in this binary).
pub fn spawn_memory_watch(interval: Duration) -> tokio::task::JoinHandle<()> {
    let soft = soft_limit_mb();
    let hard = hard_limit_mb();
    tracing::info!(
        soft_limit_mb = soft,
        hard_limit_mb = hard,
        interval_secs = interval.as_secs(),
        "memory-watch: armed"
    );
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // Skip the immediate first tick — startup RSS is still
        // climbing through model loads; the first meaningful sample
        // is one interval in.
        ticker.tick().await;
        let mut gate = WarnGate::new();
        loop {
            ticker.tick().await;
            let rss = tokio::task::spawn_blocking(current_rss_mb)
                .await
                .ok()
                .flatten();
            let Some(rss_mb) = rss else { continue };
            LATEST_RSS_MB.store(rss_mb, Ordering::Relaxed);

            if let Some(hard_mb) = hard {
                if rss_mb > hard_mb {
                    // Same forensic vocabulary as
                    // `lifecycle::log_shutdown_context` so greps line
                    // up across boot, runtime, and shutdown.
                    tracing::error!(
                        rss_mb,
                        hard_limit_mb = hard_mb,
                        peak_rss_mb = peak_rss_mb(),
                        "memory-watch: HARD limit breached — initiating graceful restart \
                         (non-zero exit; service manager relaunches clean before jetsam can SIGKILL)"
                    );
                    HARD_EXIT_REQUESTED.store(true, Ordering::SeqCst);
                    // Funnel through the EXISTING graceful path:
                    // self-SIGTERM → wait_for_shutdown forensics →
                    // axum drain → mesh.json persist → exit (the exit
                    // site maps HARD_EXIT_REQUESTED to a non-zero
                    // code).
                    #[cfg(unix)]
                    // SAFETY: raise(SIGTERM) on our own pid is
                    // async-signal-safe and handled by the daemon's
                    // signal listener.
                    unsafe {
                        libc::raise(libc::SIGTERM);
                    }
                    return;
                }
            }

            if gate.observe(rss_mb, soft, Instant::now()) {
                tracing::warn!(
                    rss_mb,
                    soft_limit_mb = soft,
                    peak_rss_mb = peak_rss_mb(),
                    "memory-watch: rss above soft limit — jetsam risk; \
                     see RUNBOOK memory section (canonical model config / RSS knobs)"
                );
            }
        }
    })
}

/// Current resident set size in MiB, platform-native (no extra deps).
fn current_rss_mb() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        // proc_pidinfo(PROC_PIDTASKINFO) reports pti_resident_size in
        // bytes. SAFETY: out-struct is zeroed and sized; the call
        // writes at most `size` bytes into it.
        let pid = std::process::id() as libc::c_int;
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        let rc = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTASKINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if rc != size {
            return None;
        }
        Some(info.pti_resident_size / (1024 * 1024))
    }
    #[cfg(target_os = "linux")]
    {
        // /proc/self/statm field 2 = resident pages.
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page_size <= 0 {
            return None;
        }
        Some(pages * page_size as u64 / (1024 * 1024))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Peak RSS in MiB via getrusage — same unit handling as
/// `lifecycle`'s shutdown forensics (macOS reports bytes, Linux KB).
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
        Some(raw / (1024 * 1024))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(raw / 1024)
    }
}

#[cfg(not(unix))]
fn peak_rss_mb() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_parse_policy() {
        // soft: default applies on unset/garbage/zero
        assert_eq!(parse_limit_mb(None, Some(20_480)), Some(20_480));
        assert_eq!(parse_limit_mb(Some("nope"), Some(20_480)), Some(20_480));
        assert_eq!(parse_limit_mb(Some("0"), Some(20_480)), Some(20_480));
        assert_eq!(parse_limit_mb(Some("4096"), Some(20_480)), Some(4096));
        // hard: unset/garbage/zero stay DISABLED (None)
        assert_eq!(parse_limit_mb(None, None), None);
        assert_eq!(parse_limit_mb(Some("nope"), None), None);
        assert_eq!(parse_limit_mb(Some("0"), None), None);
        assert_eq!(parse_limit_mb(Some("30000"), None), Some(30_000));
    }

    #[test]
    fn warn_gate_fires_on_upward_crossing_only() {
        let mut g = WarnGate::new();
        let t0 = Instant::now();
        assert!(!g.observe(100, 200, t0)); // below
        assert!(g.observe(250, 200, t0)); // crossing → warn
        assert!(!g.observe(260, 200, t0 + Duration::from_secs(60))); // above, within floor
        assert!(g.observe(260, 200, t0 + REWARN_EVERY)); // floor elapsed → re-warn
        assert!(!g.observe(150, 200, t0 + REWARN_EVERY)); // dropped below → reset
        assert!(g.observe(300, 200, t0 + REWARN_EVERY + Duration::from_secs(1))); // re-cross → warn immediately
    }

    #[test]
    fn current_rss_reports_something_plausible() {
        // We're a running process — RSS must be nonzero and sane.
        let rss = current_rss_mb().expect("rss sample");
        assert!(rss > 0, "rss should be nonzero");
        assert!(rss < 1_048_576, "rss under a TiB sanity bound");
    }
}
