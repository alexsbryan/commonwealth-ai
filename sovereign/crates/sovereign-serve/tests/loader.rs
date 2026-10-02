// SPDX-License-Identifier: AGPL-3.0-or-later
//! The loader's process-boundary tests: compute children, the distributed
//! primary and the self-manifest refresh over it, run against serve's own
//! binary. They pinned in-daemon placement until pb-serve-distributes moved
//! the loader into serve's crates; their assertions are unchanged.

#[path = "loader/compute_child_e2e.rs"]
mod compute_child_e2e;
#[path = "loader/distributed_primary_respawn_e2e.rs"]
mod distributed_primary_respawn_e2e;
#[path = "loader/named_model_routes_after_child_serves_e2e.rs"]
mod named_model_routes_after_child_serves_e2e;
