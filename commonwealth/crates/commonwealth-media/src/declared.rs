// SPDX-License-Identifier: AGPL-3.0-or-later
//! Headers this node adds to requests reaching its OWN media origin.
//!
//! # The problem this exists for
//!
//! A media origin authenticates nothing to the mesh — that is why
//! [`crate::admit_media`] refuses a stranger rather than downgrading one. But
//! most real origins authenticate to their OWN clients: Jellyfin wants an
//! `Authorization: MediaBrowser Token="<key>"`, and its API keys are
//! per-server. Twenty-five housemates
//! federating twenty-five Jellyfins therefore had exactly two options before
//! this module, and both are bad: turn authentication off, or mail everybody a
//! copy of everybody's key.
//!
//! The third option is that the HOLDER supplies its own credential, on its own
//! machine, after admission, on the way to its own origin. Nobody else ever
//! holds it. A viewer's request carries no key at all and still works, because
//! the key was never the viewer's to carry.
//!
//! # Why the value is not in `config.toml`
//!
//! Because that decision was already made and this is not the place to re-open
//! it. `sovereign-tools-base/src/mcp/auth.rs` states the rule — a token comes
//! from the secret store or the environment, "never from the on-disk
//! `config.toml` (ARCH §7)" — and `mcp/secret_store.rs` gives the reason: kept
//! apart, "the secret never rides along with anything the app shares, syncs,
//! backs up, or gossips to a mesh peer." On a mesh of housemates that last verb
//! is not hypothetical.
//!
//! # Why the FILENAME is the header name
//!
//! The MCP store keeps the name in config and the value in the file. This one
//! keeps both in the file, and the divergence is deliberate:
//! `commonwealth_rails::config::MediaSection` is `#[serde(deny_unknown_fields)]`,
//! so ANY new key makes an un-upgraded daemon refuse to boot rather than ignore
//! it. In a house where eight people upgrade on the install evening and
//! seventeen do not, a config field would hand those seventeen a daemon that
//! will not start. A directory nobody's parser reads cannot do that — a node
//! without the feature simply has an empty dir. Zero config change, zero
//! rollout hazard, one fewer place to look.
//!
//! And it is why the header NAME being data rather than a field has already
//! paid: Jellyfin 12.0.0 removed `X-Emby-Token`, `X-MediaBrowser-Token` and
//! `?api_key=` outright — probed 2026-09-12 against a live 12.0.0, all three
//! 401 and only `Authorization` 200, and its OpenAPI document declares one
//! security scheme, `apiKey` in header `Authorization`, with zero mentions of
//! the old names. A `x_emby_token: Option<String>` config field would have
//! needed a code change and a release to follow that; a filename needed a
//! rename by the person who holds the key.
//!
//! Layout, mirroring `secrets/mcp/`:
//!
//! ```text
//!   ~/.svrnmesh/secrets/media/authorization    0600   the value
//!   ~/.svrnmesh/secrets/media/                 0700
//! ```
//!
//! For Jellyfin 12 the value is the whole credential the header carries,
//! `MediaBrowser Token="<key>"`, not the bare key.

use std::path::{Path, PathBuf};

/// A header name the wire can carry and a path cannot escape: RFC 9110 token
/// characters minus the ones no origin uses, lowercased.
///
/// This is the ONLY thing standing between a filename and a request head, so
/// it rejects rather than sanitizes — a name that needed cleaning is a name
/// nobody meant, and silently repairing it would produce a header the operator
/// did not write. `..`, `/`, and every control byte fail the same test as a
/// stray space.
pub fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The directory a daemon keeps its declarations in, given its state root.
/// The ROOT is the caller's to know: this crate sits below the daemons that
/// have one, and reaching up to `sovereign_contracts::rebrand::svrnmesh_root`
/// from here would invert that edge for a path string. Rails passes its own;
/// tests pass a tempdir.
pub fn dir_under(state_root: &Path) -> PathBuf {
    state_root.join("secrets").join("media")
}

/// Every declared header, as [`crate::admit_media`] wants them. Empty when the
/// directory is absent, which is the common case and not an error.
///
/// Order is stable (sorted by name) so a head is byte-reproducible across
/// runs; a directory iteration order is not.
pub fn read_declared_in(dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if !valid_header_name(&name) {
            tracing::warn!(
                target: "media",
                file = %name,
                "declared-header: SKIPPED — not a usable header name (letters, digits, - and _ only)"
            );
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(entry.path()) else {
            tracing::warn!(target: "media", header = %name, "declared-header: unreadable, skipped");
            continue;
        };
        let value = raw.trim().to_string();
        if value.is_empty() {
            continue;
        }
        // The value is a secret: log THAT one is set, never what it is. Same
        // discipline as `PUT /v1/mcp/servers/{name}/token`.
        tracing::info!(
            target: "media",
            header = %name,
            "declared-header: this node will add its own credential to requests reaching its origin"
        );
        out.push((name, value));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Store a declared header's value in `dir`, hardening the file and the
/// directory. Writing an empty value deletes the declaration — so `unpublish`
/// and "I typed the wrong key" are the same operation, and neither leaves a
/// stale secret behind.
pub fn write_declared_in(dir: &Path, name: &str, value: &str) -> std::io::Result<()> {
    let name = name.to_ascii_lowercase();
    if !valid_header_name(&name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name:?} is not a usable header name (letters, digits, - and _ only)"),
        ));
    }
    let path = dir.join(&name);
    if value.trim().is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
    }
    std::fs::create_dir_all(dir)?;
    harden_dir(dir);
    std::fs::write(&path, value.trim())?;
    harden_file(&path);
    Ok(())
}

#[cfg(unix)]
fn harden_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(unix)]
fn harden_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn harden_file(_path: &Path) {}

#[cfg(not(unix))]
fn harden_dir(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_declaration_and_trims() {
        let dir = tempfile::tempdir().unwrap();
        write_declared_in(dir.path(), "X-Emby-Token", "  abc123\n").unwrap();
        assert_eq!(
            read_declared_in(dir.path()),
            vec![("x-emby-token".to_string(), "abc123".to_string())]
        );
    }

    /// Trimming is for the trailing newline an editor adds, not for the
    /// credential's own shape. Jellyfin 12 wants `MediaBrowser Token="<key>"`
    /// in one header value, so an inner space and two quotes have to survive
    /// the store or the declaration is unusable on every current Jellyfin.
    #[test]
    fn a_value_with_inner_spaces_and_quotes_survives_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let credential = r#"MediaBrowser Token="a-key-1234""#;
        write_declared_in(dir.path(), "Authorization", &format!("{credential}\n")).unwrap();
        assert_eq!(
            read_declared_in(dir.path()),
            vec![("authorization".to_string(), credential.to_string())]
        );
    }

    #[test]
    fn an_absent_directory_is_empty_not_an_error() {
        assert!(read_declared_in(Path::new("/nonexistent/media/secrets")).is_empty());
    }

    #[test]
    fn an_empty_value_deletes_the_declaration() {
        let dir = tempfile::tempdir().unwrap();
        write_declared_in(dir.path(), "authorization", "x").unwrap();
        write_declared_in(dir.path(), "authorization", "  ").unwrap();
        assert!(read_declared_in(dir.path()).is_empty());
        // Idempotent: deleting what is already gone is not a failure.
        write_declared_in(dir.path(), "authorization", "").unwrap();
    }

    /// The filename reaches a request head, so it must not be able to name a
    /// path or end a line. Rejected, never repaired.
    #[test]
    fn a_name_that_could_escape_the_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        for hostile in [
            "../../etc/passwd",
            "a/b",
            "x\r\ny",
            "has space",
            "",
            "..",
            ".",
        ] {
            assert!(
                write_declared_in(dir.path(), hostile, "v").is_err(),
                "{hostile:?} must be refused"
            );
        }
        assert!(read_declared_in(dir.path()).is_empty());
    }

    /// A file dropped in by hand with an unusable name is skipped rather than
    /// poisoning every other declaration on the node.
    #[test]
    fn a_hand_dropped_file_with_a_bad_name_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("not a header"), "v").unwrap();
        write_declared_in(dir.path(), "x-emby-token", "good").unwrap();
        assert_eq!(
            read_declared_in(dir.path()),
            vec![("x-emby-token".to_string(), "good".to_string())]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_declaration_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        write_declared_in(dir.path(), "x-emby-token", "abc").unwrap();
        let mode = std::fs::metadata(dir.path().join("x-emby-token"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn order_is_stable_so_a_head_is_reproducible() {
        let dir = tempfile::tempdir().unwrap();
        write_declared_in(dir.path(), "z-last", "1").unwrap();
        write_declared_in(dir.path(), "a-first", "2").unwrap();
        let names: Vec<String> = read_declared_in(dir.path())
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, vec!["a-first".to_string(), "z-last".to_string()]);
    }
}
