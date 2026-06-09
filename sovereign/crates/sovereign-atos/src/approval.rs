// SPDX-License-Identifier: AGPL-3.0-or-later
//! Spec-based feature approval — the M4 replacement for the
//! `APPROVED:<feature-id>` magic token.
//!
//! The natural gesture a developer makes when they've agreed
//! something is ready is committing it. This module anchors the
//! approval gate to that gesture: the feature `<id>` is approved
//! iff `.sovereign/features/<id>/spec.md` has at least one commit
//! in the current branch's history. No token, no manual invocation,
//! no second-committer ceremony, no convention the operator has to
//! remember. Commit the spec and you're ready to go.
//!
//! A secondary path — Commonwealth-native approval — stores a
//! `FeatureApproval` row in `MeshStore` under `app_id
//! "atos-approvals"`. This covers:
//! - working trees where the spec hasn't been committed yet but the
//!   operator wants to start iterating;
//! - scenarios where the approval lives off-repo (cross-machine
//!   review, mesh-replicated record);
//! - the `sovereign atos feature approve <id>` CLI fallback.
//!
//! [`find_approval`] checks MeshStore first (an explicit `feature
//! approve` or `spec accept` wins over an older git witness) and
//! falls back to the git path. Both paths produce the same
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
    /// Full spec text at approval/accept time. MeshStore approvals
    /// populate this so `spec diff` works offline; git approvals
    /// leave `None` and resolve via `git show <witness>:<path>`.
    /// Also survives a force-push that orphans the witness commit.
    #[serde(default)]
    pub spec_content_snapshot: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSource {
    Git,
    Commonwealth,
}

/// Resolve an approval for `feature_id` anchored at `repo_root`.
///
/// Order (MeshStore wins — by design):
/// 1. **Commonwealth-native path.** Read `atos-approvals`
///    MeshStore row. A MeshStore row exists when either the operator
///    ran `sovereign atos feature approve <id>` explicitly OR
///    `sovereign atos spec accept <id>` accepted a drift. Either way,
///    the most recent deliberate gesture should take precedence over
///    the original git witness.
/// 2. **Git path.** Walk commits touching
///    `.sovereign/features/<id>/spec.md` in HEAD order. Any commit
///    that touched the spec is sufficient — a single-author repo is
///    a valid approval as long as the spec has actually been
///    committed (uncommitted working-tree edits don't count).
/// 3. Neither → `None` (the spec file may exist but has never been
///    committed, or the feature has no spec yet).
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
    if let Some(mesh) = mesh {
        if let Some(appr) = find_approval_via_mesh(mesh, feature_id) {
            return Some(appr);
        }
    }
    find_approval_via_git(repo_root, feature_id)
}

/// Find the approval via `git log`. Shells out to the `git` CLI;
/// every user who has an ATOS feature has git already (the feature
/// directory is a git-tracked path), so the dependency is free.
///
/// Approach:
/// 1. `git log --follow --format=%H\t%ce\t%cn\t%ct -- <path>` lists
///    commits that touched the spec, newest first.
/// 2. If there is at least one commit, the newest one is the
///    approval witness — we trust that the developer committed the
///    spec deliberately.
/// 3. Build the [`FeatureApproval`] by reading the spec at that
///    commit via `git show <commit>:<path>` and hashing it with
///    SHA-256, so drift detection anchors to exactly what was
///    committed (not the current working-tree state).
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
    let (hash, email, name, ts) = stdout.lines().find_map(|line| {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 4 {
            return None;
        }
        let ts = parts[3].parse::<i64>().ok()?;
        Some((
            parts[0].to_string(),
            parts[1].to_string(),
            parts[2].to_string(),
            ts,
        ))
    })?;

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
        spec_content_snapshot: None,
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
    let snapshot = String::from_utf8(content.clone()).ok();
    let approval = FeatureApproval {
        feature_id: feature_id.to_string(),
        spec_path: spec_path_rel,
        spec_content_hash: hash_sha256(&content),
        approved_by: format!("{origin:?}"),
        approved_at: unix_now(),
        source: ApprovalSource::Commonwealth,
        witness: format!("{origin:?}"),
        spec_content_snapshot: snapshot,
    };
    let encoded = serde_json::to_vec(&approval)
        .map_err(|e| std::io::Error::other(format!("serialize FeatureApproval: {e}")))?;
    mesh.set(
        ATOS_APPROVALS_APP_ID,
        feature_id,
        Bytes::from(encoded),
        origin,
    )
    .map_err(|e| std::io::Error::other(format!("mesh.set: {e}")))?;
    Ok(approval)
}

/// Resolve the spec-at-approval content for `spec diff`.
///
/// Priority:
/// 1. `approval.spec_content_snapshot` (set on MeshStore approvals and
///    on `accept_drift` writes — survives force-push and MeshStore
///    migrations).
/// 2. Git path: `git show <witness>:<spec_path>`. The witness is the
///    approval commit; this reads the blob from history.
///
/// Returns `None` if neither path produces content (e.g., git-only
/// approval whose witness commit has been orphaned).
pub fn resolve_approved_spec_content(
    repo_root: &Path,
    approval: &FeatureApproval,
) -> Option<String> {
    if let Some(s) = approval.spec_content_snapshot.as_ref() {
        return Some(s.clone());
    }
    let output = std::process::Command::new("git")
        .args(["show"])
        .arg(format!("{}:{}", approval.witness, approval.spec_path))
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// Accept the current on-disk spec as the new approved content.
///
/// Writes (or overwrites) the MeshStore approval row with the fresh
/// hash + a full content snapshot. Preserves the original
/// `approved_by` — the committer who reviewed the first time doesn't
/// change; Yara is just updating the hash so the gate stops flagging
/// drift. The deviation note carries her justification.
///
/// MeshStore wins over git at resolution time, so an accepted drift
/// silences `detect_drift` immediately without any git churn.
pub fn accept_drift(
    mesh: &MeshStore,
    origin: NodeId,
    repo_root: &Path,
    feature_id: &str,
    prior: &FeatureApproval,
) -> std::io::Result<FeatureApproval> {
    let spec_path_rel = format!(".sovereign/features/{feature_id}/spec.md");
    let spec_full = repo_root.join(&spec_path_rel);
    let content = std::fs::read(&spec_full)?;
    let snapshot = String::from_utf8(content.clone()).ok();
    let approval = FeatureApproval {
        feature_id: feature_id.to_string(),
        spec_path: spec_path_rel,
        spec_content_hash: hash_sha256(&content),
        approved_by: prior.approved_by.clone(),
        approved_at: unix_now(),
        source: ApprovalSource::Commonwealth,
        witness: format!("{origin:?}"),
        spec_content_snapshot: snapshot,
    };
    let encoded = serde_json::to_vec(&approval)
        .map_err(|e| std::io::Error::other(format!("serialize FeatureApproval: {e}")))?;
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
///
/// Glassbox: on actual drift we emit a `tracing::warn!` naming the
/// feature and carrying short prefixes of both hashes, so an operator
/// tailing the daemon can see *when* the agent started seeing a
/// drift warning without having to reconstruct it from request
/// timestamps. Hash values are logged truncated (12 hex chars) — long
/// enough to correlate across events, short enough to avoid leaking
/// the full content fingerprint into log aggregators.
pub fn detect_drift(approval: &FeatureApproval, repo_root: &Path) -> bool {
    let Some(current) = current_spec_hash(repo_root, &approval.feature_id) else {
        // File missing post-approval is its own kind of drift, but
        // we don't want to be noisy on race conditions. Treat as
        // no-drift here; the ContextInjector will fail to inject
        // the spec and surface its own warning.
        return false;
    };
    let drifted = current != approval.spec_content_hash;
    if drifted {
        tracing::warn!(
            feature_id = %approval.feature_id,
            approved_hash = %short_hash(&approval.spec_content_hash),
            current_hash = %short_hash(&current),
            "drift: spec modified since approval"
        );
    }
    drifted
}

fn short_hash(h: &str) -> &str {
    if h.len() >= 12 {
        &h[..12]
    } else {
        h
    }
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
    fn git_approval_latest_commit_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = build_scratch_repo(tmp.path(), "fx");
        let appr = find_approval_via_git(&repo, "fx").expect("approval must be found");
        assert_eq!(appr.feature_id, "fx");
        // `build_scratch_repo` lands two commits; the newest one is
        // the witness regardless of committer identity.
        assert!(
            appr.approved_by.contains("Marcus"),
            "got: {}",
            appr.approved_by
        );
        assert_eq!(appr.source, ApprovalSource::Git);
        let current = current_spec_hash(&repo, "fx").unwrap();
        assert_eq!(appr.spec_content_hash, current);
    }

    #[test]
    fn git_approval_single_committer_is_sufficient() {
        // A solo developer commits their own spec — that one commit
        // is a valid approval. No second-committer gate.
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
        let appr = find_approval_via_git(dir, "solo").expect("solo commit must approve");
        assert!(appr.approved_by.contains("Solo"));
        assert_eq!(appr.source, ApprovalSource::Git);
    }

    #[test]
    fn git_approval_absent_when_spec_uncommitted() {
        // The spec file exists in the working tree but has never
        // been committed — no approval.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::create_dir_all(dir).unwrap();
        run_git(dir, &["init", "-q", "-b", "main"]);
        run_git(dir, &["config", "user.name", "Solo"]);
        run_git(dir, &["config", "user.email", "solo@example.test"]);
        // A bootstrap commit that doesn't touch the spec, so git's
        // HEAD exists but the spec itself is untracked.
        std::fs::write(dir.join("README.md"), "hi\n").unwrap();
        run_git(dir, &["add", "README.md"]);
        run_git(dir, &["commit", "-q", "-m", "bootstrap"]);
        let spec_dir = dir.join(".sovereign").join("features").join("solo");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(spec_dir.join("spec.md"), "# solo spec\n").unwrap();
        // spec.md is in the working tree but not added to git.
        assert!(find_approval_via_git(dir, "solo").is_none());
    }

    #[test]
    fn drift_detected_when_spec_mutates_post_approval() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = build_scratch_repo(tmp.path(), "fx");
        let appr = find_approval_via_git(&repo, "fx").unwrap();
        assert!(
            !detect_drift(&appr, &repo),
            "no drift immediately after approval"
        );

        // Yara edits spec.md mid-implementation.
        let spec = spec_path(&repo, "fx");
        std::fs::write(&spec, "# mutated\n").unwrap();
        assert!(
            detect_drift(&appr, &repo),
            "drift should fire on content change"
        );
    }

    #[test]
    fn mesh_fallback_record_and_lookup() {
        // Exercise the mesh path by working from an *uncommitted*
        // spec — `git log` returns nothing, so `find_approval` must
        // fall through to MeshStore.
        let uncommitted_tmp = tempfile::tempdir().unwrap();
        let uncommitted_dir = uncommitted_tmp.path();
        std::fs::create_dir_all(uncommitted_dir).unwrap();
        run_git(uncommitted_dir, &["init", "-q", "-b", "main"]);
        run_git(uncommitted_dir, &["config", "user.name", "Solo"]);
        run_git(
            uncommitted_dir,
            &["config", "user.email", "solo@example.test"],
        );
        // Bootstrap commit that doesn't touch the spec path.
        std::fs::write(uncommitted_dir.join("README.md"), "hi\n").unwrap();
        run_git(uncommitted_dir, &["add", "README.md"]);
        run_git(uncommitted_dir, &["commit", "-q", "-m", "bootstrap"]);
        // Spec lives in the working tree but is never committed.
        let spec_dir = uncommitted_dir
            .join(".sovereign")
            .join("features")
            .join("solo");
        std::fs::create_dir_all(&spec_dir).unwrap();
        std::fs::write(spec_dir.join("spec.md"), "# solo\n").unwrap();

        let mesh = MeshStore::in_memory().unwrap();

        // Git returns None (spec uncommitted); mesh is the only
        // path that can grant approval here.
        assert!(find_approval_via_git(uncommitted_dir, "solo").is_none());
        let appr = record_approval(&mesh, node_id(), uncommitted_dir, "solo").unwrap();
        assert_eq!(appr.source, ApprovalSource::Commonwealth);

        let resolved = find_approval(uncommitted_dir, "solo", Some(&mesh)).unwrap();
        assert_eq!(resolved.source, ApprovalSource::Commonwealth);
        assert_eq!(resolved.feature_id, "solo");

        // A repo where the spec IS committed resolves via mesh FIRST
        // when a mesh row exists for that feature id — mesh wins by
        // design, so `spec accept` can override an old git witness.
        let committed_tmp = tempfile::tempdir().unwrap();
        let committed_repo = build_scratch_repo(committed_tmp.path(), "fx");
        // No mesh row for "fx" yet — should fall through to git.
        let git_appr = find_approval(&committed_repo, "fx", Some(&mesh)).unwrap();
        assert_eq!(git_appr.source, ApprovalSource::Git);
    }

    #[test]
    fn resolve_approved_content_uses_snapshot_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = build_scratch_repo(tmp.path(), "fx");
        let mesh = MeshStore::in_memory().unwrap();
        let appr = record_approval(&mesh, node_id(), &repo, "fx").unwrap();
        assert!(appr.spec_content_snapshot.is_some());
        let content = resolve_approved_spec_content(&repo, &appr).unwrap();
        assert!(content.starts_with("# feature spec"));
    }

    #[test]
    fn resolve_approved_content_falls_back_to_git_show() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = build_scratch_repo(tmp.path(), "fx");
        let appr = find_approval_via_git(&repo, "fx").unwrap();
        assert!(appr.spec_content_snapshot.is_none());
        let content = resolve_approved_spec_content(&repo, &appr)
            .expect("git show path must resolve when witness is reachable");
        assert!(content.contains("Approved."));
    }

    #[test]
    fn accept_drift_rewrites_hash_and_silences_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = build_scratch_repo(tmp.path(), "fx");
        let mesh = MeshStore::in_memory().unwrap();
        let original = find_approval_via_git(&repo, "fx").unwrap();

        let spec = spec_path(&repo, "fx");
        std::fs::write(&spec, "# mutated spec\n\nNew invariant.\n").unwrap();
        assert!(detect_drift(&original, &repo));

        let accepted = accept_drift(&mesh, node_id(), &repo, "fx", &original).unwrap();
        assert_ne!(accepted.spec_content_hash, original.spec_content_hash);
        assert_eq!(accepted.approved_by, original.approved_by);
        assert_eq!(accepted.source, ApprovalSource::Commonwealth);
        assert!(!detect_drift(&accepted, &repo));

        // find_approval now prefers git (returns Git source), BUT
        // the mesh row is the source of truth for a future `accept`
        // cycle. Confirm the mesh row survives round-trip.
        let fetched = find_approval_via_mesh(&mesh, "fx").unwrap();
        assert_eq!(fetched.spec_content_hash, accepted.spec_content_hash);
        assert!(fetched.spec_content_snapshot.is_some());
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
