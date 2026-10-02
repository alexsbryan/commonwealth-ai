// SPDX-License-Identifier: AGPL-3.0-or-later
//! The write index (pc-rails-journal-linear): a batch continues the actor's
//! counter, and a write reads only what the log gained since the last one.
//! A sibling of `tests.rs` so that file stays under the size ceiling.

use crate::*;

use commonwealth_rail_core::tests_support::*;

fn open(dir: &std::path::Path) -> RingJournal {
    RingJournal::open(dir, NS).unwrap()
}

/// A batch continues the actor's counter, contiguously, and admits like the
/// same acts appended one at a time.
#[test]
fn a_batch_append_continues_the_counter_contiguously() {
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();
    journal
        .append(record("x"), &key(1), &r, None, &Ed25519Verifier)
        .unwrap();
    let batch = vec![record("a"), record("b"), record("c")];
    let ops = journal
        .append_all(batch, &key(1), &r, None, &Ed25519Verifier)
        .unwrap();
    assert_eq!(
        ops.iter().map(|o| o.kind.seq).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(
        journal
            .append(record("y"), &key(1), &r, None, &Ed25519Verifier)
            .unwrap()
            .kind
            .seq,
        4
    );

    let f = journal.admit(&r, &Ed25519Verifier).unwrap();
    assert!(f.is_complete(), "{:?}", f.gaps);
    assert_eq!(f.ops.len(), 5);
}

/// **A write reads what the log gained since the last one, not the whole log**
/// (pc-rails-journal-linear): re-reading it per write made the kv drain cost
/// rows x log lines a tick. Shown by overwriting a folded line in place —
/// same length, same file — which a whole re-read would see as malformed and
/// hand its seq out again. Lines another writer appended, and a compaction
/// renamed over the log by another handle, are still seen.
#[test]
fn a_write_reads_only_what_the_log_gained_since_the_last_one() {
    use std::io::{Seek, SeekFrom, Write};
    let dir = tempfile::tempdir().unwrap();
    let journal = open(dir.path());
    let r = ring();
    for _ in 0..3 {
        journal
            .append(record("x"), &key(1), &r, None, &Ed25519Verifier)
            .unwrap();
    }
    let path = journal.dir().join("ring_oplog.jsonl");
    let raw = std::fs::read_to_string(&path).unwrap();
    let last_start = raw.trim_end().rfind('\n').unwrap() + 1;
    let last_len = raw.len() - last_start - 1;
    let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.seek(SeekFrom::Start(last_start as u64)).unwrap();
    f.write_all("#".repeat(last_len).as_bytes()).unwrap();
    drop(f);
    assert_eq!(
        journal
            .append(record("x"), &key(1), &r, None, &Ed25519Verifier)
            .unwrap()
            .kind
            .seq,
        3,
        "seq 2 was handed out again: the write re-read lines it had already folded"
    );

    // Another handle on the same file appends: this one's next write sees it.
    let other = open(dir.path());
    let theirs = other
        .append(record("y"), &key(2), &r, None, &Ed25519Verifier)
        .unwrap();
    assert!(!journal.ingest(&theirs).unwrap(), "held from the tail");
    assert_eq!(
        journal
            .append(record("x"), &key(1), &r, None, &Ed25519Verifier)
            .unwrap()
            .kind
            .seq,
        4
    );

    // A compaction by the other handle renames a new file over the log; this
    // handle starts again from the top. On a fresh log: compaction refuses
    // one holding the line overwritten above.
    let fresh = tempfile::tempdir().unwrap();
    let (a, b) = (open(fresh.path()), open(fresh.path()));
    for _ in 0..3 {
        a.append(record("x"), &key(1), &r, None, &Ed25519Verifier)
            .unwrap();
    }
    a.append(RailAct::Seal, &key(1), &r, None, &Ed25519Verifier)
        .unwrap();
    let dropped = a.read().unwrap().0.remove(0);
    assert_eq!(b.compact(&r, &Ed25519Verifier).unwrap().removed, 3);
    assert!(
        a.ingest(&dropped).unwrap(),
        "a retired op is not held after the compaction, as before the index"
    );
    assert_eq!(
        a.append(record("x"), &key(1), &r, None, &Ed25519Verifier)
            .unwrap()
            .kind
            .seq,
        4
    );
}
