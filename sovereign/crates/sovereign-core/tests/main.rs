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

#[path = "main/adversarial_read.rs"]
mod adversarial_read;
#[path = "main/archive_axis_live.rs"]
mod archive_axis_live;
#[path = "main/chunk_provenance_census.rs"]
mod chunk_provenance_census;
#[path = "main/core_tests.rs"]
mod core_tests;
#[path = "main/custody_reds.rs"]
mod custody_reds;
#[path = "main/drb1_r1_reds.rs"]
mod drb1_r1_reds;
#[path = "main/drb1_r2b_reds.rs"]
mod drb1_r2b_reds;
#[path = "main/drb1_r3b_goldens.rs"]
mod drb1_r3b_goldens;
#[path = "main/drb1_t1_admission.rs"]
mod drb1_t1_admission;
#[path = "main/evidence_pool_census.rs"]
mod evidence_pool_census;
#[path = "main/f26_egress_census.rs"]
mod f26_egress_census;
#[path = "main/fr6_decorrelation.rs"]
mod fr6_decorrelation;
#[path = "main/functional.rs"]
mod functional;
#[path = "main/gate_release_census.rs"]
mod gate_release_census;
#[path = "main/golden_align.rs"]
mod golden_align;
#[path = "main/golden_fixtures.rs"]
mod golden_fixtures;
#[path = "main/golden_reframe.rs"]
mod golden_reframe;
#[path = "main/gym_deck.rs"]
mod gym_deck;
#[path = "main/harness.rs"]
mod harness;
#[path = "main/landscape_digest_splice.rs"]
mod landscape_digest_splice;
#[path = "main/lane_reach_through_census.rs"]
mod lane_reach_through_census;
#[path = "main/locator_axis_live.rs"]
mod locator_axis_live;
#[path = "main/memory_compaction_smoke.rs"]
mod memory_compaction_smoke;
#[path = "main/mode_declarations.rs"]
mod mode_declarations;
#[path = "main/oneshot_rag.rs"]
mod oneshot_rag;
#[path = "main/retrieval_ledger.rs"]
mod retrieval_ledger;
#[path = "main/retrieval_pipeline_doc.rs"]
mod retrieval_pipeline_doc;
#[path = "main/retrieval_pipeline_mechanics.rs"]
mod retrieval_pipeline_mechanics;
#[path = "main/router_bootstrap_parity.rs"]
mod router_bootstrap_parity;
#[path = "main/router_cache_fresh.rs"]
mod router_cache_fresh;
#[path = "main/routing_moves.rs"]
mod routing_moves;
#[path = "main/runtime_commission_census.rs"]
mod runtime_commission_census;
#[path = "main/serialization.rs"]
mod serialization;
#[path = "main/turn_tool_census.rs"]
mod turn_tool_census;
#[path = "main/voice_prompt_shape.rs"]
mod voice_prompt_shape;
