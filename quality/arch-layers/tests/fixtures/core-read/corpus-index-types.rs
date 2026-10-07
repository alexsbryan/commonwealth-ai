// SPDX-License-Identifier: AGPL-3.0-or-later
use serde::{Deserialize, Serialize};

pub struct IndexInfo;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinCorpus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub size_compressed_gb: f64,
    pub size_indexed_gb: f64,
    pub license: String,
    pub mesh_sharing: bool,
    #[serde(default)]
    pub parent_corpus_id: Option<String>,
    #[serde(default)]
    pub catalog_status: Option<String>,
}
