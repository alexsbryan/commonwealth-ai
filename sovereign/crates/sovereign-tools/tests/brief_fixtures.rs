//! Snapshot fixture suite for the working-set brief assembler.
//!
//! Each test:
//!   1. Builds a deterministic temp git repo + (optionally) a
//!      synthetic structural atlas + archaeology sidecar.
//!   2. Calls `assemble_brief` with fixed inputs.
//!   3. Snapshot-compares the output against a file in
//!      `tests/snapshots/<scenario>.md`.
//!
//! Iteration loop:
//!   $ cargo test -p sovereign-tools --test brief_fixtures
//! When you intentionally change brief output:
//!   $ UPDATE_SNAPSHOTS=1 cargo test -p sovereign-tools --test brief_fixtures
//!   then `git diff tests/snapshots/` and commit.
//!
//! Snapshots intentionally avoid time-varying data (no "today",
//! no real-clock recency dates). When the brief absolutely needs
//! a date, we backdate the underlying git commits to a fixed value
//! so the rendered output is deterministic across machines.

use std::path::{Path, PathBuf};
use std::process::Command as Cmd;

use sovereign_tools::code::brief::{assemble_brief, BriefInputs};

// ── Snapshot harness ─────────────────────────────────────────

/// Compare `actual` to the snapshot at
/// `tests/snapshots/<scenario>.md`. Set `UPDATE_SNAPSHOTS=1` to
/// (re)write the file.
fn assert_snapshot(scenario: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{scenario}.md"));
    if std::env::var("UPDATE_SNAPSHOTS").as_deref() == Ok("1") {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir snapshots");
        }
        std::fs::write(&path, actual).expect("write snapshot");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "snapshot missing: {} — re-run with UPDATE_SNAPSHOTS=1 to seed",
            path.display()
        )
    });
    if expected != actual {
        eprintln!("--- expected ({}) ---\n{expected}", path.display());
        eprintln!("--- actual ---\n{actual}");
        eprintln!(
            "--- hint ---\nsnapshot mismatch. Re-run with UPDATE_SNAPSHOTS=1 once you've \
             confirmed the new output is intentional, then commit the snapshot."
        );
        panic!("snapshot mismatch: {}", path.display());
    }
}

// ── Git fixture helpers ──────────────────────────────────────

fn init_repo(dir: &Path) {
    assert!(Cmd::new("git")
        .args(["init", "--initial-branch=main"])
        .current_dir(dir)
        .status()
        .unwrap()
        .success());
    for (k, v) in [("user.email", "alice@example.com"), ("user.name", "Alice")] {
        Cmd::new("git").args(["config", k, v]).current_dir(dir).status().unwrap();
    }
}

fn commit_at(dir: &Path, msg: &str, ts_iso: &str) {
    assert!(Cmd::new("git")
        .args(["commit", "-m", msg])
        .env("GIT_AUTHOR_DATE", ts_iso)
        .env("GIT_COMMITTER_DATE", ts_iso)
        .current_dir(dir)
        .status()
        .unwrap()
        .success());
}

fn write_and_add(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&p, body).unwrap();
    Cmd::new("git")
        .args(["add", rel])
        .current_dir(dir)
        .status()
        .unwrap();
}

// ── Atlas fixture helper ─────────────────────────────────────
//
// Writes a minimal-valid atoms.json + git_archaeology.json sidecar
// under <tmp>/atlas/. Returns the atlas dir for `assemble_brief`'s
// `atlas_dir` input.

fn write_atlas_fixture(
    tmp: &Path,
    atom_entries: &[(&str, &str, &str)], // (atom_id, file_path, canonical_name)
) -> PathBuf {
    let atlas_dir = tmp.join("atlas");
    std::fs::create_dir_all(&atlas_dir).unwrap();

    // atoms.json — minimal AtomsFile with Entity atoms only.
    let mut atoms_json = String::from(r#"{"schema_version":"2.0","atoms":["#);
    for (i, (id, _path, name)) in atom_entries.iter().enumerate() {
        if i > 0 {
            atoms_json.push(',');
        }
        // EnrichmentDepth is `snake_case` serde — see
        // corpus-engine/src/enrichment/pipeline/atlas.rs:62. EntityType
        // is a string_enum_with_other! ("concept" / "person" / ...).
        atoms_json.push_str(&format!(
            r#"{{"atom_type":"Entity","data":{{"id":"{id}","canonical_name":"{name}","entity_type":"concept","first_appearance":{{"chunk_id":"c{i}"}},"description":"test","salience":0.5,"enrichment_depth":"extracted"}}}}"#
        ));
    }
    atoms_json.push_str("]}");
    std::fs::write(atlas_dir.join("atoms.json"), atoms_json).unwrap();

    // git_archaeology.json — sidecar with atom_id → file_path.
    let mut arch_json = String::from(
        r#"{"corpus_id":"fixture","repo_root":"/tmp","atlas_built_at":0,"atom_count":0,"atoms_with_history":0,"follows_renames":false,"co_evolution":[],"staleness_summary":{"fresh":0,"moved":0},"provenance":["#,
    );
    for (i, (id, path, _name)) in atom_entries.iter().enumerate() {
        if i > 0 {
            arch_json.push(',');
        }
        arch_json.push_str(&format!(
            r#"{{"atom_id":"{id}","file_path":"{path}","first_seen":{{"hash":"deadbeef0000000000000000000000000000000{i}","date_iso":"2024-01-01","author_email":"alice@example.com","subject":"intro"}},"last_modified":{{"hash":"deadbeef0000000000000000000000000000000{i}","date_iso":"2024-01-01","author_email":"alice@example.com","subject":"intro"}},"stability_days":0,"modification_count":1,"primary_authors":["alice@example.com"],"staleness":"fresh"}}"#
        ));
    }
    arch_json.push_str("]}");
    std::fs::write(atlas_dir.join("git_archaeology.json"), arch_json).unwrap();

    atlas_dir
}

// ── Notes fixture helper ─────────────────────────────────────

async fn make_notes_with(rows: &[(&str, &str)]) -> (tempfile::TempDir, corpus_engine::NoteStore) {
    let tmp = tempfile::tempdir().unwrap();
    let store = corpus_engine::NoteStore::open(&tmp.path().join("notes.db")).unwrap();
    for (kind, content) in rows {
        store
            .write_note_with_relation(
                kind,
                content,
                vec![],
                vec![],
                "fixture-session",
                corpus_engine::NoteScope::Global,
                None,
                None,
            )
            .await
            .unwrap();
    }
    (tmp, store)
}

// ── Scenarios ────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_clean_main() {
    // Empty working set, empty notes, no atlas, no repo. Just the
    // header + the empty-working-set message.
    let (_notes_tmp, notes) = make_notes_with(&[]).await;
    let working_set: Vec<PathBuf> = vec![];
    let inputs = BriefInputs {
        working_set: &working_set,
        repo_root: None,
        atlas_dir: None,
        inquiries_dir: None,
        repo_name: "fixture",
        branch_name: "main",
        budget_tokens: 1500,
        feature_id: None,
    };
    let brief = assemble_brief(inputs, &notes).await.unwrap();
    assert_snapshot("01_clean_main", &brief);
}

#[tokio::test]
async fn snapshot_small_feature_branch_with_notes() {
    // 3 working-set files, 2 active notes, no atlas. Two sections
    // appear: working set + stated.
    let (_notes_tmp, notes) = make_notes_with(&[
        (
            "decision",
            "Auth flows route through loopback_guard. RFC-0017.",
        ),
        ("invariant", "No plaintext credentials in logs at any layer."),
    ])
    .await;
    let working_set = vec![
        PathBuf::from("src/auth/proxy.rs"),
        PathBuf::from("src/auth/loopback_guard.rs"),
        PathBuf::from("src/auth/middleware.rs"),
    ];
    let inputs = BriefInputs {
        working_set: &working_set,
        repo_root: None,
        atlas_dir: None,
        inquiries_dir: None,
        repo_name: "fixture",
        branch_name: "feature/auth-rate-limiting",
        budget_tokens: 1500,
        feature_id: None,
    };
    let brief = assemble_brief(inputs, &notes).await.unwrap();
    assert_snapshot("02_small_feature_branch_with_notes", &brief);
}

#[tokio::test]
async fn snapshot_large_refactor_caps_working_set_at_20() {
    // 60 working-set files, no notes, no atlas. Working-set list
    // caps at 20 with "+40 more".
    let (_notes_tmp, notes) = make_notes_with(&[]).await;
    let working_set: Vec<PathBuf> = (0..60)
        .map(|i| PathBuf::from(format!("src/mod_{i:02}.rs")))
        .collect();
    let inputs = BriefInputs {
        working_set: &working_set,
        repo_root: None,
        atlas_dir: None,
        inquiries_dir: None,
        repo_name: "fixture",
        branch_name: "refactor/big-cleanup",
        budget_tokens: 4000,
        feature_id: None,
    };
    let brief = assemble_brief(inputs, &notes).await.unwrap();
    assert_snapshot("03_large_refactor", &brief);
}

#[tokio::test]
async fn snapshot_with_atlas_and_archaeology() {
    // 2 working-set files, no notes, synthetic atlas with 2 atoms
    // anchored to those exact files. Three sections appear:
    // working set + structural (gaps section deferred to v0.5).
    let tmp = tempfile::tempdir().unwrap();
    let atlas_dir = write_atlas_fixture(
        tmp.path(),
        &[
            (
                "entity-0001",
                "src/auth/proxy.rs",
                "AuthProxy",
            ),
            (
                "entity-0002",
                "src/auth/loopback_guard.rs",
                "LoopbackGuard",
            ),
        ],
    );
    let (_notes_tmp, notes) = make_notes_with(&[]).await;
    let working_set = vec![
        PathBuf::from("src/auth/proxy.rs"),
        PathBuf::from("src/auth/loopback_guard.rs"),
    ];
    let inputs = BriefInputs {
        working_set: &working_set,
        repo_root: None,
        atlas_dir: Some(&atlas_dir),
        inquiries_dir: None,
        repo_name: "fixture",
        branch_name: "feature/auth-cleanup",
        budget_tokens: 2000,
        feature_id: None,
    };
    let brief = assemble_brief(inputs, &notes).await.unwrap();
    assert_snapshot("04_with_atlas_and_archaeology", &brief);
}

#[tokio::test]
async fn snapshot_principles_for_this_area() {
    // 2 working-set files, 2 inquiry TOMLs in a tempdir — only one
    // should match (file_globs targeting one of the two files). The
    // brief surfaces a "Principles for this area" section listing
    // the matched inquiry only, between Working set and Stated.
    let tmp = tempfile::tempdir().unwrap();
    let inquiries_dir = tmp.path().join("inquiries");
    std::fs::create_dir_all(&inquiries_dir).unwrap();
    std::fs::write(
        inquiries_dir.join("matching.toml"),
        r#"
[inquiry]
id = "principle_auth_proxy"
title = "All auth flows route through loopback_guard"
file_globs = ["**/auth/proxy.rs"]
min_score = 0.5
"#,
    )
    .unwrap();
    std::fs::write(
        inquiries_dir.join("non_matching.toml"),
        r#"
[inquiry]
id = "principle_unrelated"
title = "Unrelated principle that shouldn't surface here"
file_globs = ["**/storage/sqlite.rs"]
min_score = 0.5
"#,
    )
    .unwrap();

    let (_notes_tmp, notes) = make_notes_with(&[]).await;
    let working_set = vec![
        PathBuf::from("src/auth/proxy.rs"),
        PathBuf::from("src/auth/middleware.rs"),
    ];
    let inputs = BriefInputs {
        working_set: &working_set,
        repo_root: None,
        atlas_dir: None,
        inquiries_dir: Some(inquiries_dir.as_path()),
        repo_name: "fixture",
        branch_name: "feature/auth-rate-limiting",
        budget_tokens: 1500,
        feature_id: None,
    };
    let brief = assemble_brief(inputs, &notes).await.unwrap();
    assert_snapshot("05_principles_for_this_area", &brief);
}

#[tokio::test]
async fn snapshot_recent_activity_with_backdated_commits() {
    // Single working-set file with one commit dated within the
    // 7-day window AND one outside it; recent-activity section
    // shows only the in-window commit.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    init_repo(repo);
    write_and_add(repo, "src/auth/proxy.rs", "fn proxy_v1() {}\n");
    // Recent: today (use a far-future date relative to typical CI
    // boxes so this stays "recent" for years).
    let now_iso = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S +0000").to_string();
    commit_at(repo, "feat(auth): introduce proxy", &now_iso);
    write_and_add(repo, "src/auth/proxy.rs", "fn proxy_v2() {}\n");
    commit_at(repo, "refactor(auth): widen proxy contract", &now_iso);
    // Old: 2 years ago.
    write_and_add(repo, "src/auth/proxy.rs", "fn proxy_v0() {}\n");
    commit_at(
        repo,
        "fix(auth): legacy patch we don't expect to surface",
        "2020-01-01T00:00:00 +0000",
    );

    let (_notes_tmp, notes) = make_notes_with(&[]).await;
    let working_set = vec![PathBuf::from("src/auth/proxy.rs")];
    let inputs = BriefInputs {
        working_set: &working_set,
        repo_root: Some(repo),
        atlas_dir: None,
        inquiries_dir: None,
        repo_name: "fixture",
        branch_name: "main",
        budget_tokens: 2000,
        feature_id: None,
    };
    let brief = assemble_brief(inputs, &notes).await.unwrap();
    // Recent-activity section is non-deterministic on commit hash
    // (each `commit -m` produces a fresh hash). Pin only the
    // *structural* outcome: 2 recent + 0 old + we don't show the
    // 2020 commit subject.
    assert!(brief.contains("## Recent activity"));
    assert!(brief.contains("feat(auth): introduce proxy"));
    assert!(brief.contains("refactor(auth): widen proxy contract"));
    assert!(!brief.contains("legacy patch we don't expect"));
    // No snapshot for this scenario — its dynamic data (hashes)
    // would force a refresh on every run.
}
