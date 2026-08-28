// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest half of an ephemeral mesh link: `<root>/guest.json`.
//!
//! `svrn mesh use <sovereign://guest/…>` writes this file; `svrn chat`
//! consults it and, when it holds a live link, points its daemon base and its
//! `Authorization: Bearer` at the issuing node instead of the local daemon.
//!
//! # Why a file and not a flag
//!
//! The alternative is `svrn chat ask --daemon http://host:9741 --token …` on
//! every invocation. That is not the shape of the feature: a guest was *lent*
//! a machine for a bounded window, and the window — not the invocation — is
//! what should decide where the request goes. Persisting once and expiring on
//! its own is what makes "for two hours" true without anyone remembering it.
//!
//! # Expiry is reported, never assumed away
//!
//! [`load_live`] returns `None` for an expired link **and says so on stderr**.
//! Silently falling back to the local daemon would answer the guest's question
//! with a different machine's model and never mention it — the §18.3 failure
//! this whole feature was built to avoid. The ISSUING NODE is still the
//! authority (its store rejects an expired token regardless); this check only
//! spares a round-trip and produces a message that names the real cause.
//!
//! # 0600, tmp-then-rename
//!
//! The file holds a live bearer. Same posture as its siblings under the same
//! root — `node_key`, `client-token`, `join_key.secret`
//! (`commonwealth_transport::identity`).

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use sovereign_cli_shared::dirs::sovereign_root;

/// Filename under the sovereign root, sibling of `node_key` / `client-token`.
pub const GUEST_LINK_FILE: &str = "guest.json";

/// A guest link this node has accepted.
///
/// The persisted form of [`sovereign_mesh::deep_link::DeepLink::Guest`], minus
/// nothing: what the link carried is exactly what is stored, because the link
/// deliberately carries the minimum (token, where, until when, one display
/// string). There is no scope here for the same reason there is none on the
/// wire — the issuing node's store is the only authority on what this buys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestLink {
    /// The bearer, presented verbatim as `Authorization: Bearer <token>`.
    pub token: String,
    /// Base URL of the issuing node's client API — no trailing `/v1`.
    pub url: String,
    /// Unix SECONDS at which the grant lapses (the link's `exp=` param).
    pub expires_at: u64,
    /// Display only. What the minting node said this buys. Never consulted for
    /// a decision — see the type docs.
    #[serde(default)]
    pub summary: Option<String>,
}

impl GuestLink {
    pub fn is_live(&self, now_secs: u64) -> bool {
        now_secs < self.expires_at
    }

    /// Seconds left, or `None` once lapsed. Rendered by `mesh use` and by the
    /// stderr banner `svrn chat` prints when it routes through a guest link —
    /// a guest should never be surprised by the window closing.
    pub fn remaining_secs(&self, now_secs: u64) -> Option<u64> {
        self.expires_at.checked_sub(now_secs).filter(|&s| s > 0)
    }
}

/// Wall clock in unix seconds. One definition, so the store and its callers
/// cannot disagree about what "now" is.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn path() -> PathBuf {
    path_in(&sovereign_root())
}

pub fn path_in(root: &Path) -> PathBuf {
    root.join(GUEST_LINK_FILE)
}

pub fn save(link: &GuestLink) -> io::Result<()> {
    save_in(&sovereign_root(), link)
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

pub fn load() -> Option<GuestLink> {
    load_in(&sovereign_root())
}

/// Read the stored link, whatever its expiry. `None` for absent OR unparseable
/// — and an unparseable one is named on stderr rather than passed off as
/// absent, because the two have different repairs (`mesh use` again vs. `mesh
/// use --forget`).
pub fn load_in(root: &Path) -> Option<GuestLink> {
    let path = path_in(root);
    let raw = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<GuestLink>(&raw) {
        Ok(link) => Some(link),
        Err(e) => {
            eprintln!(
                "warning: {} exists but will not parse ({e}) — ignoring it. \
                 Run `svrn mesh use --forget` to clear it.",
                path.display()
            );
            None
        }
    }
}

pub fn load_live(now_secs: u64) -> Option<GuestLink> {
    load_live_in(&sovereign_root(), now_secs)
}

/// The accessor every consumer should use: a link that is present AND live.
/// An expired one returns `None` **loudly** — see the module docs.
pub fn load_live_in(root: &Path, now_secs: u64) -> Option<GuestLink> {
    let link = load_in(root)?;
    if link.is_live(now_secs) {
        return Some(link);
    }
    eprintln!(
        "The guest link for {} expired. Ask for a fresh one (`svrn mesh grant` \
         on their side), or `svrn mesh use --forget` to go back to your own daemon.",
        link.url
    );
    None
}

pub fn forget() -> io::Result<bool> {
    forget_in(&sovereign_root())
}

/// Drop the stored link. `Ok(false)` when there was nothing to drop — absence
/// is not an error, and `mesh use --forget` on a clean node must not fail.
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

    fn link(exp: u64) -> GuestLink {
        GuestLink {
            token: "deadbeef".into(),
            url: "http://box:9741".into(),
            expires_at: exp,
            summary: Some("models: big-27b".into()),
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        save_in(dir.path(), &link(9_000)).unwrap();
        assert_eq!(load_in(dir.path()).unwrap(), link(9_000));
    }

    #[test]
    fn an_expired_link_is_not_live() {
        let dir = tempfile::tempdir().unwrap();
        save_in(dir.path(), &link(100)).unwrap();
        // Present on disk...
        assert!(load_in(dir.path()).is_some());
        // ...and still refused, rather than quietly used.
        assert!(load_live_in(dir.path(), 101).is_none());
        assert!(load_live_in(dir.path(), 99).is_some());
    }

    #[test]
    fn remaining_goes_none_at_the_boundary() {
        let l = link(100);
        assert_eq!(l.remaining_secs(40), Some(60));
        assert_eq!(l.remaining_secs(100), None);
        assert_eq!(l.remaining_secs(101), None);
    }

    #[test]
    fn forget_is_idempotent_and_absence_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!forget_in(dir.path()).unwrap());
        save_in(dir.path(), &link(9_000)).unwrap();
        assert!(forget_in(dir.path()).unwrap());
        assert!(!forget_in(dir.path()).unwrap());
        assert!(load_in(dir.path()).is_none());
    }

    /// A corrupt file must not read as "no link" without saying so, and must
    /// not read as a link either.
    #[test]
    fn a_corrupt_file_is_ignored_not_deserialized_into_garbage() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path_in(dir.path()), "{not json").unwrap();
        assert!(load_in(dir.path()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_holding_a_bearer_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        save_in(dir.path(), &link(9_000)).unwrap();
        let mode = std::fs::metadata(path_in(dir.path()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "guest.json holds a live bearer");
    }
}
