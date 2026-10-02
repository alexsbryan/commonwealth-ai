// SPDX-License-Identifier: AGPL-3.0-or-later
//
// RED BASELINE — order `mesh-scale-t1-red`, bar `t1-notes-clean-wire`.
// Spec: research/scale-analysis/MESH_SCALE_100_USERS_1000_CORPORA.md §3, §8.3.
//
// ONE QUESTION: how many bytes does ONE gossiped global note actually put on
// the wire today, and how many notes fit under the 8 MiB body limit at that
// size?
//
// §3 derived ~16 KB/note and a ~500-note cliff from arithmetic. This harness
// measures instead of deriving, and it measures the SHIPPED path: the events
// come out of `NoteStore::notes_delta_since` (the same constructor the gossip
// pull uses, `notes.rs:2326`) and are serialized with `serde_json::to_vec`
// (the same call the daemon's sink makes, `bootstrap.rs:1340-1347`).
//
// INPUT: a REAL notes.db. The operator's live store is never opened — the
// runner takes a snapshot with sqlite's backup API and points
// `RED_BASELINE_NOTES_DB` at the copy (`NoteStore::open` migrates, which must
// never touch a live store). The harness REFUSES to run without that env var
// rather than falling back to a synthetic note: a synthetic note's content
// length is exactly the number the measurement is supposed to discover.
//
// #[ignore]d: it is a measurement, not an assertion, and it needs an input
// the build gate does not have.
//
//   RED_BASELINE_NOTES_DB=/tmp/notes-snapshot.db \
//     cargo test -p corpus-engine-notes --test main red_baseline_note_wire_size \
//     -- --ignored --nocapture

use corpus_engine_notes::NoteStore;

/// `MAX_REQUEST_BODY_BYTES` on the internal server (`server.rs:30`) — the
/// limit a full-store gossip push has to fit under.
const BODY_LIMIT_BYTES: usize = 8 * 1024 * 1024;

fn pct(sorted: &[usize], p: f64) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

#[tokio::test]
#[ignore = "measurement — needs RED_BASELINE_NOTES_DB pointing at a notes.db snapshot"]
async fn red_baseline_gossiped_note_wire_bytes() {
    let db = std::env::var("RED_BASELINE_NOTES_DB").expect(
        "RED_BASELINE_NOTES_DB must point at a SNAPSHOT of a real notes.db \
         (never the live store — NoteStore::open migrates)",
    );
    let limit: usize = std::env::var("RED_BASELINE_NOTES_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500);

    let store = NoteStore::open(std::path::Path::new(&db)).expect("open notes.db snapshot");
    // A peer id with no watermark row → the delta starts at 0, i.e. this is
    // the first push to a fresh peer: exactly the payload §3 prices.
    let events = store
        .notes_delta_since("red-baseline-fresh-peer", limit)
        .await
        .expect("notes_delta_since");

    assert!(
        !events.is_empty(),
        "the snapshot yielded no propagation events — the instrument, not the \
         system, is what this run measured"
    );

    let mut with_emb: Vec<usize> = Vec::new();
    let mut without_emb: Vec<usize> = Vec::new();
    let mut emb_only: Vec<usize> = Vec::new();
    let mut embedded_events = 0usize;
    let mut dims: std::collections::BTreeSet<i64> = Default::default();
    let mut models: std::collections::BTreeSet<String> = Default::default();

    for ev in &events {
        let full = serde_json::to_vec(ev).expect("serialize event").len();
        with_emb.push(full);
        let mut stripped = ev.clone();
        let carried = stripped.embedding.take();
        let bare = serde_json::to_vec(&stripped)
            .expect("serialize stripped")
            .len();
        without_emb.push(bare);
        if let Some(e) = carried {
            embedded_events += 1;
            emb_only.push(full - bare);
            dims.insert(e.dim);
            models.insert(e.model_id);
        }
    }

    with_emb.sort_unstable();
    without_emb.sort_unstable();
    emb_only.sort_unstable();

    let mean = |v: &[usize]| -> f64 {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<usize>() as f64 / v.len() as f64
        }
    };

    println!(
        "RED_T1_WIRE events={} embedded={}",
        events.len(),
        embedded_events
    );
    println!(
        "RED_T1_WIRE dims={:?} models={:?}",
        dims.iter().collect::<Vec<_>>(),
        models.iter().collect::<Vec<_>>()
    );
    println!(
        "RED_T1_WIRE bytes_per_note_full min={} p50={} mean={:.0} p90={} max={}",
        with_emb[0],
        pct(&with_emb, 0.5),
        mean(&with_emb),
        pct(&with_emb, 0.9),
        with_emb[with_emb.len() - 1]
    );
    println!(
        "RED_T1_WIRE bytes_per_note_embedding_stripped min={} p50={} mean={:.0} p90={} max={}",
        without_emb[0],
        pct(&without_emb, 0.5),
        mean(&without_emb),
        pct(&without_emb, 0.9),
        without_emb[without_emb.len() - 1]
    );
    if !emb_only.is_empty() {
        println!(
            "RED_T1_WIRE embedding_payload_bytes min={} p50={} mean={:.0} max={}",
            emb_only[0],
            pct(&emb_only, 0.5),
            mean(&emb_only),
            emb_only[emb_only.len() - 1]
        );
    }

    // The cliff, derived from the MEASURED size rather than from §3's
    // arithmetic. Reported at the mean (the full-store push carries the whole
    // population, so the mean is the size that decides where 8 MiB lands).
    let mean_full = mean(&with_emb);
    let mean_bare = mean(&without_emb);
    println!(
        "RED_T1_WIRE notes_to_8MiB_today={:.0} notes_to_8MiB_if_stripped={:.0} ratio={:.1}x",
        BODY_LIMIT_BYTES as f64 / mean_full,
        BODY_LIMIT_BYTES as f64 / mean_bare,
        mean_full / mean_bare,
    );
}
