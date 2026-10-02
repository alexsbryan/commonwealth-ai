// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

// ---------------------------------------------------------------------------
// Link classification off a live placement
// ---------------------------------------------------------------------------

#[test]
fn a_local_placement_has_no_link_to_classify() {
    let p = PlacementSnapshot {
        mode: "local".into(),
        total_blocks: 0,
        local_blocks: 0,
        workers: vec![],
    };
    assert_eq!(placement_link(&p), mm::LinkClass::Local);
}

#[test]
fn a_loopback_worker_endpoint_classifies_the_run_as_tunnelled() {
    let p = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 36,
        workers: vec![WorkerSnapshot {
            // What discovery hands ggml when it bridges over iroh.
            endpoint: "127.0.0.1:41337".into(),
            blocks: 12,
            holds_output: false,
        }],
    };
    assert_eq!(placement_link(&p), mm::LinkClass::Tunnel);
}

#[test]
fn a_routable_worker_endpoint_classifies_the_run_as_direct() {
    let p = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 36,
        workers: vec![WorkerSnapshot {
            endpoint: "192.168.1.2:50052".into(),
            blocks: 12,
            holds_output: false,
        }],
    };
    assert_eq!(placement_link(&p), mm::LinkClass::Direct);
}

/// The reason `carries_weight` is shared with `shards_from_placement` rather
/// than written twice.
///
/// A peer that is discovered but holding nothing is dropped from the digest —
/// so it must also be invisible to the link. If it were not, an idle tunnelled
/// peer merely being online would flip a genuinely-direct run to `Tunnel` and
/// silently invalidate every record taken on that machine.
#[test]
fn an_idle_tunnelled_peer_does_not_change_the_link() {
    let direct_only = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 36,
        workers: vec![WorkerSnapshot {
            endpoint: "192.168.1.2:50052".into(),
            blocks: 12,
            holds_output: false,
        }],
    };
    let mut with_idle_peer = direct_only.clone();
    with_idle_peer.workers.push(WorkerSnapshot {
        endpoint: "127.0.0.1:41337".into(),
        blocks: 0,
        holds_output: false,
    });

    assert_eq!(placement_link(&direct_only), mm::LinkClass::Direct);
    assert_eq!(
        placement_link(&with_idle_peer),
        mm::LinkClass::Direct,
        "a worker carrying nothing is not part of the placement, so it cannot \
         change the link — the same rule the digest applies"
    );
    // And the digest agrees: the idle peer is absent from both. Only the
    // weight-carrying worker needs to resolve — the idle one is dropped before
    // its identity is ever asked for, which is why an old peer idling on the
    // mesh cannot block a measurement.
    let only_beefy = |ep: &str| (ep == "192.168.1.2:50052").then(|| node("BeefyMac"));
    let a =
        shards_from_placement(&direct_only, &node("RuggedFox"), 48, &only_beefy).expect("direct");
    let b = shards_from_placement(&with_idle_peer, &node("RuggedFox"), 48, &only_beefy)
        .expect("with idle");
    assert_eq!(
        mm::placement_digest(digest_mode(&a), 48, &a),
        mm::placement_digest(digest_mode(&b), 48, &b)
    );
}

/// A worker that IS carrying blocks over a tunnel changes the key — which is
/// the whole point of the field.
#[test]
fn a_tunnelled_worker_carrying_blocks_changes_the_key() {
    let direct = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 36,
        workers: vec![WorkerSnapshot {
            endpoint: "192.168.1.2:50052".into(),
            blocks: 12,
            holds_output: false,
        }],
    };
    let mut tunnelled = direct.clone();
    tunnelled.workers[0].endpoint = "127.0.0.1:41337".into();

    // Same split, same machines, same blocks — the digest is identical.
    let a = shards_from_placement(&direct, &node("RuggedFox"), 48, &|_| Some(node("BeefyMac")))
        .expect("direct");
    let b = shards_from_placement(&tunnelled, &node("RuggedFox"), 48, &|_| {
        Some(node("BeefyMac"))
    })
    .expect("tunnelled");
    assert_eq!(
        mm::placement_digest(digest_mode(&a), 48, &a),
        mm::placement_digest(digest_mode(&b), 48, &b),
        "the digest describes the split, and the split did not change"
    );
    // …so the link is the only thing that can tell these two runs apart.
    assert_ne!(placement_link(&direct), placement_link(&tunnelled));
}

#[test]
fn endpoint_host_survives_an_ipv6_literal() {
    assert_eq!(endpoint_host("192.168.1.2:50052"), "192.168.1.2");
    assert_eq!(endpoint_host("beefymac.local:50052"), "beefymac.local");
    assert_eq!(endpoint_host("[fd7a:115c::1]:50052"), "[fd7a:115c::1]");
    // A bare IPv6 address ends in an all-digit segment. Truncating it at the
    // last colon produces a "host" that is a prefix of an address — which then
    // becomes a node key that no plan will ever match.
    assert_eq!(endpoint_host("fd7a:115c::1"), "fd7a:115c::1");
    assert_eq!(endpoint_host("[fd7a:115c::1]"), "[fd7a:115c::1]");
    // Nothing after the colon is not a port.
    assert_eq!(endpoint_host("192.168.1.2:"), "192.168.1.2:");
}

#[test]
fn a_placement_that_does_not_add_up_is_refused() {
    let p = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 30,
        workers: vec![WorkerSnapshot {
            endpoint: "w:1".into(),
            blocks: 12,
            holds_output: false,
        }],
    };
    let err = shards_from_placement(&p, &node("host"), 48, &no_names).expect_err("30 + 12 != 48");
    assert!(err.contains("does not add up"), "{err}");
}

#[test]
fn a_placement_for_a_different_model_is_refused() {
    // The daemon holds 48 blocks but the config's GGUF has 64: the header that
    // produced the fingerprint is not the model that is loaded, so any record
    // filed now would describe the wrong thing.
    let p = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 36,
        workers: vec![WorkerSnapshot {
            endpoint: "w:1".into(),
            blocks: 12,
            holds_output: false,
        }],
    };
    let err = shards_from_placement(&p, &node("host"), 64, &no_names).expect_err("48 != 64");
    assert!(err.contains("not the one whose header was read"), "{err}");
}

#[test]
fn a_worker_holding_nothing_is_not_part_of_the_placement() {
    // An idle peer joining or leaving must not change the digest — it changes
    // nothing about how the model decodes.
    let p = PlacementSnapshot {
        mode: "distributed".into(),
        total_blocks: 48,
        local_blocks: 48,
        workers: vec![WorkerSnapshot {
            endpoint: "idle:1".into(),
            blocks: 0,
            holds_output: false,
        }],
    };
    let shards = shards_from_placement(&p, &node("RuggedFox"), 48, &no_names).expect("describable");
    assert_eq!(shards.len(), 1);
    assert_eq!(shards[0].node_key, "RuggedFox");
    assert_eq!(digest_mode(&shards), "local");
}

#[test]
fn a_zero_block_model_is_not_describable() {
    let p = PlacementSnapshot::default();
    assert!(shards_from_placement(&p, &node("host"), 0, &no_names).is_err());
}

// ---------------------------------------------------------------------------
// The digest must be the one `mesh plan` looks up
// ---------------------------------------------------------------------------

#[test]
fn a_solo_bench_and_a_solo_plan_agree_on_the_digest() {
    // This is the property that makes the whole store useful: bench files a
    // record under the key plan will construct for the same configuration. If
    // it ever breaks, every record written is unfindable and the tool silently
    // reports "not measured" forever.
    let bench_shards = shards_from_placement(
        &PlacementSnapshot {
            mode: "local".into(),
            total_blocks: 0,
            local_blocks: 0,
            workers: Vec::new(),
        },
        &node("RuggedFox"),
        48,
        &no_names,
    )
    .expect("describable");

    // What `mesh plan --from-mesh` builds for a single-node fit: one row, the
    // host, holding every block and the output head — and carrying the same
    // hardware fingerprint the bench read off the same `/v1/mesh/status`.
    let plan_shards = vec![mm::PlacementShard {
        node_key: "RuggedFox".into(),
        hw: Some(0xF0F),
        blocks: Some((0, 47)),
        holds_output: true,
    }];

    assert_eq!(
        mm::placement_digest(digest_mode(&bench_shards), 48, &bench_shards),
        mm::placement_digest("local", 48, &plan_shards)
    );
}

#[test]
fn a_different_split_of_the_same_model_digests_differently() {
    let solo = vec![mm::PlacementShard {
        node_key: "RuggedFox".into(),
        hw: Some(0xF0F),
        blocks: Some((0, 47)),
        holds_output: true,
    }];
    let split = vec![
        mm::PlacementShard {
            node_key: "BeefyMac".into(),
            hw: Some(0xF0F),
            blocks: Some((0, 11)),
            holds_output: false,
        },
        mm::PlacementShard {
            node_key: "RuggedFox".into(),
            hw: Some(0xF0F),
            blocks: Some((12, 47)),
            holds_output: true,
        },
    ];
    assert_ne!(
        mm::placement_digest("local", 48, &solo),
        mm::placement_digest("distributed", 48, &split),
        "if these collided, a measured solo run would be reported as the speed of \
         a split nobody has ever run"
    );
}

// ---------------------------------------------------------------------------
// Reading the daemon's JSON
// ---------------------------------------------------------------------------

#[test]
fn primary_slot_is_read_out_of_a_real_status_body() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"inference":{"resident":[
             {"role":"fast","model_id":"Qwen3.5-0.8B","resident":true},
             {"role":"primary","model_id":"Qwen3.5-122B","resident":false,
              "placement":{"mode":"child-distributed","total_blocks":0,"local_blocks":0,"workers":[]}}
           ]},"process":{"uptime_seconds":31603}}"#,
    )
    .expect("fixture parses");
    let (id, resident, placement) = primary_from_status(&body).expect("a primary slot is present");
    assert_eq!(id, "Qwen3.5-122B");
    assert!(!resident, "the lazy primary reports idle-unloaded");
    assert_eq!(placement.mode, "child-distributed");
    assert_eq!(uptime_from_status(&body), Some(31603));
}

#[test]
fn a_child_hosted_primary_counts_as_serving_though_resident_is_false() {
    // The false positive this nearly shipped with. `ComputeRoutedProvider::
    // resident_slots()` forwards the IN-PROCESS engine's view, and the
    // in-process engine never loaded this model — the child did. So `resident`
    // is false forever on a perfectly healthy child-hosted primary, and a guard
    // reading only that field would make a VALID measurement impossible here.
    let body: serde_json::Value = serde_json::from_str(
        r#"{"inference":{
             "resident":[{"role":"primary","model_id":"Qwen3.5-122B","resident":false,
                          "placement":{"mode":"child-distributed","total_blocks":0,
                                       "local_blocks":0,"workers":[]}}],
             "compute_children":[{"name":"Qwen3.5-122B","role":"generate",
                                  "model_id":"Qwen3.5-122B","lifecycle":"serving"}]}}"#,
    )
    .expect("fixture parses");
    assert!(primary_is_serving(&body, "Qwen3.5-122B"));
}

#[test]
fn a_child_that_is_only_starting_or_warming_is_not_serving() {
    // These are the states in which something ELSE answers the request — the
    // case being caught. Observed live 2026-07-28 at ~100 tok/s from a 122B.
    for phase in ["starting", "warming", "degraded", "restarting", "failed"] {
        let body: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"inference":{{
                 "resident":[{{"role":"primary","model_id":"M","resident":false}}],
                 "compute_children":[{{"model_id":"M","lifecycle":"{phase}"}}]}}}}"#
        ))
        .expect("fixture parses");
        assert!(
            !primary_is_serving(&body, "M"),
            "`{phase}` must not count as serving"
        );
    }
}

#[test]
fn an_in_process_resident_primary_needs_no_child() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"inference":{"resident":[{"role":"primary","model_id":"M","resident":true}]}}"#,
    )
    .expect("fixture parses");
    assert!(primary_is_serving(&body, "M"));
}

#[test]
fn a_different_childs_health_does_not_vouch_for_the_primary() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"inference":{
             "resident":[{"role":"primary","model_id":"M","resident":false}],
             "compute_children":[{"model_id":"SomethingElse","lifecycle":"serving"}]}}"#,
    )
    .expect("fixture parses");
    assert!(!primary_is_serving(&body, "M"));
}

// ---------------------------------------------------------------------------
// "Not serving yet" vs "will never serve"
// ---------------------------------------------------------------------------

/// The exact `/status` shape observed on RuggedFox 2026-07-29, where the bench
/// spent ten minutes insisting a permanent failure was a cold load.
#[test]
fn a_failed_compute_child_is_reported_with_the_daemons_own_reason() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"inference":{
             "resident":[{"role":"primary","model_id":"M","resident":false}],
             "compute_children":[{"model_id":"M","lifecycle":"failed","restarts":0,
               "last_transition_reason":"no eligible RPC workers",
               "last_exit":"no eligible RPC workers"}]}}"#,
    )
    .expect("fixture parses");
    assert_eq!(
        primary_children_failed(&body, "M").as_deref(),
        Some("no eligible RPC workers"),
        "the operator gets the child's account, not this command's paraphrase"
    );
}

/// Every state that is not `failed` is one the canary is right to wait out —
/// `starting`/`warming`/`restarting` ARE the cold load.
#[test]
fn a_child_that_is_still_coming_up_is_not_a_failure() {
    for lifecycle in ["starting", "warming", "restarting", "serving", "degraded"] {
        let body: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"inference":{{"compute_children":[
                 {{"model_id":"M","lifecycle":"{lifecycle}"}}]}}}}"#
        ))
        .expect("fixture parses");
        assert_eq!(
            primary_children_failed(&body, "M"),
            None,
            "{lifecycle} must not be mistaken for a dead end"
        );
    }
}

/// One dead replica in a pool is not a dead pool.
#[test]
fn a_pool_with_one_live_replica_is_not_failed() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"inference":{"compute_children":[
             {"model_id":"M","lifecycle":"failed","last_exit":"boom"},
             {"model_id":"M","lifecycle":"serving"}]}}"#,
    )
    .expect("fixture parses");
    assert_eq!(primary_children_failed(&body, "M"), None);
}

/// An in-process primary has no children, so there is nothing here to have
/// failed and residency remains the only signal. Returning a failure would
/// abort every bench on the non-distributed configuration.
#[test]
fn an_in_process_primary_has_no_child_to_have_failed() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"inference":{"resident":[{"role":"primary","model_id":"M","resident":false}]}}"#,
    )
    .expect("fixture parses");
    assert_eq!(primary_children_failed(&body, "M"), None);

    // …and another model's dead child says nothing about this one.
    let other: serde_json::Value = serde_json::from_str(
        r#"{"inference":{"compute_children":[
             {"model_id":"SomethingElse","lifecycle":"failed","last_exit":"boom"}]}}"#,
    )
    .expect("fixture parses");
    assert_eq!(primary_children_failed(&other, "M"), None);
}

/// A failed child that reports no reason still stops the wait — the absence of
/// an explanation is not a reason to keep waiting ten minutes.
#[test]
fn a_failed_child_without_a_reason_still_stops_the_wait() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"inference":{"compute_children":[{"model_id":"M","lifecycle":"failed"}]}}"#,
    )
    .expect("fixture parses");
    assert!(primary_children_failed(&body, "M").is_some());
}

#[test]
fn a_status_body_with_no_primary_yields_nothing() {
    let body: serde_json::Value =
        serde_json::from_str(r#"{"inference":{"resident":[{"role":"fast","model_id":"x"}]}}"#)
            .expect("fixture parses");
    assert!(primary_from_status(&body).is_none());
}

#[test]
fn mesh_view_resolves_rpc_endpoints_to_members_with_their_hardware() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"members":[
             {"node_id":"aaa","name":"RuggedFox","is_self":true,"status":"online",
              "hw_fingerprint":12345,"backend":"vulkan"},
             {"node_id":"bbb","name":"BeefyMac","is_self":false,"status":"online",
              "hw_fingerprint":67890},
             {"node_id":"ccc","name":"LittleMac","is_self":false,"status":"offline"}
           ],
           "rpc_workers":[{"node_id":"bbb","endpoint":"192.168.1.2:50052"}]}"#,
    )
    .expect("fixture parses");
    let v = MeshView::parse(&body);
    assert_eq!(v.self_name, "RuggedFox");
    assert_eq!(v.self_hw_fingerprint, Some(12345));
    assert_eq!(v.self_backend.as_deref(), Some("vulkan"));
    assert_eq!(
        v.endpoint_nodes.get("192.168.1.2:50052"),
        Some(&NodeIdentity {
            name: "BeefyMac".into(),
            hw: Some(67890),
        }),
        "the peer's own fingerprint travels with its name — it is half the shard key"
    );
    assert_eq!(v.online.get("BeefyMac"), Some(&true));
    assert_eq!(v.online.get("LittleMac"), Some(&false));
}

/// A peer on a daemon too old to advertise hardware still resolves — the
/// refusal belongs at the point a *key* is built, not here.
///
/// Dropping it from the map instead would report it as "not a mesh member",
/// which is a different fault with a different repair, and would send the
/// operator looking for a discovery problem that does not exist.
#[test]
fn a_peer_without_a_fingerprint_still_resolves_but_carries_no_hardware() {
    let body: serde_json::Value = serde_json::from_str(
        r#"{"members":[
             {"node_id":"bbb","name":"BeefyMac","is_self":false,"status":"online"}
           ],
           "rpc_workers":[{"node_id":"bbb","endpoint":"192.168.1.2:50052"}]}"#,
    )
    .expect("fixture parses");
    let v = MeshView::parse(&body);
    assert_eq!(
        v.endpoint_nodes.get("192.168.1.2:50052"),
        Some(&NodeIdentity {
            name: "BeefyMac".into(),
            hw: None,
        })
    );
}

#[test]
fn a_daemon_that_advertises_no_fingerprint_yields_no_host_identity() {
    // The structural bar from week 1: without a fingerprint there is no
    // `HostIdentity`, so `MeasurementKey::for_plan` cannot be called at all and
    // the command must refuse rather than file under a placeholder.
    let body: serde_json::Value = serde_json::from_str(
        r#"{"members":[{"node_id":"aaa","name":"RuggedFox","is_self":true,"status":"online"}]}"#,
    )
    .expect("fixture parses");
    let v = MeshView::parse(&body);
    assert_eq!(v.self_hw_fingerprint, None);
    assert!(mm::HostIdentity::from_live_mesh(v.self_hw_fingerprint).is_none());
}

#[test]
fn peer_liveness_excludes_the_host_itself() {
    let mut mesh = MeshView {
        self_name: "RuggedFox".into(),
        ..Default::default()
    };
    mesh.online.insert("BeefyMac".into(), true);
    let shards = vec![
        mm::PlacementShard {
            node_key: "BeefyMac".into(),
            hw: Some(0xF0F),
            blocks: Some((0, 11)),
            holds_output: false,
        },
        mm::PlacementShard {
            node_key: "RuggedFox".into(),
            hw: Some(0xF0F),
            blocks: Some((12, 47)),
            holds_output: true,
        },
    ];
    assert_eq!(
        peer_liveness(&shards, &mesh),
        vec![("BeefyMac".to_string(), true)],
        "the host's own liveness is the HostLiveness check, not this one"
    );
}

#[test]
fn a_peer_absent_from_the_mesh_reads_as_offline() {
    // Absence of evidence is not evidence of health: a shard-holder we cannot
    // see in the member list has not been shown to be up.
    let mesh = MeshView {
        self_name: "RuggedFox".into(),
        ..Default::default()
    };
    let shards = vec![mm::PlacementShard {
        node_key: "Ghost".into(),
        hw: Some(0xF0F),
        blocks: Some((0, 11)),
        holds_output: false,
    }];
    assert_eq!(peer_liveness(&shards, &mesh), vec![("Ghost".into(), false)]);
}
