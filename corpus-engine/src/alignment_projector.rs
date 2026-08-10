// SPDX-License-Identifier: AGPL-3.0-or-later
//! Alignment projector — materializes an `alignment` corpus's chunks
//! back to the filesystem under `<home>/.claude/`.
//!
//! Fires after `merge_partitions_into_canonical` when the merged
//! corpus carries `mutable_merge = SourceDocIdNewestMtime`, so a peer
//! pull → merge → projection cycle ends with the local FS holding the
//! newest-mtime version of every plan / memory / template file.
//!
//! Race-safety contract:
//!   - One-shot exclusive lock at `<home>/.claude/.alignment_lock`
//!     (O_CREAT|O_EXCL). If held, the projector reports `skipped_locked`
//!     and returns; another writer is mid-flight.
//!   - Per-file mtime-stable rule: skip when the on-disk file's mtime
//!     is greater than or equal to the chunk's mtime. The local edit
//!     wins in equal-time ties because the operator is presumed
//!     authoritative for in-flight writes.
//!   - Per-file atomic rename: write to `<path>.alignment-incoming`
//!     then `rename` into place, so a kill mid-write never leaves a
//!     half-written plan.
//!   - Path-traversal protection: source_doc_ids containing `..` or
//!     resolving outside `<home>/.claude/` are skipped with an
//!     `unsafe_path` count. Defends against a malicious peer crafting
//!     a chunk that would write outside the alignment surface.
//!   - Crash sweep: stale `*.alignment-incoming` artefacts left by a
//!     previous interrupted projection are removed at projector start.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use arrow_array::{Array, Int64Array, RecordBatch, StringArray};
use futures::TryStreamExt;
use lancedb::query::ExecutableQuery;

use crate::error::{Error, Result};
use crate::index::CorpusIndex;
use crate::recipe::MutableMergePolicy;

/// Outcome of a single projection pass. Reported back to the merge
/// caller so daemon logs / progress streams can show what landed
/// without re-walking the FS.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectReport {
    pub wrote: usize,
    pub skipped_local_newer: usize,
    pub skipped_locked: bool,
    pub skipped_unsafe_path: usize,
    pub swept_incoming: usize,
    /// Number of `notes://` rows upserted into `~/.svrnmesh/notes.db`.
    pub notes_upserted: usize,
    /// Number of `notes://` rows whose payload failed to deserialize
    /// or whose embedded id mismatched the chunk's source_doc_id.
    pub notes_deserialize_errors: usize,
}

/// Project every alignment row in the canonical at `canonical_path`
/// onto `<home>/.claude/`. Returns a per-call summary; never blocks
/// on the lock — if another projector is mid-flight, the call is a
/// no-op with `skipped_locked = true`.
///
/// Caller is expected to invoke this only when the canonical's
/// `_corpus_meta.json` carries
/// [`MutableMergePolicy::SourceDocIdNewestMtime`]; the function
/// does an explicit re-check and returns an empty report on
/// mismatch so an accidental call against a non-alignment corpus is
/// a no-op.
pub async fn project(canonical_path: &Path, home: &Path) -> Result<ProjectReport> {
    let claude_dir = home.join(".claude");
    if !claude_dir.exists() {
        // Nothing to project to. Treat as a benign no-op: a fresh
        // machine without ~/.claude/ shouldn't fail the merge.
        fs::create_dir_all(&claude_dir).map_err(|e| io_err(&claude_dir, "create", e))?;
    }

    let mut report = ProjectReport::default();
    report.swept_incoming = sweep_stale_incoming(&claude_dir);

    // Refuse to run against a non-mutable corpus. Defensive — the hook
    // only fires for the right policy, but a manual call from a CLI
    // shouldn't silently scribble onto ~/.claude/ from a Wikipedia
    // index.
    let index = CorpusIndex::open(canonical_path).await?;
    if !matches!(
        index.mutable_merge(),
        Some(MutableMergePolicy::SourceDocIdNewestMtime)
    ) {
        return Ok(report);
    }

    // Acquire the projection lock. O_CREAT|O_EXCL — a stale lock
    // file from a crash is the operator's problem to clear; we don't
    // assume liveness checks are correct.
    let lock_path = claude_dir.join(".alignment_lock");
    let _lock = match LockFile::acquire(&lock_path) {
        Ok(g) => g,
        Err(_) => {
            report.skipped_locked = true;
            return Ok(report);
        }
    };

    let batches: Vec<RecordBatch> = index
        .table()
        .query()
        .execute()
        .await
        .map_err(|e| Error::Database(format!("projector read: {e}")))?
        .try_collect()
        .await
        .map_err(|e| Error::Database(format!("projector collect: {e}")))?;

    // Buffer the `notes://...` chunks for one batched import after
    // the FS walk; one DB connection serves the whole pass.
    let mut note_chunks: Vec<(String, String)> = Vec::new();

    for batch in &batches {
        let content_col = batch
            .column_by_name("content")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let source_col = batch
            .column_by_name("source_doc_id")
            .and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let mtime_col = batch
            .column_by_name("mtime")
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>());

        let (Some(content_col), Some(source_col)) = (content_col, source_col) else {
            continue;
        };

        for row in 0..batch.num_rows() {
            if source_col.is_null(row) || content_col.is_null(row) {
                continue;
            }
            let source_doc_id = source_col.value(row);
            let content = content_col.value(row);
            let chunk_mtime = mtime_col
                .and_then(|c| {
                    if c.is_null(row) {
                        None
                    } else {
                        Some(c.value(row))
                    }
                })
                .unwrap_or(0);

            // Notes branch: defer to the SQLite upsert path.
            if source_doc_id.starts_with("notes://") {
                note_chunks.push((source_doc_id.to_string(), content.to_string()));
                continue;
            }

            match resolve_safe(&claude_dir, source_doc_id) {
                None => {
                    report.skipped_unsafe_path += 1;
                    tracing::warn!(
                        source_doc_id,
                        "alignment_projector: refused unsafe path; row dropped"
                    );
                }
                Some(target) => match write_if_newer(&target, content, chunk_mtime) {
                    Ok(WriteOutcome::Wrote) => report.wrote += 1,
                    Ok(WriteOutcome::SkippedLocalNewer) => report.skipped_local_newer += 1,
                    Err(e) => {
                        tracing::warn!(
                            source_doc_id,
                            target = %target.display(),
                            error = %e,
                            "alignment_projector: write failed; row skipped"
                        );
                    }
                },
            }
        }
    }

    // Notes import — same gate as `notes_sync`. On feature-stripped
    // builds the rows are silently dropped, which matches the
    // extractor's behaviour and keeps the projector a no-op rather
    // than a partial write.
    if !note_chunks.is_empty() {
        let notes_db = home.join(".sovereign").join("notes.db");
        let pairs = note_chunks.iter().map(|(s, c)| (s.as_str(), c.as_str()));
        match import_notes_compat(&notes_db, pairs) {
            Ok(notes_report) => {
                report.notes_upserted = notes_report.upserted;
                report.notes_deserialize_errors = notes_report.deserialize_errors;
            }
            Err(e) => tracing::warn!(
                error = %e,
                "alignment_projector: notes import failed; FS projection stands"
            ),
        }
    }

    tracing::info!(
        canonical = %canonical_path.display(),
        wrote = report.wrote,
        skipped_local_newer = report.skipped_local_newer,
        skipped_unsafe_path = report.skipped_unsafe_path,
        swept_incoming = report.swept_incoming,
        "alignment_projector: projection complete"
    );

    Ok(report)
}

/// Reject `source_doc_id`s that would escape `<home>/.claude/`.
/// Allows `plans/foo.md`, `projects/-Users-alex/memory/x.md`. Rejects
/// anything starting with `/`, `..`, or containing path components
/// that resolve outside `claude_dir`.
fn resolve_safe(claude_dir: &Path, source_doc_id: &str) -> Option<PathBuf> {
    if source_doc_id.is_empty() || source_doc_id.starts_with('/') {
        return None;
    }
    let rel = Path::new(source_doc_id);
    for c in rel.components() {
        match c {
            Component::Normal(_) => continue,
            // Reject anything resolved (Prefix/RootDir) or traversing
            // (`..`). `.` collapses to a no-op so allow it.
            Component::CurDir => continue,
            _ => return None,
        }
    }
    Some(claude_dir.join(rel))
}

enum WriteOutcome {
    Wrote,
    SkippedLocalNewer,
}

fn write_if_newer(target: &Path, content: &str, chunk_mtime: i64) -> Result<WriteOutcome> {
    if let Some(local_mtime) = on_disk_mtime(target) {
        if local_mtime >= chunk_mtime {
            return Ok(WriteOutcome::SkippedLocalNewer);
        }
    }

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| io_err(parent, "mkdir", e))?;
    }

    let staging = staging_path(target);
    fs::write(&staging, content).map_err(|e| io_err(&staging, "stage write", e))?;
    fs::rename(&staging, target).map_err(|e| {
        // Rename failed — clean up the staging file so the next sweep
        // doesn't accumulate scratch.
        let _ = fs::remove_file(&staging);
        io_err(&staging, "rename", e)
    })?;

    // Stamp the file's mtime to match the chunk's so a follow-up
    // re-ingest of the FS reproduces the same chunks (and so the
    // mtime-stable rule is symmetric across the merge).
    if chunk_mtime > 0 {
        let _ = filetime_set(target, chunk_mtime);
    }

    Ok(WriteOutcome::Wrote)
}

fn on_disk_mtime(path: &Path) -> Option<i64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() as i64)
}

fn staging_path(target: &Path) -> PathBuf {
    let mut s = target.as_os_str().to_owned();
    s.push(".alignment-incoming");
    PathBuf::from(s)
}

#[cfg(unix)]
fn filetime_set(path: &Path, mtime: i64) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    // utimensat via libc would be cleaner, but the std API requires
    // nightly. Open + sync is a no-op for mtime; instead use the
    // `filetime` crate if available — for now, set via touch-style
    // futimens by re-opening for write and using set_modified.
    let f = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(0)
        .open(path)?;
    let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime as u64);
    f.set_modified(when)?;
    Ok(())
}

#[cfg(not(unix))]
fn filetime_set(_path: &Path, _mtime: i64) -> std::io::Result<()> {
    Ok(())
}

/// Walk `<claude_dir>` recursively and remove `*.alignment-incoming`
/// scratch left by an earlier interrupted projection. Returns the
/// number of files swept.
fn sweep_stale_incoming(claude_dir: &Path) -> usize {
    fn walk(dir: &Path, count: &mut usize) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Skip the lock dir's own children if any future
                // version creates them; the lock itself is a single
                // file at the top level.
                walk(&path, count);
            } else if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.ends_with(".alignment-incoming") && fs::remove_file(&path).is_ok() {
                    *count += 1;
                }
            }
        }
    }
    let mut count = 0;
    walk(claude_dir, &mut count);
    count
}

/// Wrap a `std::io::Error` with a `(path, op)` context so the
/// resulting `Error::Io` displays usefully in tracing/test output.
fn io_err(path: &Path, op: &str, e: std::io::Error) -> Error {
    Error::Io(std::io::Error::new(
        e.kind(),
        format!("alignment_projector {op} {}: {e}", path.display()),
    ))
}

/// Minimal report shape so the projector's call site can stay
/// feature-agnostic. Mirrors `notes_sync::ImportReport` field names.
#[derive(Default)]
struct NotesImportReportShim {
    upserted: usize,
    deserialize_errors: usize,
}

#[cfg(feature = "treesitter")]
fn import_notes_compat<'a, I>(notes_db: &Path, chunks: I) -> Result<NotesImportReportShim>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let r = crate::notes_sync::import_notes_from_chunks(notes_db, chunks)?;
    Ok(NotesImportReportShim {
        upserted: r.upserted,
        deserialize_errors: r.deserialize_errors,
    })
}

#[cfg(not(feature = "treesitter"))]
fn import_notes_compat<'a, I>(_notes_db: &Path, _chunks: I) -> Result<NotesImportReportShim>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    // Notes sync requires the `treesitter` feature; without it the
    // notes:// rows are silently passed over rather than written
    // into a partial schema.
    Ok(NotesImportReportShim::default())
}

/// O_CREAT|O_EXCL lock-file guard. Drop removes the file. We keep
/// the API small — projection is one-shot and short-lived, so a
/// real fcntl advisory lock would be overkill.
struct LockFile {
    path: PathBuf,
}

impl LockFile {
    fn acquire(path: &Path) -> std::io::Result<Self> {
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{CorpusIndex, InsertChunk, InsertCodeMeta};
    use std::time::SystemTime;

    fn make_test_embedding(seed: f32) -> Vec<f32> {
        (0..8).map(|i| seed + i as f32 * 0.1).collect()
    }

    async fn alignment_canonical(dir: &Path, rows: &[(&str, i64, &str)]) -> PathBuf {
        let path = dir.join("alignment-canonical");
        let index = CorpusIndex::create(
            &path,
            "alignment",
            "Alignment",
            "test-model",
            8,
            true,
            "private",
        )
        .await
        .unwrap();
        let chunks: Vec<_> = rows
            .iter()
            .map(|(doc, mtime, body)| {
                (
                    InsertChunk {
                        content: (*body).into(),
                        title: Some((*doc).into()),
                        url: None,
                        metadata: None,
                        content_hash: Some(format!("h-{doc}-{mtime}")),
                        source_doc_id: Some((*doc).into()),
                        source_file: None,
                        code: InsertCodeMeta {
                            mtime: Some(*mtime),
                            ..Default::default()
                        },
                        unit_id: None,
                    },
                    make_test_embedding(*mtime as f32),
                )
            })
            .collect();
        index.insert_batch(&chunks).await.unwrap();
        index
            .set_mutable_merge(Some(MutableMergePolicy::SourceDocIdNewestMtime))
            .unwrap();
        path
    }

    #[tokio::test]
    async fn projects_chunks_to_filesystem() {
        let work = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let canonical = alignment_canonical(
            work.path(),
            &[
                ("plans/foo.md", 100, "foo body"),
                ("projects/-Users-alex/memory/keep.md", 200, "kept memory"),
            ],
        )
        .await;

        let report = project(&canonical, home.path()).await.unwrap();
        assert_eq!(report.wrote, 2);
        assert_eq!(report.skipped_local_newer, 0);
        assert_eq!(report.skipped_unsafe_path, 0);
        assert!(!report.skipped_locked);

        let foo = home.path().join(".claude/plans/foo.md");
        assert!(foo.exists());
        assert_eq!(std::fs::read_to_string(&foo).unwrap(), "foo body");
        let memory = home
            .path()
            .join(".claude/projects/-Users-alex/memory/keep.md");
        assert!(memory.exists());
        assert_eq!(std::fs::read_to_string(&memory).unwrap(), "kept memory");
    }

    #[tokio::test]
    async fn skips_when_local_newer() {
        let work = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let plans = home.path().join(".claude/plans");
        std::fs::create_dir_all(&plans).unwrap();
        let local = plans.join("foo.md");
        std::fs::write(&local, "local newer body").unwrap();
        // Stamp local mtime to a known-future value.
        let local_mtime = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 1_000;
        filetime_set(&local, local_mtime).unwrap();

        let canonical = alignment_canonical(
            work.path(),
            &[("plans/foo.md", local_mtime - 100, "remote older body")],
        )
        .await;

        let report = project(&canonical, home.path()).await.unwrap();
        assert_eq!(report.wrote, 0);
        assert_eq!(report.skipped_local_newer, 1);
        assert_eq!(
            std::fs::read_to_string(&local).unwrap(),
            "local newer body",
            "local edit preserved"
        );
    }

    #[tokio::test]
    async fn rejects_unsafe_paths() {
        let work = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let canonical = alignment_canonical(
            work.path(),
            &[
                ("../escape.md", 100, "would escape"),
                ("/abs/path.md", 100, "abs path"),
                ("plans/safe.md", 100, "safe"),
            ],
        )
        .await;
        let report = project(&canonical, home.path()).await.unwrap();
        assert_eq!(report.skipped_unsafe_path, 2);
        assert_eq!(report.wrote, 1);
        assert!(home.path().join(".claude/plans/safe.md").exists());
        assert!(!home.path().join("escape.md").exists());
    }

    #[tokio::test]
    async fn sweeps_stale_incoming() {
        let work = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let plans = home.path().join(".claude/plans");
        std::fs::create_dir_all(&plans).unwrap();
        let stale = plans.join("foo.md.alignment-incoming");
        std::fs::write(&stale, "leftover").unwrap();

        let canonical = alignment_canonical(work.path(), &[("plans/bar.md", 100, "fresh")]).await;
        let report = project(&canonical, home.path()).await.unwrap();
        assert_eq!(report.swept_incoming, 1);
        assert!(!stale.exists());
    }

    #[cfg(feature = "treesitter")]
    #[tokio::test]
    async fn projects_notes_chunk_into_local_notes_db() {
        let work = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();

        // Build a chunk whose source_doc_id encodes a notes:// row.
        let exported = serde_json::json!({
            "id": "n42",
            "kind": "decision",
            "content": "ride the mesh",
            "symbols": "[]",
            "files": "[]",
            "session_id": "s",
            "created_at": 100,
            "updated_at": 200,
            "scope": "global",
            "source": "agent",
        })
        .to_string();
        let canonical = work.path().join("alignment-canonical");
        let index = CorpusIndex::create(
            &canonical,
            "alignment",
            "Alignment",
            "test-model",
            8,
            true,
            "private",
        )
        .await
        .unwrap();
        index
            .insert_batch(&[(
                InsertChunk {
                    content: exported.clone(),
                    title: Some("note:n42".into()),
                    url: None,
                    metadata: None,
                    content_hash: Some("h".into()),
                    source_doc_id: Some("notes://n42".into()),
                    source_file: None,
                    code: InsertCodeMeta {
                        mtime: Some(200),
                        ..Default::default()
                    },
                    unit_id: None,
                },
                make_test_embedding(1.0),
            )])
            .await
            .unwrap();
        index
            .set_mutable_merge(Some(MutableMergePolicy::SourceDocIdNewestMtime))
            .unwrap();

        let report = project(&canonical, home.path()).await.unwrap();
        assert_eq!(report.notes_upserted, 1);
        assert_eq!(report.notes_deserialize_errors, 0);
        assert_eq!(report.wrote, 0, "no FS row in this corpus");

        let notes_db = home.path().join(".sovereign/notes.db");
        assert!(notes_db.exists(), "import created the DB");
        let conn = rusqlite::Connection::open(&notes_db).unwrap();
        let body: String = conn
            .query_row("SELECT content FROM notes WHERE id = 'n42'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(body, "ride the mesh");
    }

    #[tokio::test]
    async fn ignores_non_mutable_corpus() {
        let work = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        // Build a canonical without setting mutable_merge.
        let path = work.path().join("classic");
        let index = CorpusIndex::create(&path, "classic", "Classic", "test-model", 8, true, "MIT")
            .await
            .unwrap();
        index
            .insert_batch(&[(
                InsertChunk {
                    content: "ignore me".into(),
                    title: None,
                    url: None,
                    metadata: None,
                    content_hash: None,
                    source_doc_id: Some("plans/foo.md".into()),
                    source_file: None,
                    code: InsertCodeMeta::default(),
                    unit_id: None,
                },
                make_test_embedding(1.0),
            )])
            .await
            .unwrap();

        let report = project(&path, home.path()).await.unwrap();
        assert_eq!(report.wrote, 0);
        assert!(!home.path().join(".claude/plans/foo.md").exists());
    }
}
