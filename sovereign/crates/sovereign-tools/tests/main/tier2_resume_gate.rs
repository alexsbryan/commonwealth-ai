// SPDX-License-Identifier: AGPL-3.0-or-later
//! `resume_inflight_tier2` — the boot scan must not re-spawn extraction
//! over a source corpus whose enrichment the stall-sweep already killed.
//!
//! **The failing input.** On 2026-09-12 corpus `agent-sessions`
//! (this machine's Claude Code transcripts as `threaded_turns` chunks,
//! `[enrichment] type = "tiered"`) stalled at 12:09 local and every boot
//! resume scan re-entered its GLiNER pass. Instrumented on pid 47944 the
//! daemon went 20.2 GB → 79.9 GB in eight minutes with ZERO requests and
//! took two jetsam SIGTERMs. Four boot scans read that same sidecar and
//! all four said "resume"; this pins the tier-2 one.
//!
//! The predicate itself (`EnrichmentState::declared_dead`) is unit-tested
//! in `corpus-engine/src/enrichment/state.rs`. What this file adds is that
//! `resume_inflight_tier2` actually CONSULTS it — a gate nobody calls is
//! not a gate.

use std::path::Path;

use sovereign_tools::atlas_postinstall::{resume_inflight_tier2, AUTO_MANAGED_MARKER};

/// A tier-2 workspace the boot scan would otherwise resume: auto-managed
/// marker present, `config.json` present (so the launch path skips
/// `enrich init` and goes straight to a fast spawn attempt), a chapters
/// manifest with two chapters and an empty checkpoint (`done 0 < total 2`).
fn resumable_workspace(enrichment_dir: &Path, indexes_dir: &Path, source_corpus_id: &str) {
    let workspace_id = format!("{source_corpus_id}-tier2");
    let workspace_dir = enrichment_dir.join(&workspace_id);
    std::fs::create_dir_all(workspace_dir.join("runs")).unwrap();
    std::fs::write(workspace_dir.join(AUTO_MANAGED_MARKER), "").unwrap();
    std::fs::write(workspace_dir.join("config.json"), "{}").unwrap();

    let ws_index_dir = indexes_dir.join(&workspace_id);
    std::fs::create_dir_all(&ws_index_dir).unwrap();
    std::fs::write(
        ws_index_dir.join("chapters.json"),
        r#"{"chapters":[{"chapter_id":"c1"},{"chapter_id":"c2"}]}"#,
    )
    .unwrap();
}

/// Write the source corpus's `_enrichment_state.json` verbatim — the shape
/// a real daemon left on disk, not whatever the current serializer emits.
fn write_source_state(indexes_dir: &Path, source_corpus_id: &str, body: &str) {
    let dir = indexes_dir.join(source_corpus_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("_enrichment_state.json"), body).unwrap();
}

/// The positive control, and it is load-bearing: without it a gate that
/// skipped EVERYTHING would pass the test below while proving nothing.
/// `cli_binary` points at a path that does not exist, so the launch fails
/// fast — what matters is that the scan REACHED the launch.
#[tokio::test]
async fn a_live_source_corpus_still_resumes_tier2_extraction() {
    let tmp = tempfile::tempdir().unwrap();
    let enrichment_dir = tmp.path().join("enrichment");
    let indexes_dir = tmp.path().join("indexes");
    std::fs::create_dir_all(&enrichment_dir).unwrap();
    std::fs::create_dir_all(&indexes_dir).unwrap();
    resumable_workspace(&enrichment_dir, &indexes_dir, "wikipedia");

    let outcomes = resume_inflight_tier2(
        enrichment_dir,
        indexes_dir,
        tmp.path().join("no-such-sovereign-cli-llm"),
    )
    .await;

    assert_eq!(
        outcomes.len(),
        1,
        "an in-flight workspace with a healthy source must still be resumed"
    );
}

/// The failing input. Same workspace, same everything — only the source
/// corpus's sidecar changes, and the scan must refuse.
#[tokio::test]
async fn a_stalled_source_corpus_is_not_resumed() {
    let tmp = tempfile::tempdir().unwrap();
    let enrichment_dir = tmp.path().join("enrichment");
    let indexes_dir = tmp.path().join("indexes");
    std::fs::create_dir_all(&enrichment_dir).unwrap();
    std::fs::create_dir_all(&indexes_dir).unwrap();
    resumable_workspace(&enrichment_dir, &indexes_dir, "agent-sessions");
    write_source_state(
        &indexes_dir,
        "agent-sessions",
        r#"{"schema_version":1,"corpus_id":"agent-sessions",
            "pipeline_id":"folder_tiered","phase":"stalled",
            "step_current":0,"step_total":0,
            "started_at":1789200000,"last_progress_at":1789203017,
            "error":"stalled — no progress for 3017s (daemon likely restarted mid-pipeline)"}"#,
    );

    let outcomes = resume_inflight_tier2(
        enrichment_dir,
        indexes_dir,
        tmp.path().join("no-such-sovereign-cli-llm"),
    )
    .await;

    assert!(
        outcomes.is_empty(),
        "a stall-swept source corpus must not have its tier-2 extraction \
         re-spawned on boot; resume is an explicit operator action"
    );
}

/// The gate must be narrow: a source corpus that finished cleanly is not
/// dead, and its tier-2 deepening must still resume.
#[tokio::test]
async fn a_completed_source_corpus_still_resumes() {
    let tmp = tempfile::tempdir().unwrap();
    let enrichment_dir = tmp.path().join("enrichment");
    let indexes_dir = tmp.path().join("indexes");
    std::fs::create_dir_all(&enrichment_dir).unwrap();
    std::fs::create_dir_all(&indexes_dir).unwrap();
    resumable_workspace(&enrichment_dir, &indexes_dir, "wikipedia");
    write_source_state(
        &indexes_dir,
        "wikipedia",
        r#"{"schema_version":1,"corpus_id":"wikipedia","phase":"complete",
            "started_at":1789200000,"last_progress_at":1789203017,
            "completed_at":1789203017}"#,
    );

    let outcomes = resume_inflight_tier2(
        enrichment_dir,
        indexes_dir,
        tmp.path().join("no-such-sovereign-cli-llm"),
    )
    .await;

    assert_eq!(outcomes.len(), 1);
}
