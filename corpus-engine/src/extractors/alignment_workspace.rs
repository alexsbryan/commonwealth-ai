//! Alignment workspace extractor.
//!
//! Walks the user's `~/.claude/` tree and yields one `ExtractedDoc` per
//! markdown file under the plan / memory / template surface, with
//! `source_id` set to the path relative to the source root and `mtime`
//! plumbed through `metadata` so the schema's `mtime` column is
//! populated. Pairs with `mutable_merge =
//! "source_doc_id_newest_mtime"` so two daemons that edit the same
//! file converge on the newer copy after a mesh merge.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::json;

use super::{ExtractedDoc, Extractor};
use crate::error::{Error, Result};

/// Yields one `ExtractedDoc` per `.md` file under the source root that
/// matches the canonical alignment subset:
///
///   - `<source>/plans/**/*.md` — Claude Code plan files
///   - `<source>/projects/-*/memory/**/*.md` — auto-memory entries
///
/// The hidden lock + temp-write artifacts the post-merge projector
/// uses (`.alignment_lock`, `*.alignment-incoming`) are skipped so a
/// re-ingest never trips on its own scratch.
#[derive(Default)]
pub struct AlignmentWorkspaceExtractor;

impl Extractor for AlignmentWorkspaceExtractor {
    fn extract(
        &self,
        source_path: &Path,
    ) -> Result<Box<dyn Iterator<Item = Result<ExtractedDoc>> + Send>> {
        let mut files: Vec<PathBuf> = Vec::new();
        let plans_root = source_path.join("plans");
        if plans_root.is_dir() {
            walk_md(&plans_root, &mut files)?;
        }
        let projects_root = source_path.join("projects");
        if projects_root.is_dir() {
            for entry in fs::read_dir(&projects_root).map_err(io_err(&projects_root))? {
                let entry = entry.map_err(io_err(&projects_root))?;
                let memory_dir = entry.path().join("memory");
                if memory_dir.is_dir() {
                    walk_md(&memory_dir, &mut files)?;
                }
            }
        }

        // Pre-export notes.db rows once; the iterator yields them
        // after the markdown files are exhausted. The default
        // `~/.sovereign/notes.db` location is derived from the
        // source root: the recipe sets `[acquire] path = "~/.claude"`
        // so the source is `<home>/.claude` and the parent is
        // `<home>` — no `dirs` dep on corpus-engine needed.
        let notes_docs: Vec<ExtractedDoc> = derive_notes_db(source_path)
            .map(|db| export_notes_compat(&db))
            .unwrap_or_default();

        let root = source_path.to_path_buf();
        Ok(Box::new(AlignmentIterator {
            files: files.into(),
            root,
            extra: notes_docs.into(),
        }))
    }
}

/// Resolve the canonical notes.db location from the alignment source
/// path. Returns None when the source layout doesn't match what the
/// recipe specifies (`~/.claude`); callers treat that as "no notes
/// to sync."
fn derive_notes_db(source_path: &Path) -> Option<PathBuf> {
    let parent = source_path.parent()?;
    Some(parent.join(".sovereign").join("notes.db"))
}

/// Run the notes export when the `treesitter` feature (which gates
/// `notes_sync`) is enabled; otherwise return an empty list so the
/// extractor still produces markdown chunks on a feature-stripped
/// build.
#[cfg(feature = "treesitter")]
fn export_notes_compat(db: &Path) -> Vec<ExtractedDoc> {
    crate::notes_sync::export_notes_as_docs(db).unwrap_or_else(|e| {
        tracing::warn!(
            db = %db.display(),
            error = %e,
            "alignment_workspace: notes export failed; falling back to markdown only"
        );
        Vec::new()
    })
}

#[cfg(not(feature = "treesitter"))]
fn export_notes_compat(_db: &Path) -> Vec<ExtractedDoc> {
    Vec::new()
}

struct AlignmentIterator {
    files: VecDeque<PathBuf>,
    root: PathBuf,
    /// Notes / future non-file sources buffered up at construction
    /// time. Drained after the markdown walk completes so a single
    /// extractor call yields markdown + notes in one stream.
    extra: VecDeque<ExtractedDoc>,
}

impl Iterator for AlignmentIterator {
    type Item = Result<ExtractedDoc>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(path) = self.files.pop_front() {
                match read_md_file(&path, &self.root) {
                    Ok(Some(doc)) => return Some(Ok(doc)),
                    Ok(None) => continue,
                    Err(e) => return Some(Err(e)),
                }
            }
            return self.extra.pop_front().map(Ok);
        }
    }
}

fn walk_md(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).map_err(io_err(dir))? {
        let entry = entry.map_err(io_err(dir))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Skip the projector's own scratch artifacts.
        if name == ".alignment_lock" || name.ends_with(".alignment-incoming") {
            continue;
        }
        // Skip dotfiles generally — Claude Code writes nothing useful
        // there for our purposes and the dot prefix usually means
        // session-local.
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk_md(&path, out)?;
        } else if name.ends_with(".md") {
            out.push(path);
        }
    }
    Ok(())
}

fn read_md_file(path: &Path, root: &Path) -> Result<Option<ExtractedDoc>> {
    let body = fs::read_to_string(path).map_err(|e| {
        Error::Extraction(format!("alignment_workspace read {}: {e}", path.display()))
    })?;
    if body.trim().is_empty() {
        return Ok(None);
    }
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    let title = path.file_stem().and_then(|s| s.to_str()).map(String::from);
    let mtime = fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok(Some(ExtractedDoc {
        title,
        content: body,
        url: None,
        source_id: rel,
        // `mtime` rides through metadata so `code_meta_from_json`
        // (the shared insert-time hook) lifts it into the schema's
        // `mtime` column. Mutable-merge consults that column.
        metadata: Some(json!({ "mtime": mtime })),
        source_file: None,
        embed_text: None,
    }))
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> Error {
    let p = path.display().to_string();
    move |e| Error::Extraction(format!("alignment_workspace walk {p}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(tmp: &Path, rel: &str, body: &str) {
        let p = tmp.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn walks_plans_and_memory_subset() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "plans/foo.md", "# Foo plan");
        write(root, "plans/_TEMPLATE.md", "# Template");
        write(
            root,
            "projects/-Users-alex-repo/memory/feedback_x.md",
            "feedback body",
        );
        write(root, "plans/.hidden.md", "should skip");
        write(root, "plans/foo.md.alignment-incoming", "should skip");
        write(
            root,
            "projects/-Users-alex-repo/other/note.md",
            "skip non-memory",
        );
        write(root, "elsewhere/x.md", "skip elsewhere");

        let ext = AlignmentWorkspaceExtractor;
        let mut docs: Vec<ExtractedDoc> = ext.extract(root).unwrap().map(|r| r.unwrap()).collect();
        docs.sort_by(|a, b| a.source_id.cmp(&b.source_id));

        let ids: Vec<&str> = docs.iter().map(|d| d.source_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "plans/_TEMPLATE.md",
                "plans/foo.md",
                "projects/-Users-alex-repo/memory/feedback_x.md",
            ],
        );

        // mtime is plumbed through metadata.
        for d in &docs {
            let m = d.metadata.as_ref().expect("metadata present");
            let mtime = m.get("mtime").and_then(|v| v.as_i64()).unwrap_or(0);
            assert!(mtime > 0, "mtime populated for {}", d.source_id);
        }
    }

    #[test]
    fn empty_files_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "plans/empty.md", "   \n\t\n");
        let ext = AlignmentWorkspaceExtractor;
        let docs: Vec<_> = ext.extract(root).unwrap().collect();
        assert_eq!(docs.len(), 0);
    }
}
