// SPDX-License-Identifier: AGPL-3.0-or-later
//! **An `ingest:v1` unit runs through cw-rails' donor and completes inside
//! this daemon's process** (pb-work-donor's proof).
//!
//! cw-rails runs the one donor drive. The daemon serves `IngestExecutor` as an
//! execute origin and registers it in cw-rails' origin table (`Admit::Local`).
//! Here a REAL cw-rails, spawned from `CW_RAILS_BIN` (a binary: the boundary
//! gate counts dev edges), holds a `[work_offer]` for `ingest:v1`; the origin
//! is the daemon's own `work_origin::spawn` over a real `CorpusEngine` with a
//! local recipe. A unit sealed and submitted through cw-rails' doors is then:
//!
//! * held while the daemon's foreground deadline stands (the yield door,
//!   posted through the daemon's own `rails_client::post_work_yield`);
//! * leased ONCE, by cw-rails' own actor, and completed with a passed verdict;
//! * ingested by THIS process's engine — its partition directory is on this
//!   engine's disk, which no other process can write;
//! * credited to the registrant's node id, the roster identity the origin
//!   named (phase-b-39 fork 1), on cw-rails' own contributions ledger.
//!
//! The kind is offered only after the origin registers: before that the
//! donor drops it by name (fork 2), and the first reading below is the proof
//! that the offer followed the registration rather than the config alone.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corpus_index::ingest_port::cancel::CancellationRegistry;
use corpus_index::ingest_port::daemon::{IngestPort, IngestResult};
use corpus_index::ingest_port::double::{IngestPortDouble, SliceIngest};
use kernel_types::{NodeId, Verdict};
use oicp_types::work::projection::{WorkProjection, WorkUnitStatus};
use oicp_types::work_queue::WorkUnit;
use oicp_types::JobKind;
use sovereign_daemon::ingest_executor::{IngestExecutor, IngestPayload, INGEST_KIND};
use sovereign_daemon::work_origin::{spawn, WorkOrigin};

use crate::common::work_rails::WorkRails;

const CORPUS: &str = "origin-slice";

/// The roster identity the origin names for its units' credit.
fn daemon_node() -> NodeId {
    NodeId::from_u128(0x44ae << 64)
}

/// The chunks the programmed slice ingest reports its partition holds.
const SLICE_CHUNKS: u64 = 3;

/// Ingest's port double as this process's engine (pb-ingest-dial-daemon-tests):
/// a slice ingest makes the partition dir it was handed and reports
/// [`SLICE_CHUNKS`], and every ask is kept for the test to read. That the
/// engine's own `ingest_with_overrides` writes this recipe's slice into
/// its partition is corpus-engine's daemon_port_parity
/// `a_slice_ingests_into_the_engines_own_partition`. Returned with its temp
/// root and the asks.
fn an_engine() -> (
    tempfile::TempDir,
    Arc<IngestPortDouble>,
    Arc<Mutex<Vec<SliceIngest>>>,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let indexes = dir.path().join("indexes");
    std::fs::create_dir_all(&indexes).unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&asked);
    let engine = IngestPortDouble::new()
        .on_partition_path(move |id| crate::common::corpus_at(&indexes, id).partition("local"))
        .with_cancel_registry(CancellationRegistry::new())
        .on_ingest_with_overrides(move |slice| {
            record.lock().unwrap().push(slice.clone());
            Box::pin(async move {
                std::fs::create_dir_all(&slice.output_path)?;
                Ok(IngestResult {
                    corpus_id: slice.recipe_id,
                    chunks_created: SLICE_CHUNKS,
                    index_size_bytes: 0,
                    duration_secs: 0,
                    docs_skipped: 0,
                })
            })
        });
    (dir, Arc::new(engine), asked)
}

async fn poll<T>(
    what: &str,
    rails: &WorkRails,
    within: Duration,
    mut f: impl FnMut(&WorkProjection) -> Option<T>,
) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(found) = f(&rails.projection().await) {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "never saw: {what}, within {within:?}; cw-rails' log:\n{}",
            rails.log()
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::test]
async fn an_ingest_unit_runs_through_cw_rails_donor_inside_this_process() {
    let kind = JobKind::parse(INGEST_KIND).unwrap();
    let rails = WorkRails::spawn(
        None,
        "\n[work_offer]\nkinds = [\"ingest:v1\"]\nmax_concurrent = 1\n\
         yield_to_foreground = true\naccept = \"anyone\"\n",
    )
    .await;
    let actor: String = reqwest::get(format!("{}/v1/rail/actor", rails.base))
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["actor"]
        .as_str()
        .unwrap()
        .to_string();

    // No origin yet: the donor names the kind and does not offer it. Two
    // rounds (5 s each) are long enough for the offer to have appeared if the
    // config alone could publish it.
    tokio::time::sleep(Duration::from_secs(11)).await;
    let before = rails.projection().await;
    assert!(
        before.offers.values().all(|o| !o.kinds.contains(&kind)),
        "ingest:v1 was offered before any origin registered: {:?}",
        before.offers
    );

    // The daemon's origin registers; the offer follows it.
    let (_dir, engine, asked) = an_engine();
    let origin = Arc::new(WorkOrigin::new(
        IngestExecutor::new(engine.clone()),
        daemon_node(),
    ));
    let _origin = spawn(origin, rails.base.clone())
        .await
        .expect("the origin serves");
    poll(
        "this node's offer naming ingest:v1",
        &rails,
        Duration::from_secs(30),
        |p| {
            p.offers
                .iter()
                .find(|(a, o)| a.as_str() == actor && o.kinds.contains(&kind))
                .map(|_| ())
        },
    )
    .await;

    // The foreground holds new takes: the daemon's own post, the call its
    // publisher makes.
    let hold_ms = 12_000;
    let until_ms = commonwealth_core::clock::unix_now_millis() + hold_ms;
    sovereign_daemon::rails_client::post_work_yield(&rails.base, until_ms)
        .await
        .expect("the yield door takes the deadline");

    let payload = serde_json::to_value(IngestPayload::slice(
        CORPUS,
        CORPUS,
        0,
        WorkUnit::JsonlRange { start: 0, end: 100 },
    ))
    .unwrap();
    let unit = rails.seal(&kind, vec![payload]).await.remove(0);
    let submitted: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/v1/work/submit", rails.base))
        .json(&serde_json::json!({ "kind": kind, "units": [unit] }))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .expect("the submit door appended")
        .json()
        .await
        .unwrap();
    assert!(submitted["handoff"].is_array() || submitted["handoff"].is_string());

    // Held while the deadline stands: two donor rounds, still queued.
    tokio::time::sleep(Duration::from_secs(8)).await;
    assert!(
        commonwealth_core::clock::unix_now_millis() < until_ms,
        "the hold window elapsed before it was read; the reading proves nothing"
    );
    let held = rails.projection().await;
    let status = held
        .handoffs
        .values()
        .find_map(|h| h.units.get(&unit.unit_hash))
        .expect("the unit is on the fold")
        .status_at(commonwealth_core::clock::unix_now_millis());
    assert!(
        matches!(status, WorkUnitStatus::Queued { .. }),
        "the donor took a unit inside the foreground deadline: {status:?}"
    );

    // After it: leased once, by cw-rails, and completed with a passed verdict.
    let (lessee, outcome, result, attempts) = poll(
        "the unit's Complete",
        &rails,
        Duration::from_secs(90),
        |p| {
            let u = p
                .handoffs
                .values()
                .find_map(|h| h.units.get(&unit.unit_hash))?;
            match u.status_at(commonwealth_core::clock::unix_now_millis()) {
                WorkUnitStatus::Complete {
                    lessee,
                    outcome,
                    result,
                    attempts,
                    ..
                } => Some((lessee, outcome, result, attempts)),
                _ => None,
            }
        },
    )
    .await;
    assert_eq!(lessee.as_str(), actor, "cw-rails' own donor leased it");
    assert_eq!(attempts, 1, "ONE drive: one lease, one run");
    assert_eq!(outcome.verdict(), Verdict::Passed, "{outcome:?}");

    // It ran inside THIS process: the partition is on this engine's disk.
    let partition = engine.partition_path(CORPUS);
    assert_eq!(result["partition_path"], partition.display().to_string());
    assert!(
        partition.is_dir(),
        "the slice's partition must be on this process's engine: {}",
        partition.display()
    );
    assert_eq!(
        result["partition_chunks_total"].as_u64(),
        Some(SLICE_CHUNKS),
        "{result}"
    );
    // ONE slice ingest, of the unit's recipe and range, into that partition.
    assert_eq!(
        *asked.lock().unwrap(),
        vec![SliceIngest {
            recipe_id: CORPUS.into(),
            file_indices: None,
            article_range: Some((0, 100)),
            output_path: partition.clone(),
            unit_id: Some(0),
        }],
        "the donor's unit reached ingest's port as one slice"
    );

    // Credited to the roster identity the origin named.
    let events: Vec<serde_json::Value> =
        reqwest::get(format!("{}/v1/ledger/contributions", rails.base))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let credit = events
        .iter()
        .find(|e| e.to_string().contains(&unit.unit_hash))
        .unwrap_or_else(|| panic!("no credit for the unit on cw-rails' ledger: {events:?}"));
    assert_eq!(
        credit["node_id"],
        serde_json::to_value(daemon_node()).unwrap(),
        "credited to the registrant's node, the roster identity: {credit}"
    );
}
