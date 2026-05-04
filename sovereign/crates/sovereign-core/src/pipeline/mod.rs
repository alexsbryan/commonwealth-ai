//! Situated team-pipeline (Router → Retriever → Curator → Drafter
//! → Presenter).
//!
//! The Fast slot does heavy assembly (route, retrieve, curate,
//! plan a per-section budget) so the Primary slot draws inside a
//! tight, structured task. A Presenter pass after the draft shapes
//! the voice. The intent — per the situated-team plan in
//! `/Users/alexsbryan/.claude/plans/there-s-a-fast-slot-delightful-peach.md` —
//! is that bounded expression on a curated package is the task
//! open-weight Primary models are actually good at, and that
//! blowouts become rare planner-quality regression signals rather
//! than runtime hiccups the user has to absorb.
//!
//! This crate-internal module ships in phases:
//!
//! - **Phase 1** (foundation): typed `StreamFrame` / `FinishReason`
//!   plumbing + `NarrationPhase` stage-frame variants. Lives in
//!   `types.rs` / `traits.rs` / `embedded.rs` — not here.
//! - **Phase 2** (this module's first wave): `Curator` stage. Owns
//!   `CuratedPackage`, the curate prompt, and the
//!   `should_curate` bypass policy.
//! - **Phase 3** (next): `Presenter` stage. Voice-shaping pass over
//!   the Drafter's raw output, with an async voice judge.
//! - **Phase 4** (last): runtime wire-up + kill-switch
//!   (`SOVEREIGN_TEAM_PIPELINE=0` reverts to the legacy chat path).
//!
//! Until Phase 4, nothing here is invoked from `Runtime`. The seams
//! are in place so each stage can be tuned in isolation against the
//! curator-unit / presenter-delta `voice_eval` modes the plan
//! describes (§Iteration loops).

pub mod curator;
pub mod judge;
pub mod presenter;
pub mod prompts;
pub mod runner;
pub mod stages;

pub use curator::{curate, should_curate};
pub use judge::{
    parse_judge_score, should_judge, spawn_voice_judge, voice_judge_request, JudgeScore,
};
pub use presenter::{present, PresentedOutput};
pub use runner::{
    is_team_pipeline_enabled, run_team_pipeline, NarrationSink, NoopNarrationSink,
    RoutingEventNarrationSink, TeamPipelineInputs, TeamPipelineOutput,
    DEFAULT_TEAM_PIPELINE_MAX_TOKENS, TEAM_PIPELINE_ENV_VAR,
};
pub use stages::{
    CuratedPackage, DraftBudget, RetrievedChunk, SkeletonSection, Sufficiency,
};
