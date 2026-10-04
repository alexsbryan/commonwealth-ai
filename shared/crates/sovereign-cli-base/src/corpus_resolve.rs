// SPDX-License-Identifier: AGPL-3.0-or-later
//! Human-friendly corpus resolution for CLI arguments.
//!
//! Corpus ids are structural keys (`obsidian-vault-959ee8a8f330`) —
//! stable, citation-grade, and unpleasant to type. Every CLI surface
//! that takes a corpus argument should accept, in priority order:
//!
//! 1. the exact corpus id (always wins — scripts stay stable),
//! 2. the exact display name (`corpus_name` in `_corpus_meta.json`,
//!    case-insensitive),
//! 3. a UNIQUE case-insensitive substring of either ("vault", "959e").
//!
//! Ambiguity is an error that lists the candidates — never a guess:
//! a corpus argument selects what gets read, written, or benched.

use std::path::Path;

/// One installed corpus, as discovered from the indexes dir.
#[derive(Debug, Clone)]
pub struct CorpusEntry {
    pub corpus_id: String,
    pub corpus_name: String,
}

/// Scan `indexes_dir` for installed corpora (dirs carrying a
/// `_corpus_meta.json`). Out-of-band names (`.legacy-backup`,
/// `.retired`, hidden dirs) are skipped, matching
/// `installed_indexes()`' convention.
pub fn list_installed(indexes_dir: &Path) -> Vec<CorpusEntry> {
    let mut out = Vec::new();
    let Ok(read) = std::fs::read_dir(indexes_dir) else {
        return out;
    };
    for entry in read.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.contains(".legacy-backup") || name.contains(".retired") {
            continue;
        }
        let meta_path = path.join("_corpus_meta.json");
        let Ok(raw) = std::fs::read_to_string(&meta_path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let corpus_id = v
            .get("corpus_id")
            .and_then(|x| x.as_str())
            .unwrap_or(&name)
            .to_string();
        let corpus_name = v
            .get("corpus_name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        out.push(CorpusEntry {
            corpus_id,
            corpus_name,
        });
    }
    out
}

/// Resolve a user-supplied corpus argument to a corpus id. See module
/// doc for the matching ladder. `Err` carries a ready-to-print message
/// (unknown query lists everything installed; ambiguous query lists
/// the clashing candidates).
pub fn resolve_corpus_id(indexes_dir: &Path, query: &str) -> Result<String, String> {
    let entries = list_installed(indexes_dir);
    let q = query.trim();
    let q_folded = q.to_lowercase();

    // 1. Exact id.
    if let Some(e) = entries.iter().find(|e| e.corpus_id == q) {
        return Ok(e.corpus_id.clone());
    }
    // 2. Exact display name (case-insensitive). Names are not unique
    //    by construction — require uniqueness here too.
    let by_name: Vec<&CorpusEntry> = entries
        .iter()
        .filter(|e| !e.corpus_name.is_empty() && e.corpus_name.to_lowercase() == q_folded)
        .collect();
    match by_name.as_slice() {
        [only] => return Ok(only.corpus_id.clone()),
        [] => {}
        many => {
            return Err(format!(
                "corpus name '{q}' is ambiguous — matching ids: {}",
                many.iter()
                    .map(|e| e.corpus_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    // 3. Unique substring of id or name.
    let by_sub: Vec<&CorpusEntry> = entries
        .iter()
        .filter(|e| {
            e.corpus_id.to_lowercase().contains(&q_folded)
                || e.corpus_name.to_lowercase().contains(&q_folded)
        })
        .collect();
    match by_sub.as_slice() {
        [only] => Ok(only.corpus_id.clone()),
        [] => Err(format!(
            "no installed corpus matches '{q}'. Installed: {}",
            if entries.is_empty() {
                "(none)".to_string()
            } else {
                entries
                    .iter()
                    .map(|e| {
                        if e.corpus_name.is_empty() {
                            e.corpus_id.clone()
                        } else {
                            format!("{} ({})", e.corpus_id, e.corpus_name)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )),
        many => Err(format!(
            "'{q}' is ambiguous — matches: {}",
            many.iter()
                .map(|e| e.corpus_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dir: &Path, id: &str, name: &str) {
        let d = dir.join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("_corpus_meta.json"),
            format!(r#"{{"corpus_id":"{id}","corpus_name":"{name}"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn resolution_ladder_id_name_then_unique_substring() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path(), "obsidian-vault-959ee8a8f330", "Obsidian Vault");
        fixture(tmp.path(), "wikipedia", "Wikipedia");
        fixture(tmp.path(), "sep", "Stanford Encyclopedia of Philosophy");

        // Exact id always wins.
        assert_eq!(
            resolve_corpus_id(tmp.path(), "wikipedia").unwrap(),
            "wikipedia"
        );
        // Exact display name, case-insensitive.
        assert_eq!(
            resolve_corpus_id(tmp.path(), "obsidian vault").unwrap(),
            "obsidian-vault-959ee8a8f330"
        );
        // Unique substring of id…
        assert_eq!(
            resolve_corpus_id(tmp.path(), "959e").unwrap(),
            "obsidian-vault-959ee8a8f330"
        );
        // …or of name.
        assert_eq!(resolve_corpus_id(tmp.path(), "stanford").unwrap(), "sep");
        // Ambiguous substring errors with candidates.
        let err = resolve_corpus_id(tmp.path(), "i").unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
        // Unknown lists what's installed.
        let err = resolve_corpus_id(tmp.path(), "nope").unwrap_err();
        assert!(err.contains("no installed corpus"), "{err}");
        assert!(err.contains("wikipedia"), "{err}");
    }
}
