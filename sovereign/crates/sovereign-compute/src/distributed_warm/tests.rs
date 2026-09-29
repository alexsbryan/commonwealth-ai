// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests for the warm orchestrator's rules and the `/internal/rpc-warm` wire
//! (moved with them from the daemon's `tests/rpc_warm_http.rs`).

use super::*;

#[test]
fn raw_fallback_refused_for_loopback_worker() {
    // A loopback worker_ip means a bridge-local endpoint (task 6): the
    // hand-built raw URL would target OURSELF, and a self-warm reports
    // success while the real worker stays cold → upload deadlock.
    assert!(!raw_warm_fallback_allowed("127.0.0.1"));
    assert!(!raw_warm_fallback_allowed("::1"));
    // Real remote IPs and unparseable hostnames keep the legacy fallback.
    assert!(raw_warm_fallback_allowed("192.168.1.2"));
    assert!(raw_warm_fallback_allowed("100.104.36.28"));
    assert!(raw_warm_fallback_allowed("beefymac.local"));
}

#[test]
fn request_round_trips_through_json() {
    let req = RpcWarmShardRequest {
        model_id: "m.gguf".into(),
        device_index: 1,
        plan: Vec::new(),
        source: RpcWarmSource::ByteRanges {
            source_urls: vec!["http://h/internal/v1/models/file/m.gguf".into()],
            tensors: vec![TensorRange {
                gguf_offset: 10,
                nbytes: 20,
                hash: 30,
                file_idx: 0,
            }],
            file_urls: vec![],
        },
        host_node_id: Some("0123456789abcdef0123456789abcdef".into()),
    };
    let v = serde_json::to_value(&req).unwrap();
    // The opaque-JSON seam in commonwealth-api reads `model_id` for path
    // resolution — it must be a top-level string.
    assert_eq!(v.get("model_id").and_then(|x| x.as_str()), Some("m.gguf"));
    let back: RpcWarmShardRequest = serde_json::from_value(v).unwrap();
    assert_eq!(back.device_index, 1);
    assert_eq!(
        back.host_node_id.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
    matches!(back.source, RpcWarmSource::ByteRanges { .. });
}

#[test]
fn old_wire_body_without_host_node_id_still_parses() {
    // A pre-identity host omits the field entirely — a rolling-upgrade
    // worker must not reject the body.
    let v = serde_json::json!({
        "model_id": "m.gguf",
        "device_index": 0,
        "plan": [],
        "source": { "mode": "whole_gguf", "peer_bases": ["http://10.0.0.1:9742"] }
    });
    let req: RpcWarmShardRequest = serde_json::from_value(v).unwrap();
    assert_eq!(req.host_node_id, None);
}
