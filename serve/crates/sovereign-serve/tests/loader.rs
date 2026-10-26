// SPDX-License-Identifier: AGPL-3.0-or-later
//! The loader's process-boundary tests: compute children, run against serve's own
//! binary. They pinned in-daemon placement until pb-serve-distributes moved
//! the loader into serve's crates; their assertions are unchanged.

#[path = "loader/compute_child_e2e.rs"]
mod compute_child_e2e;
