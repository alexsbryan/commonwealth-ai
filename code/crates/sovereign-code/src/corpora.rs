// SPDX-License-Identifier: AGPL-3.0-or-later
//! The code corpora under an indexes root, and the one judgement of a corpus
//! a caller names. A code corpus is an index directory holding a
//! `scip_graph.db`, what the merged graph loads
//! (`corpus_engine_scip::merged_graph`). Moved from sovereign-cli-dev's
//! `converge_cmd` so the MCP mount's admission and the CLI's cwd resolver read
//! the same list.

use std::path::{Path, PathBuf};

use corpus_index::corpus::Corpus;

/// A code corpus under `indexes_dir`: its id, and the tree it was built from
/// (`_corpus_meta.json`'s `source_path`, absent on a corpus whose chunk index
/// was never built). Sorted by id.
pub fn code_corpora(indexes_dir: &Path) -> Vec<(String, Option<PathBuf>)> {
    let mut v: Vec<(String, Option<PathBuf>)> = std::fs::read_dir(indexes_dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().join("scip_graph.db").exists())
                .filter_map(|e| {
                    let id = e.file_name().to_str()?.to_string();
                    let src = std::fs::read_to_string(Corpus::meta_in(e.path()))
                        .ok()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                        .and_then(|v| v.get("source_path")?.as_str().map(PathBuf::from));
                    Some((id, src))
                })
                .collect()
        })
        .unwrap_or_else(|e| {
            // A missing root is a fresh install with no corpora; any other
            // failure reads the same downstream, so it is said here.
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(target: "sovereign_code::corpora",
                    dir = %indexes_dir.display(), error = %e,
                    "indexes root unreadable: no code corpus is listed");
            }
            Vec::new()
        });
    v.sort();
    v
}

/// Whether a caller may scope its calls to `corpus` under `indexes_dir`.
/// `None` (every corpus) always may; a corpus id that names no code corpus
/// is refused, and the refusal names every one that exists, so a misnamed
/// repo config says how to fix itself.
pub fn admit_corpus(indexes_dir: &Path, corpus: Option<&str>) -> Result<(), String> {
    let Some(id) = corpus else {
        return Ok(());
    };
    let known = code_corpora(indexes_dir);
    if known.iter().any(|(k, _)| k == id) {
        return Ok(());
    }
    let names: Vec<&str> = known.iter().map(|(k, _)| k.as_str()).collect();
    Err(format!(
        "`{id}` is not an indexed code corpus, so no code tool can answer about it; \
         the indexed code corpora under {}: {}",
        indexes_dir.display(),
        if names.is_empty() {
            "none".to_string()
        } else {
            names.join(", ")
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(root: &Path, id: &str, source: Option<&str>) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("scip_graph.db"), b"").unwrap();
        if let Some(src) = source {
            std::fs::write(
                Corpus::meta_in(&dir),
                serde_json::json!({ "source_path": src }).to_string(),
            )
            .unwrap();
        }
    }

    #[test]
    fn a_code_corpus_is_a_directory_holding_a_graph() {
        let tmp = tempfile::tempdir().unwrap();
        corpus(tmp.path(), "b", Some("/w/b"));
        corpus(tmp.path(), "a", None);
        std::fs::create_dir_all(tmp.path().join("prose")).unwrap();
        assert_eq!(
            code_corpora(tmp.path()),
            vec![
                ("a".to_string(), None),
                ("b".to_string(), Some(PathBuf::from("/w/b")))
            ]
        );
    }

    #[test]
    fn an_unknown_corpus_is_refused_naming_the_known_ones() {
        let tmp = tempfile::tempdir().unwrap();
        corpus(tmp.path(), "a", None);
        corpus(tmp.path(), "b", None);
        assert!(admit_corpus(tmp.path(), None).is_ok());
        assert!(admit_corpus(tmp.path(), Some("a")).is_ok());
        let why = admit_corpus(tmp.path(), Some("nope")).unwrap_err();
        assert!(why.contains("`nope`") && why.contains("a, b"), "{why}");
        // A directory without a graph is not a code corpus, whatever its name.
        std::fs::create_dir_all(tmp.path().join("prose")).unwrap();
        assert!(admit_corpus(tmp.path(), Some("prose")).is_err());
    }
}
