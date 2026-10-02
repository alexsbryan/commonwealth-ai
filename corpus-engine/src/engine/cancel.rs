// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cooperative ingest cancellation lives in the `corpus-index` leaf beside the
//! port that hands it to svrn's daemon (pb-ingest-dial-daemon-ports).
pub use corpus_index::ingest_port::cancel::*;
