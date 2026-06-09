// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared application state managed by Tauri. Holds the SQLite cache
//! connection, the keychain-backed credential store, and helpers to
//! resolve the active host into an authed [`ApiClient`].

use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::connection::{store as conn_store, CredentialStore, HostConnection};
use crate::error::{Error, Result};
use crate::remote::ApiClient;

pub struct AppState {
    /// Single cache connection behind a mutex. Locked only for short,
    /// await-free critical sections (the stream path writes at
    /// completion without holding it across I/O).
    pub db: Arc<Mutex<Connection>>,
    /// Token store — OS keychain at ship time (dev-file placeholder now).
    pub credentials: Box<dyn CredentialStore>,
}

impl AppState {
    pub fn new(db: Connection, credentials: Box<dyn CredentialStore>) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            credentials,
        }
    }

    fn with_db<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self.db.lock().map_err(|_| Error::Other("db poisoned".into()))?;
        f(&conn)
    }

    pub fn active_host(&self) -> Result<HostConnection> {
        self.with_db(|c| conn_store::get_default(c))?
            .ok_or(Error::NoActiveHost)
    }

    /// Build an authed client for the active host. Reads the token from
    /// the keychain — never from SQLite.
    pub fn active_client(&self) -> Result<ApiClient> {
        let host = self.active_host()?;
        let token = self
            .credentials
            .get_token(&host.id)?
            .ok_or(Error::Unauthenticated)?;
        Ok(ApiClient::new(&host.tailnet_address, token))
    }
}
