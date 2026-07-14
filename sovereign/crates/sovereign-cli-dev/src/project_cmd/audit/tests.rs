// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the `svrn project audit` rollup — feature-row discovery and
//! the multi-source note-assembly / rendering path. Co-located with
//! `super` (audit/mod.rs); split from the file only to keep each under
//! the ARCH §3.1 1,200-line ceiling.

use super::*;

// ─── Phase 6: directory-only features show up in audit ────────

/// `collect_feature_rows` returns one row for a feature with a
/// `.sovereign/features/<id>/spec.md` on disk and no
/// `features.db`. Phase 6: this is the new default — users do
/// NOT need to run `svrn atos provision` to have a feature
/// surface in the audit; writing the spec is sufficient.
#[tokio::test]
async fn audit_lists_directory_only_feature_without_features_db() {
    let tmp = tempfile::tempdir().unwrap();
    let sov = tmp.path().to_path_buf();
    // Spec on disk, no features.db at all.
    let foo = sov.join("features").join("foo");
    std::fs::create_dir_all(&foo).unwrap();
    std::fs::write(foo.join("spec.md"), b"# foo\n").unwrap();

    let rows = collect_feature_rows(&sov).await;
    assert_eq!(rows.len(), 1, "expected one row for directory-only foo");
    assert_eq!(rows[0].id, "foo");
    assert_eq!(rows[0].state, "(directory only)");
    assert!(
        rows[0].spec_present,
        "directory-only feature with spec.md must report spec_present=true"
    );
    assert!(
        !rows[0].auto_redteam,
        "directory-only feature defaults to auto_redteam=false"
    );
}

/// A feature directory WITHOUT a `spec.md` still appears in the
/// audit (so the user sees the empty scaffold) but
/// `spec_present` is false. The audit will render this as
/// "missing" so the user knows to write the spec.
#[tokio::test]
async fn audit_lists_directory_only_feature_with_missing_spec() {
    let tmp = tempfile::tempdir().unwrap();
    let sov = tmp.path().to_path_buf();
    let foo = sov.join("features").join("foo");
    std::fs::create_dir_all(&foo).unwrap();
    // Sibling file, but no spec.md.
    std::fs::write(foo.join("brief.md"), b"# foo\n").unwrap();

    let rows = collect_feature_rows(&sov).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "foo");
    assert!(
        !rows[0].spec_present,
        "feature dir without spec.md must report spec_present=false"
    );
}

/// Empty `.sovereign/` (no features dir, no db) → empty Vec, so
/// the audit emits the "no features yet" pointer.
#[tokio::test]
async fn audit_returns_empty_when_no_features_anywhere() {
    let tmp = tempfile::tempdir().unwrap();
    let rows = collect_feature_rows(tmp.path()).await;
    assert!(
        rows.is_empty(),
        "expected no rows in an empty sovereign dir"
    );
}

/// Two features on disk → two rows, sorted alphabetically. The
/// BTreeMap key ordering is part of the contract — operators
/// scanning the audit table benefit from stable layout.
#[tokio::test]
async fn audit_returns_alphabetically_sorted_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let features = tmp.path().join("features");
    for id in &["zeta", "alpha", "mu"] {
        let dir = features.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("spec.md"), b"# spec\n").unwrap();
    }
    let rows = collect_feature_rows(tmp.path()).await;
    let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    assert_eq!(ids, vec!["alpha", "mu", "zeta"]);
}

// ─── Phase 7.3: multi-source audit assembly tests ─────────────

use corpus_engine_notes::{NoteScope, NoteSource, NoteStore};

async fn write_note(
    store: &NoteStore,
    kind: &str,
    body: &str,
    source: NoteSource,
    supersedes: Option<&str>,
) -> String {
    store
        .write_note_with_source(
            kind,
            body,
            Vec::new(),
            Vec::new(),
            "audit-test",
            NoteScope::Global,
            None,
            None,
            source,
            supersedes,
        )
        .await
        .unwrap()
}

/// Decisions section sorts by source priority (agent > committed
/// > extracted > inferred > observed) and renders the
/// `[source]` suffix on each row. Same-priority rows fall back
/// to created_at desc.
#[tokio::test]
async fn render_decisions_orders_by_source_priority_with_source_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::open(&dir.path().join("notes.db")).unwrap();
    // Mix sources so the priority sort has work to do.
    let _e = write_note(
        &store,
        "decision",
        "extracted decision E",
        NoteSource::Extracted,
        None,
    )
    .await;
    let _a = write_note(
        &store,
        "decision",
        "agent decision A",
        NoteSource::Agent,
        None,
    )
    .await;
    let _c = write_note(
        &store,
        "decision",
        "committed decision C",
        NoteSource::Committed,
        None,
    )
    .await;

    let notes = gather_audit_notes(&store).await;
    // Ordering: A (agent, p=4), C (committed, p=3), E (extracted, p=2).
    assert_eq!(notes.decisions[0].content, "agent decision A");
    assert_eq!(notes.decisions[1].content, "committed decision C");
    assert_eq!(notes.decisions[2].content, "extracted decision E");

    let rendered = render_decisions(&notes);
    // Each row carries the [source] suffix.
    assert!(rendered.contains("_[agent]_"));
    assert!(rendered.contains("_[committed]_"));
    assert!(rendered.contains("_[extracted]_"));
    // The agent row appears BEFORE committed/extracted in the
    // rendered output.
    let i_a = rendered.find("agent decision A").unwrap();
    let i_c = rendered.find("committed decision C").unwrap();
    let i_e = rendered.find("extracted decision E").unwrap();
    assert!(i_a < i_c, "agent should render above committed");
    assert!(i_c < i_e, "committed should render above extracted");
}

/// Empty store → "no decisions recorded yet" placeholder, no panic.
#[tokio::test]
async fn render_decisions_empty_store_renders_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::open(&dir.path().join("notes.db")).unwrap();
    let notes = gather_audit_notes(&store).await;
    let rendered = render_decisions(&notes);
    assert!(rendered.contains("no decisions recorded yet"));
}

/// A reversal — note with `supersedes` set to a prior id —
/// renders under the original as an indented "↳ REVERSED"
/// sub-line. The reversal does NOT also appear at the top
/// level (already_rendered guard).
#[tokio::test]
async fn render_decisions_renders_reversal_under_original() {
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::open(&dir.path().join("notes.db")).unwrap();
    let original_id = write_note(
        &store,
        "decision",
        "BTreeMap over HashMap — ordered iteration",
        NoteSource::Agent,
        None,
    )
    .await;
    let _reversal_id = write_note(
        &store,
        "decision",
        "HashMap over BTreeMap — random access pattern",
        NoteSource::Extracted,
        Some(&original_id),
    )
    .await;

    let notes = gather_audit_notes(&store).await;
    let rendered = render_decisions(&notes);

    assert!(
        rendered.contains("BTreeMap over HashMap"),
        "original missing from render: {rendered}"
    );
    assert!(
        rendered.contains("↳ REVERSED"),
        "reversal marker missing: {rendered}"
    );
    assert!(
        rendered.contains("HashMap over BTreeMap"),
        "reversal text missing: {rendered}"
    );
    // The reversal text should appear BELOW the original, AFTER
    // the "↳ REVERSED" marker.
    let i_original = rendered.find("BTreeMap over HashMap").unwrap();
    let i_reverse_marker = rendered.find("↳ REVERSED").unwrap();
    let i_reverse_text = rendered.find("HashMap over BTreeMap").unwrap();
    assert!(
        i_original < i_reverse_marker,
        "original should come before the reversal marker"
    );
    assert!(
        i_reverse_marker < i_reverse_text,
        "reversal marker should come before reversal body text"
    );
    // The reversal does NOT appear as a separate top-level row
    // (the renderer skips supersedes-set rows that have a
    // visible original).
    let count_top = rendered.matches("HashMap over BTreeMap").count();
    assert_eq!(
        count_top, 1,
        "reversal should appear exactly once (under the original); got {count_top}"
    );
}

/// Reversal pointing at a row that's no longer in our visible
/// set (e.g. retired separately) renders as a top-level
/// orphan rather than being silently dropped.
#[tokio::test]
async fn render_decisions_orphan_reversal_renders_at_top_level() {
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::open(&dir.path().join("notes.db")).unwrap();
    // The "original" id is fabricated; the reversal points at a
    // row we never wrote, so the renderer's by_id lookup misses
    // and the reversal must show as a standalone row.
    let _r = write_note(
        &store,
        "decision",
        "orphan reversal — original gone",
        NoteSource::Extracted,
        Some("nonexistent-id"),
    )
    .await;

    let notes = gather_audit_notes(&store).await;
    let rendered = render_decisions(&notes);
    assert!(
        rendered.contains("orphan reversal"),
        "orphan reversal should still render: {rendered}"
    );
}

/// Open-questions section flags `source=inferred` rows as low
/// confidence so the reviewer sees the trust ordering.
#[tokio::test]
async fn render_open_questions_marks_inferred_as_low_confidence() {
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::open(&dir.path().join("notes.db")).unwrap();
    let _agent = write_note(
        &store,
        "uncertainty",
        "what happens on a partial commit?",
        NoteSource::Agent,
        None,
    )
    .await;
    let _inferred = write_note(
        &store,
        "uncertainty",
        "is the cache TTL correct?",
        NoteSource::Inferred,
        None,
    )
    .await;

    let notes = gather_audit_notes(&store).await;
    let rendered = render_open_questions(&notes);
    assert!(rendered.contains("partial commit"));
    assert!(rendered.contains("cache TTL"));
    // The inferred row carries the low-confidence suffix.
    let inferred_line = rendered.lines().find(|l| l.contains("cache TTL")).unwrap();
    assert!(
        inferred_line.contains("low confidence") || rendered.contains("(low confidence)"),
        "inferred row missing low-confidence marker: {rendered}"
    );
    // The agent row does NOT carry it.
    let agent_line = rendered
        .lines()
        .find(|l| l.contains("partial commit"))
        .unwrap();
    assert!(
        !agent_line.contains("low confidence"),
        "agent row should not be flagged as low confidence: {agent_line}"
    );
}

/// Observed patterns section pulls notes tagged with
/// `source=observed` regardless of kind.
#[tokio::test]
async fn render_observed_patterns_lists_observed_source_notes() {
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::open(&dir.path().join("notes.db")).unwrap();
    let _o = write_note(
        &store,
        "reflection",
        "Investigated impact (blast) before running build.",
        NoteSource::Observed,
        None,
    )
    .await;

    let notes = gather_audit_notes(&store).await;
    let rendered = render_observed_patterns(&notes);
    assert!(rendered.contains("## Observed patterns"));
    assert!(rendered.contains("Investigated impact"));
    assert!(rendered.contains("_[observed]_"));
}

/// Empty observed list → no section header at all (we don't
/// want an empty section dangling in the audit).
#[tokio::test]
async fn render_observed_patterns_empty_renders_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::open(&dir.path().join("notes.db")).unwrap();
    // Decision-only — no observed rows.
    let _d = write_note(
        &store,
        "decision",
        "agent decision",
        NoteSource::Agent,
        None,
    )
    .await;
    let notes = gather_audit_notes(&store).await;
    let rendered = render_observed_patterns(&notes);
    assert!(
        rendered.is_empty(),
        "observed-patterns section should be empty when no observed notes exist; got: {rendered}"
    );
}

/// `gather_audit_notes` populates the by_id index so the
/// reversal lookup in `render_decisions` works.
#[tokio::test]
async fn gather_audit_notes_populates_by_id_index() {
    let dir = tempfile::tempdir().unwrap();
    let store = NoteStore::open(&dir.path().join("notes.db")).unwrap();
    let id = write_note(&store, "decision", "anchor", NoteSource::Agent, None).await;
    let notes = gather_audit_notes(&store).await;
    assert!(notes.by_id.contains_key(&id));
    assert_eq!(notes.by_id.get(&id).unwrap().content, "anchor");
}
