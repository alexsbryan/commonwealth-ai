// SPDX-License-Identifier: AGPL-3.0-or-later
//! The work donor's checkouts: where a leased unit runs.
//!
//! Its own file because `work_donor.rs` crossed ARCH §3.2's 1200-line
//! ceiling when the ring rail's port landed (fp-54). The block moved
//! verbatim — `#[path]`, so the names are unchanged and every caller reads
//! the same.

use std::path::{Path, PathBuf};

use commonwealth_work::refusal::{UnmetRequirement, WorkRefusal};
use sovereign_contracts::oicp::{JobUnit, WorkOffer};

/// The directory a unit runs in.
///
/// A unit pinned to a `repo_rev` runs in **one checkout per repo, reused and
/// checked forward** — `scripts/evidence-verdict.py:529 ensure_worktree`'s
/// recipe, with `git clean -x` deliberately omitted so `target/` stays warm.
/// A per-unit checkout would invert that: the whole reason a dev peer is a
/// good donor is the warm target directory it already has, and a fresh
/// checkout per unit throws it away and pays minutes per unit for nothing.
///
/// The checkout is a self-contained local CLONE, not a `git worktree`. A
/// worktree's `.git` is a file pointing into the parent repo's `.git/`, which
/// is outside the one directory the boundary mounts — so inside the boundary
/// `git` answered "not a git repository" and ten tests that shell out to it
/// (conformance tags, refactor destinations, the donor's own rev
/// attribution) failed on the environment, not the code (watched
/// 2026-09-11, 12,730 / 13 in the boundary against 12,731 / 0 at the
/// reference). A local clone hardlinks the objects, so it costs no space and
/// carries its own history inside the mount.
///
/// A unit with no `repo_rev` runs in one reused scratch directory, for the
/// same reason and by the same rule.
pub(super) async fn resolve_workdir(
    offer: &WorkOffer,
    unit: &JobUnit,
    donor_root: &Path,
) -> Result<PathBuf, WorkRefusal> {
    let Some(rev) = unit.requirements.repo_rev.clone() else {
        let scratch = donor_root.join("scratch");
        return std::fs::create_dir_all(&scratch)
            .map(|()| scratch.clone())
            .map_err(|e| WorkRefusal::PayloadNotCanonical {
                detail: format!("this donor could not create its scratch workdir: {e}"),
            });
    };
    let repos: Vec<(String, PathBuf)> = offer
        .repos
        .iter()
        .map(|r| (r.url.clone(), PathBuf::from(&r.path)))
        .collect();
    let root = donor_root.join("worktrees");
    let rev_for_err = rev.clone();
    let resolved = tokio::task::spawn_blocking(move || checkout_at(&repos, &root, &rev))
        .await
        .unwrap_or_else(|e| Err(format!("the checkout task panicked: {e}")));
    resolved.map_err(|host| {
        WorkRefusal::RequirementUnmet(UnmetRequirement::RepoRev {
            required: rev_for_err,
            host,
        })
    })
}

/// Find the first offered repo that can resolve `rev`, and hand back its one
/// reused worktree checked forward to it. Blocking; called from
/// `spawn_blocking`.
pub(super) fn checkout_at(
    repos: &[(String, PathBuf)],
    worktree_root: &Path,
    rev: &str,
) -> Result<PathBuf, String> {
    if repos.is_empty() {
        return Err("this donor offers no repos".to_string());
    }
    let mut refused: Vec<String> = Vec::new();
    for (url, path) in repos {
        if git(path, &["cat-file", "-e", &format!("{rev}^{{commit}}")]).is_err() {
            refused.push(format!("{url} does not have {rev}"));
            continue;
        }
        // ONE worktree per repo, keyed by the repo's own directory name so two
        // checkouts of different repos never share one (ARCH §7.5 — identity
        // from essence, and the URL is the essence here).
        let key = stable_repo_key(url);
        let worktree = worktree_root.join(&key);
        if let Err(e) = std::fs::create_dir_all(worktree_root) {
            return Err(format!("could not create {}: {e}", worktree_root.display()));
        }
        let source = path.display().to_string();
        let dot_git = worktree.join(".git");
        if !dot_git.exists() {
            git(
                path,
                &[
                    "clone",
                    "--quiet",
                    "--no-checkout",
                    "--",
                    &source,
                    &worktree.display().to_string(),
                ],
            )
            .map_err(|e| format!("`git clone` failed for {url}: {e}"))?;
        } else if dot_git.is_file() {
            // A worktree LINK from before the clone rule: convert it in place
            // and keep everything else (target/ above all). The link's parent
            // entry is pruned so the parent repo stops listing a worktree
            // that is now a repository of its own.
            let staging = worktree_root.join(format!("{key}.converting"));
            let _ = std::fs::remove_dir_all(&staging);
            git(
                path,
                &[
                    "clone",
                    "--quiet",
                    "--no-checkout",
                    "--",
                    &source,
                    &staging.display().to_string(),
                ],
            )
            .map_err(|e| format!("`git clone` (conversion) failed for {url}: {e}"))?;
            // Order matters: the parent's entry is pruned only while the
            // link is GONE and nothing sits at `.git` yet — `git worktree
            // prune` keeps an entry whose path still exists, a directory
            // included (watched in the test's first run).
            std::fs::remove_file(&dot_git).map_err(|e| {
                format!(
                    "removing the worktree link at {} failed: {e}",
                    dot_git.display()
                )
            })?;
            if let Err(e) = git(path, &["worktree", "prune"]) {
                // Not fatal for the unit — the clone below is complete either
                // way — but a stale entry in the parent is worth a line.
                tracing::warn!(
                    target: super::TRACE_TARGET,
                    parent = %path.display(),
                    error = %e,
                    "work donor: the parent repo still lists the converted checkout as a worktree — `git worktree prune` failed"
                );
            }
            std::fs::rename(staging.join(".git"), &dot_git).map_err(|e| {
                format!(
                    "moving the clone's .git into {} failed: {e}",
                    worktree.display()
                )
            })?;
            let _ = std::fs::remove_dir_all(&staging);
            tracing::info!(
                target: super::TRACE_TARGET,
                checkout = %worktree.display(),
                "work donor: converted a worktree link into a self-contained clone — git now works inside the boundary"
            );
        }
        // The clone may predate `rev`; a local fetch brings it in (objects
        // copied from the offered path, nothing crosses a network).
        git(&worktree, &["fetch", "--quiet", "--", &source, rev])
            .map_err(|e| format!("`git fetch {rev}` failed in {}: {e}", worktree.display()))?;
        git(&worktree, &["checkout", "-q", "-f", "--detach", rev])
            .map_err(|e| format!("`git checkout` failed in {}: {e}", worktree.display()))?;
        // `-x` is deliberately absent: target/ is ignored and warm, and the
        // point of one reused worktree is to keep it that way.
        git(&worktree, &["clean", "-fdq"])
            .map_err(|e| format!("`git clean` failed in {}: {e}", worktree.display()))?;
        return Ok(worktree);
    }
    Err(refused.join("; "))
}

/// A filesystem-safe, stable directory name for a repo URL.
///
/// Derived from the URL — the thing that identifies a repo across donors —
/// rather than from a counter or the offer's position in a list (ARCH §7.5).
pub(super) fn stable_repo_key(url: &str) -> String {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    let safe: String = last
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if safe.is_empty() {
        "repo".to_string()
    } else {
        safe
    }
}

pub(super) fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
