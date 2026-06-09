// SPDX-License-Identifier: AGPL-3.0-or-later
//! `atoms.meta.json` sidecar — gates re-extraction.
//!
//! The manifest pins what produced the current `atoms.json` so the
//! next `finalize_corpus` can decide "is this still current?" without
//! re-running 80 LLM calls. Re-extraction triggers when:
//!
//! - the manifest is missing or unparseable (first run / corruption),
//! - `produced_by` doesn't match [`PRODUCED_BY`] (orchestrator bump),
//! - `raptor_nodes_hash` differs from the current leaves' hash, or
//! - `vault_themes_hash` differs from the current themes' hash.
//!
//! Hashes are blake3 over a canonical concatenation. The leaf hash
//! folds in every level-0 leaf's `(node_id, summary, primary_entities_json)`
//! sorted by `node_id`; the theme hash folds in every theme's
//! `(theme_id, summary)` sorted by `theme_id`. Embedding bytes are
//! deliberately excluded — float drift across embedding model
//! revisions would otherwise force re-extraction even when the
//! extractable content is unchanged.

use std::collections::HashMap;
use std::path::Path;

use blake3::Hasher;
use serde::{Deserialize, Serialize};
use sovereign_core::conv_tiered::{ConvRaptorNodeRow, VaultThemeRow};

/// Filename for the sidecar inside `atlas_dir`.
pub const MANIFEST_FILENAME: &str = "atoms.meta.json";

/// Bumped when the manifest structure changes in a backward-incompat
/// way. v1 is the first cut.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedExtensionManifest {
    pub schema_version: u32,
    pub produced_by: String,
    pub raptor_nodes_hash: String,
    pub vault_themes_hash: String,
    pub extracted_at_unix: i64,
    pub pass_a_calls: u32,
    pub pass_b_calls: u32,
    pub atoms_per_kind: HashMap<String, u32>,
}

impl TypedExtensionManifest {
    /// Read the manifest from `{atlas_dir}/atoms.meta.json`. Returns
    /// `Ok(None)` when the file doesn't exist; `Ok(Some(..))` on a
    /// successful parse; `Err` only when the file exists but cannot
    /// be opened. A parse error is treated as a missing manifest so
    /// the next run re-extracts cleanly rather than failing the
    /// finalize step.
    pub fn load(atlas_dir: &Path) -> std::io::Result<Option<Self>> {
        let path = atlas_dir.join(MANIFEST_FILENAME);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)?;
        match serde_json::from_str::<Self>(&raw) {
            Ok(m) => Ok(Some(m)),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "typed_extension: manifest parse failed; treating as missing"
                );
                Ok(None)
            }
        }
    }

    /// Write atomically: serialise to a sibling `.tmp` then rename.
    /// Mirrors the atlas writer's discipline so a mid-write crash
    /// can't leave a partial sidecar.
    pub fn write(&self, atlas_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(atlas_dir)?;
        let final_path = atlas_dir.join(MANIFEST_FILENAME);
        let tmp_path = atlas_dir.join(format!("{MANIFEST_FILENAME}.tmp"));
        let serialised = serde_json::to_string_pretty(self).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("manifest serialise: {e}"),
            )
        })?;
        std::fs::write(&tmp_path, serialised)?;
        std::fs::rename(&tmp_path, &final_path)?;
        Ok(())
    }
}

/// Hash level-0 RAPTOR leaves in a way that's stable across re-runs
/// over identical content. Sorts by `node_id` so SQL ordering doesn't
/// perturb the digest.
pub fn hash_raptor_leaves(leaves: &[ConvRaptorNodeRow]) -> String {
    let mut sorted: Vec<&ConvRaptorNodeRow> = leaves.iter().collect();
    sorted.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    let mut hasher = Hasher::new();
    for leaf in sorted {
        hasher.update(leaf.node_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(leaf.summary.as_bytes());
        hasher.update(b"\0");
        hasher.update(leaf.primary_entities_json.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

/// Hash vault_themes for the manifest. Sorts by `theme_id`.
pub fn hash_vault_themes(themes: &[VaultThemeRow]) -> String {
    let mut sorted: Vec<&VaultThemeRow> = themes.iter().collect();
    sorted.sort_by(|a, b| a.theme_id.cmp(&b.theme_id));
    let mut hasher = Hasher::new();
    for theme in sorted {
        hasher.update(theme.theme_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(theme.summary.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_leaf(node_id: &str, summary: &str) -> ConvRaptorNodeRow {
        ConvRaptorNodeRow {
            node_id: node_id.into(),
            corpus_id: "c".into(),
            conv_uuid: "u".into(),
            level: 0,
            summary: summary.into(),
            summary_embedding: Vec::new(),
            centroid_embedding: Vec::new(),
            children_node_ids_json: "[]".into(),
            direct_member_chunk_ids_json: None,
            evidence_chunk_ids_json: "[]".into(),
            quote_spans_json: "[]".into(),
            primary_entities_json: "[]".into(),
            cluster_coherence: 0.9,
            created_at: 0,
        }
    }

    fn mk_theme(theme_id: &str, summary: &str) -> VaultThemeRow {
        VaultThemeRow {
            corpus_id: "c".into(),
            theme_id: theme_id.into(),
            summary: summary.into(),
            summary_embedding: Vec::new(),
            member_source_doc_ids_json: "[]".into(),
            cluster_coherence: 0.8,
            created_at: 0,
        }
    }

    #[test]
    fn raptor_hash_is_order_independent() {
        let a = vec![mk_leaf("n1", "alpha"), mk_leaf("n2", "beta")];
        let b = vec![mk_leaf("n2", "beta"), mk_leaf("n1", "alpha")];
        assert_eq!(hash_raptor_leaves(&a), hash_raptor_leaves(&b));
    }

    #[test]
    fn raptor_hash_changes_on_summary_edit() {
        let a = vec![mk_leaf("n1", "alpha")];
        let b = vec![mk_leaf("n1", "alpha but edited")];
        assert_ne!(hash_raptor_leaves(&a), hash_raptor_leaves(&b));
    }

    #[test]
    fn raptor_hash_ignores_embedding_drift() {
        // Embedding bytes deliberately excluded — they're not part of
        // the manifest's invalidation signal.
        let mut a = mk_leaf("n1", "alpha");
        a.summary_embedding = vec![0.1, 0.2];
        let mut b = mk_leaf("n1", "alpha");
        b.summary_embedding = vec![0.7, 0.8, 0.9];
        assert_eq!(hash_raptor_leaves(&[a]), hash_raptor_leaves(&[b]));
    }

    #[test]
    fn theme_hash_is_order_independent_and_summary_sensitive() {
        let a = vec![mk_theme("t1", "AAA"), mk_theme("t2", "BBB")];
        let b = vec![mk_theme("t2", "BBB"), mk_theme("t1", "AAA")];
        assert_eq!(hash_vault_themes(&a), hash_vault_themes(&b));
        let c = vec![mk_theme("t1", "AAA"), mk_theme("t2", "DIFFERENT")];
        assert_ne!(hash_vault_themes(&a), hash_vault_themes(&c));
    }

    #[test]
    fn manifest_round_trips_through_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let mut atoms_per_kind = HashMap::new();
        atoms_per_kind.insert("mechanism".to_string(), 12u32);
        atoms_per_kind.insert("named_position".to_string(), 5u32);
        let m = TypedExtensionManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            produced_by: "test_v1".into(),
            raptor_nodes_hash: "deadbeef".into(),
            vault_themes_hash: "cafebabe".into(),
            extracted_at_unix: 1_700_000_000,
            pass_a_calls: 42,
            pass_b_calls: 3,
            atoms_per_kind,
        };
        m.write(tmp.path()).expect("write");
        let loaded = TypedExtensionManifest::load(tmp.path())
            .expect("io")
            .expect("present");
        assert_eq!(loaded.raptor_nodes_hash, "deadbeef");
        assert_eq!(loaded.pass_a_calls, 42);
        assert_eq!(loaded.atoms_per_kind.get("mechanism").copied(), Some(12));
    }

    #[test]
    fn manifest_load_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let loaded = TypedExtensionManifest::load(tmp.path()).expect("io");
        assert!(loaded.is_none());
    }

    #[test]
    fn manifest_load_treats_parse_error_as_missing() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(MANIFEST_FILENAME), "this is not json").unwrap();
        let loaded = TypedExtensionManifest::load(tmp.path()).expect("io");
        assert!(loaded.is_none());
    }
}
