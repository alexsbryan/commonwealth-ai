// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client's READ of a route's answer is pinned to the route's TYPE.
//!
//! `lc_http::IngestProgress` cannot come down to
//! `sovereign_contracts::daemon_wire` whole (it closes over the enrichment
//! phase file), so the contract crate carries the subset a client reads
//! (`IngestProgressView`), parsed from the same bytes. That is a second parse
//! of one wire, and a second parse can drift silently — a renamed route field
//! arrives as a serde default or a parse error at a user's desk. So the pair
//! is pinned here: serialise the route's type, parse the client's, compare
//! field by field (ARCH principle 5 — rename any field on either side and
//! this is red). (`/v1/mesh/status` left svrn for cw-rails at
//! pb-mesh-exit-transport; its client read is pinned by
//! `sovereign_contracts::daemon_wire::rails_status`.)

use sovereign_contracts::daemon_wire::{DaemonIdentity, IngestProgressView};
use sovereign_daemon::corpus_watch_http::IngestOutcome;
use sovereign_daemon::lc_http::IngestProgress;

#[test]
fn ingest_progress_view_reads_the_route_answer_field_for_field() {
    let state = corpus_index::enrichment_state::EnrichmentState {
        step_current: 3,
        step_total: 9,
        message: Some("embedding".into()),
        ..corpus_index::enrichment_state::EnrichmentState::new("c1", None)
    };
    let route = IngestProgress {
        corpus_id: "c1".into(),
        state: Some(state),
        outcome: Some(IngestOutcome {
            corpus_id: "c1".into(),
            job_id: "j1".into(),
            finished_at: 11,
            stats: None,
            error: Some("disk full".into()),
        }),
        finished: true,
    };
    let bytes = serde_json::to_string(&route).unwrap();
    let view: IngestProgressView = serde_json::from_str(&bytes).unwrap();
    assert_eq!(view.corpus_id, route.corpus_id);
    assert_eq!(view.finished, route.finished);
    let st = view.state.as_ref().unwrap();
    let rs = route.state.as_ref().unwrap();
    assert_eq!(
        (st.step_current, st.step_total),
        (rs.step_current, rs.step_total)
    );
    assert_eq!(st.message, rs.message);
    let o = view.outcome.as_ref().unwrap();
    let ro = route.outcome.as_ref().unwrap();
    assert_eq!(
        (o.corpus_id.as_str(), o.job_id.as_str(), o.finished_at),
        (ro.corpus_id.as_str(), ro.job_id.as_str(), ro.finished_at)
    );
    assert_eq!(o.error, ro.error);
    assert!(o.stats.is_none());

    // The typed arm: a client that links `sovereign-tools` reads the counts
    // as the manager's own type, from the same bytes.
    let route = IngestProgress {
        outcome: Some(IngestOutcome {
            stats: Some(sovereign_tools::local_corpus::manager::IngestStats {
                corpus_id: "c1".into(),
                files_indexed: 4,
                chunks_written: 40,
                runtime_failures: Vec::new(),
                excerpt_chunks: Vec::new(),
                duration_secs: 2,
            }),
            error: None,
            ..route.outcome.unwrap()
        }),
        ..route
    };
    let bytes = serde_json::to_string(&route).unwrap();
    let typed: IngestProgressView = serde_json::from_str(&bytes).unwrap();
    let stats = typed.outcome.unwrap().stats.unwrap();
    assert_eq!((stats.files_indexed, stats.chunks_written), (4, 40));
}

#[test]
fn daemon_identity_is_the_status_routes_node_id() {
    // Field presence and type pinned from the route's side: this closure
    // fails to compile if `/status` stops carrying a `String` `node_id`.
    let _pin = |s: sovereign_daemon::routes_status::StatusResponse| -> String { s.node_id };
    let v: DaemonIdentity = serde_json::from_value(serde_json::json!({
        "node_id": "0123456789abcdef0123456789abcdef",
        "mesh": { "name": "m" },
        "process": {}
    }))
    .unwrap();
    assert_eq!(v.node_id, "0123456789abcdef0123456789abcdef");
    assert!(serde_json::from_value::<DaemonIdentity>(serde_json::json!({ "mesh": {} })).is_err());
}
