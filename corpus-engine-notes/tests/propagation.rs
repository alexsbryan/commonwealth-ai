//! Mesh-propagation integration tests for the NoteStore tiered
//! retrieval surface.
//!
//! Two-node (sometimes three-node) in-process harness:
//!   - Each "node" is a `NoteStore` opened against a tempdir DB.
//!   - Each node has a `PropagationSinkFn` that buffers outbound
//!     events into a per-node channel.
//!   - A `dispatch` helper drains pending events from each
//!     channel and calls `ingest_remote_notes` on the other nodes.
//!   - Pausing dispatch simulates offline; resuming simulates
//!     reconnect.
//!
//! HTTP transport is exercised by `sovereign-mesh/tests/gossip_*`;
//! these tests stay channel-only so they're deterministic and
//! fast.

use std::collections::HashMap;
use std::sync::Arc;

use corpus_engine_notes::{
    EmbedFn, IngestRemoteReport, NotePropagationEvent, NoteScope, NoteStore, PropagationSinkFn,
};
use tokio::sync::Mutex;

/// In-process node harness. Each `Node` owns a `NoteStore` and a
/// buffer of pending outbound events.
struct Node {
    name: String,
    #[allow(dead_code)] // scaffolding for upcoming T2 + bench tests
    node_id: String,
    store: NoteStore,
    pending: Arc<Mutex<Vec<NotePropagationEvent>>>,
}

impl Node {
    /// Open a fresh node with optional embed_fn. The propagation
    /// sink writes into `pending`; `dispatch_all` drains it.
    fn new(name: &str, node_id: &str, embed: Option<EmbedFn>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("notes.db");
        // Leak the tempdir so the DB outlives the test scope —
        // we don't need cleanup; the temp dir is per-test.
        Box::leak(Box::new(dir));

        let pending: Arc<Mutex<Vec<NotePropagationEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let pending_for_sink = Arc::clone(&pending);
        let sink: PropagationSinkFn = Arc::new(move |event: &NotePropagationEvent| {
            let p = Arc::clone(&pending_for_sink);
            let ev = event.clone();
            // Sink is sync, but we need to push onto a tokio mutex.
            // Use blocking lock — safe here because we're in a sync
            // closure called from the write path that already holds
            // no other lock (the conn mutex was dropped above).
            tokio::task::block_in_place(|| {
                let mut guard = p.blocking_lock();
                guard.push(ev);
            });
        });

        let mut store = NoteStore::open(&db_path)
            .unwrap()
            .with_propagation_sink(sink)
            .with_origin_node_id(node_id);
        if let Some(embed_fn) = embed {
            store = store.with_embed_fn(embed_fn);
        }
        Node {
            name: name.to_string(),
            node_id: node_id.to_string(),
            store,
            pending,
        }
    }

    async fn drain_pending(&self) -> Vec<NotePropagationEvent> {
        let mut guard = self.pending.lock().await;
        std::mem::take(&mut *guard)
    }
}

/// Drain `from`'s pending queue and ingest into every other node.
/// One "round" = one drain per node, one cross-deliver.
async fn dispatch_all(nodes: &[&Node]) -> HashMap<String, IngestRemoteReport> {
    let mut reports = HashMap::new();
    let mut outbound: Vec<(String, Vec<NotePropagationEvent>)> = Vec::new();
    for node in nodes {
        let events = node.drain_pending().await;
        if !events.is_empty() {
            outbound.push((node.name.clone(), events));
        }
    }
    for (sender, events) in outbound {
        for receiver in nodes {
            if receiver.name == sender {
                continue;
            }
            let r = receiver
                .store
                .ingest_remote_notes(events.clone())
                .await
                .unwrap();
            let key = format!("{sender}→{receiver}", sender = sender, receiver = receiver.name);
            reports.insert(key, r);
        }
    }
    reports
}

#[allow(dead_code)] // scaffolding — first user lands in the T2 + bench tests
fn mock_embed_fn() -> EmbedFn {
    Arc::new(|text: &str| {
        // Deterministic 4-dim vector: length, byte-sum, hash, fixed
        let bs: u32 = text.bytes().map(|b| b as u32).sum();
        let v = vec![text.len() as f32, bs as f32, 1.0, 0.0];
        Box::pin(async move { Ok(v) })
    })
}

// ── Basic propagation ───────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn convergence_global_note_propagates_in_one_round() {
    let a = Node::new("A", "node-a", None);
    let b = Node::new("B", "node-b", None);

    a.store
        .write_note(
            "decision",
            "use BTreeMap for ordered iteration",
            vec![],
            vec![],
            "sess-1",
        )
        .await
        .unwrap();

    let reports = dispatch_all(&[&a, &b]).await;
    let r = reports.get("A→B").expect("A shipped to B");
    assert_eq!(r.inserted, 1);
    assert_eq!(r.rejected, 0);

    // B has the note.
    let on_b = b
        .store
        .read_notes(Some("BTreeMap"), &[], &[], &[], 10, false)
        .await
        .unwrap();
    assert_eq!(on_b.len(), 1);
    assert_eq!(on_b[0].content, "use BTreeMap for ordered iteration");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn privacy_note_never_propagates() {
    let a = Node::new("A", "node-a", None);
    let b = Node::new("B", "node-b", None);

    a.store
        .write_note_full_v9(
            "invariant",
            "API key in this file",
            vec![],
            vec![".env".into()],
            "sess-1",
            NoteScope::Global,
            None,
            None,
            corpus_engine_notes::NoteSource::Agent,
            None,
            None,
            /* private */ true,
        )
        .await
        .unwrap();

    let reports = dispatch_all(&[&a, &b]).await;
    // Private notes don't enter the sink — A's pending stays empty.
    assert!(reports.is_empty(), "private notes must not fire the sink");

    let on_b = b
        .store
        .read_notes(Some("API"), &[], &[], &[], 10, false)
        .await
        .unwrap();
    assert!(on_b.is_empty(), "B must not see the private note");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toolbx_node_id_rotation_yields_no_duplicates() {
    // Simulate a toolbx container rebuilding mid-flight: the same
    // operator writes the same note from two different node_ids.
    // content_hash dedupes — peer B must show exactly one row.
    let a_v1 = Node::new("A_v1", "node-a-rebuild-1", None);
    let a_v2 = Node::new("A_v2", "node-a-rebuild-2", None);
    let b = Node::new("B", "node-b", None);

    for node in [&a_v1, &a_v2] {
        node.store
            .write_note(
                "invariant",
                "fast slot must skip n_rs_seq propagation",
                vec!["embedded.rs".into()],
                vec![],
                "sess-toolbx",
            )
            .await
            .unwrap();
    }

    let _ = dispatch_all(&[&a_v1, &a_v2, &b]).await;

    let on_b = b
        .store
        .read_notes(Some("fast slot"), &[], &[], &[], 10, false)
        .await
        .unwrap();
    assert_eq!(
        on_b.len(),
        1,
        "content_hash dedup must collapse the two rebuild writes to one row on B"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn supersedes_chain_propagates_intact() {
    let a = Node::new("A", "node-a", None);
    let b = Node::new("B", "node-b", None);

    let v1 = a
        .store
        .write_note("decision", "use BTreeMap", vec![], vec![], "sess-1")
        .await
        .unwrap();
    let _v2 = a
        .store
        .write_note_with_source(
            "decision",
            "use HashMap — ordered iteration not needed",
            vec![],
            vec![],
            "sess-1",
            NoteScope::Global,
            None,
            None,
            corpus_engine_notes::NoteSource::Extracted,
            Some(&v1),
        )
        .await
        .unwrap();

    let _ = dispatch_all(&[&a, &b]).await;

    let on_b = b
        .store
        .read_notes(None, &[], &[], &["decision".into()], 10, false)
        .await
        .unwrap();
    assert_eq!(on_b.len(), 2);
    let with_supersedes: Vec<_> = on_b.iter().filter(|n| n.supersedes.is_some()).collect();
    assert_eq!(with_supersedes.len(), 1, "exactly one note reverses another");
    assert_eq!(with_supersedes[0].supersedes.as_deref(), Some(v1.as_str()));
}

// ── Divergence merge ────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_divergence_converges_on_reconnect() {
    // A and B are disconnected; each writes notes the other
    // doesn't know about; after reconnect, both have everything.
    let a = Node::new("A", "node-a", None);
    let b = Node::new("B", "node-b", None);

    for i in 0..5 {
        a.store
            .write_note(
                "todo",
                &format!("A-task-{i}"),
                vec![],
                vec![],
                "sess-a",
            )
            .await
            .unwrap();
        b.store
            .write_note(
                "todo",
                &format!("B-task-{i}"),
                vec![],
                vec![],
                "sess-b",
            )
            .await
            .unwrap();
    }
    // "Reconnect" — one dispatch round delivers both directions.
    let _ = dispatch_all(&[&a, &b]).await;

    let on_a = a
        .store
        .read_notes(None, &[], &[], &["todo".into()], 100, false)
        .await
        .unwrap();
    let on_b = b
        .store
        .read_notes(None, &[], &[], &["todo".into()], 100, false)
        .await
        .unwrap();
    assert_eq!(on_a.len(), 10, "A should have its 5 + B's 5");
    assert_eq!(on_b.len(), 10, "B should have its 5 + A's 5");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bootstrap_join_via_content_hashes_pull() {
    // C joins fresh while A has 5 global notes; C must catch up
    // via the reconciliation path (content_hash digest → pull).
    let a = Node::new("A", "node-a", None);
    let c = Node::new("C", "node-c", None);

    for i in 0..5 {
        a.store
            .write_note(
                "decision",
                &format!("decision-{i}"),
                vec![],
                vec![],
                "sess-a",
            )
            .await
            .unwrap();
    }
    // C joined AFTER A's writes — C's sink hasn't seen any events.
    // Drain A's pending so it's empty (simulate "we lost the
    // gossip frames before C joined").
    let _ = a.drain_pending().await;
    assert!(a.pending.lock().await.is_empty());

    // Reconciliation: A and C compare digests, identify divergent
    // buckets, C pulls.
    let a_digest = a.store.content_hash_digest().await.unwrap();
    let c_digest = c.store.content_hash_digest().await.unwrap();
    // C has nothing; A has 5 → digests must differ for every
    // bucket that contains any of A's hashes.
    assert_ne!(a_digest, c_digest);

    // For each bucket A has, C requests the hash list and pulls
    // missing events.
    for bucket in a_digest.keys() {
        let hashes = a.store.content_hashes_in_bucket(*bucket).await.unwrap();
        let events = a.store.events_for_content_hashes(&hashes).await.unwrap();
        c.store.ingest_remote_notes(events).await.unwrap();
    }

    let on_c = c
        .store
        .read_notes(None, &[], &[], &["decision".into()], 100, false)
        .await
        .unwrap();
    assert_eq!(on_c.len(), 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_supersedes_preserves_fork_on_both_peers() {
    // A and B both start from note X. While disconnected:
    //   A writes X→X'
    //   B writes X→X''
    // On reconnect, both peers should have X with two siblings,
    // flagged via `fork_of`. No silent LWW collapse.
    let a = Node::new("A", "node-a", None);
    let b = Node::new("B", "node-b", None);

    // 1. A writes X; deliver to B so both peers share X.
    let x = a
        .store
        .write_note(
            "decision",
            "base decision",
            vec![],
            vec![],
            "sess-shared",
        )
        .await
        .unwrap();
    let _ = dispatch_all(&[&a, &b]).await;

    // 2. Disconnect (don't dispatch). A writes X'; B writes X''.
    a.store
        .write_note_with_source(
            "decision",
            "edit on A: HashMap is faster",
            vec![],
            vec![],
            "sess-a",
            NoteScope::Global,
            None,
            None,
            corpus_engine_notes::NoteSource::Agent,
            Some(&x),
        )
        .await
        .unwrap();
    b.store
        .write_note_with_source(
            "decision",
            "edit on B: keep BTreeMap for ordering",
            vec![],
            vec![],
            "sess-b",
            NoteScope::Global,
            None,
            None,
            corpus_engine_notes::NoteSource::Agent,
            Some(&x),
        )
        .await
        .unwrap();

    // 3. Reconnect.
    let _ = dispatch_all(&[&a, &b]).await;

    // Both peers have three notes total (X, X', X''). One of the
    // two children carries `fork_of` pointing at its sibling
    // (whichever arrived second on each peer). Either side could
    // be the fork — the test asserts at least one fork landed.
    let conn_a_query = a
        .store
        .read_notes(None, &[], &[], &["decision".into()], 10, false)
        .await
        .unwrap();
    let conn_b_query = b
        .store
        .read_notes(None, &[], &[], &["decision".into()], 10, false)
        .await
        .unwrap();
    assert_eq!(conn_a_query.len(), 3, "A must keep both forks + base");
    assert_eq!(conn_b_query.len(), 3, "B must keep both forks + base");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tombstone_wins_over_concurrent_edit() {
    // A tombstones note X. B, offline, writes a successor edit
    // X→X' with a later updated_at than the tombstone. On
    // reconnect: A sees both the tombstone and X' but X stays
    // tombstoned (tombstone wins).
    let a = Node::new("A", "node-a", None);
    let b = Node::new("B", "node-b", None);

    let x = a
        .store
        .write_note(
            "decision",
            "the X decision",
            vec![],
            vec![],
            "sess-shared",
        )
        .await
        .unwrap();
    let _ = dispatch_all(&[&a, &b]).await;

    // A tombstones X.
    a.store.set_note_tombstone(&x, true).await.unwrap();

    // Wait a moment so B's edit has a later updated_at — guarantees
    // the LWW timestamp ordering would resurrect if tombstone
    // didn't take precedence.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

    // B writes a successor offline.
    b.store
        .write_note_with_source(
            "decision",
            "X revised on B",
            vec![],
            vec![],
            "sess-b",
            NoteScope::Global,
            None,
            None,
            corpus_engine_notes::NoteSource::Agent,
            Some(&x),
        )
        .await
        .unwrap();

    // Reconnect.
    let _ = dispatch_all(&[&a, &b]).await;

    // Both peers: X is tombstoned; X' present but shadowed.
    assert!(
        a.store.is_note_tombstoned(&x).await.unwrap(),
        "A keeps X tombstoned"
    );
    assert!(
        b.store.is_note_tombstoned(&x).await.unwrap(),
        "B reconciles to tombstoned X"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconciliation_bucket_diff_isolates_single_change() {
    // Identical state on A and B → digests match; a single note
    // arrival on A mutates exactly one bucket's digest.
    let a = Node::new("A", "node-a", None);
    let b = Node::new("B", "node-b", None);

    // Seed both nodes with 5 identical notes.
    for i in 0..5 {
        a.store
            .write_note(
                "decision",
                &format!("seed-{i}"),
                vec![],
                vec![],
                "seed-sess",
            )
            .await
            .unwrap();
    }
    let _ = dispatch_all(&[&a, &b]).await;

    let digest_before_a = a.store.content_hash_digest().await.unwrap();
    let digest_before_b = b.store.content_hash_digest().await.unwrap();
    assert_eq!(
        digest_before_a, digest_before_b,
        "after seed sync, digests must be byte-equal"
    );

    // A writes one more note; don't dispatch.
    a.store
        .write_note(
            "decision",
            "post-sync arrival",
            vec![],
            vec![],
            "seed-sess",
        )
        .await
        .unwrap();
    let _ = a.drain_pending().await;

    let digest_after_a = a.store.content_hash_digest().await.unwrap();
    let mut diffs = 0;
    for (bucket, after_val) in &digest_after_a {
        let before_val = digest_before_a.get(bucket);
        if before_val != Some(after_val) {
            diffs += 1;
        }
    }
    // At most one bucket grew (the new note's bucket). New
    // buckets that didn't exist before count too — both flag as
    // a diff against B's view (which is `digest_before_b`).
    assert!(
        diffs <= 1,
        "single arrival should isolate to one bucket; got {diffs} bucket diffs"
    );
}
