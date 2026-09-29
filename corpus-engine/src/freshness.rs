// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-document index recency (`_doc_freshness.json`) — defined in the
//! `corpus-index` leaf beside the index it describes, so the atlas view
//! reads it without the engine (pb-ingest-dial-tools).

pub use corpus_index::freshness::*; // shim: moved by pb-ingest-dial-tools
