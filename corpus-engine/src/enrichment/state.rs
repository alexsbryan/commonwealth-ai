// SPDX-License-Identifier: AGPL-3.0-or-later
//! The enrichment state file (`_enrichment_state.json`), its heartbeat and
//! progress sinks — defined in the `corpus-index` leaf beside the index they
//! describe, so a host that reads or stamps it links no engine
//! (pb-ingest-dial-tools).

pub use corpus_index::enrichment_state::*; // shim: moved by pb-ingest-dial-tools
