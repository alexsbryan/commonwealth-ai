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

    /// Same hysteresis, inverted: fires when `value` falls BELOW `floor`.
    ///
    /// Headroom is a floor, not a ceiling — a separate method rather than a
    /// clever argument inversion at the call site, because
    /// `observe(floor.saturating_sub(avail), 0)` is correct and unreadable,
    /// and a guard nobody can read is a guard nobody maintains.
    fn observe_below(&mut self, value: u64, floor: u64, now: Instant) -> bool {
        self.observe(floor.saturating_sub(value), 0, now)
    }
}

// ── Host headroom: the resource that actually runs out ──────────────────
//
// The RSS limits above guard THIS PROCESS's footprint. On 2026-08-25 that was
// measured to be the wrong noun on the host it was written for (note
// `05cbffed`): six kernel OOM kills in 24 hours, and the guard was silent
// through every one. The numbers leave no room for interpretation — the kernel
// killed at anon-rss 36.4 GiB while steady state under judging was 36.0 GiB, so
// there was no separation to fire in, and `=auto` resolves to 85% of RAM
// (~108 GB) which the process never approaches.
//
// The exhausted resource was GTT — GPU-mapped host memory, 46.4 GB in the
// daemon plus 12.55 GB in an unrelated `gnome-shell` — and RSS does not count
// it. So a process can sit at a modest RSS while the BOX has nothing left,
// which is the exact state the kernel resolves by killing this daemon: it
// carries `oom_score_adj:200`, making it the most killable process present.
//
// A guard that cannot fail is worse than none, because it is read as coverage
// (ARCH §18.1). What follows samples what actually runs out.

/// Latest sampled host headroom in MiB. 0 = not yet sampled.
static LATEST_HEADROOM_MB: AtomicU64 = AtomicU64::new(0);

pub fn latest_headroom_mb() -> Option<u64> {
    match LATEST_HEADROOM_MB.load(Ordering::Relaxed) {
        0 => None,
        v => Some(v),
    }
}

/// Absolute floor under the RAM-derived headroom warning, so a small host does
/// not warn continuously about a headroom level that is normal for it.
const HEADROOM_FLOOR_MB: u64 = 4096;

/// Fraction of RAM below which headroom is worth saying out loud.
///
/// 15% is derived from the measurement, not chosen: the pre-restart daemon sat
/// at 19.1 GB MemAvailable on a 125 GB box (15.3%) and that is the regime the
/// kills happened in — so a warning at 18.75 GB fires while there is still
/// room to act, and a lower threshold would only announce the OOM.
const HEADROOM_SOFT_PCT: u64 = 15;

/// Warn threshold for host headroom. Always on: unlike a self-SIGTERM, saying
/// "the box is running out" costs nothing and is the signal that was missing
/// through all six kills.
pub fn headroom_soft_mb() -> u64 {
    let derived = derived_headroom_soft_mb(total_system_ram_mb());
    parse_limit_mb(
        std::env::var("SOVEREIGN_HEADROOM_SOFT_MB").ok().as_deref(),
        Some(derived),
    )
    .unwrap_or(derived)
}

/// Pure derivation, so the threshold is testable without a host.
fn derived_headroom_soft_mb(total_ram_mb: Option<u64>) -> u64 {
    total_ram_mb
        .map(|ram| (ram * HEADROOM_SOFT_PCT / 100).max(HEADROOM_FLOOR_MB))
        .unwrap_or(HEADROOM_FLOOR_MB)
}

/// Hard floor: graceful self-restart when host headroom falls below it.
///
/// **Default OFF**, for the same reason the RSS hard limit is: a self-SIGTERM
/// only helps under a supervisor that relaunches. This is NOT the inert-guard
/// problem above — that guard measured a quantity that could not reach its
/// threshold; this one is opt-in but fires on the quantity that does.
pub fn headroom_hard_mb() -> Option<u64> {
    headroom_hard_policy(
        std::env::var("SOVEREIGN_HEADROOM_HARD_MB").ok().as_deref(),
        total_system_ram_mb(),
    )
}

/// Pure policy, mirroring [`hard_limit_policy`]'s precedence so the two hard
/// limits cannot drift into disagreeing about what `off` or garbage means.
fn headroom_hard_policy(raw: Option<&str>, total_ram_mb: Option<u64>) -> Option<u64> {
    match raw.map(str::trim) {
        None | Some("0") | Some("off") | Some("OFF") => None,
        Some(v) if v.eq_ignore_ascii_case("auto") => {
            total_ram_mb.map(|ram| (ram * HEADROOM_SOFT_PCT / 200).max(HEADROOM_FLOOR_MB / 2))
        }
        Some(v) => v.parse::<u64>().ok().filter(|&n| n > 0),
    }
}

/// Host memory headroom in MiB — what is left for EVERYONE, not what this
/// process holds.
///
/// Reports absence rather than a guess when the platform has no answer
/// (ARCH §18.3): a headroom of `None` must never be read as a headroom of 0,
/// which would trip the floor on every tick.
fn host_available_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // `MemAvailable` is the kernel's own estimate of what a new allocation
        // can get without swapping — reclaimable page cache included. That is
        // the right number precisely because it is NOT `MemFree`: most of a
        // healthy box's RAM is cache, and a free-only reading would fire
        // continuously on a machine that is perfectly fine.
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in meminfo.lines() {
            if let Some(rest) = line.strip_prefix("MemAvailable:") {
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb / 1024);
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        // No MemAvailable equivalent. free + inactive + purgeable is the
        // closest honest analogue: those are the pages the pager hands to a
        // new allocation before it starts compressing.
        let mut count: libc::mach_msg_type_number_t = (std::mem::size_of::<libc::vm_statistics64>()
            / std::mem::size_of::<libc::integer_t>())
            as libc::mach_msg_type_number_t;
        let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
        // SAFETY: out-struct is zeroed and `count` is its size in integer_t
        // units, which is what host_statistics64 expects.
        let rc = unsafe {
            libc::host_statistics64(
                libc::mach_host_self(),
                libc::HOST_VM_INFO64,
                &mut stats as *mut _ as *mut libc::integer_t,
                &mut count,
            )
        };
        if rc != libc::KERN_SUCCESS {
            return None;
        }
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page <= 0 {
            return None;
        }
        let pages =
            stats.free_count as u64 + stats.inactive_count as u64 + stats.purgeable_count as u64;
        Some(pages * page as u64 / (1024 * 1024))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
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
        headroom_soft_mb = headroom_soft_mb(),
        headroom_hard_mb = headroom_hard_mb(),
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
        let mut headroom_gate = WarnGate::new();
        let headroom_soft = headroom_soft_mb();
        let headroom_hard = headroom_hard_mb();
        loop {
            ticker.tick().await;

            // Host headroom FIRST: it is the quantity that actually runs out,
            // and it is sampled even when the RSS read fails so a box in
            // trouble still says so.
            if let Some(avail_mb) = tokio::task::spawn_blocking(host_available_mb)
                .await
                .ok()
                .flatten()
            {
                LATEST_HEADROOM_MB.store(avail_mb, Ordering::Relaxed);
                if let Some(floor) = headroom_hard {
                    if avail_mb < floor {
                        tracing::error!(
                            headroom_mb = avail_mb,
                            headroom_hard_mb = floor,
                            rss_mb = latest_rss_mb(),
                            "memory-watch: HOST HEADROOM below floor — initiating graceful \
                             restart. NOTE: this is not an accusation. Headroom is shared, \
                             the daemon may not be what consumed it, and it exits because \
                             it is the process that can come back cleanly"
                        );
                        HARD_EXIT_REQUESTED.store(true, Ordering::SeqCst);
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
                if headroom_gate.observe_below(avail_mb, headroom_soft, Instant::now()) {
                    tracing::warn!(
                        headroom_mb = avail_mb,
                        headroom_soft_mb = headroom_soft,
                        rss_mb = latest_rss_mb(),
                        "memory-watch: host memory headroom low — the BOX is running out, \
                         which is what the kernel OOM killer resolves by killing this \
                         daemon (oom_score_adj:200). Not necessarily this process's doing"
                    );
                }
            }

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

    // ── host headroom ──────────────────────────────────────────────────

    /// THE NAMED FAILING INPUT (ARCH §18.1) — the state the old guard slept
    /// through, replayed as an assertion.
    ///
    /// Measured on the Halo 2026-08-25 (note `05cbffed`): 125 GB box, daemon
    /// at 36.4 GiB anon-rss when the kernel killed it, MemAvailable down to
    /// 19.1 GB. The RSS hard limit at `auto` was ~108 GB, so it could not
    /// fire — and its steady state was 36.0 GiB, so no explicit value could
    /// have separated normal from fatal either. A headroom warning at 15% of
    /// RAM is 18.75 GB, which is inside that regime.
    #[test]
    fn the_headroom_warning_fires_where_the_rss_limit_could_not() {
        const HALO_RAM_MB: u64 = 125 * 1024;
        let soft = derived_headroom_soft_mb(Some(HALO_RAM_MB));

        // The RSS guard, for contrast. Asserted against the process size the
        // kernel actually killed at (36.4 GiB) rather than against a literal
        // limit: HARD_PCT is platform-conditional (85% Linux / 65% macOS), so
        // a hardcoded expectation passes on one host and fails on the other
        // while testing nothing. The defect is the RATIO — the limit sits
        // multiples above where death occurs, on either platform.
        const OBSERVED_KILL_RSS_MB: u64 = 37_274; // 36.4 GiB
        let rss_auto = hard_limit_policy(Some("auto"), Some(HALO_RAM_MB)).unwrap();
        assert!(
            rss_auto > OBSERVED_KILL_RSS_MB * 2,
            "the RSS auto limit ({rss_auto} MiB) sits more than 2x above the \
             {OBSERVED_KILL_RSS_MB} MiB the process was at when the kernel killed \
             it — which is why it never fired"
        );

        // The headroom guard fires in the observed pre-kill regime.
        let observed_pre_kill_mb = 19_100;
        assert!(
            observed_pre_kill_mb < soft,
            "headroom soft ({soft} MiB) must be above the 19.1 GB the box was \
             actually at before the kills, or this guard is inert too"
        );
        // …and stays quiet in the healthy regime measured after the restart.
        let observed_post_restart_mb = 48_700;
        assert!(
            observed_post_restart_mb > soft,
            "a healthy box (48.7 GB free after restart) must not warn"
        );
    }

    /// A small host must not warn continuously about a headroom level that is
    /// normal for it — 15% of 16 GB is 2.4 GB, which a laptop lives at.
    #[test]
    fn a_small_host_uses_the_absolute_floor() {
        assert_eq!(derived_headroom_soft_mb(Some(16 * 1024)), HEADROOM_FLOOR_MB);
        assert_eq!(derived_headroom_soft_mb(None), HEADROOM_FLOOR_MB);
        // A large host uses the percentage, not the floor.
        assert!(derived_headroom_soft_mb(Some(125 * 1024)) > HEADROOM_FLOOR_MB);
    }

    /// The hard floor is OFF unless asked for, and agrees with the RSS hard
    /// limit about what `off`/garbage mean — two guards that disagree about
    /// their own kill-switch is the §10.6 smell.
    #[test]
    fn the_headroom_hard_floor_is_off_by_default() {
        let ram = Some(125 * 1024);
        for raw in [None, Some("0"), Some("off"), Some("OFF"), Some("banana")] {
            assert_eq!(
                headroom_hard_policy(raw, ram),
                None,
                "{raw:?} must leave the floor disabled"
            );
            assert_eq!(
                hard_limit_policy(raw, ram),
                None,
                "{raw:?} — RSS side agrees"
            );
        }
        assert!(headroom_hard_policy(Some("auto"), ram).is_some());
        assert_eq!(headroom_hard_policy(Some("2048"), ram), Some(2048));
    }

    /// `observe_below` is the hysteresis the warning rides: one line on the
    /// downward crossing, silence while it stays low, and re-arm on recovery.
    #[test]
    fn observe_below_fires_on_the_downward_crossing_only() {
        let mut g = WarnGate::new();
        let t0 = Instant::now();
        assert!(!g.observe_below(20_000, 18_000, t0), "healthy — silent");
        assert!(
            g.observe_below(17_000, 18_000, t0),
            "crossed down — warn once"
        );
        assert!(!g.observe_below(16_000, 18_000, t0), "still low — no spam");
        assert!(!g.observe_below(19_000, 18_000, t0), "recovered — silent");
        assert!(
            g.observe_below(17_500, 18_000, t0),
            "crossed down again after recovery — warn again"
        );
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
