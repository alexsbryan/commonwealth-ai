// SPDX-License-Identifier: AGPL-3.0-or-later
//! # sovereign-compute — the supervised compute-child process boundary
//!
//! `DISTRIBUTED_PILOT_READINESS.md` P1: inference compute runs in a
//! supervised **child process**, so a ggml `SIGABRT` (worker loss,
//! version mismatch, OOM) kills only the child — the daemon keeps
//! gossip, `/status`, the client API, and the desktop bridge alive and
//! observes the child's exit as an *event* it can re-plan around.
//!
//! This crate is the runtime-layer home for that boundary. It was seeded
//! by extracting the child-process supervisor out of `sovereign-desktop`
//! (which daemon crates cannot depend on) so both the desktop daemon
//! supervisor and the daemon's compute-child manager share one, tested
//! supervision state machine.
//!
//! ## Modules
//! - [`supervisor`] — spawn / heartbeat / backoff / crash-loop-budget /
//!   crash-log state machine, publishing every transition over a
//!   `broadcast` channel. Reused verbatim by the desktop.
//! - [`wire`] — the native lossless wire contract (route constants, body
//!   types, NDJSON codec, error envelope).
//! - [`server`] — the child's axum router over an `Arc<dyn
//!   InferenceProvider>`.
//! - [`client`] — [`client::ComputeChildClient`], the daemon-side typed
//!   HTTP client for a child.
//! - [`child_main`] — the child process entrypoint (`--compute-child`),
//!   reached by re-executing the daemon binary.

pub mod child;
pub mod child_main;
pub mod client;
pub mod distribution;
pub mod manager;
pub mod server;
pub mod supervisor;
pub mod wire;
