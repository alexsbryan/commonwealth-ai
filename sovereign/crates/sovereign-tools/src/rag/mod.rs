// SPDX-License-Identifier: AGPL-3.0-or-later
//! `chunk` (paragraph chunking) and `section` (section-aware chunking) moved to
//! the leaf-pure `sovereign-tools-base` crate; re-exported here so
//! `sovereign_tools::rag::{chunk, section}` is unchanged for all callers.
//! `parse` and `ingest` stay in-crate — `parse` reaches into `local_corpus`
//! PDF extraction, so it is not leaf-pure.
pub use sovereign_tools_base::rag::{chunk, section};

pub mod ingest;
pub mod parse;
