// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the snapshot publisher reads back from an installed index to fill
//! the manifest. A sibling of `snapshot.rs`, which is at its arch-gate pin;
//! re-exported there, so `corpus_engine::snapshot::read_local_index_meta`
//! keeps its path.

use std::path::Path;

use crate::error::{Error, Result};

/// Look up an existing index's `_corpus_meta.json` on disk and produce
/// the subset of fields the snapshot manifest needs, plus the recipe stamp
/// beside it. Helper used by the publisher; not part of the manifest itself.
pub fn read_local_index_meta(index_dir: &Path) -> Result<LocalIndexMetaSummary> {
    let meta_path = crate::corpus::Corpus::meta_in(index_dir);
    let bytes = std::fs::read(&meta_path)?;
    let raw: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::Serialization(format!("parse {}: {e}", meta_path.display())))?;
    let text = |key: &str| {
        raw.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    Ok(LocalIndexMetaSummary {
        corpus_id: text("corpus_id"),
        corpus_name: text("corpus_name"),
        embedding_model: text("embedding_model"),
        embedding_dimensions: raw
            .get("embedding_dimensions")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize,
        canonical_fingerprint: raw
            .get("canonical_fingerprint")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        filter_signature: raw
            .pointer("/scope/filter_signature")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        recipe_sha256: crate::corpus::Corpus::recipe_sha256_in(index_dir),
    })
}

/// Subset of `IndexMeta` the publisher reads back to populate the
/// snapshot manifest. Kept narrow so the snapshot module doesn't pin
/// itself to every field of the private `IndexMeta` struct.
#[derive(Debug, Clone)]
pub struct LocalIndexMetaSummary {
    pub corpus_id: String,
    pub corpus_name: String,
    pub embedding_model: String,
    pub embedding_dimensions: usize,
    pub canonical_fingerprint: Option<String>,
    pub filter_signature: Option<String>,
    /// The sha256 of the recipe the corpus was installed from, from its
    /// stamp (`Corpus::recipe_sha256_in`); `None` for a corpus installed by
    /// registry id or before the stamp existed.
    pub recipe_sha256: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The publisher reads the install's recipe stamp beside the meta, which
    /// is what fills a manifest's `source_recipe_sha256`. Failing input: the
    /// field read as `None` here is the hard-coded `None` it replaced.
    #[test]
    fn the_recipe_stamp_reaches_the_manifest_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(
            crate::corpus::Corpus::meta_in(dir),
            r#"{"corpus_id":"notes","corpus_name":"Notes","embedding_model":"m","embedding_dimensions":4}"#,
        )
        .unwrap();
        assert_eq!(read_local_index_meta(dir).unwrap().recipe_sha256, None);
        let sha = crate::corpus::recipe_sha256("[corpus]\nid = \"notes\"\n");
        let c = crate::corpus::Corpus::named(
            dir.parent().unwrap(),
            dir.file_name().unwrap().to_str().unwrap(),
        )
        .unwrap();
        c.stamp_recipe_sha256(&sha).unwrap();
        let summary = read_local_index_meta(dir).unwrap();
        assert_eq!(summary.corpus_id, "notes");
        assert_eq!(summary.recipe_sha256, Some(sha));
    }
}
