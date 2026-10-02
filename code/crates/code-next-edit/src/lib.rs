// SPDX-License-Identifier: AGPL-3.0-or-later
//! # code-next-edit
//!
//! The code-intel package's next-edit crate (docs/CODE_TOOLING_BOUNDARY.md §2;
//! the package's crates are the root siblings beside this one — `corpus-engine-notes`
//! and the rest — until the doc's Phase 5 rename). It is the package's TENTH
//! crate, and the first its §2 table does not name: `quality/DOMAINS.md` §4 calls
//! next-edit "Workbench's clearest lost child" (spec `sovereign/docs/NEXT_EDIT.md`).
//!
//! The five pure workbench modules moved here at domains `dm-next-edit-move`
//! (2026-09-17), out of `sovereign-api`:
//!
//! - [`next_edit`] — the rule lane: pure induction, no inference/state
//! - [`next_edit_model`] — the model lane's pure half: consult gate,
//!   needle derivation, region selection, prompt shaping, parse, diff
//! - [`next_edit_symbols`] — the symbol lane: call-site navigation
//! - [`next_edit_syntax`] — tree-sitter site filtering
//! - [`next_edit_journal`] — the journal policy: build the record, append
//!   it off the request path
//!
//! The sixth workbench file, `routes_edit_predictions.rs`, is an axum route
//! shell — it parses a request, calls one context and shapes a reply — so
//! DAEMON_CORE.md §4.1's placement test puts it in the host, not here. It
//! stays in `sovereign-api` beside the `client_router` that mounts it (and
//! moves to `sovereign-daemon` with that router at `dm-daemon-api-edge`).
//!
//! ## The reaches, resolved
//!
//! - `sovereign_core::types::{JournalLine, NextEditEpisode}` are
//!   `sovereign-contracts` re-exports; the moved files name
//!   `sovereign_contracts::types` directly (the same Phase 1 repoint
//!   CODE_TOOLING_BOUNDARY.md §2 does for every package crate).
//! - The FIM handle is already `sovereign_core::traits::LocalInferenceService`
//!   and `oicp_types::{FimCompletionRequest, EditSlotStatus, LocalInferenceError}`;
//!   the route shell takes the provider the daemon holds, so no new port type is
//!   minted.
//! - The tree-sitter grammar registry is `corpus-engine`'s, and a package crate
//!   may not name it. It arrives through [`grammar::GrammarLookup`] — a value the
//!   host supplies at the one boundary that holds an engine — so the registry
//!   keeps ONE implementation and `.tsx` routing cannot drift (ARCH §8).
//! - The journal's outcome route (`POST /v1/edit_predictions/outcome`) is a host
//!   route shell; it lives with the surface that mounts it and calls
//!   [`next_edit_journal::record_next_edit`].

pub mod grammar;
pub mod next_edit;
pub mod next_edit_journal;
pub mod next_edit_model;
pub mod next_edit_symbols;
pub mod next_edit_syntax;
