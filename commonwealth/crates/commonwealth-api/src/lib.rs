// SPDX-License-Identifier: AGPL-3.0-or-later
//! Commonwealth HTTP API — the `:9741` front door.
//!
//! Panic-ratchet (tech-debt PR4): production code in this crate is held
//! to `clippy::unwrap_used` / `clippy::expect_used`. A panic in a request
//! handler surfaces to the caller as an opaque 500, so prefer `?` / typed
//! errors / graceful fallbacks (`unwrap_or_default`, poison-recovery via
//! `into_inner`, `let … else`) over `unwrap()` / `expect()`. Genuine
//! infallible-by-construction sites carry a local `#[allow(...)]` with a
//! rationale. Test code is exempt via the workspace `clippy.toml`
//! (`allow-{unwrap,expect}-in-tests`).
//!
//! Soft `warn`, not `deny` / CI gate — surfaces new production
//! `unwrap()`/`expect()` in `cargo clippy` output without blocking the
//! build. Clippy-only; does NOT affect `cargo check`/`cargo build`. The
//! production surface was clean (0 sites) as of the PR4 sweep.
#![warn(clippy::unwrap_used, clippy::expect_used)]

pub mod admission;
pub mod auto_recover;
pub mod client_auth;
pub mod frontdoor;
pub mod headers;
pub mod middleware;
pub mod next_edit;
pub mod next_edit_journal;
pub mod next_edit_model;
pub mod next_edit_symbols;
pub mod next_edit_syntax;
pub mod openai_types;
pub mod principal;
pub mod reshaping;
pub mod responses_types;
pub mod routes_app_internal;
pub mod routes_apps;
pub mod routes_completions;
pub mod routes_edit_predictions;
pub mod routes_inference;
pub mod routes_internal;
pub mod routes_knowledge;
pub mod routes_oicp;
pub mod routes_oicp_ingest;
pub mod routes_ollama;
pub mod routes_responses;
pub mod routes_status;
pub mod server;
pub mod state;
pub mod yield_hook;

pub use commonwealth_core::{Error, Result};
