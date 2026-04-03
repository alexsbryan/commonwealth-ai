use std::collections::HashMap;

use commonwealth_core::capabilities::*;
use commonwealth_core::ids::*;
use commonwealth_core::mesh::*;
use commonwealth_core::scheduler::*;

use commonwealth_discovery::gossip::*;
use commonwealth_discovery::membership;

use commonwealth_scheduler::leader;
use commonwealth_scheduler::plan_builder;

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

#[test]
fn shard_plan_architecture_scenario() {
    let mesh = architecture_five_node_mesh();
    let model = test_model(1, "test-70b", 80, 40);

    let caps = mesh.node_capabilities();
    let node_configs: HashMap<NodeId, plan_builder::NodeConfig> = mesh
        .nodes
        .iter()
        .map(|n| {
            (
                n.node_id,
                plan_builder::NodeConfig {
                    reserve_vram_gb: 4,
                    reserve_ram_gb: 8,
                    internal_port: 9742,
                },
            )
        })
        .collect();

    let plan =
        plan_builder::build_shard_plan(&model, &caps, &node_configs, &mesh.latency_matrix, None)
            .unwrap();

    // All 80 layers must be assigned.
    let total_layers: u32 = plan.assignments.iter().map(|a| a.layers.count()).sum();
    assert_eq!(total_layers, 80);

    // Ranges must be contiguous.
    let mut expected = 0;
    for a in &plan.assignments {
        assert_eq!(a.layers.start, expected, "gap in layer ranges");
        expected = a.layers.end;
    }
    assert_eq!(expected, 80);

    // Carol (node 3, 144 GB GPU) should have the most layers.
    let carol_layers = plan
        .assignments
        .iter()
        .find(|a| a.node_id == NodeId::from_u128(3))
        .unwrap()
        .layers
        .count();
    assert!(
        carol_layers > 30,
        "Carol should have the most layers, got {carol_layers}"
    );

    // Performance estimates should be positive.
    assert!(plan.estimated_tokens_per_sec > 0.0);
    assert!(plan.estimated_ttft_ms > 0);
}

#[test]
fn shard_plan_single_node_gets_all_layers() {
    let mut mesh = SimulatedMesh::new("Single Node Test");
    mesh.add_node(
        SimulatedNodeBuilder::new(1, "Only Node")
            .gpu("RTX 4090", 24, ComputeType::Cuda)
            .ram_gb(64),
    );

    let model = test_model(1, "small-model", 32, 8);
    let caps = mesh.node_capabilities();
    let configs: HashMap<NodeId, plan_builder::NodeConfig> = [(
        NodeId::from_u128(1),
        plan_builder::NodeConfig {
            reserve_vram_gb: 4,
            reserve_ram_gb: 8,
            internal_port: 9742,
        },
    )]
    .into_iter()
    .collect();

    let plan = plan_builder::build_shard_plan(&model, &caps, &configs, &mesh.latency_matrix, None)
        .unwrap();

    assert_eq!(plan.assignments.len(), 1);
    assert_eq!(plan.assignments[0].layers, LayerRange::new(0, 32));
}

#[test]
fn shard_plan_privacy_prefers_requester_as_entry() {
    let mesh = architecture_five_node_mesh();
    let model = test_model(1, "test-model", 64, 17);
    let caps = mesh.node_capabilities();
    let configs: HashMap<NodeId, plan_builder::NodeConfig> = mesh
        .nodes
        .iter()
        .map(|n| {
            (
                n.node_id,
                plan_builder::NodeConfig {
                    reserve_vram_gb: 4,
                    reserve_ram_gb: 8,
                    internal_port: 9742,
                },
            )
        })
        .collect();

    // Request that Eve (node 5) be the entry node.
    let plan = plan_builder::build_shard_plan(
        &model,
        &caps,
        &configs,
        &mesh.latency_matrix,
        Some(NodeId::from_u128(5)),
    )
    .unwrap();

    // Eve should be the entry node (layer 0 host).
    assert_eq!(plan.entry_node, NodeId::from_u128(5));
    assert_eq!(plan.assignments[0].node_id, NodeId::from_u128(5));
}

// ============================================================================
// Scenario: Leader Election
// Deterministic lowest-NodeId-wins.
// ============================================================================

#[test]
fn leader_election_is_deterministic_across_orderings() {
    let ids_a = vec![
        NodeId::from_u128(5),
        NodeId::from_u128(2),
        NodeId::from_u128(8),
        NodeId::from_u128(1),
        NodeId::from_u128(3),
    ];
    let ids_b = vec![
        NodeId::from_u128(3),
        NodeId::from_u128(1),
        NodeId::from_u128(8),
        NodeId::from_u128(5),
        NodeId::from_u128(2),
    ];

    let leader_a = leader::elect_leader(&ids_a);
    let leader_b = leader::elect_leader(&ids_b);
    assert_eq!(leader_a, leader_b);
    assert_eq!(leader_a, Some(NodeId::from_u128(1)));
}

// ============================================================================
// Scenario: Inference E2E (Phase 6)
// HTTP request → router → llama-server mock → response.
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
    mesh.nodes[0].register_model(model.clone()).await;
    mesh.nodes[0]
        .set_llama_server_address(model_id, mock_addr)
        .await;

    // Set an inference plan so the model is "loaded".
    mesh.nodes[0]
        .set_inference_plan(InferencePlan {
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
        })
        .await;

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
            "oicp_version": "0.1.0",
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
    mesh.nodes[0].register_model(coding_model(1)).await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let (status, response) = http_get(client_addr, "/oicp/v1/capabilities").await;
    assert_eq!(status, 200);
    assert_eq!(response["oicp_version"], "0.2.0");
    assert_eq!(response["models"].as_array().unwrap().len(), 1);
    assert_eq!(response["models"][0]["id"], "qwen3-coder-30b");
}

#[tokio::test]
async fn models_endpoint_lists_registered_models() {
    let mut mesh = SimulatedMesh::new("Models Test");
    mesh.add_node(SimulatedNodeBuilder::new(1, "Node").gpu("GPU", 24, ComputeType::Cuda));
    let addrs = mesh.start_all().await;
    let client_addr = addrs[0].0;

    mesh.nodes[0].register_model(coding_model(1)).await;
    mesh.nodes[0].register_model(general_model(2)).await;

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

    let (status, _) = http_post(
        internal_addr,
        "/internal/gossip",
        &serde_json::json!({"entries": []}),
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
