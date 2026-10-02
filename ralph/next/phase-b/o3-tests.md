# O3: a546a456b's 15 stopped harness tests, accounted at C

Ship gate Tier 1, O3, ruled "successors" (operator, phase-b-93). Census at
f9b15ac80. "new" rows were written by pb-distribution-o3-tests and each was
watched red against a planted break of its behaviour (the row's last commit
carries the red lines). Paths are under `sovereign/crates/` unless they start
with `cmnwlth/`.

| test (a546a456b) | behaviour | owner | successor |
|---|---|---|---|
| inference_e2e_with_mock_llama_server | a turn with no in-process engine is forwarded to the model's llama-server and its answer returned | svrn daemon (`routes_inference::forward_to_model`) | new: sovereign-daemon/src/routes_inference/forward_tests.rs:146 `a_turn_is_forwarded_to_the_models_backend_and_its_answer_returned` |
| inference_e2e_rejects_local_only_privacy | a `local_only` turn with no local engine is refused 400, naming `local_only` | svrn daemon (`chat_completions` privacy gate) | sovereign-daemon/src/tests/server.rs:264 `chat_completions_rejects_local_only` |
| status_endpoint_reflects_mesh_state | `/status` names the mesh and counts its members | svrn daemon `/status`, over cw-rails' roster port | sovereign-daemon/tests/main/membership_port.rs:154 `status_counts_the_mesh_from_the_membership_port` |
| oicp_capabilities_returns_registered_models | `/oicp/v1/capabilities` offers the models this node serves | serve's manifest, read on svrn's port | sovereign-stock/tests/serve_routes/status_answers_from_serve.rs:234 `a_reload_is_seen_by_every_reader_boot_wired` (asserts `models[].id`) |
| models_endpoint_lists_registered_models | `/v1/models` lists registered models | svrn daemon (`routes_inference::list_models`) | sovereign-daemon/src/tests/server.rs:565 `models_endpoint_with_registered_model`; sovereign-daemon/tests/main/models_http_e2e.rs:119 `locally_owned_model_appears_in_v1_models_response` |
| internal_gossip_endpoint_accepts_payload | `/internal/gossip` merges a same-mesh round and answers 200 | cw-rails (the daemon's copy left in the flip, 1c120f23d, ledger row "gossip_route.rs (8) S") | cmnwlth/crates/commonwealth-rails/src/internal.rs:324 `a_round_from_the_same_mesh_is_merged_and_answered` |
| internal_latency_probe_responds | `/internal/latency/probe` answers 200 | svrn daemon (internal router, server.rs:363) | sovereign-daemon/src/tests/server.rs:339 `internal_latency_probe_endpoint` |
| inference_503_retry_after_on_backend_failure | a backend that does not answer is 503 with `retry-after` and an "unavailable" error type, not 502 | svrn daemon (`forward_to_model`) | new: sovereign-daemon/src/routes_inference/forward_tests.rs:162 `an_unreachable_backend_is_a_503_with_retry_after` |
| oicp_routing_selects_correct_model | an OICP capability hint routes to the model whose claims match | svrn daemon (`route_with_oicp`, Priority 1) | new: sovereign-daemon/src/routes_inference/forward_tests.rs:189 `an_oicp_hint_routes_to_the_model_whose_claims_match` |
| knowledge_search_returns_results_for_assigned_corpora | `/v1/knowledge/search` answers 200 with results from the node that hosts the corpus | svrn daemon (`routes_knowledge` fan-out) | sovereign-daemon/tests/main/knowledge_fanout_e2e.rs:153 `joiner_fans_out_to_peer_when_corpus_not_local` |
| knowledge_search_empty_when_no_shards | with no reachable shard the search answers 200 and empty, not 503 | svrn daemon (`routes_knowledge`) | sovereign-daemon/tests/main/knowledge_fanout_e2e.rs:291 `offline_peer_is_excluded_from_fan_out_plan` |
| omo_model_alias_routes_to_coding_model | a client model name the alias table knows (`gpt-5.3-codex`, `claude-opus-4-6`) routes by the alias's requirements | svrn daemon (Priority 3) over oicp-types' `ModelAliasTable` | new: sovereign-daemon/src/routes_inference/forward_tests.rs:228 `an_aliased_model_name_routes_by_the_aliases_requirements`; the table: shared/crates/oicp-types/src/model_aliases.rs `omo_codex_resolves_to_code_hint` |
| unknown_model_name_falls_through_to_default | on the forwarding path an unmatched name is served by the plan's default model | svrn daemon (Priority 4) | new: sovereign-daemon/src/routes_inference/forward_tests.rs:257 `an_unknown_model_name_falls_through_to_the_default_model` |
| node_activity_endpoint_returns_204_for_all_known_levels | `/internal/node/activity` answers 204 for hot, warm, cool and idle | svrn daemon (`mesh_admin::node_activity`) | new: sovereign-daemon/src/routes_internal/mesh_admin/tests.rs:49 `every_known_level_returns_204_no_content` |
| node_activity_hot_then_idle_reflected_in_gossip_response | an activity level reaches the availability peers see | svrn daemon sets it; cw-rails carries it (the daemon's gossip round retired in the flip, 1c120f23d; availability now rides the origin registration's claims) | sovereign-daemon/src/routes_internal/mesh_admin/tests.rs:106 `idle_level_sets_availability_to_100` (hot then idle) → sovereign-daemon/src/peer_origin.rs:204 `the_claims_source_declares_the_state_at_each_call` → cmnwlth/crates/commonwealth-rails/tests/origins.rs:698 `a_renew_carries_the_registrants_new_declaration_into_gossip` |

The containment guard (c23f3b4c0's `containment_guard_e2e.rs`) is restored
as a process e2e at sovereign-stock/tests/containment_guard_e2e.rs and, for
serve alone, sovereign-serve/tests/containment_guard_e2e.rs (cb25c02c2).
