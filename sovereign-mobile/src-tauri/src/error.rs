//! Crate error type. Serializes to a string so it can cross the Tauri
//! command boundary into the WebView (`Result<T, Error>` from a
//! `#[tauri::command]`).

use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no active host connection")]
    NoActiveHost,

    #[error("host is busy; retry after {retry_after_secs}s")]
    HostBusy { retry_after_secs: u64 },

    #[error("not authenticated (no token in keychain for this host)")]
    Unauthenticated,

    #[error("off tailnet: the host is only reachable over the tailnet")]
    OffTailnet,

    #[error("http error: {0}")]
    Http(String),

    #[error("websocket error: {0}")]
    WebSocket(String),

    #[error("cache error: {0}")]
    Cache(String),

    #[error("keychain error: {0}")]
    Keychain(String),

    #[error("{0}")]
    Other(String),
}

impl Serialize for Error {
    // NB: spell out `std::result::Result` — the crate's one-param `Result`
    // alias (below) would otherwise shadow serde's two-arg return type.
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Cache(e.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Http(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
