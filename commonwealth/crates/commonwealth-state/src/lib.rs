//! `commonwealth-state` — distributed KV store for mesh applications.
//!
//! Provides `MeshStore` (SQLite-backed, LWW-merged) and `RetentionGc`.

mod backend;
pub mod error;
pub mod gc;
pub mod store;

pub use error::{Error, Result};
pub use gc::RetentionGc;
pub use store::{MeshStore, StoreEntry};
