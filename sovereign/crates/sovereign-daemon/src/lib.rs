// SPDX-License-Identifier: AGPL-3.0-or-later
//! The node's host crate: assembly, the surface shells, the edge and the
//! adapters.
//!
//! `sovereign-daemon` is the host library at tier 5 in the `mesh-api` layer
//! beside `sovereign-mesh` (DAEMON_CORE.md §4.1). Not `sovereign-cli-daemon`:
//! at tier 6, `sovereign-cli-llm` and `sovereign-cli-dev` would depend on a
//! sibling host, which the layer map forbids, and `sovereign-mesh`'s own tests
//! could not reach it.
//!
//! Its modules follow DAEMON_CORE.md §3's classes, so each phase lands inside
//! one module family:
//!
//! - `assemble` — constructs the node's parts in dependency order
//! - `edge` — who may call, and a caller's dialect translated into the node's
//!   protocol (`client_auth`, `loopback_guard`, `local_only`, `headers`,
//!   `frontdoor`)
//! - `turn` — the turn surface
//! - `jobs` — the job surface and its drivers
//! - `node` — the node's own state
//! - `resources` — the corpus engine handle, the storage budget and the
//!   foreground signal
//! - `store` — the store surface
//! - `adapters` — one context's port implemented with another's capability
//!   (DAEMON_CORE.md §4.3)
//!
//! Nothing has moved in yet: the crate is a stub so the boundary has a crate
//! to name. Its consumers are `sovereign-cli-daemon`, `sovereign-cli-llm` for
//! the `MeshAdmin` variant and worker mode until Phase 6, and its own tests
//! (DAEMON_CORE.md §4.1).
