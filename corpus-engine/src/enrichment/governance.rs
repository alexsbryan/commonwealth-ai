// SPDX-License-Identifier: AGPL-3.0-or-later
//! The governance acts and their fold live in the atlas-reader leaf since fp-60
//! (FIVE_PROGRAMS §12 decision 1); re-exported here at the historical path.
//! `now_secs` stays host-side: the leaf does not read the wall clock.

pub use corpus_engine_atlas_reader::governance::*;
pub use corpus_engine_yield::time::unix_now as now_secs;
