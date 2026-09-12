// SPDX-License-Identifier: AGPL-3.0-or-later
//! The client's READ of a route's answer is pinned to the route's TYPE.
//!
//! Two answers cannot come down to `sovereign_contracts::daemon_wire` whole:
//! `mesh_http::StatusResponse` closes over the worker-eligibility view and a
//! cross-family transport path, `lc_http::IngestProgress` over the
//! enrichment phase file. For each, the contract crate carries the subset a
//! client reads (`MeshStatusSummary`, `IngestProgressView`), parsed from the
//! same bytes. That is a second parse of one wire, and a second parse can
//! drift silently — a renamed route field arrives as a serde default or a
//! parse error at a user's desk. So each pair is pinned here: serialise the
//! route's type, parse the client's, compare field by field (ARCH principle
//! 5 — a check with a failing input you can name: rename any field on either
//! side and this is red).

use sovereign_contracts::daemon_wire::{
    DaemonIdentity, IngestProgressView, MeshStatusSummary, ReachabilityStatus, RecoveryEvent,
    SelfReachability,
};
use sovereign_mesh::corpus_watch_http::IngestOutcome;
use sovereign_mesh::lc_http::IngestProgress;
use sovereign_mesh::mesh_http::{KnownMeshDto, MemberDto, StatusResponse};

fn full_status() -> StatusResponse {
    StatusResponse {
        running: true,
        node_class: "holder".into(),
        entry_node: None,
        meshes: vec![KnownMeshDto {
            mesh_id: "ab".into(),
            name: "m".into(),
            members_total: 2,
            is_active: true,
            last_seen_unix: 7,
        }],
        mesh_name: Some("m".into()),
        members_online: 1,
        members_total: 2,
        members: vec![MemberDto {
            node_id: "n1".into(),
            name: "one".into(),
            is_self: true,
            status: "online".into(),
            vram_gb: 24,
            can_anchor: true,
            addresses: vec!["10.0.0.1:9742".into()],
            origins: vec![oicp_types::OriginKind::Media],
            node_pubkey: Some("ff".into()),
            active: true,
            hw_fingerprint: Some(9),
            backend: Some("metal".into()),
        }],
        join_key: Some("cwth-a-b-c".into()),
        join_link: Some("sovereign://join/cwth-a-b-c".into()),
        client_token: Some("tok".into()),
        rpc_workers: Vec::new(),
        shared_model_host: false,
        shared_model: None,
        peer_inflight_current: 0,
        peer_inflight_ceiling: 4,
        fanout_inflight_current: 0,
        active_corpus_ingests: 1,
        iroh_transport: Vec::new(),
        self_reachability: Some(SelfReachability {
            dial: Some("k@relay".into()),
            endpoint_id: "e".into(),
            health: ReachabilityStatus {
                relay_homed: true,
                relay_urls: vec!["https://r".into()],
                discovery_ok: Some(true),
                last_error: None,
                last_recovery: Some(RecoveryEvent {
                    action: "relay_nudge".into(),
                    at_unix: 3,
                    ok: true,
                }),
                rebuilds: 1,
                peer_paths_total: 1,
                peer_paths_active: 1,
                peer_paths_wedged: false,
                degraded: false,
            },
        }),
        device_memory: Vec::new(),
        device_memory_observed_unix: None,
        rpc_block_split_pin: None,
    }
}

#[test]
fn mesh_status_summary_reads_the_route_answer_field_for_field() {
    let route = full_status();
    let bytes = serde_json::to_string(&route).unwrap();
    let view: MeshStatusSummary = serde_json::from_str(&bytes).unwrap();
    assert_eq!(view.running, route.running);
    assert_eq!(view.mesh_name, route.mesh_name);
    assert_eq!(view.members_online, route.members_online);
    assert_eq!(view.members_total, route.members_total);
    assert_eq!(view.join_key, route.join_key);
    assert_eq!(view.join_link, route.join_link);
    assert_eq!(view.client_token, route.client_token);
    // Rows are the SAME type on both sides now; equality of bytes is the pin.
    assert_eq!(
        serde_json::to_value(&view.meshes).unwrap(),
        serde_json::to_value(&route.meshes).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&view.members).unwrap(),
        serde_json::to_value(&route.members).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&view.self_reachability).unwrap(),
        serde_json::to_value(&route.self_reachability).unwrap()
    );
    // The bytes a client is handed round-trip through the view unchanged
    // for every field it carries — the view adds nothing the route did not.
    let reserialised = serde_json::to_value(&view).unwrap();
    let full = serde_json::to_value(&route).unwrap();
    for (k, v) in reserialised.as_object().unwrap() {
        assert_eq!(
            full.get(k),
            Some(v),
            "field {k} differs between view and route"
        );
    }
}

#[test]
fn mesh_status_summary_still_reads_the_pre_rename_reachability_key() {
    // `self_reachability` was `founder_reachability` for one release and
    // attach mode lets a desktop meet an older daemon.
    let mut full = serde_json::to_value(full_status()).unwrap();
    let reach = full
        .as_object_mut()
        .unwrap()
        .remove("self_reachability")
        .unwrap();
    full["founder_reachability"] = reach;
    let view: MeshStatusSummary = serde_json::from_value(full).unwrap();
    assert_eq!(view.self_reachability.unwrap().endpoint_id, "e");
}

#[test]
fn ingest_progress_view_reads_the_route_answer_field_for_field() {
    let state = corpus_engine::enrichment::state::EnrichmentState {
        step_current: 3,
        step_total: 9,
        message: Some("embedding".into()),
        ..corpus_engine::enrichment::state::EnrichmentState::new("c1", None)
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
    let typed: IngestProgressView<sovereign_tools::local_corpus::manager::IngestStats> =
        serde_json::from_str(&bytes).unwrap();
    let stats = typed.outcome.unwrap().stats.unwrap();
    assert_eq!((stats.files_indexed, stats.chunks_written), (4, 40));
}

#[test]
fn daemon_identity_is_the_status_routes_node_id() {
    // Field presence and type pinned from the route's side: this closure
    // fails to compile if `/status` stops carrying a `String` `node_id`.
    let _pin = |s: commonwealth_api::routes_status::StatusResponse| -> String { s.node_id };
    let v: DaemonIdentity = serde_json::from_value(serde_json::json!({
        "node_id": "0123456789abcdef0123456789abcdef",
        "mesh": { "name": "m" },
        "process": {}
    }))
    .unwrap();
    assert_eq!(v.node_id, "0123456789abcdef0123456789abcdef");
    assert!(serde_json::from_value::<DaemonIdentity>(serde_json::json!({ "mesh": {} })).is_err());
}
