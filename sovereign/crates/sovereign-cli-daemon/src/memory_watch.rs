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
//!    (`SOVEREIGN_RSS_SOFT_LIMIT_MB`), re-warning at most every
//!    15 minutes while above, and
//! 3. triggers a graceful self-SIGTERM with a **non-zero exit code**
//!    on hard-limit breach (`SOVEREIGN_RSS_HARD_LIMIT_MB`), so a
//!    service-managed daemon (launchd `KeepAlive.SuccessfulExit=false`
//!    / systemd `Restart=on-failure`) is relaunched clean *before*
//!    jetsam can SIGKILL it mid-write. In-process beats unit-file
//!    MemoryMax: cgroup limits deliver SIGKILL with no drain — exactly
//!    the corruption being avoided.
//!
//! **Both limits default ON, derived from total system RAM**
//! (DAEMON_RESILIENCE.md P0.3). The defense originally shipped
//! disabled-unless-env-set, and the only thing that set the env was
//! `scripts/daemon-supervised.sh` — so every launchd/systemd install
//! ran with the hard limit off and died to kernel-OOM/jetsam SIGKILL
//! with no drain (observed twice at ~39.5 GB, 2026-07). Defaults:
//! hard = 85% of RAM / soft = 70% on Linux; 65% / 50% on macOS, where
//! jetsam has been observed SIGKILLing at ~69% of RAM (~44 GB on a
//! 64 GB host — see the incident above), so the limit must sit below
//! jetsam's trigger zone to be reachable at all. On Linux a cgroup v2
//! `memory.max` (container / toolbox deployments) caps the detected
//! total. Env still overrides either limit; `SOVEREIGN_RSS_HARD_LIMIT_MB=0`
//! (or `off`) is the explicit kill-switch. RAM detection failing falls
//! back to the legacy posture (soft 20 GiB, hard disabled).
//!
//! Loop shape cloned from `log_rotation::rotation_loop`
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

/// RAM-fraction defaults (percent). macOS sits well under the observed
/// jetsam trigger zone (~69% of RAM); Linux leaves headroom for the
/// rest of the system before the kernel OOM killer engages. Aligned
/// with the inference fit gate's refuse-to-load headroom
/// (`sovereign-inference` capacity check), so a config the daemon
/// agreed to load does not sit above its own hard limit.
const HARD_PCT: u64 = if cfg!(target_os = "macos") { 65 } else { 85 };
const SOFT_PCT: u64 = if cfg!(target_os = "macos") { 50 } else { 70 };

/// Legacy fallback when total RAM cannot be detected: the historical
/// defaults (soft 20 GiB, hard disabled).
const LEGACY_SOFT_MB: u64 = 20_480;

/// Soft warn threshold. Env-overridable; default 70% (Linux) / 50%
/// (macOS) of total RAM, legacy 20 GiB when RAM is undetectable.
pub fn soft_limit_mb() -> u64 {
    let default = derived_soft_limit_mb(total_system_ram_mb()).unwrap_or(LEGACY_SOFT_MB);
    parse_limit_mb(
        std::env::var("SOVEREIGN_RSS_SOFT_LIMIT_MB").ok().as_deref(),
        Some(default),
    )
    .unwrap_or(default)
}

/// Hard restart threshold. **Default OFF** — a self-SIGTERM hard limit is
/// only useful under a supervisor that relaunches the daemon, so it must be
/// opted into. Enable with `SOVEREIGN_RSS_HARD_LIMIT_MB=<mb>` (explicit) or
/// `=auto` (RAM-derived: 85% Linux / 65% macOS); `0`/`off`/unset/garbage all
/// leave it disabled. `scripts/daemon-supervised.sh` sets it; bare
/// `svrn daemon run` leaves it off (soft-warn still fires).
pub fn hard_limit_mb() -> Option<u64> {
    hard_limit_policy(
        std::env::var("SOVEREIGN_RSS_HARD_LIMIT_MB").ok().as_deref(),
        total_system_ram_mb(),
    )
}

fn derived_soft_limit_mb(total_ram_mb: Option<u64>) -> Option<u64> {
    total_ram_mb.map(|ram| ram * SOFT_PCT / 100)
}

fn derived_hard_limit_mb(total_ram_mb: Option<u64>) -> Option<u64> {
    total_ram_mb.map(|ram| ram * HARD_PCT / 100)
}

/// Pure policy for the hard limit so the precedence (disabled by default >
/// explicit disable > `auto` RAM-derived > explicit value) is unit-testable.
///
/// **Disabled unless explicitly enabled.** A hard limit self-SIGTERMs the
/// daemon, which only makes sense under a supervisor that relaunches it
/// (`scripts/daemon-supervised.sh`, or a launchd/systemd unit that sets the
/// env); on an unsupervised `svrn daemon run` it is pure downtime with
/// no recovery. So the default is OFF and supervisors opt in — matching
/// DAEMON_RESILIENCE.md ("hard limit disabled unless env set"). The soft
/// limit (warn-only, non-fatal) stays on as the observability signal.
fn hard_limit_policy(raw: Option<&str>, total_ram_mb: Option<u64>) -> Option<u64> {
    match raw.map(str::trim) {
        // Unset → DISABLED. This is the change: no accidental self-kill on
        // an unsupervised daemon.
        None => None,
        // Explicit kill-switch (redundant with the default, kept explicit).
        Some("0") | Some("off") | Some("OFF") => None,
        // Opt in at the RAM-derived default (65% macOS / 85% Linux).
        Some(v) if v.eq_ignore_ascii_case("auto") => derived_hard_limit_mb(total_ram_mb),
        // Explicit MB value. Non-numeric/garbage is treated as OFF rather
        // than silently deriving a limit — no accidental enable.
        Some(v) => v.parse::<u64>().ok().filter(|&n| n > 0),
    }
}

fn parse_limit_mb(raw: Option<&str>, default: Option<u64>) -> Option<u64> {
    match raw {
        None => default,
        Some(v) => v.trim().parse::<u64>().ok().filter(|&n| n > 0).or(default),
    }
}

/// Total system RAM in MiB. Linux additionally respects a cgroup v2
/// `memory.max` below the host total (container / toolbox deployments
/// see their real ceiling, not the host's). `None` on detection
/// failure — callers fall back to the legacy posture.
fn total_system_ram_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // /proc/meminfo "MemTotal:  131072000 kB"
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let host_mb = meminfo.lines().find_map(|l| {
            let rest = l.strip_prefix("MemTotal:")?;
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            Some(kb / 1024)
        })?;
        // cgroup v2 unified hierarchy; "max" = unlimited. Best-effort —
        // absent/unparseable just means the host total stands.
        let cgroup_mb = std::fs::read_to_string("/sys/fs/cgroup/memory.max")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(|bytes| bytes / (1024 * 1024));
        Some(match cgroup_mb {
            Some(limit) if limit < host_mb => limit,
            _ => host_mb,
        })
    }
    #[cfg(target_os = "macos")]
    {
        // sysctl hw.memsize (bytes). SAFETY: fixed-size out-param with
        // its size passed alongside; the call writes at most `len` bytes.
        let mut bytes: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        let name = std::ffi::CString::new("hw.memsize").expect("static name");
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                &mut bytes as *mut _ as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || bytes == 0 {
            return None;
        }
        Some(bytes / (1024 * 1024))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
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

/// The watch-loop body, exposed so the daemon can run it under
/// `supervise::spawn_supervised` — a panic here used to silently
/// disarm the OOM defense for the rest of the process's life
/// (DAEMON_RESILIENCE.md P0.4). Limits are re-read on entry, so a
/// supervised restart re-arms with a fresh "armed" log line.
pub async fn watch_loop(interval: Duration) {
    let soft = soft_limit_mb();
    let hard = hard_limit_mb();
    let total_ram = total_system_ram_mb();
    tracing::info!(
        soft_limit_mb = soft,
        hard_limit_mb = hard,
        total_ram_mb = total_ram,
        soft_from_env = std::env::var_os("SOVEREIGN_RSS_SOFT_LIMIT_MB").is_some(),
        hard_from_env = std::env::var_os("SOVEREIGN_RSS_HARD_LIMIT_MB").is_some(),
        interval_secs = interval.as_secs(),
        "memory-watch: armed"
    );
    if hard.is_none() {
        // Grep-stable breadcrumb: a daemon running WITHOUT the OOM
        // defense should say so once at boot. This is the DEFAULT for an
        // unsupervised daemon (the self-SIGTERM only helps when something
        // relaunches). Soft-warn still fires; kernel OOM/jetsam would
        // SIGKILL with no drain. Enable under a supervisor with
        // SOVEREIGN_RSS_HARD_LIMIT_MB=<mb> or =auto.
        tracing::info!(
            "memory-watch: hard limit disabled (default; unsupervised daemon). \
             Set SOVEREIGN_RSS_HARD_LIMIT_MB=<mb>|auto to enable under a supervisor \
             that relaunches on exit."
        );
    }
    if let Some(hard_mb) = hard {
        if soft >= hard_mb {
            tracing::warn!(
                soft_limit_mb = soft,
                hard_limit_mb = hard_mb,
                "memory-watch: soft limit >= hard limit (env override inversion) — \
                 soft warnings will never fire before the hard restart"
            );
        }
    }
    {
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
    }
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
    }

    #[test]
    fn hard_limit_defaults_off_unless_opted_in() {
        let ram = Some(100_000);
        // Default OFF: unset env leaves the hard limit disabled, even with
        // detectable RAM — a self-SIGTERM only helps under a supervisor.
        assert_eq!(hard_limit_policy(None, ram), None);
        // Garbage env is OFF too — no accidental enable via derivation.
        assert_eq!(hard_limit_policy(Some("nope"), ram), None);
        // Opt in at the RAM-derived default via `auto`.
        assert_eq!(
            hard_limit_policy(Some("auto"), ram),
            Some(100_000 * HARD_PCT / 100)
        );
        assert_eq!(
            hard_limit_policy(Some("AUTO"), ram),
            Some(100_000 * HARD_PCT / 100)
        );
        // Explicit MB value opts in at that value.
        assert_eq!(hard_limit_policy(Some("30000"), ram), Some(30_000));
        // Explicit kill-switch stays disabled.
        assert_eq!(hard_limit_policy(Some("0"), ram), None);
        assert_eq!(hard_limit_policy(Some("off"), ram), None);
        assert_eq!(hard_limit_policy(Some(" OFF "), ram), None);
        // RAM undetectable: `auto` can't derive → disabled; explicit still works.
        assert_eq!(hard_limit_policy(Some("auto"), None), None);
        assert_eq!(hard_limit_policy(Some("30000"), None), Some(30_000));
        assert_eq!(hard_limit_policy(None, None), None);
    }

    #[test]
    fn derived_limits_track_platform_percentages() {
        assert_eq!(derived_soft_limit_mb(Some(100_000)), Some(SOFT_PCT * 1000));
        assert_eq!(derived_hard_limit_mb(Some(100_000)), Some(HARD_PCT * 1000));
        assert_eq!(derived_soft_limit_mb(None), None);
        // Soft must sit below hard by construction so the warn ladder
        // fires before the restart.
        assert!(SOFT_PCT < HARD_PCT);
    }

    #[test]
    fn total_ram_detects_on_supported_platforms() {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let ram = total_system_ram_mb().expect("total RAM detectable");
            assert!(ram >= 1024, "implausibly small RAM: {ram} MiB");
            assert!(ram < 16 * 1024 * 1024, "implausibly large RAM: {ram} MiB");
        }
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
        assert!(g.observe(300, 200, t0 + REWARN_EVERY + Duration::from_secs(1)));
        // re-cross → warn immediately
    }

    #[test]
    fn current_rss_reports_something_plausible() {
        // We're a running process — RSS must be nonzero and sane.
        let rss = current_rss_mb().expect("rss sample");
        assert!(rss > 0, "rss should be nonzero");
        assert!(rss < 1_048_576, "rss under a TiB sanity bound");
    }
}
