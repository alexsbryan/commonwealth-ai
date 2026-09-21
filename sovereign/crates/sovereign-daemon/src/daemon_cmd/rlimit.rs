// SPDX-License-Identifier: AGPL-3.0-or-later
//! Process resource limits the daemon raises for itself at start.

/// Raise the open-file soft limit to what the hard limit allows.
///
/// A daemon spawned from launchd or `daemon start` inherits macOS's 256
/// (`launchctl limit maxfiles`), and a hybrid LanceDB search over a 2M-row
/// table holds dozens of fragment and index files per reader. Measured
/// 2026-09-02 (G5b, issue #57): three concurrent claim searches over
/// `wikipedia` while the newsworthy tick committed to it failed 9 of 17
/// searches with "Too many open files (os error 24)" — each one a claim
/// the gate could not judge. Both outcomes are logged so the limit in
/// force is never a guess.
#[cfg(unix)]
pub(super) fn raise_open_file_limit() {
    // SAFETY: plain libc calls on a stack-local rlimit struct.
    unsafe {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            tracing::warn!("open-file limit: getrlimit failed; leaving it alone");
            return;
        }
        // macOS refuses a soft limit above OPEN_MAX (10240) even when the
        // hard limit reads "unlimited"; try the larger first, then that.
        for want in [65_536u64, 10_240] {
            let want = (want as libc::rlim_t).min(lim.rlim_max);
            if want <= lim.rlim_cur {
                break;
            }
            let new = libc::rlimit {
                rlim_cur: want,
                rlim_max: lim.rlim_max,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &new) == 0 {
                tracing::info!(from = lim.rlim_cur, to = want, "open-file limit raised");
                return;
            }
        }
        tracing::warn!(
            soft = lim.rlim_cur,
            hard = lim.rlim_max,
            "open-file limit NOT raised — concurrent corpus searches may fail with EMFILE"
        );
    }
}
