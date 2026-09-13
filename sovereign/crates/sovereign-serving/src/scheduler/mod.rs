// SPDX-License-Identifier: AGPL-3.0-or-later
//! Mesh scheduling — the LIVE surface only.
//!
//! 2026-06-10 rationalization: this directory used to carry a second,
//! never-invoked scheduler generation (`adaptive::InferenceScheduler`,
//! `plan_builder`/`layer_assignment` shard planning, `portfolio`,
//! `usage_predictor`, a duplicate `leader` election, and a duplicate
//! OICP scorer in `oicp_select`). All of it had zero runtime callers
//! and was deleted — git history is the archive. The live equivalents:
//! per-request OICP scoring lives in `oicp-types`
//! (`score_with_adjustments`) consumed by sovereign-mesh and
//! sovereign-inference; distributed model placement lives in
//! `sovereign-inference::embedded::rpc_distribution`; leader election
//! lives in `commonwealth_core::partition::elect_leader`.
//!
//! What remains here is the collaborative-ingestion planner, which IS
//! load-bearing (driven by `commonwealth-api`'s corpus_collaborate
//! routes).
pub mod knowledge_assignment;
