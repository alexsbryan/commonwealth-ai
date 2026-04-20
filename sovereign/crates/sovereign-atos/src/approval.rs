//! Spec-based feature approval — the M4 replacement for the
//! `APPROVED:<feature-id>` magic token.
//!
//! The natural gesture a developer makes when they've agreed
//! something is ready is committing it. This module anchors the
//! approval gate to that gesture: the feature `<id>` is approved
//! iff `.sovereign/features/<id>/spec.md` has a commit in the
//! current branch's history whose committer is not the spec's
//! author. No token, no manual invocation, no convention the
//! operator has to remember.
//!
//! A secondary path — Commonwealth-native approval — stores a
//! `FeatureApproval` row in `MeshStore` under `app_id
//! "atos-approvals"`. This covers:
//! - collectives without strict git hygiene;
//! - scenarios where a reviewer is on a different machine than the
//!   repo and never lands a physical commit;
//! - the `sovereign atos feature approve <id>` CLI fallback.
//!
//! [`find_approval`] tries the git path first and falls back to
//! MeshStore. Whichever resolves wins; both paths produce the same
//! [`FeatureApproval`] shape so downstream code doesn't branch on
//! origin.
//!
//! Drift: every request hashes the current `spec.md` content and
//! compares to the hash recorded at approval time. Mismatches
//! *do not block* — they produce a `deviation`-kind note (see
//! `NoteStore::write_note_scoped`). The agent's next turn
//! acknowledges it or reverts.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use commonwealth_core::ids::NodeId;
use commonwealth_state::MeshStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// App id for the Commonwealth-native approval table in MeshStore.
pub const ATOS_APPROVALS_APP_ID: &str = "atos-approvals";

/// A single approval record. Produced by either the git walker or
/// [`record_approval`]; consumed by middleware to gate writes and
/// by drift detection to spot post-approval spec edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeatureApproval {
    pub feature_id: String,
    /// Path to the approved spec, relative to the repo root.
    pub spec_path: String,
    /// SHA-256 of spec.md content at approval time. Used by
    /// [`detect_drift`] — the middleware compares this to a fresh
    /// hash of the current file on every request.
    pub spec_content_hash: String,
    /// Who approved. Git path: committer identity string
    /// (`name <email>`). MeshStore path: stringified `NodeId`.
    pub approved_by: String,
    /// Unix seconds.
    pub approved_at: i64,
    /// Whichever path produced this record — for operator visibility.
    pub source: ApprovalSource,
    /// Commit hash (git path) or node id hex (MeshStore path).
    pub witness: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSource {
    Git,
    Commonwealth,
}

/// Resolve an approval for `feature_id` anchored at `repo_root`.
///
/// Order:
/// 1. **Git path.** Walk commits touching
///    `.sovereign/features/<id>/spec.md` in HEAD order. Return the
///    first commit whose committer identity is not the spec's
///    original author. (Spec author = the committer of the FIRST
///    commit that added the file; reviewer = any later committer.
///    Two-commit minimum: author creates, reviewer approves.)
/// 2. **Commonwealth-native path.** Read `atos-approvals`
///    MeshStore row.
/// 3. Neither → `None`.
///
/// Errors are intentionally silenced — a corrupt git repo or a
/// missing MeshStore row returns `None` rather than crashing the
/// middleware; the approval gate interprets `None` as "unapproved"
/// and blocks writes appropriately.
pub fn find_approval(
    repo_root: &Path,
    feature_id: &str,
    mesh: Option<&MeshStore>,
) -> Option<FeatureApproval> {
    if let Some(appr) = find_approval_via_git(repo_root, feature_id) {
        return Some(appr);
    }
    if let Some(mesh) = mesh {
        if let Some(appr) = find_approval_via_mesh(mesh, feature_id) {
            return Some(appr);
        }
    }
    None
}

/// Find the approval via `git log`. Shells out to the `git` CLI;
/// every user who has an ATOS feature has git already (the feature
/// directory is a git tracked path), so the dependency is free.
///
/// Approach:
/// 1. `git log --follow --format=%H\t%ce\t%ct -- <path>` lists
///    commits that touched the spec, newest first.
/// 2. The FIRST (newest) commit's committer is the "approval
///    candidate." The LAST commit's committer is the "author." If
///    they differ, we have a valid approval.
/// 3. To build the FeatureApproval, we read the file at the
///    approval commit via `git show <commit>:<path>` and hash it
///    with SHA-256.
fn find_approval_via_git(repo_root: &Path, feature_id: &str) -> Option<FeatureApproval> {
    let spec_relative = format!(".sovereign/features/{feature_id}/spec.md");

    let output = std::process::Command::new("git")
        .args(["log", "--follow", "--format=%H%x09%ce%x09%cn%x09%ct", "--"])
        .arg(&spec_relative)
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut commits: Vec<(String, String, String, i64)> = Vec::new(); // (hash, email, name, unix_ts)
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 4 {
            continue;
        }
        let hash = parts[0].to_string();
        let email = parts[1].to_string();
        let name = parts[2].to_string();
        let ts = parts[3].parse::<i64>().unwrap_or(0);
        commits.push((hash, email, name, ts));
    }
    if commits.len() < 2 {
        // Fewer than two commits means no reviewer has yet touched
        // the file; there's no one who could have played the
        // approver role. Single-author commits are never approvals.
        return None;
    }

    // Newest commit (first in log) is the approval candidate.
    let (hash, email, name, ts) = commits.first()?.clone();
    // Oldest commit (last in log) is the author.
    let (_, author_email, _, _) = commits.last()?.clone();
    if email == author_email {
        // Same email top-to-bottom — author self-approved, not
        // eligible.
        return None;
    }

    // Read the spec content AT that commit and hash it. `git show
    // <hash>:<path>` streams the blob.
    let spec_out = std::process::Command::new("git")
        .args(["show"])
        .arg(format!("{hash}:{spec_relative}"))
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !spec_out.status.success() {
        return None;
    }
    let spec_hash = hash_sha256(&spec_out.stdout);

    Some(FeatureApproval {
        feature_id: feature_id.to_string(),
        spec_path: spec_relative,
        spec_content_hash: spec_hash,
        approved_by: format!("{name} <{email}>"),
        approved_at: ts,
        source: ApprovalSource::Git,
        witness: hash,
    })
}

fn find_approval_via_mesh(mesh: &MeshStore, feature_id: &str) -> Option<FeatureApproval> {
    let entry = mesh.get(ATOS_APPROVALS_APP_ID, feature_id).ok()??;
    serde_json::from_slice(&entry.value).ok()
}

/// Write a Commonwealth-native approval row. Used by `sovereign atos
/// feature approve <id>` when the collective isn't using git for
/// review gating.
pub fn record_approval(
    mesh: &MeshStore,
    origin: NodeId,
    repo_root: &Path,
    feature_id: &str,
) -> std::io::Result<FeatureApproval> {
    let spec_path_rel = format!(".sovereign/features/{feature_id}/spec.md");
    let spec_full = repo_root.join(&spec_path_rel);
    let content = std::fs::read(&spec_full)?;
    let approval = FeatureApproval {
        feature_id: feature_id.to_string(),
        spec_path: spec_path_rel,
        spec_content_hash: hash_sha256(&content),
        approved_by: format!("{origin:?}"),
        approved_at: unix_now(),
        source: ApprovalSource::Commonwealth,
        witness: format!("{origin:?}"),
    };
    let encoded = serde_json::to_vec(&approval).map_err(|e| {
        std::io::Error::other(format!("serialize FeatureApproval: {e}"))
    })?;
    mesh.set(
        ATOS_APPROVALS_APP_ID,
        feature_id,
        Bytes::from(encoded),
        origin,
    )
    .map_err(|e| std::io::Error::other(format!("mesh.set: {e}")))?;
    Ok(approval)
}

/// Compute the SHA-256 of the on-disk spec.md for drift comparison.
/// Returns `None` when the file is missing — treated by the middleware
/// as "no drift" because there's no baseline to compare against.
pub fn current_spec_hash(repo_root: &Path, feature_id: &str) -> Option<String> {
    let path = spec_path(repo_root, feature_id);
    let content = std::fs::read(&path).ok()?;
    Some(hash_sha256(&content))
}

/// Absolute path to the feature's spec.md.
pub fn spec_path(repo_root: &Path, feature_id: &str) -> PathBuf {
    repo_root
        .join(".sovereign")
        .join("features")
        .join(feature_id)
        .join("spec.md")
}

/// `true` when the on-disk spec differs from the one at approval
/// time. Used by ApprovalGate.
pub fn detect_drift(approval: &FeatureApproval, repo_root: &Path) -> bool {
    let Some(current) = current_spec_hash(repo_root, &approval.feature_id) else {
        // File missing post-approval is its own kind of drift, but
        // we don't want to be noisy on race conditions. Treat as
        // no-drift here; the ContextInjector will fail to inject
        // the spec and surface its own warning.
        return false;
    };
    current != approval.spec_content_hash
}

fn hash_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonwealth_core::ids::NodeId;
    use std::process::Command;

    fn node_id() -> NodeId {
        NodeId::from_u128(42)
    }

    /// Build a scratch repo at `dir` with two commits by distinct
    /// identities. Returns the path.
    fn build_scratch_repo(dir: &Path, feature_id: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        run_git(dir, &["init", "-q", "-b", "main"]);
        run_git(dir, &["config", "user.name", "Yara Author"]);
        run_git(dir, &["config", "user.email", "yara@example.test"]);

        let spec_dir = dir.join(".sovereign").join("features").join(feature_id);
        std::fs::create_dir_all(&spec_dir).unwrap();
        let spec_path = spec_dir.join("spec.md");
        std::fs::write(&spec_path, "# feature spec\n\nInitial content.\n").unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "author adds spec"]);

        // Flip identity to reviewer for the next commit.
        run_git(dir, &["config", "user.name", "Marcus Reviewer"]);
        run_git(dir, &["config", "user.email", "marcus@example.test"]);
        // Reviewer adds a trailing line (a reasonable approval
        // gesture — they fixed a typo or added a comment).
        std::fs::write(
            &spec_path,
            "# feature spec\n\nInitial content.\n\nApproved.\n",
        )
        .unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "reviewer approves"]);

        dir.to_path_buf()
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git must be on PATH for approval tests");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn git_approval_distinct_committer_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = build_scratch_repo(tmp.path(), "fx");
        let appr = find_approval_via_git(&repo, "fx").expect("approval must be found");
        assert_eq!(appr.feature_id, "fx");
        assert!(appr.approved_by.contains("Marcus"), "got: {}", appr.approved_by);
        assert_eq!(appr.source, ApprovalSource::Git);
        // Blob hash should match the current file (reviewer's edit).
        let current = current_spec_hash(&repo, "fx").unwrap();
        assert_eq!(appr.spec_content_hash, current);
    }

    #[test]
    fn git_approval_missing_when_only_one_committer() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir).unwrap();
        run_git(dir, &["init", "-q", "-b", "main"]);
        run_git(dir, &["config", "user.name", "Solo"]);
        run_git(dir, &["config", "user.email", "solo@example.test"]);
        let spec_dir = dir.join(".sovereign").join("features").join("solo");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(spec_dir.join("spec.md"), "# solo spec\n").unwrap();
        run_git(dir, &["add", "."]);
        run_git(dir, &["commit", "-q", "-m", "only commit"]);
        assert!(find_approval_via_git(dir, "solo").is_none());
    }

    #[test]
    fn drift_detected_when_spec_mutates_post_approval() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = build_scratch_repo(tmp.path(), "fx");
        let appr = find_approval_via_git(&repo, "fx").unwrap();
        assert!(!detect_drift(&appr, &repo), "no drift immediately after approval");

        // Yara edits spec.md mid-implementation.
        let spec = spec_path(&repo, "fx");
        std::fs::write(&spec, "# mutated\n").unwrap();
        assert!(detect_drift(&appr, &repo), "drift should fire on content change");
    }

    #[test]
    fn mesh_fallback_record_and_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = build_scratch_repo(tmp.path(), "fx");
        let mesh = MeshStore::in_memory().unwrap();

        // No git-approval-eligible commit yet on this branch?
        // Build a single-committer repo:
        let solo_tmp = tempfile::tempdir().unwrap();
        let solo_dir = solo_tmp.path();
        std::fs::create_dir_all(solo_dir).unwrap();
        run_git(solo_dir, &["init", "-q", "-b", "main"]);
        run_git(solo_dir, &["config", "user.name", "Solo"]);
        run_git(solo_dir, &["config", "user.email", "solo@example.test"]);
        std::fs::create_dir_all(
            solo_dir.join(".sovereign").join("features").join("solo"),
        )
        .unwrap();
        std::fs::write(
            solo_dir.join(".sovereign").join("features").join("solo").join("spec.md"),
            "# solo\n",
        )
        .unwrap();
        run_git(solo_dir, &["add", "."]);
        run_git(solo_dir, &["commit", "-q", "-m", "x"]);

        // No git approval here; record via mesh.
        let appr = record_approval(&mesh, node_id(), solo_dir, "solo").unwrap();
        assert_eq!(appr.source, ApprovalSource::Commonwealth);

        let resolved = find_approval(solo_dir, "solo", Some(&mesh)).unwrap();
        assert_eq!(resolved.source, ApprovalSource::Commonwealth);
        assert_eq!(resolved.feature_id, "solo");

        // find_approval_via_git still returns None on the solo repo.
        assert!(find_approval_via_git(solo_dir, "solo").is_none());

        // And the two-committer repo still prefers git.
        let git_appr = find_approval(&repo, "fx", Some(&mesh)).unwrap();
        assert_eq!(git_appr.source, ApprovalSource::Git);
    }

    #[test]
    fn spec_path_layout() {
        let p = spec_path(Path::new("/tmp/repo"), "zotero-acquirer");
        assert_eq!(
            p,
            Path::new("/tmp/repo/.sovereign/features/zotero-acquirer/spec.md")
        );
    }
}
