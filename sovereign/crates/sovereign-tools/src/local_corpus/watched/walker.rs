//! Walks a watched folder and produces a snapshot keyed on doc_id.
//!
//! Reuses `PreScanner::run_blocking` for directory traversal +
//! extension filtering + hidden-dir skipping. On top of that it adds:
//!   - `(mtime, size)` fast-path: skip the content hash when the
//!     file's metadata matches the prior manifest entry exactly.
//!   - SHA-256 short content hash (16 hex chars) when the fast-path
//!     misses or the file is new.
//!   - exclude-glob matching against the path relative to the
//!     watched root.
//!
//! The walk is CPU-bound (PDF classification, file IO, hashing) so
//! callers run it inside `tokio::task::spawn_blocking`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::local_corpus::config::LocalCorpusConfig;
use crate::local_corpus::pre_scanner::{FileMeta, PreScanResult, PreScanner};

/// One entry in a `WalkSnapshot`. The doc_id (relative path) lives in
/// the snapshot's HashMap key, not duplicated here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntryRecord {
    pub absolute_path: PathBuf,
    /// File mtime in seconds since the Unix epoch. Combined with
    /// `size_bytes` to short-circuit the content hash on the fast
    /// path. Cross-platform mtime granularity differs (APFS vs ext4
    /// vs NTFS); the hash is the ground truth, and a sweep that finds
    /// zero diffs is cheap so the fast-path is purely an optimisation.
    pub mtime_unix: i64,
    pub size_bytes: u64,
    /// Lowercase hex prefix of `sha256(file_bytes)`, 16 chars. Short
    /// enough to keep the state file under the 100k-tombstone cap;
    /// long enough to make collision astronomically unlikely.
    pub content_hash: String,
}

/// One sweep's view of the folder. `prior_meta` lets the next sweep
/// short-circuit hashing for files whose mtime+size haven't changed.
pub type WalkSnapshot = HashMap<String, EntryRecord>;

/// Outcome of a walk pass — both the snapshot for the diff and the
/// raw `PreScanResult` so the worker can surface skipped/failed-file
/// counts in status without re-walking.
pub struct WalkOutcome {
    pub snapshot: WalkSnapshot,
    pub raw: PreScanResult,
    pub visited: usize,
}

/// Run one walk pass. CPU-bound — call from inside `spawn_blocking`.
///
/// `prior_meta` is a `(doc_id → (mtime, size, content_hash))` map
/// reconstructed from the previous `WatchedFolderState`. When the file
/// at a given path matches `(mtime, size)` exactly, the prior hash is
/// reused unchanged. When the metadata differs (or the file is new),
/// the hash is recomputed from disk.
///
/// `exclude_globs` are matched against the path relative to
/// `root_path`, using the `glob::Pattern` matcher (not gitignore — the
/// spec defers `.sovereignignore` syntax to Phase 2).
pub fn walk_folder(
    config: &LocalCorpusConfig,
    prior_meta: &HashMap<String, EntryRecord>,
    exclude_globs: &[String],
) -> std::io::Result<WalkOutcome> {
    let scanner = PreScanner::new(config);
    let raw = scanner.run_blocking(|_, _| {});
    let visited = raw.total_visited as usize;

    // Compile globs once. Invalid patterns are skipped with a warn —
    // we don't want one bad config string to wedge an entire watched
    // corpus.
    let compiled: Vec<glob::Pattern> = exclude_globs
        .iter()
        .filter_map(|g| match glob::Pattern::new(g) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(
                    pattern = %g,
                    "watched_folder:exclude_glob_invalid: {e}"
                );
                None
            }
        })
        .collect();

    // Per-corpus `.sovereignignore` (spec §5.4 layer 2). Hot-reloaded
    // every sweep — no caching, the file is small. Gitignore syntax
    // via the `ignore` crate so the user's mental model matches every
    // other gitignore-syntax file they've written. Negation (`!`),
    // anchored vs floating, dir-only `/` all work the way they do in
    // git. Missing file is the common case → silently no-op.
    let sovereignignore_matcher = build_sovereignignore_matcher(&config.root_path);

    let mut snapshot = WalkSnapshot::new();

    // PreScanResult.readable is the canonical "we'd ingest this"
    // bucket. We additionally walk `large_files` (still indexed per
    // PreScanResult invariants) but NOT `corrupt_files` /
    // `protected_pdfs` / `scanned_pdfs` — those are surfaced in
    // status, not the diff.
    let mut considered: Vec<&FileMeta> = Vec::with_capacity(
        raw.readable.len() + raw.large_files.len(),
    );
    considered.extend(&raw.readable);
    considered.extend(&raw.large_files);

    for meta in considered {
        let Some(rel) = doc_id_for(&config.root_path, &meta.path) else {
            // Should not happen — PreScanner walks underneath root —
            // but log + skip rather than panic if it does.
            tracing::warn!(
                path = %meta.path.display(),
                root = %config.root_path.display(),
                "watched_folder:doc_id_failed"
            );
            continue;
        };

        if compiled.iter().any(|p| p.matches(&rel)) {
            continue;
        }
        if let Some(matcher) = &sovereignignore_matcher {
            // `matched_path_or_any_parents` walks the parent chain so
            // a directory rule like `scratch/` excludes
            // `scratch/foo.md`. `is_dir = false` because PreScanner
            // only surfaces file paths. The path must be absolute for
            // the parent walk to terminate at root — `meta.path`
            // already is.
            let m = matcher.matched_path_or_any_parents(&meta.path, /* is_dir = */ false);
            if m.is_ignore() {
                continue;
            }
        }

        let (mtime_unix, size_bytes) = match read_metadata(&meta.path) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    path = %meta.path.display(),
                    "watched_folder:stat_failed: {e}"
                );
                continue;
            }
        };

        // Fast path: mtime + size match prior → reuse hash. Skips
        // the file read entirely.
        let content_hash = match prior_meta.get(&rel) {
            Some(prev) if prev.mtime_unix == mtime_unix && prev.size_bytes == size_bytes => {
                prev.content_hash.clone()
            }
            _ => match hash_file(&meta.path) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(
                        path = %meta.path.display(),
                        "watched_folder:hash_failed: {e}"
                    );
                    continue;
                }
            },
        };

        snapshot.insert(
            rel,
            EntryRecord {
                absolute_path: meta.path.clone(),
                mtime_unix,
                size_bytes,
                content_hash,
            },
        );
    }

    Ok(WalkOutcome { snapshot, raw, visited })
}

/// Project `prior_meta` from a snapshot — extracts just the
/// `(doc_id → content_hash)` map shape that
/// `diff::compute_diff` consumes.
pub fn prior_hash_map(snapshot: &WalkSnapshot) -> HashMap<String, String> {
    snapshot
        .iter()
        .map(|(k, v)| (k.clone(), v.content_hash.clone()))
        .collect()
}

/// Compute the doc_id (relative path string, forward slashes) for a
/// file under `root`. Returns `None` if `path` isn't under `root`.
pub fn doc_id_for(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Build a gitignore matcher from `{root}/.sovereignignore`. Returns
/// `None` when the file doesn't exist (the common case). Soft-fails
/// on read or parse errors with a `warn!` rather than wedging the
/// sweep — a malformed ignore file shouldn't take a corpus offline.
pub fn build_sovereignignore_matcher(root: &Path) -> Option<ignore::gitignore::Gitignore> {
    let path = root.join(".sovereignignore");
    if !path.exists() {
        return None;
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    if let Some(err) = builder.add(&path) {
        tracing::warn!(
            path = %path.display(),
            "watched_folder:sovereignignore_read_failed: {err}"
        );
        return None;
    }
    match builder.build() {
        Ok(gi) => Some(gi),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                "watched_folder:sovereignignore_compile_failed: {e}"
            );
            None
        }
    }
}

fn read_metadata(path: &Path) -> std::io::Result<(i64, u64)> {
    let m = std::fs::metadata(path)?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok((mtime, m.len()))
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    Ok(hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(p: &Path, s: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, s).unwrap();
    }

    fn watched_cfg(root: &Path) -> LocalCorpusConfig {
        use crate::local_corpus::config::WatchedFolderConfig;
        LocalCorpusConfig::watched_folder(
            root.to_path_buf(),
            "test".into(),
            WatchedFolderConfig::default(),
        )
    }

    #[test]
    fn doc_id_for_strips_root_and_normalises_separators() {
        let r = PathBuf::from("/tmp/notes");
        assert_eq!(
            doc_id_for(&r, &PathBuf::from("/tmp/notes/sub/a.md")).as_deref(),
            Some("sub/a.md")
        );
        assert_eq!(doc_id_for(&r, &PathBuf::from("/elsewhere/x")), None);
    }

    #[test]
    fn walk_picks_up_new_files() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("a.md"), "hello");
        write(&dir.path().join("sub/b.md"), "world");

        let cfg = watched_cfg(dir.path());
        let prior = HashMap::new();
        let out = walk_folder(&cfg, &prior, &[]).unwrap();

        assert!(out.snapshot.contains_key("a.md"));
        assert!(out.snapshot.contains_key("sub/b.md"));
        assert_eq!(out.snapshot.len(), 2);
    }

    #[test]
    fn walk_uses_fast_path_when_metadata_matches() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.md");
        write(&path, "hello");

        let cfg = watched_cfg(dir.path());
        let first = walk_folder(&cfg, &HashMap::new(), &[]).unwrap();
        let prior_entry = first.snapshot.get("a.md").unwrap().clone();

        // Inject a synthetic prior entry with a deliberately wrong
        // hash but matching mtime+size — the walker must reuse it
        // verbatim, proving it didn't recompute the hash.
        let mut prior = HashMap::new();
        let bogus_hash = "BOGUS_HASH_XYZ".to_string();
        prior.insert(
            "a.md".into(),
            EntryRecord {
                absolute_path: prior_entry.absolute_path.clone(),
                mtime_unix: prior_entry.mtime_unix,
                size_bytes: prior_entry.size_bytes,
                content_hash: bogus_hash.clone(),
            },
        );

        let second = walk_folder(&cfg, &prior, &[]).unwrap();
        assert_eq!(
            second.snapshot.get("a.md").unwrap().content_hash,
            bogus_hash,
            "fast path should reuse prior hash without recomputing"
        );
    }

    #[test]
    fn walk_rehashes_when_size_changes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.md");
        write(&path, "hello");

        let cfg = watched_cfg(dir.path());
        let first = walk_folder(&cfg, &HashMap::new(), &[]).unwrap();
        let original_hash = first.snapshot.get("a.md").unwrap().content_hash.clone();

        // Modify the file — content + size both change.
        write(&path, "hello, world");

        let second = walk_folder(&cfg, &first.snapshot, &[]).unwrap();
        let new_hash = second.snapshot.get("a.md").unwrap().content_hash.clone();
        assert_ne!(original_hash, new_hash);
    }

    #[test]
    fn exclude_globs_filter_paths() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("a.md"), "keep");
        write(&dir.path().join("scratch/b.md"), "drop");

        let cfg = watched_cfg(dir.path());
        let out = walk_folder(&cfg, &HashMap::new(), &["scratch/**".to_string()]).unwrap();

        assert!(out.snapshot.contains_key("a.md"));
        assert!(!out.snapshot.contains_key("scratch/b.md"));
    }

    #[test]
    fn sovereignignore_filters_paths() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("a.md"), "keep");
        write(&dir.path().join("draft/b.md"), "drop");
        write(&dir.path().join(".sovereignignore"), "draft/\n");

        let cfg = watched_cfg(dir.path());
        let out = walk_folder(&cfg, &HashMap::new(), &[]).unwrap();

        assert!(out.snapshot.contains_key("a.md"));
        assert!(
            !out.snapshot.contains_key("draft/b.md"),
            "files under `draft/` should be ignored by .sovereignignore"
        );
    }

    #[test]
    fn sovereignignore_is_hot_reloaded_between_sweeps() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("scratch/n.md"), "scratch note");

        let cfg = watched_cfg(dir.path());

        // First sweep: no ignore file → file is picked up.
        let out1 = walk_folder(&cfg, &HashMap::new(), &[]).unwrap();
        assert!(out1.snapshot.contains_key("scratch/n.md"));

        // Add the ignore file and sweep again. No restart, no
        // matcher cache to bust — `walk_folder` re-reads the file
        // every call.
        write(&dir.path().join(".sovereignignore"), "scratch/\n");
        let out2 = walk_folder(&cfg, &out1.snapshot, &[]).unwrap();
        assert!(
            !out2.snapshot.contains_key("scratch/n.md"),
            "next sweep after writing .sovereignignore should drop the file"
        );
    }

    #[test]
    fn sovereignignore_supports_negation() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("scratch/keep.md"), "keep");
        write(&dir.path().join("scratch/drop.md"), "drop");
        write(
            &dir.path().join(".sovereignignore"),
            "scratch/\n!scratch/keep.md\n",
        );

        let cfg = watched_cfg(dir.path());
        let out = walk_folder(&cfg, &HashMap::new(), &[]).unwrap();

        assert!(
            out.snapshot.contains_key("scratch/keep.md"),
            "negation `!scratch/keep.md` should re-include the file"
        );
        assert!(!out.snapshot.contains_key("scratch/drop.md"));
    }

    #[test]
    fn sovereignignore_is_additive_to_exclude_globs() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("a.md"), "keep");
        write(&dir.path().join("flag/b.md"), "drop via cli");
        write(&dir.path().join("ignore/c.md"), "drop via file");
        write(&dir.path().join(".sovereignignore"), "ignore/\n");

        let cfg = watched_cfg(dir.path());
        let out = walk_folder(&cfg, &HashMap::new(), &["flag/**".to_string()]).unwrap();

        assert!(out.snapshot.contains_key("a.md"));
        assert!(!out.snapshot.contains_key("flag/b.md"));
        assert!(!out.snapshot.contains_key("ignore/c.md"));
    }

    #[test]
    fn invalid_glob_pattern_is_skipped_not_fatal() {
        let dir = tempdir().unwrap();
        write(&dir.path().join("a.md"), "ok");

        let cfg = watched_cfg(dir.path());
        // `[` is an unclosed character class — invalid pattern.
        let out = walk_folder(&cfg, &HashMap::new(), &["[unclosed".to_string()]).unwrap();
        assert!(out.snapshot.contains_key("a.md"));
    }
}
