//! Voice-judge wrapper.
//!
//! Re-exports the canonical request builder + score type from
//! `sovereign_core::pipeline::judge` (added in Phase 3.2 of the
//! situated-team plan so the runtime can call the same judge the
//! Tier-B harness does). The single source of truth for the rubric
//! shape, the JSON schema, and the parser lives in sovereign-core
//! now — this module exists as a thin re-export so the existing
//! `voice_eval` import paths keep working unchanged.

#![allow(dead_code)]

pub use sovereign_core::pipeline::judge::{
    parse_judge_score, voice_judge_request, JudgeScore,
};
