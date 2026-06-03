//! Client-owned connection state: the HOST_CONNECTION record (SQLite)
//! and the CREDENTIAL token (keychain).

pub mod keychain;
pub mod store;

pub use keychain::{CredentialStore, DevFileCredentialStore};
pub use store::HostConnection;
