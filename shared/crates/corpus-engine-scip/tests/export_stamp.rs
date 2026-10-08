// SPDX-License-Identifier: AGPL-3.0-or-later
//! `export_to_live` stamps the graph with the HEAD its export started from.
//! A repo that commits during an export (zoracite's ralph loop, 11 commits in
//! an hour on 2026-10-06) would otherwise be stamped with a commit the
//! exporter may not have read, and the git poll, which compares HEAD to the
//! stamp, would see nothing left to rebuild.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use corpus_engine_scip::scip_export::ScipProgress;
use corpus_engine_scip::ScipGraph;

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t.invalid"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A commit made while the exporter runs leaves the stamp at the commit the
/// export started from. FAILING INPUT: HEAD read after `export_all` returns,
/// which stamps the mid-export commit.
#[tokio::test]
#[ignore = "runs scip-python from PATH: cargo test -p corpus-engine-scip --test export_stamp -- --ignored"]
async fn the_stamp_names_the_head_the_export_started_from() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("a.py"), "def f():\n    return 1\n").unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["add", "a.py"]);
    git(repo.path(), &["commit", "-q", "-m", "a"]);
    let started_at = git(repo.path(), &["rev-parse", "HEAD"]);

    let committed = AtomicBool::new(false);
    let root = repo.path().to_path_buf();
    let progress = |p: ScipProgress<'_>| {
        if matches!(p, ScipProgress::Exporting { .. }) && !committed.swap(true, Ordering::SeqCst) {
            git(
                &root,
                &["commit", "-q", "--allow-empty", "-m", "mid-export"],
            );
        }
    };
    let db = tempfile::tempdir().unwrap();
    let live = db.path().join("scip_graph.db");
    let out = ScipGraph::export_to_live(repo.path(), None, &live, "t", "test", 0, &progress)
        .await
        .expect("scip-python exports a.py");

    assert!(committed.load(Ordering::SeqCst), "no Exporting progress");
    assert_ne!(git(repo.path(), &["rev-parse", "HEAD"]), started_at);
    assert_eq!(out.head.as_deref(), Some(started_at.as_str()));
    let graph = ScipGraph::open_with_integrity(&live, "t").unwrap();
    assert_eq!(
        graph.last_indexed_head().await.as_deref(),
        Some(started_at.as_str())
    );
}
