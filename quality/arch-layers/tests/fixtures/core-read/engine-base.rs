// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::Path;

use async_trait::async_trait;
use corpus_index::{
    index::CorpusIndex,
    source::IndexSource,
    types::{BuiltinCorpus, IndexInfo},
    Result,
};

pub struct ProjectionEngine;

#[async_trait]
impl IndexSource for ProjectionEngine {
    async fn usable_indexes(&self) -> Result<Vec<IndexInfo>> {
        unimplemented!("compile projection only")
    }

    async fn open_index(&self, _path: &Path) -> Result<CorpusIndex> {
        unimplemented!("compile projection only")
    }
}
