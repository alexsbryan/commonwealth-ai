//! Write-back coordinator: snapshot → write tags → (index notes) →
//! rollback / clean. Everything in this module has filesystem side
//! effects; the rest of `local_corpus` is purely in-memory
//! transformation.
//!
//! Invariants enforced here (spec §6.5):
//!   - Snapshot is taken BEFORE the first file is touched. If the
//!     snapshot write fails, no tag write ever runs.
//!   - Every file write is atomic: write-to-tempfile-in-same-dir +
//!     rename. A crash mid-write never leaves a half-written note.
//!   - Snapshot directory lives OUTSIDE the vault
//!     (`~/.sovereign/vault-snapshots/{corpus_id}/`). Storing inside
//!     the vault would ingest the snapshots on the next re-scan.
//!   - Retention: 3 most recent snapshots per corpus. `take_snapshot`
//!     prunes on-write.
//!   - Only `<namespace>/*` tags and `<namespace>_*` keys are
//!     touched. Everything else round-trips via
//!     [`frontmatter::merge_frontmatter`].

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sovereign_core::error::{Error, Result};

use super::config::WriteBackConfig;
use super::frontmatter::{self, MergeInputs};
use super::preview::{ClusterSummary, FileAssignment, VaultPreview};

// ─── Types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteSnapshot {
    pub vault_path: PathBuf,
    pub corpus_id: String,
    pub taken_at: DateTime<Utc>,
    pub sovereign_version: u32,
    pub git_commit: Option<String>,
    pub entries: Vec<SnapshotEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub relative_path: String,
    /// Full file contents before Sovereign touched it. We store the
    /// whole file (not just the frontmatter block) so rollback is a
    /// single `fs::write` and we never reconstruct user body text
    /// from a diff.
    pub raw_before: String,
    /// Whether the file existed at snapshot time. Files created by
    /// the write-back (e.g. cluster index notes) have `false` here
    /// so rollback knows to delete rather than restore them.
    pub existed_before: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub taken_at: DateTime<Utc>,
    pub sovereign_version: u32,
    pub file_count: usize,
    pub git_commit: Option<String>,
    pub snapshot_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteBackResult {
    pub files_tagged: usize,
    pub files_skipped: Vec<FailedWrite>,
    pub index_notes_created: usize,
    pub snapshot_path: PathBuf,
    pub sovereign_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedWrite {
    pub relative_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackResult {
    pub files_restored: usize,
    pub files_skipped: Vec<FailedWrite>,
    pub index_notes_deleted: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanResult {
    pub tags_removed_from: usize,
    pub index_notes_deleted: usize,
}

// ─── WriteBack ───────────────────────────────────────────────────────

pub struct WriteBack {
    pub config: WriteBackConfig,
    pub vault_path: PathBuf,
    pub corpus_id: String,
}

impl WriteBack {
    pub fn new(config: WriteBackConfig, vault_path: PathBuf, corpus_id: String) -> Self {
        Self {
            config,
            vault_path,
            corpus_id,
        }
    }

    /// Snapshot the current contents of every file we're about to
    /// touch (plus the index-note paths so rollback can delete them).
    /// Writes the JSON atomically before returning.
    pub async fn take_snapshot(
        &self,
        preview: &VaultPreview,
        version: u32,
        git_commit: Option<String>,
    ) -> Result<(WriteSnapshot, PathBuf)> {
        let mut entries = Vec::new();

        // 1. Files that will be tagged.
        for summary in &preview.clusters {
            for a in &summary.assignments {
                let abs = self.absolute_note_path(&a.relative_path);
                entries.push(entry_from_path(&abs, &a.relative_path)?);
            }
        }

        // 2. Index notes we're about to generate. We might overwrite
        //    an existing user file by accident — recording the prior
        //    state means rollback either restores it or deletes the
        //    freshly-written one.
        for summary in &preview.clusters {
            let rel = self.cluster_index_relative_path(&summary.cluster.tag_path);
            let abs = self.absolute_note_path(&rel);
            entries.push(entry_from_path(&abs, &rel)?);
        }

        let snapshot = WriteSnapshot {
            vault_path: self.vault_path.clone(),
            corpus_id: self.corpus_id.clone(),
            taken_at: Utc::now(),
            sovereign_version: version,
            git_commit,
            entries,
        };

        let path = self.snapshot_file_path(version);
        atomic_write_json(&path, &snapshot)?;
        self.prune_old_snapshots()?;
        Ok((snapshot, path))
    }

    /// Merge + atomically write one note's frontmatter.
    pub fn write_file_tags(
        &self,
        assignment: &FileAssignment,
        cluster_display_name: &str,
        version: u32,
    ) -> std::result::Result<(), FailedWrite> {
        let abs = self.absolute_note_path(&assignment.relative_path);
        let existing = std::fs::read_to_string(&abs).map_err(|e| FailedWrite {
            relative_path: assignment.relative_path.clone(),
            reason: format!("read: {e}"),
        })?;

        let inputs = MergeInputs {
            primary_tag: &assignment.primary_tag,
            additional_tags: &assignment.additional_tags,
            cluster_display_name,
            confidence: assignment.confidence,
            version,
        };
        let merged = frontmatter::merge_frontmatter(&existing, &inputs, &self.config.namespace);

        atomic_write_string(&abs, &merged).map_err(|e| FailedWrite {
            relative_path: assignment.relative_path.clone(),
            reason: format!("write: {e}"),
        })
    }

    /// Render + write a Map-of-Content index note for one cluster.
    pub fn write_cluster_index(
        &self,
        summary: &ClusterSummary,
        version: u32,
    ) -> std::result::Result<PathBuf, FailedWrite> {
        let rel = self.cluster_index_relative_path(&summary.cluster.tag_path);
        let abs = self.absolute_note_path(&rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| FailedWrite {
                relative_path: rel.clone(),
                reason: format!("mkdir: {e}"),
            })?;
        }
        let body = render_cluster_index(summary, &self.config.namespace, version);
        atomic_write_string(&abs, &body).map_err(|e| FailedWrite {
            relative_path: rel,
            reason: format!("write: {e}"),
        })?;
        Ok(abs)
    }

    /// Orchestrate the full write-back: snapshot → tag every
    /// assignment → write one index note per cluster. Individual
    /// failures are collected; the overall operation does NOT abort
    /// on a per-file error (the user can rollback the whole thing).
    pub async fn execute(
        &self,
        preview: &VaultPreview,
        version: u32,
        git_commit: Option<String>,
    ) -> Result<WriteBackResult> {
        let (_snapshot, snapshot_path) = self.take_snapshot(preview, version, git_commit).await?;

        let mut files_tagged = 0;
        let mut files_skipped = Vec::new();

        for summary in &preview.clusters {
            for a in &summary.assignments {
                match self.write_file_tags(a, &summary.cluster.display_name, version) {
                    Ok(()) => files_tagged += 1,
                    Err(f) => files_skipped.push(f),
                }
            }
        }

        let mut index_notes_created = 0;
        for summary in &preview.clusters {
            match self.write_cluster_index(summary, version) {
                Ok(_) => index_notes_created += 1,
                Err(f) => files_skipped.push(f),
            }
        }

        Ok(WriteBackResult {
            files_tagged,
            files_skipped,
            index_notes_created,
            snapshot_path,
            sovereign_version: version,
        })
    }

    /// Restore the vault to a previous snapshot. Files that no
    /// longer exist are skipped, not errored. Index notes that were
    /// created fresh (i.e. `existed_before == false`) are deleted.
    pub async fn rollback(&self, snapshot: &WriteSnapshot) -> Result<RollbackResult> {
        let mut files_restored = 0;
        let mut files_skipped = Vec::new();
        let mut index_notes_deleted = 0;

        for entry in &snapshot.entries {
            let abs = self.absolute_note_path(&entry.relative_path);
            if entry.existed_before {
                if let Err(e) = atomic_write_string(&abs, &entry.raw_before) {
                    files_skipped.push(FailedWrite {
                        relative_path: entry.relative_path.clone(),
                        reason: format!("restore: {e}"),
                    });
                } else {
                    files_restored += 1;
                }
            } else {
                // File was created by write-back. Remove if still
                // present.
                if abs.exists() {
                    match std::fs::remove_file(&abs) {
                        Ok(()) => index_notes_deleted += 1,
                        Err(e) => files_skipped.push(FailedWrite {
                            relative_path: entry.relative_path.clone(),
                            reason: format!("delete: {e}"),
                        }),
                    }
                }
            }
        }

        Ok(RollbackResult {
            files_restored,
            files_skipped,
            index_notes_deleted,
        })
    }

    /// List every snapshot we've taken for this corpus, newest first.
    pub fn list_snapshots(&self) -> Result<Vec<SnapshotMeta>> {
        let dir = &self.config.snapshot_dir;
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(out);
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(snap) = serde_json::from_str::<WriteSnapshot>(&raw) else {
                continue;
            };
            out.push(SnapshotMeta {
                taken_at: snap.taken_at,
                sovereign_version: snap.sovereign_version,
                file_count: snap.entries.len(),
                git_commit: snap.git_commit,
                snapshot_path: path,
            });
        }
        out.sort_by(|a, b| b.taken_at.cmp(&a.taken_at));
        Ok(out)
    }

    /// Load a specific snapshot by path. Used by rollback to avoid
    /// re-walking the directory.
    pub fn load_snapshot(&self, snapshot_path: &Path) -> Result<WriteSnapshot> {
        let raw = std::fs::read_to_string(snapshot_path)
            .map_err(|e| Error::Execution(format!("read snapshot: {e}")))?;
        serde_json::from_str(&raw)
            .map_err(|e| Error::Execution(format!("parse snapshot: {e}")))
    }

    /// Remove all `<namespace>/*` tags and `<namespace>_*` keys from
    /// every markdown file in the vault, and delete the index-note
    /// directory. Does NOT touch snapshots — user intent is "remove
    /// Sovereign from my vault", snapshots are stored outside it.
    pub async fn clean(&self) -> Result<CleanResult> {
        use walkdir::WalkDir;
        let mut tags_removed_from = 0;
        let namespace = &self.config.namespace;

        for entry in WalkDir::new(&self.vault_path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || !is_hidden(e.path()))
        {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            if entry.path().extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let abs = entry.path();
            let Ok(raw) = std::fs::read_to_string(abs) else {
                continue;
            };
            let stripped = frontmatter::strip_sovereign(&raw, namespace);
            if stripped != raw {
                if atomic_write_string(abs, &stripped).is_ok() {
                    tags_removed_from += 1;
                }
            }
        }

        let index_dir = self.vault_path.join(&self.config.index_dir);
        let index_notes_deleted = if index_dir.exists() {
            let count = count_files(&index_dir);
            let _ = std::fs::remove_dir_all(&index_dir);
            count
        } else {
            0
        };

        Ok(CleanResult {
            tags_removed_from,
            index_notes_deleted,
        })
    }

    // ─── Path helpers ────────────────────────────────────────────

    fn absolute_note_path(&self, relative: &str) -> PathBuf {
        self.vault_path.join(relative)
    }

    fn cluster_index_relative_path(&self, tag_path: &str) -> String {
        // `{index_dir}/{tag_path}.md` — tag paths are
        // "epistemology/philosophy-of-mind" so the nested directories
        // mirror the tag hierarchy in Obsidian's file tree. Obsidian
        // treats this as a normal markdown file.
        format!("{}/{}.md", self.config.index_dir, tag_path)
    }

    fn snapshot_file_path(&self, version: u32) -> PathBuf {
        let ts = Utc::now().format("%Y%m%dT%H%M%SZ");
        self.config
            .snapshot_dir
            .join(format!("snapshot-v{version:04}-{ts}.json"))
    }

    fn prune_old_snapshots(&self) -> Result<()> {
        let snaps = self.list_snapshots()?;
        let retention = self.config.snapshot_retention.max(1);
        for extra in snaps.into_iter().skip(retention) {
            let _ = std::fs::remove_file(&extra.snapshot_path);
        }
        Ok(())
    }
}

// ─── I/O primitives ──────────────────────────────────────────────────

fn entry_from_path(abs: &Path, relative: &str) -> Result<SnapshotEntry> {
    match std::fs::read_to_string(abs) {
        Ok(raw) => Ok(SnapshotEntry {
            relative_path: relative.to_string(),
            raw_before: raw,
            existed_before: true,
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SnapshotEntry {
            relative_path: relative.to_string(),
            raw_before: String::new(),
            existed_before: false,
        }),
        Err(e) => Err(Error::Execution(format!(
            "snapshot read {}: {e}",
            abs.display()
        ))),
    }
}

pub(crate) fn atomic_write_string(path: &Path, contents: &str) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent dir"))?;
    std::fs::create_dir_all(parent)?;
    let tmp = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::write(tmp.path(), contents.as_bytes())?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Execution(format!("mkdir {}: {e}", parent.display())))?;
    }
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| Error::Execution(format!("serialize snapshot: {e}")))?;
    atomic_write_string(path, &json)
        .map_err(|e| Error::Execution(format!("write snapshot: {e}")))
}

fn is_hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with('.') && n != "." && n != "..")
        .unwrap_or(false)
}

fn count_files(dir: &Path) -> usize {
    use walkdir::WalkDir;
    WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .count()
}

// ─── Index-note rendering ────────────────────────────────────────────

fn render_cluster_index(summary: &ClusterSummary, namespace: &str, version: u32) -> String {
    // Spec §6.6 template. Tags in the note's own frontmatter point
    // at `<ns>/index` and `<ns>/<tag_path>` so Obsidian's graph view
    // treats the MoC note as part of the cluster.
    let tag_path = &summary.cluster.tag_path;
    let display = &summary.cluster.display_name;
    let now = Utc::now().to_rfc3339();

    let mut core_rows = String::new();
    for a in summary.assignments.iter().take(10) {
        core_rows.push_str(&format!(
            "| [[{title}]] | {pct}% |\n",
            title = a.note_title.replace("]]", "\\]\\]"),
            pct = (a.confidence.clamp(0.0, 1.0) * 100.0).round() as u32,
        ));
    }

    format!(
        "---\n\
tags:\n  - {ns}/index\n  - {ns}/{tag_path}\nsovereign_generated: true\nsovereign_version: {version}\nsovereign_generated_at: {now}\n\
---\n\n\
# {display}\n\n\
*Generated by Sovereign — last updated {now}.*\n\n\
## About this cluster\n\n\
{desc}\n\n\
## Core notes\n\n\
| Note | Confidence |\n|------|------------|\n{core}\n\
---\n\
*Sovereign organizes by pattern, not by judgment. These groupings reflect\n\
statistical similarity, not editorial curation. Trust your own sense of\n\
what belongs together.*\n",
        ns = namespace,
        tag_path = tag_path,
        version = version,
        now = now,
        display = display,
        desc = if summary.cluster.description.is_empty() {
            "_No description available._".to_string()
        } else {
            summary.cluster.description.clone()
        },
        core = core_rows,
    )
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_corpus::clusterer::LabeledCluster;
    use tempfile::tempdir;

    fn make_writeback(vault: &Path, snap: &Path) -> WriteBack {
        let cfg = WriteBackConfig {
            namespace: "sovereign".into(),
            index_dir: "_sovereign-index".into(),
            snapshot_dir: snap.to_path_buf(),
            snapshot_retention: 3,
        };
        WriteBack::new(cfg, vault.to_path_buf(), "obsidian-test".into())
    }

    fn simple_assignment(title: &str, cluster_id: i32, confidence: f32) -> FileAssignment {
        FileAssignment {
            chunk_id: 1,
            relative_path: format!("{title}.md"),
            note_title: title.into(),
            primary_tag: format!("sovereign/test/cluster-{cluster_id}"),
            additional_tags: Vec::new(),
            confidence,
            existing_tags: Vec::new(),
        }
    }

    fn simple_preview(files: &[(&str, i32, f32)]) -> VaultPreview {
        let mut by_cluster: std::collections::HashMap<i32, Vec<FileAssignment>> =
            std::collections::HashMap::new();
        for (name, cid, conf) in files {
            by_cluster
                .entry(*cid)
                .or_default()
                .push(simple_assignment(name, *cid, *conf));
        }
        let clusters: Vec<ClusterSummary> = by_cluster
            .into_iter()
            .map(|(cid, assignments)| ClusterSummary {
                cluster: LabeledCluster {
                    id: cid,
                    tag_path: format!("test/cluster-{cid}"),
                    display_name: format!("Cluster {cid}"),
                    description: format!("Notes grouped as cluster {cid}."),
                    note_count: assignments.len(),
                    centroid_chunk_ids: Vec::new(),
                },
                assignments,
            })
            .collect();
        let tagged = clusters.iter().map(|c| c.assignments.len()).sum();
        VaultPreview {
            clusters,
            outliers: Vec::new(),
            flagged: Vec::new(),
            total_notes: tagged,
            tagged_notes: tagged,
            outlier_count: 0,
            open_questions: Vec::new(),
            namespace: "sovereign".into(),
        }
    }

    fn write_note(vault: &Path, rel: &str, body: &str) {
        let abs = vault.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(abs, body).unwrap();
    }

    #[tokio::test]
    async fn execute_tags_files_and_generates_index_notes() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        let snap = dir.path().join("snap");
        std::fs::create_dir_all(&vault).unwrap();
        write_note(&vault, "alpha.md", "# Alpha\n\nBody A.\n");
        write_note(&vault, "beta.md", "---\ntags: [draft]\n---\n# Beta\n");

        let wb = make_writeback(&vault, &snap);
        let preview = simple_preview(&[("alpha", 1, 0.9), ("beta", 1, 0.8)]);

        let result = wb.execute(&preview, 1, None).await.unwrap();
        assert_eq!(result.files_tagged, 2);
        assert_eq!(result.index_notes_created, 1);
        assert!(result.files_skipped.is_empty());

        // Alpha got a fresh frontmatter fence.
        let alpha = std::fs::read_to_string(vault.join("alpha.md")).unwrap();
        assert!(alpha.starts_with("---\n"));
        assert!(alpha.contains("sovereign/test/cluster-1"));
        // Beta's user tag survived.
        let beta = std::fs::read_to_string(vault.join("beta.md")).unwrap();
        assert!(beta.contains("draft"), "user tag must be preserved: {beta}");
        assert!(beta.contains("sovereign/test/cluster-1"));

        // Index note exists.
        let idx = vault.join("_sovereign-index/test/cluster-1.md");
        assert!(idx.exists(), "cluster index note should exist at {idx:?}");
        let idx_body = std::fs::read_to_string(&idx).unwrap();
        assert!(idx_body.contains("sovereign/index"));
        assert!(idx_body.contains("Cluster 1"));

        // Snapshot recorded.
        assert!(result.snapshot_path.exists());
        let snaps = wb.list_snapshots().unwrap();
        assert_eq!(snaps.len(), 1);
    }

    #[tokio::test]
    async fn rollback_restores_original_frontmatter() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        let snap = dir.path().join("snap");
        std::fs::create_dir_all(&vault).unwrap();
        let original = "---\ntype: note\ntags: [draft]\n---\n# Beta\nBody.\n";
        write_note(&vault, "beta.md", original);

        let wb = make_writeback(&vault, &snap);
        let preview = simple_preview(&[("beta", 2, 0.7)]);
        let result = wb.execute(&preview, 1, None).await.unwrap();

        // After write, content changed.
        let post = std::fs::read_to_string(vault.join("beta.md")).unwrap();
        assert_ne!(post, original);

        let snapshot = wb.load_snapshot(&result.snapshot_path).unwrap();
        let rb = wb.rollback(&snapshot).await.unwrap();
        assert_eq!(rb.files_restored, 1);
        assert_eq!(rb.index_notes_deleted, 1);

        // Back to the original bytes.
        let restored = std::fs::read_to_string(vault.join("beta.md")).unwrap();
        assert_eq!(restored, original);
        // Index note deleted.
        assert!(!vault.join("_sovereign-index/test/cluster-2.md").exists());
    }

    #[tokio::test]
    async fn rollback_is_idempotent() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        let snap = dir.path().join("snap");
        std::fs::create_dir_all(&vault).unwrap();
        write_note(&vault, "a.md", "# A\n");

        let wb = make_writeback(&vault, &snap);
        let preview = simple_preview(&[("a", 1, 0.9)]);
        let result = wb.execute(&preview, 1, None).await.unwrap();

        let snapshot = wb.load_snapshot(&result.snapshot_path).unwrap();
        let first = wb.rollback(&snapshot).await.unwrap();
        let second = wb.rollback(&snapshot).await.unwrap();
        assert_eq!(
            first.files_restored, second.files_restored,
            "rollback must be idempotent"
        );
    }

    #[tokio::test]
    async fn rollback_handles_deleted_files_as_skipped() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        let snap = dir.path().join("snap");
        std::fs::create_dir_all(&vault).unwrap();
        write_note(&vault, "a.md", "# A\n");

        let wb = make_writeback(&vault, &snap);
        let preview = simple_preview(&[("a", 1, 0.9)]);
        let result = wb.execute(&preview, 1, None).await.unwrap();

        // User deletes the file between write and rollback.
        std::fs::remove_file(vault.join("a.md")).unwrap();

        let snapshot = wb.load_snapshot(&result.snapshot_path).unwrap();
        let rb = wb.rollback(&snapshot).await.unwrap();
        // File is re-created from the snapshot (restored), not
        // errored. This is the spec's "rollback is idempotent" guarantee.
        assert_eq!(rb.files_restored, 1);
        assert!(vault.join("a.md").exists());
    }

    #[tokio::test]
    async fn clean_removes_sovereign_and_leaves_user_content() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        let snap = dir.path().join("snap");
        std::fs::create_dir_all(&vault).unwrap();
        write_note(&vault, "a.md", "# A\n");
        write_note(&vault, "b.md", "---\ntags: [mine]\n---\nBody\n");

        let wb = make_writeback(&vault, &snap);
        let preview = simple_preview(&[("a", 1, 0.9), ("b", 1, 0.8)]);
        let _ = wb.execute(&preview, 1, None).await.unwrap();

        let cleaned = wb.clean().await.unwrap();
        assert!(cleaned.tags_removed_from >= 2);
        assert!(cleaned.index_notes_deleted >= 1);

        let b = std::fs::read_to_string(vault.join("b.md")).unwrap();
        assert!(b.contains("mine"), "user tag survives clean: {b}");
        assert!(!b.contains("sovereign/"), "sovereign tags gone: {b}");
        assert!(!b.contains("sovereign_"), "sovereign keys gone: {b}");
        assert!(!vault.join("_sovereign-index").exists());
    }

    #[tokio::test]
    async fn snapshot_retention_prunes_oldest() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        let snap = dir.path().join("snap");
        std::fs::create_dir_all(&vault).unwrap();
        write_note(&vault, "a.md", "# A\n");

        let wb = make_writeback(&vault, &snap);
        let preview = simple_preview(&[("a", 1, 0.9)]);

        // Four snapshots at distinct versions. Retention is 3 → the
        // oldest must be gone after the fourth.
        for v in 1..=4 {
            // Force distinct timestamps so the filename + taken_at
            // diverge even inside the same-second run. We sleep 1s
            // on the 2nd → 4th iteration.
            if v > 1 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            let _ = wb.execute(&preview, v, None).await.unwrap();
        }
        let snaps = wb.list_snapshots().unwrap();
        assert_eq!(snaps.len(), 3, "expected retention of 3; got {snaps:?}");
    }
}
