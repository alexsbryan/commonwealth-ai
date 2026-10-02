// SPDX-License-Identifier: AGPL-3.0-or-later
//! The collaborative-ingest queue's records. They live in
//! `oicp_types::work_queue` since pb-mesh-exit-core; this path re-exports them
//! so every existing `commonwealth_core::knowledge::*` site resolves unchanged.
pub use oicp_types::work_queue::*;
