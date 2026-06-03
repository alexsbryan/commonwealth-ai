//! CREDENTIAL token storage.
//!
//! The spec requires the tenant token to live in the **iOS Keychain /
//! Android Keystore** — never in SQLite. This module defines the
//! abstraction; the real per-platform backing is a **pin-time task**
//! (see the crate README + `Cargo.toml`): wire a secure-storage Tauri
//! plugin, or a thin per-platform plugin (Swift `SecItem` / Kotlin
//! `KeyStore`).
//!
//! Until then, [`DevFileCredentialStore`] is a **DEV-ONLY** placeholder
//! that writes tokens to a file under the app data dir. It is NOT
//! secure and MUST NOT ship — it exists so the rest of the core
//! compiles and runs against a local host during development. The
//! `key` is namespaced per host connection so multi-host can land later.

use crate::error::{Error, Result};

/// Where the tenant token for a host connection is stored. One impl per
/// platform at ship time; the trait keeps the rest of the core unaware
/// of the backing store.
pub trait CredentialStore: Send + Sync {
    fn set_token(&self, host_connection_id: &str, token: &str) -> Result<()>;
    fn get_token(&self, host_connection_id: &str) -> Result<Option<String>>;
    fn delete_token(&self, host_connection_id: &str) -> Result<()>;
}

fn key(host_connection_id: &str) -> String {
    format!("sovereign.token.{host_connection_id}")
}

/// DEV-ONLY file-backed store. Replace with the OS keychain before ship.
pub struct DevFileCredentialStore {
    dir: std::path::PathBuf,
}

impl DevFileCredentialStore {
    pub fn new(dir: impl Into<std::path::PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, host_connection_id: &str) -> std::path::PathBuf {
        self.dir.join(format!("{}.token", key(host_connection_id)))
    }
}

impl CredentialStore for DevFileCredentialStore {
    fn set_token(&self, host_connection_id: &str, token: &str) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|e| Error::Keychain(e.to_string()))?;
        std::fs::write(self.path(host_connection_id), token)
            .map_err(|e| Error::Keychain(e.to_string()))
    }

    fn get_token(&self, host_connection_id: &str) -> Result<Option<String>> {
        match std::fs::read_to_string(self.path(host_connection_id)) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Keychain(e.to_string())),
        }
    }

    fn delete_token(&self, host_connection_id: &str) -> Result<()> {
        match std::fs::remove_file(self.path(host_connection_id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Keychain(e.to_string())),
        }
    }
}
