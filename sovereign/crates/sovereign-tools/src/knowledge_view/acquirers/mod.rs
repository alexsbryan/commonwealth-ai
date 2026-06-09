// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runtime-registered acquirers for the KnowledgeView pipeline.
//!
//! Each acquirer implements the `corpus-engine` `CustomAcquirerFn`
//! contract (params blob + download_dir → JSONL path) and is
//! registered on the shared `CorpusEngine` by `KnowledgeViewManager`
//! at Runtime startup.
//!
//! The design keeps `corpus-engine` database-free: rusqlite lives
//! here, beside `sovereign-store`'s connection, rather than being
//! pulled into the knowledge-layer crate.

pub mod sqlite;
pub use sqlite::{register as register_sqlite, SqliteAcquirerParams};
