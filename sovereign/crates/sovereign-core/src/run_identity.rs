// SPDX-License-Identifier: AGPL-3.0-or-later
//! The process generation, as a key for everything it logs.
//!
//! `daemon.err` is opened append-only on purpose, so generations concatenate
//! — and until 2026-09-02 no line in it carried anything that identified the
//! process that wrote it: not the binary, not its mtime, not a run id. Two
//! investigations that day nearly drew wrong conclusions from that file, one
//! from a stale binary and one from a stale line of a previous generation
//! (`concurrency=4`, impossible on the host being measured). A measurement
//! without a key is not attributable (ARCH §7.5, §18.4).
//!
//! `run_id` is minted once per process and is meant to appear on the daemon's
//! startup banner AND on the events an investigation joins against it; the
//! build fields say which binary this generation actually is.

use std::sync::OnceLock;

/// Eight hex characters, unique per process start. Short enough to grep,
/// long enough that two generations in one log do not collide.
pub fn run_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| uuid::Uuid::new_v4().simple().to_string()[..8].to_string())
}

/// Which binary this process is, and when it was built.
#[derive(Debug, Clone)]
pub struct BuildIdentity {
    pub pid: u32,
    /// `current_exe()`, or the reason it could not be read.
    pub exe: String,
    /// The binary's mtime as RFC 3339 UTC — the field that tells a stale
    /// build from a fresh one. `None` when the metadata could not be read.
    pub exe_mtime: Option<String>,
}

pub fn build() -> &'static BuildIdentity {
    static B: OnceLock<BuildIdentity> = OnceLock::new();
    B.get_or_init(|| {
        let exe_path = std::env::current_exe();
        let exe_mtime = exe_path
            .as_ref()
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());
        BuildIdentity {
            pid: std::process::id(),
            exe: match exe_path {
                Ok(p) => p.display().to_string(),
                Err(e) => format!("<unreadable: {e}>"),
            },
            exe_mtime,
        }
    })
}

/// True when the binary on disk changed AFTER this process captured its
/// build identity — i.e. the running process is older than the file it
/// was loaded from. `build()` is initialized at daemon startup (the
/// banner), so its `exe_mtime` is the file AS LOADED; a newer mtime means
/// a build landed under a live process and every verb it serves executes
/// stale code while the tree — and `doctor` — say fresh. Measured cost of
/// that trap on 2026-09-02: six solve attempts served by a pre-dawn
/// daemon. Callers REFUSE with the repair (`sovereign daemon restart`);
/// `SOVEREIGN_ALLOW_STALE_SOLVE=1` opts out for deliberate archaeology.
pub fn exe_rebuilt_since_start() -> bool {
    if std::env::var("SOVEREIGN_ALLOW_STALE_SOLVE").as_deref() == Ok("1") {
        return false;
    }
    let b = build();
    let Some(started) = &b.exe_mtime else {
        return false;
    };
    let Ok(started) = chrono::DateTime::parse_from_rfc3339(started) else {
        return false;
    };
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let Ok(meta) = std::fs::metadata(&exe) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    chrono::DateTime::<chrono::Utc>::from(modified) > started
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_id_is_stable_within_a_process_and_eight_hex_chars() {
        let a = run_id();
        let b = run_id();
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn the_build_identity_names_this_binary() {
        let b = build();
        assert_eq!(b.pid, std::process::id());
        assert!(!b.exe.is_empty());
        assert!(
            b.exe_mtime.is_some(),
            "the test binary exists, so its mtime is readable"
        );
    }

    /// In a test process the identity is captured at first use from the
    /// same file that is still on disk — nothing has rebuilt under us —
    /// so the detector must read false. A true here would refuse every
    /// verb the daemon serves (the caller treats true as stale).
    #[test]
    fn a_process_is_not_stale_against_its_own_binary() {
        assert!(!exe_rebuilt_since_start());
    }
}
