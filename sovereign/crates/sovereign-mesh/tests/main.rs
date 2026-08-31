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

#[path = "main/common/mod.rs"]
mod common;
#[path = "main/canonical_pull_e2e.rs"]
mod canonical_pull_e2e;
#[path = "main/capabilities_published.rs"]
mod capabilities_published;
#[path = "main/chat_completion_e2e.rs"]
mod chat_completion_e2e;
#[path = "main/client_exposure.rs"]
mod client_exposure;
#[path = "main/corpus_sharing_over_iroh_e2e.rs"]
mod corpus_sharing_over_iroh_e2e;
#[path = "main/corpus_watch_http_e2e.rs"]
mod corpus_watch_http_e2e;
#[path = "main/daemon_variant_census.rs"]
mod daemon_variant_census;
#[path = "main/daemon_wiring.rs"]
mod daemon_wiring;
#[path = "main/dst_scenarios.rs"]
mod dst_scenarios;
#[path = "main/embeddings_e2e.rs"]
mod embeddings_e2e;
#[path = "main/emitter_origin_concurrency.rs"]
mod emitter_origin_concurrency;
#[path = "main/finish_reason_streaming.rs"]
mod finish_reason_streaming;
#[path = "main/gossip_auth.rs"]
mod gossip_auth;
#[path = "main/gossip_integration.rs"]
mod gossip_integration;
#[path = "main/gossip_push_surfacing.rs"]
mod gossip_push_surfacing;
#[path = "main/guest_lender_routing.rs"]
mod guest_lender_routing;
#[path = "main/guest_over_iroh_e2e.rs"]
mod guest_over_iroh_e2e;
#[path = "main/injection_order.rs"]
mod injection_order;
#[path = "main/iroh_dialer_admission_e2e.rs"]
mod iroh_dialer_admission_e2e;
#[path = "main/iroh_transport_e2e.rs"]
mod iroh_transport_e2e;
#[path = "main/join_handshake.rs"]
mod join_handshake;
#[path = "main/join_key_persistence.rs"]
mod join_key_persistence;
#[path = "main/join_parks_not_leaves.rs"]
mod join_parks_not_leaves;
#[path = "main/knowledge_client_unavailability.rs"]
mod knowledge_client_unavailability;
#[path = "main/knowledge_fanout_e2e.rs"]
mod knowledge_fanout_e2e;
#[path = "main/knowledge_served_e2e.rs"]
mod knowledge_served_e2e;
#[path = "main/landscape_digest_http_e2e.rs"]
mod landscape_digest_http_e2e;
#[path = "main/load_awareness_e2e.rs"]
mod load_awareness_e2e;
#[path = "main/local_only_corpus_locality.rs"]
mod local_only_corpus_locality;
#[path = "main/local_pod_smoke.rs"]
mod local_pod_smoke;
#[path = "main/loopback_parity.rs"]
mod loopback_parity;
#[path = "main/manifest_fanout_concurrency.rs"]
mod manifest_fanout_concurrency;
#[path = "main/mesh_sim_scoreboard.rs"]
mod mesh_sim_scoreboard;
#[path = "main/mesh_switch.rs"]
mod mesh_switch;
#[path = "main/models_http_e2e.rs"]
mod models_http_e2e;
#[path = "main/node_id_persistence.rs"]
mod node_id_persistence;
#[path = "main/openai_finish_reason.rs"]
mod openai_finish_reason;
#[path = "main/pattern_observation_e2e.rs"]
mod pattern_observation_e2e;
#[path = "main/peer_preference_manifest.rs"]
mod peer_preference_manifest;
#[path = "main/peer_tally_status_e2e.rs"]
mod peer_tally_status_e2e;
#[path = "main/plaintext_join_over_iroh_e2e.rs"]
mod plaintext_join_over_iroh_e2e;
#[path = "main/port_config.rs"]
mod port_config;
#[path = "main/reading_http_e2e.rs"]
mod reading_http_e2e;
#[path = "main/responses_adapter_e2e.rs"]
mod responses_adapter_e2e;
#[path = "main/rotate_pre_split_guard.rs"]
mod rotate_pre_split_guard;
#[path = "main/scheduler_decision_records.rs"]
mod scheduler_decision_records;
#[path = "main/scheduler_replay_agreement.rs"]
mod scheduler_replay_agreement;
#[path = "main/spec_gate_e2e.rs"]
mod spec_gate_e2e;
#[path = "main/storage_snapshot_e2e.rs"]
mod storage_snapshot_e2e;
#[path = "main/throughput_ledger_emission.rs"]
mod throughput_ledger_emission;
#[path = "main/try_resume_first_gossip.rs"]
mod try_resume_first_gossip;
#[path = "main/turn_surface.rs"]
mod turn_surface;
#[path = "main/worker_e2e.rs"]
mod worker_e2e;
