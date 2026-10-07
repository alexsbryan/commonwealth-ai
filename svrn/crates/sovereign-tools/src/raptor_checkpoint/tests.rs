// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unit tests for `raptor_checkpoint`, in a sibling file so the module stays
//! under the 800-line band (ARCH §3.1). Mounted with `#[path]`, so names are
//! unchanged.

use super::*;

fn dummy_node(level: u8, idx: usize) -> RaptorNode {
    RaptorNode {
        node_id: format!("L{level}-N{idx}"),
        level,
        summary: format!("summary for cluster {idx} at level {level}"),
        summary_embedding: vec![0.1, 0.2, 0.3],
        centroid_embedding: vec![0.4, 0.5, 0.6],
        children_node_ids: Vec::new(),
        direct_member_chunk_ids: vec![idx as u32 * 10, idx as u32 * 10 + 1],
        evidence_chunk_ids: vec![idx as u32 * 10, idx as u32 * 10 + 1],
        quote_spans: Vec::new(),
        primary_entities: Vec::new(),
        cluster_coherence: 0.8,
        created_at: chrono::Utc::now(),
        prompt_version: String::new(),
        summarizer_model: String::new(),
    }
}

#[test]
fn input_hash_is_order_independent() {
    let h1 = RaptorCheckpointHandle::compute_input_hash(&[3, 1, 2], 1024, "pv1", 20);
    let h2 = RaptorCheckpointHandle::compute_input_hash(&[1, 2, 3], 1024, "pv1", 20);
    assert_eq!(h1, h2);
}

#[test]
fn input_hash_changes_on_member_change() {
    let h1 = RaptorCheckpointHandle::compute_input_hash(&[1, 2, 3], 1024, "pv1", 20);
    let h2 = RaptorCheckpointHandle::compute_input_hash(&[1, 2, 4], 1024, "pv1", 20);
    assert_ne!(h1, h2);
}

#[test]
fn input_hash_changes_on_prompt_version_change() {
    // T1 P1.3: a prompt bump must invalidate the checkpoint —
    // otherwise a resume resurrects old-prompt cluster summaries
    // into a build that claims the new prompt version.
    let h1 = RaptorCheckpointHandle::compute_input_hash(&[1, 2, 3], 1024, "pv1", 20);
    let h2 = RaptorCheckpointHandle::compute_input_hash(&[1, 2, 3], 1024, "pv2", 20);
    assert_ne!(h1, h2);
}

/// Leaf size is checkpoint identity, but only off the default: a
/// default-shape checkpoint keeps the hash the pre-field formula gave it.
#[test]
fn input_hash_carries_leaf_target_only_off_the_default() {
    let mut legacy = blake3::Hasher::new();
    for id in [1u32, 2, 3] {
        legacy.update(&id.to_le_bytes());
    }
    legacy.update(&1024u32.to_le_bytes());
    legacy.update(b"pv1");
    let legacy = legacy.finalize().to_hex().to_string();
    let default = RaptorCheckpointHandle::compute_input_hash(&[1, 2, 3], 1024, "pv1", 20);
    let seven = RaptorCheckpointHandle::compute_input_hash(&[1, 2, 3], 1024, "pv1", 7);
    assert_eq!(default, legacy);
    assert_ne!(seven, default);
}

/// Reopening keeps level 0 (clustering and nodes) and drops every level
/// above it, so `load_all_nodes` cannot read back a stale upper node.
#[test]
fn reopen_above_leaves_keeps_level_0_and_clears_completion() {
    let tmp = tempfile::tempdir().unwrap();
    let h = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
    h.ensure_manifest().unwrap();
    h.write_cluster_node(0, 0, &dummy_node(0, 0)).unwrap();
    h.write_cluster_node(1, 0, &dummy_node(1, 0)).unwrap();
    h.mark_complete(4).unwrap();
    assert_eq!(h.read_manifest().unwrap().root_ceiling, Some(4));
    h.reopen_above_leaves().unwrap();
    let m = h.read_manifest().unwrap();
    assert!(m.completed_at.is_none() && m.root_ceiling.is_none());
    let levels: Vec<u8> = h
        .load_all_nodes()
        .unwrap()
        .iter()
        .map(|n| n.level)
        .collect();
    assert_eq!(levels, vec![0]);
}

#[test]
fn decide_returns_fresh_when_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let h = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
    assert!(matches!(h.decide(), CheckpointDecision::Fresh));
}

#[test]
fn decide_returns_resume_when_hash_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let h = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
    h.ensure_manifest().unwrap();
    assert!(matches!(h.decide(), CheckpointDecision::Resume(_)));
}

#[test]
fn decide_returns_stale_when_hash_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let h_old = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
    h_old.ensure_manifest().unwrap();
    let h_new = RaptorCheckpointHandle::at(tmp.path(), "hash-b");
    assert!(matches!(h_new.decide(), CheckpointDecision::StaleAndReset));
}

#[test]
fn at_note_scopes_checkpoints_per_conversation() {
    // The skip-already-built guard. Under the old shared slot, note
    // A completing its build overwrote the single manifest, so note
    // B's decide() saw A's input_hash → StaleAndReset → full rebuild,
    // once per already-built note on every mid-vault restart.
    // Per-note keying must keep the two slots independent: A being
    // Resume-able must not perturb B, and vice versa.
    let tmp = tempfile::tempdir().unwrap();
    let note_a = RaptorCheckpointHandle::at_note(tmp.path(), "Parable of Yakumo.md", "hash-a");
    let note_b = RaptorCheckpointHandle::at_note(tmp.path(), "Grandmother Sato.md", "hash-b");

    // Distinct on-disk slots.
    assert_ne!(note_a.dir, note_b.dir);

    // The index-root accessor is the layout contract `at_note`
    // creates: two parents up from the slot, for every slot.
    assert_eq!(note_a.index_dir(), tmp.path());
    assert_eq!(note_b.index_dir(), tmp.path());

    // Note A finishes and writes its manifest…
    note_a.ensure_manifest().unwrap();
    assert!(matches!(note_a.decide(), CheckpointDecision::Resume(_)));
    // …and note B is untouched — NOT dragged to StaleAndReset by A's
    // completion (this is the exact shared-slot bug).
    assert!(matches!(note_b.decide(), CheckpointDecision::Fresh));

    // Now B finishes too; both remain independently resumable.
    note_b.ensure_manifest().unwrap();
    assert!(matches!(note_a.decide(), CheckpointDecision::Resume(_)));
    assert!(matches!(note_b.decide(), CheckpointDecision::Resume(_)));
}

#[test]
fn at_note_hashes_path_like_uuids_to_a_single_safe_component() {
    // conv_uuids are root-relative paths; a nested one must resolve
    // to exactly one directory below the checkpoint root, never
    // escaping it or spawning surprise subdirs.
    let tmp = tempfile::tempdir().unwrap();
    let nested = RaptorCheckpointHandle::at_note(tmp.path(), "sub/dir/Deep Note.md", "hash-a");
    let root = tmp.path().join(CHECKPOINT_SUBDIR);
    assert_eq!(nested.dir.parent(), Some(root.as_path()));
    let slot = nested.dir.file_name().unwrap().to_str().unwrap();
    assert!(slot.starts_with("note-"), "slot = {slot}");
    assert!(!slot.contains('/') && !slot.contains(std::path::MAIN_SEPARATOR));
}

#[test]
fn round_trips_cluster_node() {
    let tmp = tempfile::tempdir().unwrap();
    let h = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
    h.ensure_manifest().unwrap();
    let node = dummy_node(0, 7);
    h.write_cluster_node(0, 7, &node).unwrap();
    let read = h.read_cluster_node(0, 7).unwrap().unwrap();
    assert_eq!(read.node_id, node.node_id);
    assert_eq!(read.summary, node.summary);
}

#[test]
fn load_all_nodes_sorts_by_level_then_idx() {
    let tmp = tempfile::tempdir().unwrap();
    let h = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
    h.ensure_manifest().unwrap();
    h.write_cluster_node(0, 2, &dummy_node(0, 2)).unwrap();
    h.write_cluster_node(0, 0, &dummy_node(0, 0)).unwrap();
    h.write_cluster_node(1, 0, &dummy_node(1, 0)).unwrap();
    let all = h.load_all_nodes().unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].node_id, "L0-N0");
    assert_eq!(all[1].node_id, "L0-N2");
    assert_eq!(all[2].node_id, "L1-N0");
}

/// The defect: nodes written to a PER-NOTE slot are invisible to the
/// shared-slot reader, and the miss is shaped exactly like "no tree".
/// `summary_atoms::load_tree` read the shared slot, so every `Summary`
/// atom on a per-note corpus was projected with `evidence: []` and
/// `children: []` — uncitable — and said so under a wrong explanation.
#[test]
fn per_note_nodes_are_invisible_to_the_shared_slot_and_found_by_the_corpus_reader() {
    let tmp = tempfile::tempdir().unwrap();
    let note = RaptorCheckpointHandle::at_note(tmp.path(), "The Pilot and His Wife.txt", "h");
    note.ensure_manifest().unwrap();
    note.write_cluster_node(0, 0, &dummy_node(0, 0)).unwrap();
    note.write_cluster_node(1, 0, &dummy_node(1, 0)).unwrap();

    // The old reader: a directory full of `note-*` matches no `level-`
    // prefix, so this is Ok(empty) — not an error, just silence.
    let shared = RaptorCheckpointHandle::at(tmp.path(), String::new())
        .load_all_nodes()
        .unwrap();
    assert!(
        shared.is_empty(),
        "shared slot must not see per-note nodes — that is the trap, not a bug to fix here"
    );

    let all = RaptorCheckpointHandle::load_corpus_nodes(tmp.path()).unwrap();
    assert_eq!(
        all.len(),
        2,
        "the corpus reader must find both per-note nodes"
    );
    assert!(RaptorCheckpointHandle::corpus_has_slots(tmp.path()));
}

#[test]
fn a_corpus_with_no_checkpoint_at_all_has_no_slots() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(!RaptorCheckpointHandle::corpus_has_slots(tmp.path()));
    assert!(RaptorCheckpointHandle::load_corpus_nodes(tmp.path())
        .unwrap()
        .is_empty());
}

#[test]
fn the_corpus_reader_unions_both_layouts() {
    let tmp = tempfile::tempdir().unwrap();
    let shared = RaptorCheckpointHandle::at(tmp.path(), "h");
    shared.ensure_manifest().unwrap();
    shared.write_cluster_node(0, 0, &dummy_node(0, 0)).unwrap();
    let note = RaptorCheckpointHandle::at_note(tmp.path(), "b.md", "h");
    note.ensure_manifest().unwrap();
    note.write_cluster_node(1, 0, &dummy_node(1, 0)).unwrap();
    assert_eq!(
        RaptorCheckpointHandle::load_corpus_nodes(tmp.path())
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn reset_wipes_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let h = RaptorCheckpointHandle::at(tmp.path(), "hash-a");
    h.ensure_manifest().unwrap();
    h.write_cluster_node(0, 0, &dummy_node(0, 0)).unwrap();
    assert!(h.dir.exists());
    h.reset();
    assert!(!h.dir.exists());
}
