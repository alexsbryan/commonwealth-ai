// SPDX-License-Identifier: AGPL-3.0-or-later
//! Child-process supervisor for the desktop's Local-mode daemon.
//!
//! The implementation moved to the runtime-layer crate
//! `sovereign-compute` (2026-07-20) so the daemon's compute-child
//! manager can share the same tested supervision state machine — the
//! desktop crate cannot be a dependency of the daemon crates. This
//! module re-exports it so `crate::supervisor::{Supervisor,
//! SupervisorConfig, SupervisorState}` keeps resolving unchanged at every
//! existing call site (`supervisor_setup.rs`, `mobile_host_setup.rs`,
//! `state.rs`, `commands/supervisor_ctl.rs`).

pub use sovereign_compute::supervisor::*;
