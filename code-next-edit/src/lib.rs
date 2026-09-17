// SPDX-License-Identifier: AGPL-3.0-or-later
//! # code-next-edit
//!
//! The code-intel package's next-edit crate (docs/CODE_TOOLING_BOUNDARY.md §2;
//! the package's crates are the root siblings beside this one — `corpus-engine-notes`
//! and the rest — until the doc's Phase 5 rename). It is the package's TENTH
//! crate, and the first its §2 table does not name: `quality/DOMAINS.md` §4 calls
//! next-edit "Workbench's clearest lost child" (spec `sovereign/docs/NEXT_EDIT.md`).
//!
//! Nothing has moved in yet: the crate is a stub so the boundary has a crate to
//! name, as `sovereign-daemon` and `sovereign-scheduler` were. `dm-next-edit-move`
//! performs the split decided here.
//!
//! ## The placement decision
//!
//! The workbench cluster in `sovereign-api` is six files. Five are the pure
//! next-edit half and move here:
//!
//! - `next_edit.rs` (983) — the rule lane: pure induction, no inference/state
//! - `next_edit_model.rs` (2462) — the model lane's pure half: consult gate,
//!   needle derivation, region selection, prompt shaping, parse, diff
//! - `next_edit_symbols.rs` (714) — the symbol lane: call-site navigation
//! - `next_edit_syntax.rs` (342) — tree-sitter site filtering
//! - `next_edit_journal.rs` (389) — the journal policy: build the record, append
//!   it off the request path, accept the outcome report
//!
//! The sixth, `routes_edit_predictions.rs` (1459), is an axum route shell — it
//! parses a request, calls one context and shapes a reply — so DAEMON_CORE.md
//! §4.1's placement test puts it in `sovereign-daemon`, not here.
//!
//! ## The reaches, resolved
//!
//! - `sovereign_core::types::{JournalLine, NextEditEpisode}` are
//!   `sovereign-contracts` re-exports; the moved files repoint to
//!   `sovereign_contracts::types` directly (the same Phase 1 repoint
//!   CODE_TOOLING_BOUNDARY.md §2 does for every package crate).
//! - `next_edit_journal.rs`'s unused `State<AppState>` extractor was already
//!   deleted (`REVIEW-build-api-host-decouple`, `0cb82a426`), so the module
//!   carries no host state.
//! - The FIM handle is already `sovereign_core::traits::LocalInferenceService`
//!   and `oicp_types::{FimCompletionRequest, EditSlotStatus, LocalInferenceError}`;
//!   the route shell takes the provider the daemon holds, so no new port type is
//!   minted.
//!
//! ## What `dm-next-edit-move` must still resolve
//!
//! **One reach is package-illegal.** `next_edit_symbols.rs` (`:197`, `:237`) and
//! `next_edit_syntax.rs` (`:111`) call
//! `corpus_engine::extractors::code::language_for_extension`, the tree-sitter
//! grammar registry. The code-intel package may name only its own crates plus the
//! shared leaves `oicp-types` / `sovereign-contracts` / `oicp-client`, and the
//! campaign forbids a new `[[exception]]` (quality/DOMAINS.toml, workbench cluster
//! note). `corpus-engine` is neither, so a `corpus-engine` dependency is a
//! `PackageEdge` boundary-gate refuses. The move must resolve this — carve the
//! registry into a leaf both sides name, or reach it through a port — before those
//! two files land. The package-legal reaches are `corpus-engine-scip` (the SCIP
//! graph, `next_edit_symbols.rs:56`) and `corpus-engine-notes`.
//!
//! **`next_edit_journal.rs` carries one route shell.** Its `OutcomeWire` +
//! `edit_prediction_outcome` handler is registered at `server.rs:175`; by DC §4.1's
//! same placement test it is route-shell-shaped. The move either splits it (the
//! handler with the daemon's surface, the pure `episode_from` / `record` /
//! `record_next_edit` policy here) or takes an `axum` dependency to carry it whole.
