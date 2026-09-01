// SPDX-License-Identifier: AGPL-3.0-or-later
//! A guest link this node has ACCEPTED — the holder's side of a grant.
//!
//! `svrn mesh use <sovereign://guest/…>` writes `guest.json`; two very
//! different consumers read it, which is why the type lives here rather than
//! in either of them:
//!
//! - the **CLI**, to point `svrn chat` at the lender, and
//! - the **daemon**, to resolve a granted model id to the lending node when a
//!   turn runs locally (`sovereign_mesh::guest_lender`).
//!
//! The daemon half is the reason for the move. `svrn chat ask` is a surface —
//! the turn runs on the daemon — so a guest's CONVERSATION must stay on their
//! own machine while only the completion crosses. That means the daemon needs
//! the link, and a daemon cannot depend on a CLI crate.
//!
//! # What is deliberately not here
//!
//! No scope. The link carries the minimum — token, where, until when, one
//! display string — and the ISSUING node's store is the only authority on what
//! it buys. A scope cached here would be a second answer to that question and
//! would go stale the moment the lender revoked (§10.6).
//!
//! `summary` is display only, for the same reason. Never branch on it.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Filename under the sovereign root, sibling of `node_key` / `client-token`.
pub const GUEST_LINK_FILE: &str = "guest.json";

/// A guest link this node has accepted.
///
/// The persisted form of `sovereign_mesh::deep_link::DeepLink::Guest`, minus
/// nothing: what the link carried is exactly what is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestLink {
    /// The bearer, presented verbatim as `Authorization: Bearer <token>`.
    pub token: String,
    /// Base URL of the issuing node's client API — no trailing `/v1`.
    ///
    /// Where the bearer goes when [`Self::dial`] is absent; otherwise
    /// provenance and display only. Never read directly to build a request.
    pub url: String,
    /// The lender's iroh dial string, when the plaintext API is not the way
    /// in. `#[serde(default)]` so a `guest.json` written before this field
    /// existed still reads as "no tunnel" rather than as a corrupt file.
    #[serde(default)]
    pub dial: Option<String>,
    /// Unix SECONDS at which the grant lapses (the link's `exp=` param).
    pub expires_at: u64,
    /// Display only. What the minting node said this buys. Never consulted
    /// for a decision — see the module docs.
    #[serde(default)]
    pub summary: Option<String>,
}

impl GuestLink {
    /// Whether the link's own stated window is still open.
    ///
    /// **This is necessary and not sufficient.** The lender holds grants in
    /// memory (`commonwealth_knowledge::guest_grant`), so a restart on their
    /// side ends the grant early and the only way to learn that is to be
    /// refused. A caller must treat a 401 as authoritative over this.
    pub fn is_live(&self, now_secs: u64) -> bool {
        now_secs < self.expires_at
    }

    /// Seconds left, or `None` once lapsed.
    pub fn remaining_secs(&self, now_secs: u64) -> Option<u64> {
        self.expires_at.checked_sub(now_secs).filter(|s| *s > 0)
    }
}

/// Unix seconds. One reader of the clock so the CLI and the daemon cannot
/// disagree about what "now" is.
pub fn now_secs() -> u64 {
    crate::time::unix_now_u64()
}

/// Where `guest.json` lives under a sovereign root.
pub fn path_in(root: &Path) -> PathBuf {
    root.join(GUEST_LINK_FILE)
}

/// Write `guest.json` 0600, tmp-then-rename so a crash mid-write cannot leave
/// a half-parsed credential behind.
pub fn save_in(root: &Path, link: &GuestLink) -> io::Result<()> {
    std::fs::create_dir_all(root)?;
    let target = path_in(root);
    let tmp = target.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(link)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, body.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &target)?;
    Ok(())
}

/// Read the stored link, whatever its expiry. `None` for absent OR
/// unparseable — and an unparseable one is TRACED rather than passed off as
/// absent, because the two have different repairs (`mesh use` again vs.
/// `mesh use --forget`).
pub fn load_in(root: &Path) -> Option<GuestLink> {
    let path = path_in(root);
    let raw = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<GuestLink>(&raw) {
        Ok(link) => Some(link),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "guest link exists but will not parse — ignoring it \
                 (`svrn mesh use --forget` clears it)"
            );
            None
        }
    }
}

/// Present AND within its stated window. Silent: the CLI renders its own
/// message for the expired case, and the daemon must not print to stderr.
pub fn load_live_in(root: &Path, now_secs: u64) -> Option<GuestLink> {
    load_in(root).filter(|l| l.is_live(now_secs))
}

/// Drop the stored link. `Ok(false)` when there was nothing to drop —
/// absence is not an error, so `--forget` is safe to run twice.
pub fn forget_in(root: &Path) -> io::Result<bool> {
    let path = path_in(root);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(expires_at: u64) -> GuestLink {
        GuestLink {
            token: "t".into(),
            url: "http://lender:9741".into(),
            dial: None,
            expires_at,
            summary: Some("a-model".into()),
        }
    }

    #[test]
    fn a_round_trip_preserves_every_field() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = link(9_999);
        l.dial = Some("abc@relay".into());
        save_in(dir.path(), &l).unwrap();
        assert_eq!(load_in(dir.path()).unwrap(), l);
    }

    /// A `guest.json` written before `dial` existed must read as "no tunnel",
    /// not as a corrupt file — the difference is a working link and a link
    /// the holder is told to re-request.
    #[test]
    fn a_link_without_dial_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            path_in(dir.path()),
            r#"{"token":"t","url":"http://lender:9741","expires_at":9999}"#,
        )
        .unwrap();
        let l = load_in(dir.path()).unwrap();
        assert!(l.dial.is_none());
        assert!(l.summary.is_none());
    }

    /// Unparseable is NOT absent. Both return `None` here, but the trace is
    /// what tells the two apart, and a silent drop would send the holder to
    /// the wrong repair.
    #[test]
    fn an_unparseable_link_is_none_rather_than_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path_in(dir.path()), "{ not json").unwrap();
        assert!(load_in(dir.path()).is_none());
    }

    #[test]
    fn load_live_refuses_an_expired_link_that_load_still_returns() {
        let dir = tempfile::tempdir().unwrap();
        save_in(dir.path(), &link(100)).unwrap();
        assert!(load_in(dir.path()).is_some(), "still on disk");
        assert!(load_live_in(dir.path(), 101).is_none(), "but not live");
        assert!(load_live_in(dir.path(), 99).is_some());
    }

    #[test]
    fn remaining_secs_is_none_at_the_boundary_not_zero() {
        assert_eq!(link(100).remaining_secs(40), Some(60));
        assert_eq!(link(100).remaining_secs(100), None);
        assert_eq!(link(100).remaining_secs(101), None);
    }

    #[test]
    fn forget_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        save_in(dir.path(), &link(9_999)).unwrap();
        assert!(forget_in(dir.path()).unwrap(), "first removes");
        assert!(
            !forget_in(dir.path()).unwrap(),
            "second is a no-op, not an error"
        );
    }
}
