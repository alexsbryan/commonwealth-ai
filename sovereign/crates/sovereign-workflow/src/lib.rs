// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign-workflow` — a typed dataflow substrate: **Step · Artifact · Runner**.
//!
//! A `Workflow` is a graph (authored as TOML, edges auto-derived from
//! `{step.key}` references) whose nodes are `Step`s — a deterministic
//! `transform`, a `model:` call (daemon-routed inference), or a `tool:`/`mcp:`
//! call. The single-process `Runner` topologically orders the steps and runs
//! them per source item, threading `Artifact`s between them.
//!
//! This is P0+P1 of the substrate in `docs/specs/WORKFLOW_SUBSTRATE.md`: the
//! smallest real instance, proving the abstraction on a user-authored workflow
//! before any unification. Durable/distributed execution, the content-addressed
//! artifact cache, and the inference-resource scheduler are P2+.
//!
//! The crate is **core-only**: it defines the shapes and consumes
//! `sovereign-core`'s `Tool` / `InferenceProvider` traits; the concrete provider
//! + tools are injected by the caller (see `StepRegistry::new`).

pub mod cache;
pub mod kind;
pub mod model;
pub mod runner;
pub mod steps;
pub mod template;

pub use cache::{ArtifactCache, FileArtifactCache, NoCache};
pub use kind::StepKind;
pub use model::{
    Artifact, ResolvedArgs, ResourceNeed, Resources, Scope, Source, SourceItem, StepDescriptor,
    StepSpec, Workflow,
};
pub use runner::{ItemReport, RunReport, Runner};
pub use steps::{Step, StepCtx, StepRegistry};

pub use sovereign_core::error::{Error, Result};
