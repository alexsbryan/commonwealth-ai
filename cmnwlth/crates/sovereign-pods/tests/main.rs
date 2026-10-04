// SPDX-License-Identifier: AGPL-3.0-or-later
//! The owner↔pod integration tests, one test binary. Moved from
//! sovereign-mesh's tests/main (pb-mesh-dissolve): they drive this crate's
//! `worker_controller` and `worker_daemon` over the `worker_pod` seam
//! (`sovereign_contracts::worker_pod`), and named sovereign-mesh only for its
//! re-export of that seam. `#[path]` keeps the sources in `tests/main/`, which
//! cargo does not scan for targets. `pod_worker_bin.rs` stays its own binary.

#[path = "main/local_pod_smoke.rs"]
mod local_pod_smoke;
#[path = "main/worker_e2e.rs"]
mod worker_e2e;
