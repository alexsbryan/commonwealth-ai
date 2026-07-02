// SPDX-License-Identifier: AGPL-3.0-or-later
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
    /// Folder-ingest v1 §3.1: which root produced this entry. `0`
    /// is the primary `LocalCorpusConfig.root_path`; `1..` map
    /// 1:1 onto `WatchedFolderConfig.additional_roots[idx-1]`.
    /// Persisted so the manager can soft-delete a removed root's
    /// entries without re-walking, and so the UI can group entries
    /// by root in the detail view.
    ///
    /// `#[serde(default)]` keeps pre-v1 state files round-tripping
    /// (every existing entry was implicitly from the primary root).
    #[serde(default)]
    pub source_root_index: u8,
    /// Cross-root duplicate paths. When a file with this same
    /// `content_hash` was found under another root, the canonical
    /// (lex-first) entry surfaces here as `aux_paths`. Empty when
    /// the file is unique. Phase D.2 dedup populates this; Phase
    /// D.1 leaves it empty.
    #[serde(default)]
    pub aux_paths: Vec<PathBuf>,
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
/// `exclude_globs` are matched against the path relative to each
/// root (not the absolute path), using `glob::Pattern`.
///
/// Folder-ingest v1 §3.1 — multi-root: walks `config.root_path`
/// first (entries are keyed by their plain relative path, byte-
/// identical to the pre-v1 single-root layout), then each
/// `WatchedFolderConfig.additional_roots` in order (entries
/// namespaced under `_r{idx}/` to keep same-relative-path-different-
/// content across roots from clobbering each other). For corpora
/// with no additional roots, output is identical to the pre-v1
/// shape — adding multi-root support doesn't churn existing state.
pub fn walk_folder(
    config: &LocalCorpusConfig,
    prior_meta: &HashMap<String, EntryRecord>,
    exclude_globs: &[String],
) -> std::io::Result<WalkOutcome> {
    let mut combined_snapshot = WalkSnapshot::new();
    let mut combined_raw = PreScanResult::default();
    let mut total_visited = 0usize;

    // Build the iteration list: primary root first (root_index = 0),
    // then each additional root in declared order (root_index = 1..).
    // The primary keeps its plain doc_id shape so a corpus with zero
    // additional roots stays byte-identical to the pre-v1 layout.
    let additional: Vec<&Path> = match &config.source_type {
        crate::local_corpus::config::LocalCorpusSourceType::WatchedFolder(w) => w
            .additional_roots
            .iter()
            .map(|r| r.path.as_path())
            .collect(),
        _ => Vec::new(),
    };
    let mut roots: Vec<(u8, &Path)> = Vec::with_capacity(1 + additional.len());
    roots.push((0, config.root_path.as_path()));
    for (i, p) in additional.iter().enumerate() {
        roots.push((u8::try_from(i + 1).unwrap_or(u8::MAX), *p));
    }

    for (root_index, root_path) in roots {
        let outcome = walk_one_root(config, root_path, root_index, prior_meta, exclude_globs)?;
        // Merge per-root snapshot into the combined snapshot. The
        // doc_id namespacing (`_r{n}/...` for additional roots)
        // means primary and additional entries don't collide.
        // Same-relative-path-different-content across two
        // additional roots also doesn't collide because each gets
        // its own `_r{n}/` prefix.
        for (doc_id, entry) in outcome.snapshot {
            combined_snapshot.insert(doc_id, entry);
        }
        // Merge raw counts. Counters add; per-file lists concat.
        combined_raw.total_visited = combined_raw
            .total_visited
            .saturating_add(outcome.raw.total_visited);
        combined_raw.ignored_types = combined_raw
            .ignored_types
            .saturating_add(outcome.raw.ignored_types);
        for (ext, count) in outcome.raw.skipped_by_extension {
            *combined_raw.skipped_by_extension.entry(ext).or_insert(0) += count;
        }
        combined_raw.readable.extend(outcome.raw.readable);
        combined_raw.large_files.extend(outcome.raw.large_files);
        combined_raw.corrupt_files.extend(outcome.raw.corrupt_files);
        combined_raw
            .protected_pdfs
            .extend(outcome.raw.protected_pdfs);
        combined_raw.scanned_pdfs.extend(outcome.raw.scanned_pdfs);
        total_visited = total_visited.saturating_add(outcome.visited);
    }

    // Folder-ingest v1 §3.1 cross-root dedup: when a file with
    // identical content surfaces under two roots, fold the
    // alternate path into the canonical entry's `aux_paths`. The
    // non-canonical entries are dropped from the snapshot so
    // `apply_update` only writes one chunk set per content_hash.
    // Both source paths survive on the canonical for inspection.
    let combined_snapshot = dedupe_by_content_hash(combined_snapshot);

    Ok(WalkOutcome {
        snapshot: combined_snapshot,
        raw: combined_raw,
        visited: total_visited,
    })
}

/// Group `snapshot` entries by `content_hash`; for each group with
/// more than one entry, the canonical wins as `(source_root_index,
/// doc_id)` ordered ascending and the alternates' `absolute_path`s
/// are folded onto its `aux_paths`. Non-canonical entries are
/// dropped from the returned snapshot.
///
/// `(source_root_index, doc_id)` ordering is deliberate:
/// 1. Primary root (idx 0) wins over additional roots — matches the
///    user's intuition that the path they originally registered is
///    the canonical location, with later-added roots as
///    alternates.
/// 2. Within the same root, lex-first doc_id wins — deterministic
///    across sweeps, so the canonical doesn't churn even if
///    HashMap iteration order shifts.
fn dedupe_by_content_hash(snapshot: WalkSnapshot) -> WalkSnapshot {
    use std::collections::HashMap;
    let mut groups: HashMap<String, Vec<(String, EntryRecord)>> = HashMap::new();
    for (doc_id, entry) in snapshot {
        groups
            .entry(entry.content_hash.clone())
            .or_default()
            .push((doc_id, entry));
    }

    let mut out = WalkSnapshot::new();
    for (hash, mut group) in groups {
        if group.len() == 1 {
            let (doc_id, entry) = group.into_iter().next().unwrap();
            out.insert(doc_id, entry);
            continue;
        }
        group.sort_by(|a, b| {
            a.1.source_root_index
                .cmp(&b.1.source_root_index)
                .then_with(|| a.0.cmp(&b.0))
        });
        let mut iter = group.into_iter();
        let (canonical_doc_id, mut canonical_entry) =
            iter.next().expect("group has at least 1 entry");
        for (alternate_doc_id, alternate_entry) in iter {
            tracing::debug!(
                hash = %hash,
                canonical = %canonical_doc_id,
                alternate = %alternate_doc_id,
                "watched_folder:cross_root_dedup"
            );
            canonical_entry
                .aux_paths
                .push(alternate_entry.absolute_path);
        }
        out.insert(canonical_doc_id, canonical_entry);
    }
    out
}

/// One pass over a single root. Used internally by `walk_folder` to
/// iterate over the primary root + each additional root. The
/// `root_index` is stamped onto every produced `EntryRecord` and
/// also drives the `_r{n}/` doc_id prefix for non-primary roots.
fn walk_one_root(
    config: &LocalCorpusConfig,
    root_path: &Path,
    root_index: u8,
    prior_meta: &HashMap<String, EntryRecord>,
    exclude_globs: &[String],
) -> std::io::Result<WalkOutcome> {
    let scanner = PreScanner::with_root(config, root_path);
    let raw = scanner.run_blocking(|_, _| {});
    let visited = raw.total_visited as usize;

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

    let sovereignignore_matcher = build_sovereignignore_matcher(root_path);

    let mut snapshot = WalkSnapshot::new();

    let mut considered: Vec<&FileMeta> =
        Vec::with_capacity(raw.readable.len() + raw.large_files.len());
    considered.extend(&raw.readable);
    considered.extend(&raw.large_files);

    for meta in considered {
        let Some(rel) = doc_id_for(root_path, &meta.path) else {
            tracing::warn!(
                path = %meta.path.display(),
                root = %root_path.display(),
                "watched_folder:doc_id_failed"
            );
            continue;
        };

        if compiled.iter().any(|p| p.matches(&rel)) {
            continue;
        }
        if let Some(matcher) = &sovereignignore_matcher {
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

        // Apply the multi-root namespace. Primary root (idx 0)
        // keeps the plain relative path so a single-root corpus is
        // byte-identical to the pre-v1 layout; additional roots
        // are prefixed `_r{idx}/...` to disambiguate.
        let doc_id = if root_index == 0 {
            rel
        } else {
            format!("_r{}/{}", root_index, rel)
        };

        let content_hash = match prior_meta.get(&doc_id) {
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
            doc_id,
            EntryRecord {
                absolute_path: meta.path.clone(),
                mtime_unix,
                size_bytes,
                content_hash,
                source_root_index: root_index,
                aux_paths: Vec::new(),
            },
        );
    }

    Ok(WalkOutcome {
        snapshot,
        raw,
        visited,
    })
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

/// Build a gitignore matcher from `{root}/.svrnmeshignore` (preferred) or the
/// legacy `{root}/.sovereignignore` (back-compat during the rebrand). Returns
/// `None` when neither file exists (the common case). Soft-fails on read or
/// parse errors with a `warn!` rather than wedging the sweep — a malformed
/// ignore file shouldn't take a corpus offline.
pub fn build_sovereignignore_matcher(root: &Path) -> Option<ignore::gitignore::Gitignore> {
    let path = {
        let new = root.join(".svrnmeshignore");
        if new.exists() {
            new
        } else {
            root.join(".sovereignignore")
        }
    };
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
                source_root_index: 0,
                aux_paths: Vec::new(),
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
        write(&dir.path().join(".svrnmeshignore"), "draft/\n");

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

    #[test]
    fn multi_root_walks_primary_and_additional_with_namespacing() {
        // Folder-ingest v1 §3.1: multi-root walker namespaces
        // additional roots' doc_ids under `_r{n}/` so a corpus
        // with overlapping relative paths across roots doesn't
        // clobber. The primary root keeps its plain doc_id shape
        // for byte-identical compat with single-root corpora.
        use crate::local_corpus::config::{
            LocalCorpusConfig, LocalCorpusSourceType, RootSpec, WatchedFolderConfig,
        };

        let primary = tempdir().unwrap();
        let additional = tempdir().unwrap();

        // Same relative path (`notes/a.md`) in both roots, with
        // different content. Without namespacing, the second walk
        // would clobber the first; with `_r1/` prefix, both
        // survive into the snapshot.
        write(&primary.path().join("notes/a.md"), "from primary");
        write(&primary.path().join("only-primary.txt"), "p");
        write(&additional.path().join("notes/a.md"), "from additional");
        write(&additional.path().join("only-additional.txt"), "a");

        let mut watched = WatchedFolderConfig::default();
        watched.additional_roots = vec![RootSpec {
            path: additional.path().to_path_buf(),
            added_at_unix: 0,
        }];
        let mut cfg = LocalCorpusConfig::watched_folder(
            primary.path().to_path_buf(),
            "multi-root".into(),
            watched.clone(),
        );
        // The factory canonicalises the primary path; mirror that
        // in source_type so the walker sees the same WatchedFolderConfig
        // that would be used in production.
        cfg.source_type = LocalCorpusSourceType::WatchedFolder(watched);

        let out = walk_folder(&cfg, &HashMap::new(), &[]).unwrap();

        // Primary entries: plain relative paths.
        assert!(out.snapshot.contains_key("notes/a.md"));
        assert!(out.snapshot.contains_key("only-primary.txt"));
        // Additional root entries: `_r1/` prefix.
        assert!(out.snapshot.contains_key("_r1/notes/a.md"));
        assert!(out.snapshot.contains_key("_r1/only-additional.txt"));
        // Both `notes/a.md`s coexist with distinct hashes.
        let primary_hash = &out.snapshot.get("notes/a.md").unwrap().content_hash;
        let additional_hash = &out.snapshot.get("_r1/notes/a.md").unwrap().content_hash;
        assert_ne!(
            primary_hash, additional_hash,
            "different content under same relative path should yield different hashes"
        );
        // source_root_index correctly stamped.
        assert_eq!(out.snapshot.get("notes/a.md").unwrap().source_root_index, 0);
        assert_eq!(
            out.snapshot
                .get("_r1/notes/a.md")
                .unwrap()
                .source_root_index,
            1
        );
        // Total: 4 entries (2 per root).
        assert_eq!(out.snapshot.len(), 4);
    }

    #[test]
    fn cross_root_content_hash_dedup_folds_into_aux_paths() {
        // Folder-ingest v1 §3.1: a file with identical content
        // under two roots becomes one canonical entry with the
        // alternate path on `aux_paths` — apply_update writes
        // one chunk set per content, both source paths visible.
        use crate::local_corpus::config::{
            LocalCorpusConfig, LocalCorpusSourceType, RootSpec, WatchedFolderConfig,
        };

        let primary = tempdir().unwrap();
        let additional = tempdir().unwrap();

        // Same content in both roots, different relative paths.
        write(&primary.path().join("shared.md"), "identical body");
        write(
            &additional.path().join("backup/shared.md"),
            "identical body",
        );
        // A unique file in each root for sanity.
        write(&primary.path().join("only-primary.txt"), "p");
        write(&additional.path().join("only-additional.txt"), "a");

        let mut watched = WatchedFolderConfig::default();
        watched.additional_roots = vec![RootSpec {
            path: additional.path().to_path_buf(),
            added_at_unix: 0,
        }];
        let mut cfg = LocalCorpusConfig::watched_folder(
            primary.path().to_path_buf(),
            "dedup-test".into(),
            watched.clone(),
        );
        cfg.source_type = LocalCorpusSourceType::WatchedFolder(watched);

        let out = walk_folder(&cfg, &HashMap::new(), &[]).unwrap();

        // 3 entries (4 files, 1 cross-root dup folded).
        assert_eq!(
            out.snapshot.len(),
            3,
            "got {:?}",
            out.snapshot.keys().collect::<Vec<_>>()
        );

        // Canonical is the lex-first doc_id of the dup group.
        // Primary's `shared.md` < additional's `_r1/backup/shared.md`,
        // so the primary entry wins.
        let canonical = out
            .snapshot
            .get("shared.md")
            .expect("canonical (primary `shared.md`) should survive dedup");
        assert_eq!(canonical.aux_paths.len(), 1);
        assert!(
            canonical.aux_paths[0]
                .to_string_lossy()
                .contains("backup/shared.md"),
            "expected additional root's path on aux_paths, got {:?}",
            canonical.aux_paths
        );

        // The non-canonical `_r1/backup/shared.md` is gone.
        assert!(!out.snapshot.contains_key("_r1/backup/shared.md"));

        // Unique files unaffected.
        assert!(out.snapshot.contains_key("only-primary.txt"));
        assert!(out.snapshot.contains_key("_r1/only-additional.txt"));
        for (doc_id, entry) in &out.snapshot {
            if doc_id != "shared.md" {
                assert!(
                    entry.aux_paths.is_empty(),
                    "{doc_id} should not have aux_paths"
                );
            }
        }
    }

    #[test]
    fn single_root_layout_is_byte_identical_to_pre_v1() {
        // Pinning test: a corpus with `additional_roots: vec![]`
        // must produce the exact same doc_ids it did before
        // multi-root landed. Otherwise every existing watched
        // corpus would lose its index on upgrade.
        let dir = tempdir().unwrap();
        write(&dir.path().join("notes/a.md"), "hello");
        write(&dir.path().join("b.txt"), "world");

        let cfg = watched_cfg(dir.path());
        let out = walk_folder(&cfg, &HashMap::new(), &[]).unwrap();

        assert!(out.snapshot.contains_key("notes/a.md"));
        assert!(out.snapshot.contains_key("b.txt"));
        // No `_r0/` prefix on primary — byte-identical to pre-v1.
        assert!(!out.snapshot.keys().any(|k| k.starts_with("_r0/")));
        assert_eq!(out.snapshot.len(), 2);
        // Every entry's source_root_index is 0.
        for entry in out.snapshot.values() {
            assert_eq!(entry.source_root_index, 0);
            assert!(entry.aux_paths.is_empty());
        }
    }
}
