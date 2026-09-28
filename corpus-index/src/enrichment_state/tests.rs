// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the enrichment state file (`enrichment_state.rs`).

use super::*;

#[test]
fn round_trips_state_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut state = EnrichmentState::new("c-1", Some("folder_tiered".into()));
    state.phase = EnrichmentPhase::RaptorLeaves;
    state.step_current = 17;
    state.step_total = 45;
    state.message = Some("summarising leaf 17 / 45".into());
    EnrichmentStateFile::write(tmp.path(), &state).unwrap();
    let read = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
    assert_eq!(read.corpus_id, "c-1");
    assert_eq!(read.phase, EnrichmentPhase::RaptorLeaves);
    assert_eq!(read.step_current, 17);
    assert_eq!(read.step_total, 45);
}

#[test]
fn stamp_preserves_started_at() {
    let tmp = tempfile::tempdir().unwrap();
    let initial = EnrichmentState::new("c-1", None);
    EnrichmentStateFile::write(tmp.path(), &initial).unwrap();
    // Force a delay so any naive `last_progress_at = started_at`
    // bug surfaces in the test.
    std::thread::sleep(std::time::Duration::from_secs(1));
    let after = EnrichmentStateFile::stamp(
        tmp.path(),
        "c-1",
        None,
        EnrichmentPhase::RaptorTree,
        2,
        5,
        Some("tree level 2 / 5"),
    )
    .unwrap();
    assert_eq!(after.started_at, initial.started_at);
    assert!(after.last_progress_at >= after.started_at);
}

#[test]
fn resumable_interruption_covers_stalled_and_nonterminal_but_not_done_or_failed() {
    // Resume the corpses a killed process leaves behind...
    assert!(EnrichmentPhase::Starting.is_resumable_interruption());
    assert!(EnrichmentPhase::RaptorLeaves.is_resumable_interruption());
    assert!(EnrichmentPhase::Persisting.is_resumable_interruption());
    // ...including Stalled (terminal, but the boot stall-sweep just
    // re-labelled an interrupted non-terminal build).
    assert!(EnrichmentPhase::Stalled.is_resumable_interruption());
    assert!(EnrichmentPhase::Stalled.is_terminal());
    // ...but never a clean finish or a genuine failure (would loop).
    assert!(!EnrichmentPhase::Complete.is_resumable_interruption());
    assert!(!EnrichmentPhase::Failed.is_resumable_interruption());
}

/// The failing input this predicate exists for, copied from the
/// `_enrichment_state.json` that re-entered GLiNER on every daemon
/// boot on 2026-09-12 (corpus `agent-sessions`, stalled 12:09 local,
/// resumed on every boot for the rest of the day).
#[test]
fn declared_dead_gates_a_stalled_build_and_an_errored_one() {
    let mut s = EnrichmentState::new("agent-sessions", Some("folder_tiered".into()));
    s.phase = EnrichmentPhase::Stalled;
    s.error = Some("stalled — no progress for 3017s".into());
    assert!(
        s.declared_dead(),
        "the boot stall-sweep already judged this build dead; no automatic \
             process may re-enter it"
    );

    // The phase alone is enough — the sweep writes fail() then stamp(),
    // and a crash between the two leaves Stalled with the error cleared.
    s.error = None;
    assert!(s.declared_dead(), "phase=Stalled alone is dead");

    // And the error alone is enough — the stall sweep's fail() write can
    // land while a concurrent doomed writer re-stamps a non-terminal
    // phase, which is the shape `is_stale` already defends against.
    let mut errored = EnrichmentState::new("c-1", None);
    errored.phase = EnrichmentPhase::RaptorLeaves;
    errored.error = Some("index for 'c-1' is missing _corpus_meta.json".into());
    assert!(errored.declared_dead());

    // `Failed` needs no arm of its own: fail() always stamps an error.
    let failed = EnrichmentStateFile::fail(
        tempfile::tempdir().unwrap().path(),
        "c-1",
        "provider is down",
    )
    .unwrap();
    assert_eq!(failed.phase, EnrichmentPhase::Failed);
    assert!(failed.declared_dead());
}

/// The gate must be narrow, and this is the regression that would cost
/// the most: `declared_dead` also gates the INGEST auto-resume, so a
/// corpus whose enrichment finished cleanly must still be free to
/// finish ingesting.
#[test]
fn declared_dead_is_false_for_a_live_or_completed_build() {
    let mut s = EnrichmentState::new("c-1", Some("folder_tiered".into()));
    assert!(!s.declared_dead(), "a fresh Starting build is alive");
    s.phase = EnrichmentPhase::RaptorLeaves;
    assert!(!s.declared_dead());
    s.phase = EnrichmentPhase::Complete;
    s.error = None;
    assert!(
        !s.declared_dead(),
        "a clean Complete is not dead — it is done, and it must not block \
             an in-progress ingest from resuming"
    );
}

/// The on-disk half, driven through the reader every boot resume scan
/// actually calls. Written as raw JSON rather than via `write()` so
/// the fixture pins the SHAPE a real daemon left behind, not whatever
/// the current serializer happens to emit.
#[test]
fn declared_dead_at_reads_the_sidecar_and_fails_open() {
    let write = |dir: &std::path::Path, body: &str| {
        std::fs::write(dir.join(ENRICHMENT_STATE_FILENAME), body).unwrap();
    };

    // The failing input: the sidecar `agent-sessions` carried all day
    // on 2026-09-12 while every boot re-entered its GLiNER pass.
    let dead = tempfile::tempdir().unwrap();
    write(
        dead.path(),
        r#"{"schema_version":1,"corpus_id":"agent-sessions",
                "pipeline_id":"folder_tiered","phase":"stalled",
                "step_current":0,"step_total":0,
                "started_at":1789200000,"last_progress_at":1789203017,
                "error":"stalled — no progress for 3017s (daemon likely restarted mid-pipeline)"}"#,
    );
    assert!(
        EnrichmentStateFile::read(dead.path()).unwrap().is_some(),
        "fixture must parse, else the assertion below is vacuous"
    );
    assert!(EnrichmentStateFile::declared_dead_at(dead.path()));

    // Narrow #1 — a live build still resumes.
    let live = tempfile::tempdir().unwrap();
    write(
        live.path(),
        r#"{"schema_version":1,"corpus_id":"c","phase":"raptor_leaves",
                "started_at":1789200000,"last_progress_at":1789203017}"#,
    );
    assert!(EnrichmentStateFile::read(live.path()).unwrap().is_some());
    assert!(!EnrichmentStateFile::declared_dead_at(live.path()));

    // Narrow #2 — no sidecar at all (a corpus never enriched) is the
    // case auto-resume exists for.
    let absent = tempfile::tempdir().unwrap();
    assert!(!EnrichmentStateFile::declared_dead_at(absent.path()));

    // Narrow #3 — fail OPEN on a corrupt sidecar.
    let corrupt = tempfile::tempdir().unwrap();
    write(corrupt.path(), "{not json");
    assert!(!EnrichmentStateFile::declared_dead_at(corrupt.path()));
}

#[test]
fn stamp_clears_stale_completed_at_when_a_new_run_supersedes() {
    // The folder-pipeline zombie: a prior run stamped Complete
    // (setting completed_at), then a fresh run re-enters a
    // non-terminal phase. The file must NOT keep the old
    // completed_at — that leaves `phase=non-terminal` +
    // `completed_at=set`, the exact self-contradiction that made
    // the desktop chip read "done" while work was still in flight.
    let tmp = tempfile::tempdir().unwrap();
    let mut done = EnrichmentState::new("c-1", Some("folder_tiered".into()));
    done.phase = EnrichmentPhase::Complete;
    done.completed_at = Some(done.last_progress_at);
    EnrichmentStateFile::write(tmp.path(), &done).unwrap();

    // A fresh run re-enters RaptorLeaves.
    let after = EnrichmentStateFile::stamp(
        tmp.path(),
        "c-1",
        Some("folder_tiered"),
        EnrichmentPhase::RaptorLeaves,
        0,
        8,
        Some("building RAPTOR tree"),
    )
    .unwrap();
    assert_eq!(after.phase, EnrichmentPhase::RaptorLeaves);
    assert!(
        after.completed_at.is_none(),
        "a non-terminal stamp must clear the prior run's completed_at"
    );
    assert!(!after.phase.is_terminal());
}

#[test]
fn is_stale_only_for_old_non_terminal_states() {
    let mut s = EnrichmentState::new("c-1", Some("folder_tiered".into()));
    let now = s.last_progress_at;
    // Fresh non-terminal build → not stale.
    assert!(!s.is_stale(now));
    // Just under the threshold → still live.
    assert!(!s.is_stale(now + STALL_THRESHOLD_SECS));
    // Past the threshold → wedged.
    assert!(s.is_stale(now + STALL_THRESHOLD_SECS + 1));
    // A terminal phase is never stale, however old.
    s.phase = EnrichmentPhase::Complete;
    assert!(!s.is_stale(now + STALL_THRESHOLD_SECS * 100));
    s.phase = EnrichmentPhase::Failed;
    assert!(!s.is_stale(now + STALL_THRESHOLD_SECS * 100));
    // Stalled is already terminal — not double-counted as "stale".
    s.phase = EnrichmentPhase::Stalled;
    assert!(!s.is_stale(now + STALL_THRESHOLD_SECS * 100));
}

#[test]
fn is_stale_when_error_stamped_on_nonterminal_phase() {
    // The real-world zombie: a stall sweep stamps an error but a
    // concurrent writer leaves phase=Starting and bumps
    // last_progress_at, so the age looks fresh. The error must still
    // win.
    let mut s = EnrichmentState::new("c-1", Some("folder_tiered".into()));
    let now = s.last_progress_at;
    s.error = Some("stalled — no progress for 3371s".into());
    assert!(s.phase == EnrichmentPhase::Starting);
    assert!(s.is_stale(now)); // fresh clock, but error present → stale
                              // A terminal phase with an error is a normal Failed — not "stale".
    s.phase = EnrichmentPhase::Failed;
    assert!(!s.is_stale(now));
}

#[test]
fn heartbeat_bumps_live_state_without_changing_phase() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = EnrichmentState::new("c-1", Some("folder_tiered".into()));
    s.phase = EnrichmentPhase::EntityExtraction;
    s.step_current = 3;
    s.step_total = 10;
    s.message = Some("Finding people, places, and ideas".into());
    // Backdate so a live build looks stale before the heartbeat.
    let stale_ts = now_secs() - STALL_THRESHOLD_SECS - 60;
    s.last_progress_at = stale_ts;
    EnrichmentStateFile::write(tmp.path(), &s).unwrap();
    assert!(
        s.is_stale(now_secs()),
        "precondition: backdated build is stale"
    );

    EnrichmentStateFile::heartbeat(tmp.path()).unwrap();

    let after = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
    assert!(
        after.last_progress_at > stale_ts,
        "heartbeat must advance last_progress_at"
    );
    assert!(
        !after.is_stale(now_secs()),
        "heartbeat must clear staleness"
    );
    // Phase / step / message are preserved — only the clock moves.
    assert_eq!(after.phase, EnrichmentPhase::EntityExtraction);
    assert_eq!(after.step_current, 3);
    assert_eq!(after.step_total, 10);
    assert_eq!(
        after.message.as_deref(),
        Some("Finding people, places, and ideas")
    );
}

#[test]
fn heartbeat_is_noop_on_terminal_state() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = EnrichmentState::new("c-1", None);
    s.phase = EnrichmentPhase::Complete;
    let ts = now_secs() - 500;
    s.last_progress_at = ts;
    s.completed_at = Some(ts);
    EnrichmentStateFile::write(tmp.path(), &s).unwrap();

    EnrichmentStateFile::heartbeat(tmp.path()).unwrap();

    let after = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
    assert_eq!(after.phase, EnrichmentPhase::Complete);
    assert_eq!(
        after.last_progress_at, ts,
        "heartbeat must not touch a terminal state"
    );
}

#[test]
fn heartbeat_is_noop_on_errored_nonterminal_state() {
    // A build a sweeper declared dead (error stamped on a still
    // non-terminal phase) must not be resurrected by a heartbeat.
    let tmp = tempfile::tempdir().unwrap();
    let mut s = EnrichmentState::new("c-1", Some("folder_tiered".into()));
    s.phase = EnrichmentPhase::Starting;
    s.error = Some("stalled — no progress for 3371s".into());
    let ts = now_secs() - 500;
    s.last_progress_at = ts;
    EnrichmentStateFile::write(tmp.path(), &s).unwrap();

    EnrichmentStateFile::heartbeat(tmp.path()).unwrap();

    let after = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
    assert_eq!(
        after.last_progress_at, ts,
        "errored state must stay untouched"
    );
    assert!(
        after.is_stale(now_secs()),
        "errored state stays stale after heartbeat"
    );
}

#[test]
fn heartbeat_is_noop_when_state_absent() {
    let tmp = tempfile::tempdir().unwrap();
    // No state file yet — heartbeat is a clean no-op, does not create one.
    EnrichmentStateFile::heartbeat(tmp.path()).unwrap();
    assert!(EnrichmentStateFile::read(tmp.path()).unwrap().is_none());
}

#[tokio::test]
async fn heartbeat_guard_keeps_a_live_build_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = EnrichmentState::new("c-1", Some("folder_tiered".into()));
    s.phase = EnrichmentPhase::EntityExtraction;
    s.last_progress_at = now_secs() - STALL_THRESHOLD_SECS - 60;
    EnrichmentStateFile::write(tmp.path(), &s).unwrap();

    let guard = EnrichmentHeartbeat::spawn_every(
        tmp.path().to_path_buf(),
        std::time::Duration::from_millis(20),
    );
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    let after = EnrichmentStateFile::read(tmp.path()).unwrap().unwrap();
    assert!(
        !after.is_stale(now_secs()),
        "guard should have bumped last_progress_at at least once"
    );
    assert_eq!(after.phase, EnrichmentPhase::EntityExtraction);
    drop(guard);
}

#[test]
fn sweep_marks_old_non_terminal_as_stalled() {
    let tmp = tempfile::tempdir().unwrap();
    let corpus_dir = tmp.path().join("corpus-a");
    std::fs::create_dir_all(&corpus_dir).unwrap();
    let mut stale = EnrichmentState::new("corpus-a", Some("folder_tiered".into()));
    stale.phase = EnrichmentPhase::RaptorLeaves;
    stale.last_progress_at = now_secs() - STALL_THRESHOLD_SECS - 60;
    EnrichmentStateFile::write(&corpus_dir, &stale).unwrap();

    let transitioned = sweep_stalled_states(tmp.path()).unwrap();
    assert_eq!(transitioned, vec!["corpus-a".to_string()]);
    let after = EnrichmentStateFile::read(&corpus_dir).unwrap().unwrap();
    assert_eq!(after.phase, EnrichmentPhase::Stalled);
    assert!(after.error.is_some());
}

#[test]
fn sweep_leaves_recent_progress_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let corpus_dir = tmp.path().join("corpus-b");
    std::fs::create_dir_all(&corpus_dir).unwrap();
    let mut fresh = EnrichmentState::new("corpus-b", None);
    fresh.phase = EnrichmentPhase::RaptorTree;
    fresh.last_progress_at = now_secs() - 60; // 1 min ago
    EnrichmentStateFile::write(&corpus_dir, &fresh).unwrap();

    let transitioned = sweep_stalled_states(tmp.path()).unwrap();
    assert!(transitioned.is_empty());
    let after = EnrichmentStateFile::read(&corpus_dir).unwrap().unwrap();
    assert_eq!(after.phase, EnrichmentPhase::RaptorTree);
}
