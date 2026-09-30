// SPDX-License-Identifier: AGPL-3.0-or-later
//! One integration-test binary for this crate.
//!
//! Each former `tests/<name>.rs` is now `tests/main/<name>.rs`, declared
//! below, so cargo links ONE executable instead of one per file. Every
//! test still runs; its name gains the module path as a prefix, so a
//! filter that named a file now names a module:
//!
//!     cargo test -p <crate> --test main <module>::
//!
//! `#[path]` is load-bearing: `tests/main.rs` is a CRATE ROOT, so a bare
//! `mod foo;` resolves to `tests/foo.rs` — which cargo would then also
//! link as its own test binary, which is the thing this file exists to
//! stop. The attribute keeps the sources in `tests/main/`, a directory
//! cargo does not scan for targets.
//!
//! Files still sitting directly in `tests/` are there on purpose — they
//! need process isolation, or a `.config/nextest.toml` override keys on
//! their binary name. Do not fold those in.

#[path = "main/admin_join_serves_venues_e2e.rs"]
mod admin_join_serves_venues_e2e;
#[path = "main/atlas_surface_e2e.rs"]
mod atlas_surface_e2e;
#[path = "main/binary_boot_rails_census.rs"]
mod binary_boot_rails_census;
#[path = "main/canonical_pull_e2e.rs"]
mod canonical_pull_e2e;
#[path = "main/capabilities_published.rs"]
mod capabilities_published;
#[path = "main/chat_completion_e2e.rs"]
mod chat_completion_e2e;
#[path = "main/client_auth.rs"]
mod client_auth;
#[path = "main/client_exposure.rs"]
mod client_exposure;
#[path = "main/client_tokens_e2e.rs"]
mod client_tokens_e2e;
#[path = "main/code_server_via_mcp_client.rs"]
mod code_server_via_mcp_client;
#[path = "main/common/mod.rs"]
mod common;
#[path = "main/control_plane_not_shed.rs"]
mod control_plane_not_shed;
#[path = "main/conv_surface_e2e.rs"]
mod conv_surface_e2e;
#[path = "main/corpus_lifecycle.rs"]
mod corpus_lifecycle;
#[path = "main/corpus_sharing_over_iroh_e2e.rs"]
mod corpus_sharing_over_iroh_e2e;
#[path = "main/corpus_watch_http_e2e.rs"]
mod corpus_watch_http_e2e;
#[path = "main/d6_surface_e2e.rs"]
mod d6_surface_e2e;
#[path = "main/d8_surface_e2e.rs"]
mod d8_surface_e2e;
#[path = "main/d9_turn_extras_e2e.rs"]
mod d9_turn_extras_e2e;
#[path = "main/d9a_corpus_catalog_e2e.rs"]
mod d9a_corpus_catalog_e2e;
#[path = "main/d9a_documents_e2e.rs"]
mod d9a_documents_e2e;
#[path = "main/daemon_variant_census.rs"]
mod daemon_variant_census;
#[path = "main/daemon_wiring.rs"]
mod daemon_wiring;
#[path = "main/embeddings_e2e.rs"]
mod embeddings_e2e;
#[path = "main/emitter_origin_concurrency.rs"]
mod emitter_origin_concurrency;
#[path = "main/engine_census.rs"]
mod engine_census;
#[path = "main/enrich_surface_e2e.rs"]
mod enrich_surface_e2e;
#[path = "main/finish_reason_streaming.rs"]
mod finish_reason_streaming;
#[path = "main/fold_ingest_abandoned_unit_e2e.rs"]
mod fold_ingest_abandoned_unit_e2e;
#[path = "main/fold_ingest_coverage_refusal_e2e.rs"]
mod fold_ingest_coverage_refusal_e2e;
#[path = "main/fold_ingest_cross_node_merge_e2e.rs"]
mod fold_ingest_cross_node_merge_e2e;
#[path = "main/gossip_auth.rs"]
mod gossip_auth;
#[path = "main/gossip_integration.rs"]
mod gossip_integration;
#[path = "main/gossip_offer_clock.rs"]
mod gossip_offer_clock;
#[path = "main/gossip_route.rs"]
mod gossip_route;
#[path = "main/guest_lender_routing.rs"]
mod guest_lender_routing;
#[path = "main/guest_over_iroh_e2e.rs"]
mod guest_over_iroh_e2e;
#[path = "main/ingest_origin_e2e.rs"]
mod ingest_origin_e2e;
#[path = "main/injection_order.rs"]
mod injection_order;
#[path = "main/internal_gate_e2e.rs"]
mod internal_gate_e2e;
#[path = "main/iroh_dialer_admission_e2e.rs"]
mod iroh_dialer_admission_e2e;
#[path = "main/iroh_transport_e2e.rs"]
mod iroh_transport_e2e;
#[path = "main/iroh_verified_principal_e2e.rs"]
mod iroh_verified_principal_e2e;
#[path = "main/join_handshake.rs"]
mod join_handshake;
#[path = "main/join_key_persistence.rs"]
mod join_key_persistence;
#[path = "main/join_parks_not_leaves.rs"]
mod join_parks_not_leaves;
#[path = "main/join_route.rs"]
mod join_route;
#[path = "main/knowledge_fanout.rs"]
mod knowledge_fanout;
#[path = "main/knowledge_fanout_attribution_e2e.rs"]
mod knowledge_fanout_attribution_e2e;
#[path = "main/knowledge_fanout_e2e.rs"]
mod knowledge_fanout_e2e;
#[path = "main/knowledge_served_e2e.rs"]
mod knowledge_served_e2e;
#[path = "main/landscape_digest_http_e2e.rs"]
mod landscape_digest_http_e2e;
#[path = "main/lc_surface_e2e.rs"]
mod lc_surface_e2e;
#[path = "main/load_awareness_e2e.rs"]
mod load_awareness_e2e;
// sovereign-tools' leaf-backed `LocalCorpusPort` double, shared rather
// than copied: the local-corpus routes drive the same manager over it.
#[path = "../../sovereign-tools/tests/main/local_corpus_port_double.rs"]
mod local_corpus_port_double;
#[path = "main/local_only_boot.rs"]
mod local_only_boot;
#[path = "main/local_only_corpus_locality.rs"]
mod local_only_corpus_locality;
#[path = "main/loopback_parity.rs"]
mod loopback_parity;
#[path = "main/manifest_fanout_concurrency.rs"]
mod manifest_fanout_concurrency;
#[path = "main/mcp_one_home.rs"]
mod mcp_one_home;
#[path = "main/membership_port.rs"]
mod membership_port;
#[path = "main/mesh_switch.rs"]
mod mesh_switch;
#[path = "main/meshapp_parcels_e2e.rs"]
mod meshapp_parcels_e2e;
#[path = "main/meshapp_surface_e2e.rs"]
mod meshapp_surface_e2e;
#[path = "main/models_http_e2e.rs"]
mod models_http_e2e;
#[path = "main/ner_one_load_census.rs"]
mod ner_one_load_census;
#[path = "main/no_engine_census.rs"]
mod no_engine_census;
#[path = "main/node_id_persistence.rs"]
mod node_id_persistence;
#[path = "main/openai_finish_reason.rs"]
mod openai_finish_reason;
#[path = "main/openai_wire_fidelity.rs"]
mod openai_wire_fidelity;
#[path = "main/peer_preference_manifest.rs"]
mod peer_preference_manifest;
#[path = "main/peer_tally_status_e2e.rs"]
mod peer_tally_status_e2e;
#[path = "main/peer_turn_reaches_serve_e2e.rs"]
mod peer_turn_reaches_serve_e2e;
#[path = "main/plaintext_join_over_iroh_e2e.rs"]
mod plaintext_join_over_iroh_e2e;
#[path = "main/port_config.rs"]
mod port_config;
#[path = "main/rail_kv_pump_loop_tests.rs"]
mod rail_kv_pump_loop_tests;
#[path = "main/rails_base_config.rs"]
mod rails_base_config;
#[path = "main/reading_http_e2e.rs"]
mod reading_http_e2e;
#[path = "main/recipe_surface_e2e.rs"]
mod recipe_surface_e2e;
#[path = "main/research_surface_e2e.rs"]
mod research_surface_e2e;
#[path = "main/responses_adapter_e2e.rs"]
mod responses_adapter_e2e;
#[path = "main/ring_append_nudges_sync.rs"]
mod ring_append_nudges_sync;
#[path = "main/ring_live_non_durable.rs"]
mod ring_live_non_durable;
#[path = "main/ring_return_syncs.rs"]
mod ring_return_syncs;
#[path = "main/ring_sync_by_roster.rs"]
mod ring_sync_by_roster;
#[path = "main/ring_sync_loop_tests.rs"]
mod ring_sync_loop_tests;
#[path = "main/rotate_pre_split_guard.rs"]
mod rotate_pre_split_guard;
#[path = "main/scheduler_decision_records.rs"]
mod scheduler_decision_records;
#[path = "main/status_answers_from_serve.rs"]
mod status_answers_from_serve;
#[path = "main/storage_budget_route.rs"]
mod storage_budget_route;
#[path = "main/storage_snapshot_e2e.rs"]
mod storage_snapshot_e2e;
#[path = "main/store_seed_double.rs"]
mod store_seed_double;
#[path = "main/svrn_alone_names_ingest_absent_e2e.rs"]
mod svrn_alone_names_ingest_absent_e2e;
#[path = "main/svrn_memory_without_code_e2e.rs"]
mod svrn_memory_without_code_e2e;
#[path = "main/throughput_ledger_emission.rs"]
mod throughput_ledger_emission;
#[path = "main/try_resume_first_gossip.rs"]
mod try_resume_first_gossip;
#[path = "main/turn_reshape_fidelity.rs"]
mod turn_reshape_fidelity;
#[path = "main/turn_surface.rs"]
mod turn_surface;
#[path = "main/wikipedia_fetch_e2e.rs"]
mod wikipedia_fetch_e2e;
#[path = "main/wire_view_drift.rs"]
mod wire_view_drift;
#[path = "main/work_drive_census.rs"]
mod work_drive_census;
