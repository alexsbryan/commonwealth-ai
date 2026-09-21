// SPDX-License-Identifier: AGPL-3.0-or-later
//! Memory-limit POLICY — the soft-limit formula `svrn doctor` judges the
//! daemon's reported RSS against (`checks_sovereign::check_daemon_memory`).
//!
//! The daemon-side sampler — the 60s watch loop, the hard-limit
//! self-SIGTERM, host-headroom sampling, arena trimming — moved out with
//! the run body at the de-embed (docs/FIVE_PROGRAMS.md §11 step 10) and
//! lives in `sovereign-daemon`'s fork of this module. What stays is the
//! read doctor needs: "what would the soft warn limit be on this host".
//!
//! NAMED DUPLICATION: the formula below is the same one the daemon
//! applies (its fork carries the original). Two copies can drift; the
//! honest closure is doctor reading the limit from the daemon's
//! `/status` instead of re-deriving it, which is a daemon-surface
//! change outside this cut. Until then, change both or neither.

/// RAM-fraction default (percent). macOS sits well under the observed
/// jetsam trigger zone (~69% of RAM); Linux leaves headroom for the
/// rest of the system before the kernel OOM killer engages.
const SOFT_PCT: u64 = if cfg!(target_os = "macos") { 50 } else { 70 };

/// Legacy fallback when total RAM cannot be detected: the historical
/// default (soft 20 GiB).
const LEGACY_SOFT_MB: u64 = 20_480;

/// Soft warn threshold. Env-overridable; default 70% (Linux) / 50%
/// (macOS) of total RAM, legacy 20 GiB when RAM is undetectable.
pub(crate) fn soft_limit_mb() -> u64 {
    let default = derived_soft_limit_mb(total_system_ram_mb()).unwrap_or(LEGACY_SOFT_MB);
    parse_limit_mb(
        std::env::var("SOVEREIGN_RSS_SOFT_LIMIT_MB").ok().as_deref(),
        Some(default),
    )
    .unwrap_or(default)
}

fn derived_soft_limit_mb(total_ram_mb: Option<u64>) -> Option<u64> {
    total_ram_mb.map(|ram| ram * SOFT_PCT / 100)
}

/// Explicit env > default; unset/garbage/zero fall back to the default —
/// a typo must not silently switch the limit off.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_parse_policy() {
        // default applies on unset/garbage/zero; an explicit value wins
        assert_eq!(parse_limit_mb(None, Some(20_480)), Some(20_480));
        assert_eq!(parse_limit_mb(Some("nope"), Some(20_480)), Some(20_480));
        assert_eq!(parse_limit_mb(Some("0"), Some(20_480)), Some(20_480));
        assert_eq!(parse_limit_mb(Some("4096"), Some(20_480)), Some(4_096));
    }

    #[test]
    fn derived_soft_limit_tracks_platform_percentage() {
        assert_eq!(derived_soft_limit_mb(Some(100_000)), Some(SOFT_PCT * 1_000));
        assert_eq!(derived_soft_limit_mb(None), None);
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
}
