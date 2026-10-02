// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
#[test]
fn embed_phase_is_not_filed_under_index_building() {
    // The engine renders IngestProgress::Embedding as "Building the
    // index". Keying on the prose would put embedding cost — usually
    // the biggest slice of tier 1 — under a row an operator reads as
    // index construction, and the two would be indistinguishable.
    assert_eq!(stable_ingest_phase("Building the index"), "ingest:embed");
    assert_eq!(stable_ingest_phase("Writing index"), "ingest:index_write");
    assert_eq!(
        stable_ingest_phase("Optimizing search index"),
        "ingest:index_ann_build"
    );
}

#[test]
fn completion_label_does_not_smuggle_its_own_duration_into_the_key() {
    // "Done in 7s" as a key means run A and run B never share a
    // phase name, which would silently break --compare.
    assert_eq!(stable_ingest_phase("Done in 0s"), "ingest:done");
    assert_eq!(stable_ingest_phase("Done in 431s"), "ingest:done");
}

#[test]
fn unknown_engine_labels_are_bucketed_not_minted() {
    // The Enriching arm forwards a free-form detail string; letting
    // it through would grow an unbounded set of phase names.
    assert_eq!(
        stable_ingest_phase("clustering pass 3 of 9"),
        "ingest:engine_enrich_hook"
    );
}

#[test]
fn observer_closes_a_phase_when_the_label_changes() {
    let obs = BuildObserver::new();
    obs.ingest_transition("scanning", serde_json::Value::Null);
    obs.ingest_transition("scanning", serde_json::Value::Null);
    obs.ingest_transition("staging", serde_json::Value::Null);
    obs.close_open_ingest();
    let (phases, transitions, _, _) = obs.snapshot();
    assert_eq!(transitions.len(), 3, "every transition is logged");
    let labels: Vec<&str> = phases.iter().map(|p| p.phase.as_str()).collect();
    assert_eq!(
        labels,
        vec!["scanning", "staging"],
        "repeat labels extend the open span rather than opening a new one"
    );
}

#[test]
fn unobserved_time_never_inflates_the_previous_phase() {
    // The failure this guards against, measured on a real fixture
    // run: `staging` emitted its last event at 7ms, the next event
    // arrived at 8517ms, and the intervening extract+embed work was
    // reported as 8.5s of staging.
    let obs = BuildObserver::new();
    obs.ingest_transition_at(0, "staging", serde_json::Value::Null);
    obs.ingest_transition_at(7, "staging", serde_json::Value::Null);
    obs.ingest_transition_at(8517, "ingest:index_write", serde_json::Value::Null);
    let (phases, _, _, _) = obs.snapshot();
    let staging = phases
        .iter()
        .find(|p| p.phase == "staging")
        .expect("staging span");
    assert_eq!(staging.ms, 7, "staging ends at its own last event");
    let gap = phases
        .iter()
        .find(|p| p.phase == "unattributed:after:staging")
        .expect("the unobserved window is reported, not absorbed");
    assert_eq!(gap.start_ms, 7);
    assert_eq!(gap.end_ms, 8517);
}

#[test]
fn preflight_refuses_an_unreadable_source_before_anything_is_deleted() {
    // A path that stats but cannot be listed is the macOS TCC shape,
    // and it is the case that would turn a cold reset into data loss.
    // A nonexistent path exercises the same refusal.
    let err = preflight_source_readable(
        Path::new("/definitely/not/a/real/vault/path"),
        &["md".to_string()],
    )
    .unwrap_err();
    assert!(err.contains("does not exist"), "got: {err}");
}

#[test]
fn preflight_refuses_a_readable_but_empty_source() {
    let dir = std::env::temp_dir().join("vault-report-preflight-empty");
    let _ = std::fs::create_dir_all(&dir);
    let err = preflight_source_readable(&dir, &["md".to_string()]).unwrap_err();
    assert!(err.contains("REFUSING TO RESET"), "got: {err}");
    assert!(err.contains("0 ingestible file(s)"), "got: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn preflight_refuses_a_directory_that_stats_but_cannot_be_listed() {
    // This is the TCC shape precisely: the path exists, `is_dir()`
    // is true, there ARE matching files inside — and `read_dir`
    // fails. Any guard written against `exists()`/`is_dir()` passes
    // here and proceeds to delete the corpus.
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join("vault-report-preflight-unreadable");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("real-note.md"), "content").unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();

    let listable = std::fs::read_dir(&dir).is_ok();
    if listable {
        // Running as root (or a filesystem ignoring mode bits) —
        // the precondition doesn't hold, so the test proves nothing.
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    assert!(dir.exists() && dir.is_dir(), "the naive checks still pass");
    let err = preflight_source_readable(&dir, &["md".to_string()]).unwrap_err();
    assert!(err.contains("REFUSING TO RESET"), "got: {err}");
    assert!(
        err.contains("could not be read"),
        "the refusal must name the read failure, not just report an empty folder: {err}"
    );

    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preflight_counts_matching_files_and_ignores_dot_dirs() {
    let dir = std::env::temp_dir().join("vault-report-preflight-ok");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    // .obsidian holds config, never corpus content — counting it
    // would let an all-config folder pass the guard.
    std::fs::create_dir_all(dir.join(".obsidian")).unwrap();
    std::fs::write(dir.join("a.md"), "x").unwrap();
    std::fs::write(dir.join("sub/b.md"), "x").unwrap();
    std::fs::write(dir.join("c.txt"), "x").unwrap();
    std::fs::write(dir.join(".obsidian/app.md"), "x").unwrap();
    let n = preflight_source_readable(&dir, &["md".to_string()]).unwrap();
    assert_eq!(
        n, 2,
        "nested markdown counts; dot-dirs and other extensions do not"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sub_threshold_gaps_are_not_reported_as_phases() {
    // Callback scheduling jitter between two adjacent phases is not
    // a finding; only a real observation hole is.
    let obs = BuildObserver::new();
    obs.push_gap("staging", 100, 100 + UNATTRIBUTED_GAP_MS - 1);
    let (phases, _, _, _) = obs.snapshot();
    assert!(phases.is_empty(), "got: {phases:?}");
}
