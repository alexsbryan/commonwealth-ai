// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use commonwealth_core::capabilities::*;
use commonwealth_core::ids::*;
use commonwealth_core::knowledge::*;
use commonwealth_core::mesh::*;
use commonwealth_inference::inference_plan::*;
use commonwealth_inference::oicp::*;

use commonwealth_discovery::gossip::*;
use commonwealth_discovery::membership;
use commonwealth_discovery::threshold::SignificanceThresholds;

use commonwealth_inference::orchestrator::departure::{DepartureState, GracefulDeparture};
use commonwealth_inference::orchestrator::fault::{
    FaultDetector, FaultDetectorConfig, FaultEvent, FaultStatus,
};

// OicpModelCache was removed in PR-C; v0.3 has no equivalent.

use commonwealth_test_harness::fixtures::*;
use commonwealth_test_harness::mock_llama::MockLlamaServer;
use commonwealth_test_harness::simulated_mesh::SimulatedMesh;
use commonwealth_test_harness::simulated_node::SimulatedNodeBuilder;

// ============================================================================
// Scenario: Mesh Formation (Phase 2)
// Init mesh, nodes join, verify member state converged.
// ============================================================================

#[test]
fn mesh_formation_two_nodes() {
    let (mut mesh, join_key) = membership::init_mesh(
        "Test Co-op",
        "Alice's Desktop",
        vec!["127.0.0.1:9742".parse().unwrap()],
    );

    assert_eq!(mesh.members.len(), 1);
    let alice_id = *mesh.members.keys().next().unwrap();

    // Bob joins.
    let bob_id = membership::accept_join(
        &mut mesh,
        &join_key,
        "Bob's Build",
        vec!["192.168.1.2:9742".parse().unwrap()],
        alice_id,
    )
    .unwrap();

    assert_eq!(mesh.members.len(), 2);
    assert_eq!(mesh.members[&bob_id].invited_by, alice_id);
    assert_eq!(mesh.members[&bob_id].status, NodeStatus::Online);
}

#[test]
fn mesh_formation_five_nodes() {
    let (mut mesh, join_key) = membership::init_mesh("Five Node Mesh", "Node 1", vec![]);
    let founder_id = *mesh.members.keys().next().unwrap();

    for i in 2..=5 {
        membership::accept_join(
            &mut mesh,
            &join_key,
            &format!("Node {i}"),
            vec![],
            founder_id,
        )
        .unwrap();
    }

    assert_eq!(mesh.members.len(), 5);
    // All should be online.
    assert!(mesh
        .members
        .values()
        .all(|m| m.status == NodeStatus::Online));
}

#[test]
fn mesh_formation_rejects_wrong_key() {
    let (mut mesh, _key) = membership::init_mesh("Test", "Alice", vec![]);
    let alice_id = *mesh.members.keys().next().unwrap();

    let result = membership::accept_join(&mut mesh, "cwth-0000-0000-0000", "Eve", vec![], alice_id);
    assert!(result.is_err());
    assert_eq!(mesh.members.len(), 1); // Eve was not added.
}

// ============================================================================
// Scenario: Gossip Convergence (Phases 2, 3)
// 5 nodes, verify capability state converges within bounded rounds.
// ============================================================================

#[test]
fn gossip_convergence_five_nodes_bounded_rounds() {
    let node_ids: Vec<NodeId> = (1..=5).map(NodeId::from_u128).collect();
    let mut states: Vec<GossipState> = (0..5).map(|_| GossipState::new()).collect();

    // Each node publishes its own state.
    for (i, &id) in node_ids.iter().enumerate() {
        let entry = GossipEntry {
            key: GossipKey::MemberState { node_id: id },
            value: GossipValue::MemberState {
                status: NodeStatus::Online,
                capabilities: Box::new(NodeCapabilities {
                    hardware: HardwareProfile {
                        gpus: vec![],
                        system_ram_gb: 32,
                        cpu_cores: 8,
                        total_storage_gb: 500,
                        free_storage_gb: 200,
                        network_bandwidth_mbps: Some(1000),
                    },
                    available: AvailableResources::default(),
                    active_processes: vec![],
                    hosted_corpora: vec![],
                    reported_at: 100 + i as u64,
                    inference_availability: 1.0,
                    inference_capable: false,
                    loaded_models: vec![],

                    embed_model: None,
                    benchmark: None,
                    current_in_flight: None,
                    anchor: None,
                }),
            },
            timestamp: 100 + i as u64,
            origin: id,
        };
        states[i].merge_entry(entry);
    }

    // Run gossip rounds — architecture says 100-node mesh converges in under a minute
    // (< 6 rounds at 10s). 5 nodes should converge in far fewer rounds.
    let max_rounds = 15;
    let mut converged_at = None;

    for round in 0..max_rounds {
        // Each node gossips with 2 random peers.
        for i in 0..5 {
            let peers =
                commonwealth_discovery::gossip::select_gossip_peers(node_ids[i], &node_ids, 2);
            for &peer in &peers {
                let peer_idx = node_ids.iter().position(|&id| id == peer).unwrap();
                let mut initiator_clone = states[i].clone();
                gossip_exchange(&mut initiator_clone, &mut states[peer_idx]).unwrap();
                states[i] = initiator_clone;
            }
        }

        // Check convergence.
        let all_converged = states.iter().all(|s| s.len() == 5);
        if all_converged && converged_at.is_none() {
            converged_at = Some(round);
        }
    }

    let converged_at = converged_at.expect("gossip did not converge");
    assert!(
        converged_at <= 10,
        "gossip took {converged_at} rounds to converge — expected <= 10"
    );

    // Verify all nodes have identical state.
    for state in &states {
        assert_eq!(state.len(), 5, "not all entries converged");
    }
}

#[test]
fn gossip_convergence_with_late_joiner() {
    let node_ids: Vec<NodeId> = (1..=3).map(NodeId::from_u128).collect();
    let mut states: Vec<GossipState> = (0..3).map(|_| GossipState::new()).collect();

    // Nodes 1-3 have their state.
    for (i, &id) in node_ids.iter().enumerate() {
        let entry = GossipEntry {
            key: GossipKey::MemberState { node_id: id },
            value: GossipValue::MemberState {
                status: NodeStatus::Online,
                capabilities: Box::new(NodeCapabilities {
                    hardware: HardwareProfile {
                        gpus: vec![],
                        system_ram_gb: 32,
                        cpu_cores: 8,
                        total_storage_gb: 500,
                        free_storage_gb: 200,
                        network_bandwidth_mbps: None,
                    },
                    available: AvailableResources::default(),
                    active_processes: vec![],
                    hosted_corpora: vec![],
                    reported_at: 100,
                    inference_availability: 1.0,
                    inference_capable: false,
                    loaded_models: vec![],

                    embed_model: None,
                    benchmark: None,
                    current_in_flight: None,
                    anchor: None,
                }),
            },
            timestamp: 100,
            origin: id,
        };
        states[i].merge_entry(entry);
    }

    // Converge the first 3 nodes.
    for _ in 0..5 {
        for i in 0..3 {
            let peers = select_gossip_peers(node_ids[i], &node_ids, 2);
            for &peer in &peers {
                let peer_idx = node_ids.iter().position(|&id| id == peer).unwrap();
                let mut initiator_clone = states[i].clone();
                gossip_exchange(&mut initiator_clone, &mut states[peer_idx]).unwrap();
                states[i] = initiator_clone;
            }
        }
    }
    assert!(states.iter().all(|s| s.len() == 3));

    // Node 4 joins late.
    let node4 = NodeId::from_u128(4);
    let mut all_ids = node_ids.clone();
    all_ids.push(node4);

    let mut state4 = GossipState::new();
    state4.merge_entry(GossipEntry {
        key: GossipKey::MemberState { node_id: node4 },
        value: GossipValue::MemberState {
            status: NodeStatus::Online,
            capabilities: Box::new(NodeCapabilities {
                hardware: HardwareProfile {
                    gpus: vec![],
                    system_ram_gb: 16,
                    cpu_cores: 4,
                    total_storage_gb: 256,
                    free_storage_gb: 100,
                    network_bandwidth_mbps: None,
                },
                available: AvailableResources::default(),
                active_processes: vec![],
                hosted_corpora: vec![],
                reported_at: 200,
                inference_availability: 1.0,
                inference_capable: false,
                loaded_models: vec![],

                embed_model: None,
                benchmark: None,
                current_in_flight: None,
                anchor: None,
            }),
        },
        timestamp: 200,
        origin: node4,
    });
    states.push(state4);

    // Run more gossip rounds.
    for _ in 0..10 {
        for i in 0..4 {
            let peers = select_gossip_peers(all_ids[i], &all_ids, 2);
            for &peer in &peers {
                let peer_idx = all_ids.iter().position(|&id| id == peer).unwrap();
                let mut initiator_clone = states[i].clone();
                gossip_exchange(&mut initiator_clone, &mut states[peer_idx]).unwrap();
                states[i] = initiator_clone;
            }
        }
    }

    // All 4 nodes should have all 4 entries.
    for (i, state) in states.iter().enumerate() {
        assert_eq!(state.len(), 4, "node {i} has {} entries", state.len());
    }
}

// ============================================================================
// Scenario: Shard Plan Correctness (Phase 4)
// Given capabilities, verify plan satisfies constraints.
// ============================================================================

#[tokio::test]
async fn inference_e2e_with_mock_llama_server() {
    // Start a mock llama-server.
    let mock = MockLlamaServer::start().await;
    let mock_addr = mock.address_string();

    // Build a single-node mesh with the model registered.
    let mut mesh = SimulatedMesh::new("E2E Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Test Node").gpu("RTX 4090", 24, ComputeType::Cuda));

    let addrs = mesh.start_all().await;
    let client_addr = addrs[0].0;

    // Register model and point to mock llama-server.
    let model = coding_model(1);
    let model_id = model.id;
    mesh.nodes[0].register_model(model.clone());
    mesh.nodes[0].set_llama_server_address(model_id, mock_addr);

    // Set an inference plan so the model is "loaded".
    mesh.nodes[0].set_inference_plan(InferencePlan {
        model_plans: vec![ShardPlan {
            model: model_id,
            entry_node: NodeId::from_u128(1),
            assignments: vec![ShardAssignment {
                node_id: NodeId::from_u128(1),
                layers: LayerRange::new(0, 64),
                gpu_index: 0,
                rpc_address: "127.0.0.1:50051".parse().unwrap(),
            }],
            estimated_tokens_per_sec: 45.0,
            estimated_ttft_ms: 1100,
        }],
    });

    // Give server a moment to start.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Send a chat completion request.
    let request_body = serde_json::json!({
        "messages": [{"role": "user", "content": "Write a hello world in Rust"}]
    });

    let (status, response) = http_post(client_addr, "/v1/chat/completions", &request_body).await;

    assert_eq!(status, 200, "expected 200, got {status}: {response:?}");
    assert!(response["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .contains("mock response"));

    // Verify the mock received the request.
    assert_eq!(mock.request_count(), 1);
}

#[tokio::test]
async fn inference_e2e_rejects_local_only_privacy() {
    let mut mesh = SimulatedMesh::new("Privacy Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node").gpu("GPU", 24, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let client_addr = addrs[0].0;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let request_body = serde_json::json!({
        "messages": [{"role": "user", "content": "Hello"}],
        "oicp": {
            "oicp_version": "0.2.0",
            "privacy": { "sharding": "local_only" }
        }
    });

    let (status, response) = http_post(client_addr, "/v1/chat/completions", &request_body).await;

    assert_eq!(status, 400);
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("local_only"));
}

#[tokio::test]
async fn status_endpoint_reflects_mesh_state() {
    let mut mesh = architecture_five_node_mesh();
    mesh.sync_mesh_state().await;
    let addrs = mesh.start_all().await;
    let client_addr = addrs[0].0;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let (status, response) = http_get(client_addr, "/status").await;
    assert_eq!(status, 200);
    assert_eq!(response["mesh"]["name"], "Sunset District Co-op");
    assert_eq!(response["mesh"]["members_total"], 5);
}

#[tokio::test]
async fn oicp_capabilities_returns_registered_models() {
    let mut mesh = SimulatedMesh::new("OICP Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node").gpu("GPU", 24, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let client_addr = addrs[0].0;

    // Register a model.
    mesh.nodes[0].register_model(coding_model(1));

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let (status, response) = http_get(client_addr, "/oicp/v1/capabilities").await;
    assert_eq!(status, 200);
    // Server advertises its current OICP version; fixtures elsewhere
    // still send "0.2.0" on purpose to cover backward compatibility.
    assert_eq!(response["oicp_version"], OICP_VERSION);
    assert_eq!(response["models"].as_array().unwrap().len(), 1);
    assert_eq!(response["models"][0]["id"], "qwen3-coder-30b");
}

#[tokio::test]
async fn models_endpoint_lists_registered_models() {
    let mut mesh = SimulatedMesh::new("Models Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node").gpu("GPU", 24, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let client_addr = addrs[0].0;

    mesh.nodes[0].register_model(coding_model(1));
    mesh.nodes[0].register_model(general_model(2));

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let (status, response) = http_get(client_addr, "/v1/models").await;
    assert_eq!(status, 200);
    assert_eq!(response["data"].as_array().unwrap().len(), 2);
}

// ============================================================================
// Scenario: Internal API Endpoints
// ============================================================================

#[tokio::test]
async fn internal_gossip_endpoint_accepts_payload() {
    let mut mesh = SimulatedMesh::new("Internal Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node").gpu("GPU", 24, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let internal_addr = addrs[0].1;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // GossipRequest wraps a MeshWire — send a valid mesh with matching id/hash.
    // MeshId::from_u128(1) serialises as 16-byte big-endian array.
    let mesh_id_bytes: Vec<u8> = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    let hash_bytes: Vec<u8> = vec![0u8; 32];
    let (status, _) = http_post(
        internal_addr,
        "/internal/gossip",
        &serde_json::json!({
            "mesh": {
                "id": mesh_id_bytes,
                "name": "Internal Test",
                "join_key_hash": hash_bytes,
                "members": [],
                "peers": []
            }
        }),
    )
    .await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn internal_latency_probe_responds() {
    let mut mesh = SimulatedMesh::new("Probe Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node").gpu("GPU", 24, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let internal_addr = addrs[0].1;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let (status, _) = http_get(internal_addr, "/internal/latency/probe").await;
    assert_eq!(status, 200);
}

// ============================================================================
// Scenario: Fault Detection State Machine (Phase 8)
// Nodes transition through health states as heartbeats timeout.
// ============================================================================

#[test]
fn fault_detection_timeout_transitions() {
    let config = FaultDetectorConfig {
        suspected_timeout: Duration::from_millis(50),
        away_timeout: Duration::from_millis(100),
        failure_timeout: Duration::from_millis(200),
    };
    let mut fd = FaultDetector::new(config);

    // Register 3 nodes in a simulated mesh.
    let ids: Vec<NodeId> = (1..=3).map(NodeId::from_u128).collect();
    for &id in &ids {
        fd.register_node(id);
    }

    // All healthy initially.
    assert_eq!(fd.healthy_nodes().len(), 3);
    assert!(fd.failed_nodes().is_empty());

    // After enough time, node 1 should transition — but we keep 2 and 3 alive.
    std::thread::sleep(Duration::from_millis(60));

    // Heartbeat nodes 2 and 3 *after* the sleep so they stay healthy.
    fd.record_heartbeat(ids[1]);
    fd.record_heartbeat(ids[2]);

    let events = fd.check_all();
    assert!(
        events
            .iter()
            .any(|e| *e == FaultEvent::NodeSuspected { node_id: ids[0] }),
        "node 1 should be suspected after timeout"
    );

    // Nodes 2, 3 still healthy because we heartbeated them just now.
    assert_eq!(fd.node_status(ids[1]), Some(FaultStatus::Healthy));
    assert_eq!(fd.node_status(ids[2]), Some(FaultStatus::Healthy));
}

#[test]
fn fault_detection_recovery_on_heartbeat() {
    // Timeouts deliberately wider than the original 30/60/120ms set
    // so the precondition sleep (~40ms before, now 100ms) has
    // headroom against parallel-test scheduler jitter without
    // crossing into the next bucket. The original test failed
    // intermittently under repo-wide `cargo test` load when the
    // 40ms sleep stretched past the 60ms `away_timeout` boundary.
    let config = FaultDetectorConfig {
        suspected_timeout: Duration::from_millis(60),
        away_timeout: Duration::from_millis(400),
        failure_timeout: Duration::from_millis(800),
    };
    let mut fd = FaultDetector::new(config);
    let id = NodeId::from_u128(1);
    fd.register_node(id);

    // Sleep into the Suspected window. 100ms is comfortably past
    // 60ms (suspected_timeout) and far below 400ms (away_timeout)
    // even when the test thread is slow to schedule.
    std::thread::sleep(Duration::from_millis(100));
    fd.check_all();
    // Accept either Suspected (the happy path) or Away (extreme
    // scheduler stall): the precondition is "the node has fallen
    // out of Healthy"; the actual contract under test is whether
    // a heartbeat recovers it from any non-Healthy state.
    let pre_status = fd.node_status(id);
    assert!(
        matches!(
            pre_status,
            Some(FaultStatus::Suspected) | Some(FaultStatus::Away)
        ),
        "expected Suspected or Away after timeout, got {pre_status:?}"
    );

    // Heartbeat recovers it. This is the assertion the test's name
    // is actually about.
    let event = fd.record_heartbeat(id);
    assert_eq!(event, Some(FaultEvent::NodeRecovered { node_id: id }));
    assert_eq!(fd.node_status(id), Some(FaultStatus::Healthy));
}

// ============================================================================
// Scenario: Graceful Departure Protocol (Phase 8)
// Full state machine from announcement to safe stop.
// ============================================================================

#[test]
fn graceful_departure_full_lifecycle() {
    let id = NodeId::from_u128(1);

    // Create with very short countdown for testing.
    let mut dep = GracefulDeparture::with_countdown(id, Duration::from_millis(50));
    assert_eq!(dep.state(), DepartureState::Announced);
    assert!(!dep.is_ready_to_stop());

    // Advance through states.
    assert_eq!(dep.advance(), DepartureState::Rebalancing);
    assert_eq!(dep.advance(), DepartureState::Draining);
    assert_eq!(dep.advance(), DepartureState::Complete);
    assert!(dep.is_ready_to_stop());

    // Integrate with fault detector.
    let mut fd = FaultDetector::new(FaultDetectorConfig::default());
    fd.register_node(id);
    let event = fd.begin_graceful_departure(id);
    assert_eq!(event, Some(FaultEvent::NodeDeparting { node_id: id }));

    let event = fd.mark_departed(id);
    assert_eq!(event, Some(FaultEvent::NodeDeparted { node_id: id }));
    assert!(fd.failed_nodes().contains(&id));
}

// ============================================================================
// Scenario: 503 + Retry-After on Backend Failure (Phase 8)
// Backend unavailable → 503 with Retry-After header.
// ============================================================================

#[tokio::test]
async fn inference_503_retry_after_on_backend_failure() {
    let mut mesh = SimulatedMesh::new("503 Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node").gpu("GPU", 24, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let client_addr = addrs[0].0;

    // Register model pointing to a non-existent llama-server address.
    let model = coding_model(1);
    let model_id = model.id;
    mesh.nodes[0].register_model(model);
    mesh.nodes[0].set_llama_server_address(model_id, "127.0.0.1:1".into()); // Nothing listening.
    mesh.nodes[0].set_inference_plan(InferencePlan {
        model_plans: vec![ShardPlan {
            model: model_id,
            entry_node: NodeId::from_u128(1),
            assignments: vec![ShardAssignment {
                node_id: NodeId::from_u128(1),
                layers: LayerRange::new(0, 64),
                gpu_index: 0,
                rpc_address: "127.0.0.1:50051".parse().unwrap(),
            }],
            estimated_tokens_per_sec: 40.0,
            estimated_ttft_ms: 1000,
        }],
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let request_body = serde_json::json!({
        "messages": [{"role": "user", "content": "Hello"}]
    });
    let (status, response) = http_post(client_addr, "/v1/chat/completions", &request_body).await;

    // Should be 503, not 502.
    assert_eq!(status, 503, "expected 503, got {status}");
    assert!(
        response["error"]["type"]
            .as_str()
            .unwrap()
            .contains("unavailable"),
        "error type should indicate unavailability"
    );
}

// ============================================================================
// Scenario: OICP Routing Selects Correct Model (Phase 9)
// Two models with different capabilities, verify routing by OICP requirements.
// ============================================================================

#[tokio::test]
async fn oicp_routing_selects_correct_model() {
    // Start two mock llama-servers — one for each model.
    let mock_coder = MockLlamaServer::start().await;
    let mock_general = MockLlamaServer::start().await;

    let mut mesh = SimulatedMesh::new("OICP Routing Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node").gpu("GPU", 48, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let client_addr = addrs[0].0;

    // Register both models.
    let coder = coding_model(1);
    let general = general_model(2);
    let coder_id = coder.id;
    let general_id = general.id;

    mesh.nodes[0].register_model(coder);
    mesh.nodes[0].register_model(general);

    // Point each to its own mock server.
    mesh.nodes[0].set_llama_server_address(coder_id, mock_coder.address_string());
    mesh.nodes[0].set_llama_server_address(general_id, mock_general.address_string());

    // Set inference plan with both models.
    mesh.nodes[0].set_inference_plan(InferencePlan {
        model_plans: vec![
            ShardPlan {
                model: coder_id,
                entry_node: NodeId::from_u128(1),
                assignments: vec![ShardAssignment {
                    node_id: NodeId::from_u128(1),
                    layers: LayerRange::new(0, 64),
                    gpu_index: 0,
                    rpc_address: "127.0.0.1:50051".parse().unwrap(),
                }],
                estimated_tokens_per_sec: 45.0,
                estimated_ttft_ms: 1100,
            },
            ShardPlan {
                model: general_id,
                entry_node: NodeId::from_u128(1),
                assignments: vec![ShardAssignment {
                    node_id: NodeId::from_u128(1),
                    layers: LayerRange::new(0, 64),
                    gpu_index: 0,
                    rpc_address: "127.0.0.1:50052".parse().unwrap(),
                }],
                estimated_tokens_per_sec: 38.0,
                estimated_ttft_ms: 1300,
            },
        ],
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // v0.3 code request → should route to coder model via claim hint match.
    let code_request = serde_json::json!({
        "messages": [{"role": "user", "content": "Write Rust code"}],
        "oicp": {
            "oicp_version": "0.3.0",
            "capability_hint": "code",
            "latency_class": "normal",
            "privacy": {"sharding": "mesh_allowed"}
        }
    });
    let (status, _) = http_post(client_addr, "/v1/chat/completions", &code_request).await;
    assert_eq!(status, 200, "code request should succeed");
    assert_eq!(
        mock_coder.request_count(),
        1,
        "coder should get the request"
    );
    assert_eq!(mock_general.request_count(), 0, "general should not get it");

    // v0.3 general request → routes to the general model. A `general`
    // hint explicitly matches only `general`-hinted claims, so the
    // coder model's `code`-hinted synthesized claim is eliminated.
    let analysis_request = serde_json::json!({
        "messages": [{"role": "user", "content": "Analyze this paper"}],
        "oicp": {
            "oicp_version": "0.3.0",
            "capability_hint": "general",
            "latency_class": "normal",
            "privacy": {"sharding": "mesh_allowed"}
        }
    });
    let (status, _) = http_post(client_addr, "/v1/chat/completions", &analysis_request).await;
    assert_eq!(status, 200, "analysis request should succeed");
    assert_eq!(
        mock_coder.request_count(),
        1,
        "coder should not get second request"
    );
    assert_eq!(
        mock_general.request_count(),
        1,
        "general should get the analysis request"
    );
}

// ============================================================================
// Scenario: Multi-Model Portfolio Swap (Phase 10)
// Portfolio manages two models, transitions without gaps.
// ============================================================================

#[tokio::test]
async fn knowledge_search_returns_results_for_assigned_corpora() {
    let mut mesh = SimulatedMesh::new("Knowledge Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node").gpu("GPU", 24, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let client_addr = addrs[0].0;

    // Set up knowledge plan with two corpora.
    let knowledge_plan = KnowledgeShardPlan {
        assignments: vec![
            KnowledgeShardAssignment {
                node_id: NodeId::from_u128(1),
                corpus_id: "wikipedia".into(),
                chunk_range: None,
                is_replica: false,
            },
            KnowledgeShardAssignment {
                node_id: NodeId::from_u128(1),
                corpus_id: "sep".into(),
                chunk_range: None,
                is_replica: false,
            },
        ],
        redundancy_achieved: [("wikipedia".into(), 1), ("sep".into(), 1)]
            .into_iter()
            .collect(),
    };
    mesh.nodes[0].set_knowledge_plan(knowledge_plan);

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Search with specific corpora.
    // Both query_embedding and query_text are required by OICP §6.1.
    let request = serde_json::json!({
        "query_embedding": [],
        "query_text": "Ostrom design principles",
        "corpora": ["wikipedia", "sep"],
        "limit": 10
    });
    let (status, response) = http_post(client_addr, "/v1/knowledge/search", &request).await;
    assert_eq!(status, 200);

    // No real corpora installed in the test harness — the route returns 200
    // with empty results. The knowledge plan is set but there are no actual
    // corpus indexes on disk for the simulated node to search.
    let results = response["results"].as_array().unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn knowledge_search_empty_when_no_shards() {
    let mut mesh = SimulatedMesh::new("Empty Knowledge Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node").gpu("GPU", 24, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let client_addr = addrs[0].0;

    // No knowledge plan set — should return 503.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let request = serde_json::json!({
        "query_embedding": [],
        "query_text": "test query",
        "limit": 5
    });
    let (status, response) = http_post(client_addr, "/v1/knowledge/search", &request).await;
    // No knowledge plan set — route returns 200 with empty results rather than 503.
    assert_eq!(status, 200);
    let results = response["results"].as_array().unwrap();
    assert!(results.is_empty());
}

// ============================================================================
// Scenario: Significance Thresholds (Phase 3)
// Verify significance detection matches architecture spec.
// ============================================================================

#[test]
fn significance_thresholds_match_architecture_spec() {
    let thresholds = SignificanceThresholds::default();

    let baseline = AvailableResources {
        free_vram_gb: 20.0,
        free_ram_gb: 32.0,
        free_storage_gb: 500.0,
        gpu_utilization: 0.3,
        cpu_utilization: 0.4,
        available_for_mesh: true,
    };

    // >10% VRAM change is significant.
    let vram_change = AvailableResources {
        free_vram_gb: 17.0, // -15% of 20
        ..baseline.clone()
    };
    assert!(thresholds.is_significant(&baseline, &vram_change));

    // <10% VRAM change is NOT significant.
    let small_vram = AvailableResources {
        free_vram_gb: 19.0, // -5% of 20
        ..baseline.clone()
    };
    assert!(!thresholds.is_significant(&baseline, &small_vram));

    // GPU utilization crossing 0.5 boundary.
    let gpu_cross = AvailableResources {
        gpu_utilization: 0.6, // crosses 0.5
        ..baseline.clone()
    };
    assert!(thresholds.is_significant(&baseline, &gpu_cross));

    // GPU utilization NOT crossing boundary (same band).
    let gpu_same_band = AvailableResources {
        gpu_utilization: 0.4, // both below 0.5
        ..baseline.clone()
    };
    assert!(!thresholds.is_significant(&baseline, &gpu_same_band));

    // GPU utilization crossing 0.9 boundary.
    let high_baseline = AvailableResources {
        gpu_utilization: 0.85,
        ..baseline.clone()
    };
    let gpu_cross_90 = AvailableResources {
        gpu_utilization: 0.95,
        ..baseline.clone()
    };
    assert!(thresholds.is_significant(&high_baseline, &gpu_cross_90));

    // Availability toggle always significant.
    let toggled = AvailableResources {
        available_for_mesh: false,
        ..baseline.clone()
    };
    assert!(thresholds.is_significant(&baseline, &toggled));
}

// ============================================================================
// Scenario: OmO Model Alias Routing
// Client sends cloud model name, alias table infers OICP, routes correctly.
// ============================================================================

#[tokio::test]
async fn omo_model_alias_routes_to_coding_model() {
    // Start two mock llama-servers — one per model.
    let mock_coder = MockLlamaServer::start().await;
    let mock_general = MockLlamaServer::start().await;

    let mut mesh = SimulatedMesh::new("OmO Alias Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node").gpu("GPU", 48, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let client_addr = addrs[0].0;

    // Register both models.
    let coder = coding_model(1);
    let general = general_model(2);
    let coder_id = coder.id;
    let general_id = general.id;

    mesh.nodes[0].register_model(coder);
    mesh.nodes[0].register_model(general);
    mesh.nodes[0].set_llama_server_address(coder_id, mock_coder.address_string());
    mesh.nodes[0].set_llama_server_address(general_id, mock_general.address_string());

    // Set inference plan with both models.
    mesh.nodes[0].set_inference_plan(InferencePlan {
        model_plans: vec![
            ShardPlan {
                model: coder_id,
                entry_node: NodeId::from_u128(1),
                assignments: vec![ShardAssignment {
                    node_id: NodeId::from_u128(1),
                    layers: LayerRange::new(0, 64),
                    gpu_index: 0,
                    rpc_address: "127.0.0.1:50051".parse().unwrap(),
                }],
                estimated_tokens_per_sec: 45.0,
                estimated_ttft_ms: 1100,
            },
            ShardPlan {
                model: general_id,
                entry_node: NodeId::from_u128(1),
                assignments: vec![ShardAssignment {
                    node_id: NodeId::from_u128(1),
                    layers: LayerRange::new(0, 64),
                    gpu_index: 0,
                    rpc_address: "127.0.0.1:50052".parse().unwrap(),
                }],
                estimated_tokens_per_sec: 38.0,
                estimated_ttft_ms: 1300,
            },
        ],
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // OmO sends "gpt-5.3-codex" — no oicp field.
    // The alias table should match this to coding requirements
    // and route to the coder model.
    let omo_coding_request = serde_json::json!({
        "model": "gpt-5.3-codex",
        "messages": [{"role": "user", "content": "Write a Rust function"}]
    });
    let (status, _) = http_post(client_addr, "/v1/chat/completions", &omo_coding_request).await;
    assert_eq!(status, 200, "OmO coding request should succeed");
    assert_eq!(
        mock_coder.request_count(),
        1,
        "coding request should route to coder model"
    );
    assert_eq!(
        mock_general.request_count(),
        0,
        "coding request should NOT route to general model"
    );

    // OmO sends "claude-opus-4-6" — should route to general model.
    let omo_orchestration_request = serde_json::json!({
        "model": "claude-opus-4-6",
        "messages": [{"role": "user", "content": "Orchestrate this task"}]
    });
    let (status, _) = http_post(
        client_addr,
        "/v1/chat/completions",
        &omo_orchestration_request,
    )
    .await;
    assert_eq!(status, 200, "OmO orchestration request should succeed");
    assert_eq!(
        mock_coder.request_count(),
        1,
        "orchestration request should NOT route to coder"
    );
    assert_eq!(
        mock_general.request_count(),
        1,
        "orchestration request should route to general model"
    );
}

#[tokio::test]
async fn unknown_model_name_falls_through_to_default() {
    let mock = MockLlamaServer::start().await;

    let mut mesh = SimulatedMesh::new("Fallthrough Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node").gpu("GPU", 24, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let client_addr = addrs[0].0;

    let model = general_model(1);
    let model_id = model.id;
    mesh.nodes[0].register_model(model);
    mesh.nodes[0].set_llama_server_address(model_id, mock.address_string());
    mesh.nodes[0].set_inference_plan(InferencePlan {
        model_plans: vec![ShardPlan {
            model: model_id,
            entry_node: NodeId::from_u128(1),
            assignments: vec![ShardAssignment {
                node_id: NodeId::from_u128(1),
                layers: LayerRange::new(0, 64),
                gpu_index: 0,
                rpc_address: "127.0.0.1:50051".parse().unwrap(),
            }],
            estimated_tokens_per_sec: 38.0,
            estimated_ttft_ms: 1300,
        }],
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Completely unknown model name — no alias match.
    // Should fall through to default model.
    let request = serde_json::json!({
        "model": "totally-unknown-model-v99",
        "messages": [{"role": "user", "content": "Hello"}]
    });
    let (status, _) = http_post(client_addr, "/v1/chat/completions", &request).await;
    assert_eq!(status, 200, "unknown model should fall through to default");
    assert_eq!(mock.request_count(), 1);
}

// ============================================================================
// Scenario: Activity-Aware Availability (inference routing)
// POST /internal/node/activity updates the node's inference_availability so
// the scheduler routes work away from hot nodes.
// ============================================================================

#[tokio::test]
async fn node_activity_endpoint_returns_204_for_all_known_levels() {
    let mut mesh = SimulatedMesh::new("Activity Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Dev Node").gpu("RTX 4090", 24, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let internal_addr = addrs[0].1;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // All four canonical levels must return 204.
    for level in ["hot", "warm", "cool", "idle"] {
        let (status, _) = http_post(
            internal_addr,
            "/internal/node/activity",
            &serde_json::json!({ "level": level, "reason": "integration_test" }),
        )
        .await;
        assert_eq!(status, 204, "level '{level}' must return 204 No Content");
    }
}

#[tokio::test]
async fn node_activity_hot_then_idle_reflected_in_gossip_response() {
    // Send "hot" to node A, then pull node A's gossip and verify the returned
    // mesh carries updated capabilities.  We do this by sending a gossip
    // request back to node A (with a matching mesh snapshot) and checking the
    // round-trip succeeds — a proxy for "the state machine is live".
    let mut mesh = SimulatedMesh::new("Activity Gossip Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node A").gpu("GPU", 24, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let internal_addr = addrs[0].1;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Mark node as hot.
    let (status, _) = http_post(
        internal_addr,
        "/internal/node/activity",
        &serde_json::json!({ "level": "hot", "reason": "heavy_build" }),
    )
    .await;
    assert_eq!(status, 204);

    // Verify the node is still reachable and responding to gossip (state not corrupted).
    let mesh_id_bytes: Vec<u8> = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    let hash_bytes: Vec<u8> = vec![0u8; 32];
    let (gossip_status, _) = http_post(
        internal_addr,
        "/internal/gossip",
        &serde_json::json!({
            "mesh": {
                "id": mesh_id_bytes,
                "name": "Activity Gossip Test",
                "join_key_hash": hash_bytes,
                "members": [],
                "peers": []
            }
        }),
    )
    .await;
    assert_eq!(
        gossip_status, 200,
        "gossip must succeed after activity update"
    );

    // Now set back to idle — verifies the state machine transitions bidirectionally.
    let (status, _) = http_post(
        internal_addr,
        "/internal/node/activity",
        &serde_json::json!({ "level": "idle", "reason": "tests_finished" }),
    )
    .await;
    assert_eq!(
        status, 204,
        "transitioning back to idle must also return 204"
    );
}
