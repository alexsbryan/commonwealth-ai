// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::Path;

use corpus_engine_yield::ForegroundLease;
use corpus_index::{
    index::CorpusIndex,
    source::IndexSource,
    types::{BuiltinCorpus, IndexInfo},
    Result,
};
