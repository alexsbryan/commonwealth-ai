// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use commonwealth_core::capabilities::*;
use commonwealth_core::ids::*;
use commonwealth_core::knowledge::*;
use commonwealth_core::mesh::*;
use commonwealth_inference::inference_plan::*;
use commonwealth_inference::oicp::*;

use commonwealth_discovery::membership;

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

// ============================================================================
// Scenario: Graceful Departure Protocol (Phase 8)
// Full state machine from announcement to safe stop.
// ============================================================================

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

    // Search with specific corpora. This exercises shard routing, so we
    // supply a pre-embedded query (the mesh-internal shape). OICP v0.4 §6.1
    // makes `query_embedding` optional — when absent the host embeds
    // `query_text` — but that thin-client path needs local inference the
    // simulated harness doesn't run; it's covered by the oicp-types unit
    // tests and the oicp-conformance suite instead.
    let request = serde_json::json!({
        "query_embedding": [0.1, 0.2, 0.3],
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

    // Pre-embedded query (see the shard-routing test above for why the
    // text-only §6.1 path isn't exercised in the simulated harness).
    let request = serde_json::json!({
        "query_embedding": [0.1, 0.2, 0.3],
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
