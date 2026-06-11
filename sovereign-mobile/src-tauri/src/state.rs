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
    /// Per-host iroh bridges for `endpoint_kind = 'iroh'` rows. Lazy:
    /// nothing binds until the first iroh-kind dial.
    pub bridges: crate::iroh_bridge::BridgeManager,
    /// A `sovereign://pair#…` deep link that arrived before the
    /// frontend was listening (cold launch via QR scan). The pairing
    /// screen drains it via `take_pending_pair_link` on mount; warm
    /// opens ride the `pair-link` event instead.
    pub pending_pair_link: Mutex<Option<String>>,
}

impl AppState {
    pub fn new(db: Connection, credentials: Box<dyn CredentialStore>) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            credentials,
            bridges: crate::iroh_bridge::BridgeManager::new(),
            pending_pair_link: Mutex::new(None),
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
    pub async fn active_client(&self) -> Result<ApiClient> {
        let host = self.active_host()?;
        let token = self
            .credentials
            .get_token(&host.id)?
            .ok_or(Error::Unauthenticated)?;
        self.client_for_host(&host, token).await
    }

    /// Transport seam: the host's `endpoint_kind` decides how its
    /// address becomes a dialable client. Tailnet → plain HTTP to
    /// the address; Iroh → plain HTTP to a localhost bridge that
    /// tunnels to the host's Ed25519 key (HTTP and the WS stream
    /// both ride it — `ws_url` derives from the same base). Unknown
    /// kinds (rows written by a newer app build) fail loudly in
    /// `EndpointKind::parse`.
    async fn client_for_host(&self, host: &HostConnection, token: String) -> Result<ApiClient> {
        match crate::connection::EndpointKind::parse(&host.endpoint_kind)? {
            crate::connection::EndpointKind::Tailnet => {
                Ok(ApiClient::new(&host.tailnet_address, token))
            }
            crate::connection::EndpointKind::Iroh => {
                let local = self
                    .bridges
                    .bridge_for(&host.id, &host.tailnet_address)
                    .await?;
                Ok(ApiClient::new(&local.to_string(), token))
            }
        }
    }
}
