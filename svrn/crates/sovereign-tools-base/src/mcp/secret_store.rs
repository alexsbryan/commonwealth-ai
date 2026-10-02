// SPDX-License-Identifier: AGPL-3.0-or-later
//! File-backed MCP credential storage.
//!
//! Tokens live in `~/.svrnmesh/secrets/mcp/<NAME>.token`, **separate from
//! `config.toml` and from the SQLite store** — so the secret never rides along
//! with anything the app shares, syncs, backs up, or gossips to a mesh peer.
//! For a local-first app that's the dominant leak vector (a plaintext token in
//! a file you might commit, screenshare, or attach to a bug report), and a
//! standalone secrets dir nothing else reads is excluded from all of it by
//! construction. The full keychain due-diligence (mac/Linux/Windows + headless)
//! landed on this as the proportionate choice — see the desktop UX note.
//!
//! Files are `0600` and the dir `0700` on unix; on other platforms the
//! user-profile ACL is the protection. This is the primary store the desktop
//! writes to; [`super::auth::McpAuth::resolve`] reads it first, then falls back
//! to the `SOVEREIGN_MCP_TOKEN_<NAME>` env var (headless nodes / CI).

use std::path::{Path, PathBuf};

use super::auth::sanitized_name;

/// `~/.svrnmesh/secrets/mcp`.
fn secrets_dir() -> Option<PathBuf> {
    Some(
        sovereign_contracts::rebrand::svrnmesh_root()
            .join("secrets")
            .join("mcp"),
    )
}

fn token_path_in(dir: &Path, server_name: &str) -> PathBuf {
    dir.join(format!("{}.token", sanitized_name(server_name)))
}

/// The stored token for `server_name`, or `None` if unset/empty.
pub fn read_token(server_name: &str) -> Option<String> {
    read_token_in(&secrets_dir()?, server_name)
}

/// Whether a non-empty token is stored for `server_name`.
pub fn has_token(server_name: &str) -> bool {
    read_token(server_name).is_some()
}

/// Store `token` for `server_name`, replacing any existing one. A blank token
/// clears it instead. Best-effort `0700`/`0600` perms on unix.
pub fn write_token(server_name: &str, token: &str) -> std::io::Result<()> {
    if token.trim().is_empty() {
        return delete_token(server_name);
    }
    let dir = secrets_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory"))?;
    write_token_in(&dir, server_name, token)
}

/// Remove any stored token for `server_name`. A no-op if none exists.
pub fn delete_token(server_name: &str) -> std::io::Result<()> {
    let Some(dir) = secrets_dir() else {
        return Ok(());
    };
    delete_token_in(&dir, server_name)
}

// ── Base-dir-injectable cores (so the unit tests stay hermetic) ──────────

fn read_token_in(dir: &Path, server_name: &str) -> Option<String> {
    std::fs::read_to_string(token_path_in(dir, server_name))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_token_in(dir: &Path, server_name: &str, token: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    harden_dir(dir);
    let path = token_path_in(dir, server_name);
    std::fs::write(&path, token.trim().as_bytes())?;
    harden_file(&path);
    Ok(())
}

fn delete_token_in(dir: &Path, server_name: &str) -> std::io::Result<()> {
    match std::fs::remove_file(token_path_in(dir, server_name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
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
    fn round_trips_a_token_and_trims() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(read_token_in(dir, "vision"), None);
        write_token_in(dir, "vision", "  sekret  ").unwrap();
        assert_eq!(read_token_in(dir, "vision").as_deref(), Some("sekret"));
        delete_token_in(dir, "vision").unwrap();
        assert_eq!(read_token_in(dir, "vision"), None);
        // delete is idempotent
        delete_token_in(dir, "vision").unwrap();
    }

    #[test]
    fn name_maps_to_the_same_sanitized_slug_as_the_env_var() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write_token_in(dir, "my-vision", "t").unwrap();
        // Same fold the env var uses, so file + env var agree on the slug.
        assert!(dir.join("MY_VISION.token").exists());
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        write_token_in(tmp.path(), "vision", "t").unwrap();
        let mode = std::fs::metadata(tmp.path().join("VISION.token"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
