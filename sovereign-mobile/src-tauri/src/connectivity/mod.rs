// SPDX-License-Identifier: AGPL-3.0-or-later
//! Connectivity state — three distinct, user-actionable states plus
//! the healthy one, surfaced to the UI so each is its own affordance
//! (not one generic "can't connect"). See `MOBILE.md` §1, §6, §7.

pub mod monitor;
pub mod reachability;

// `ConnState` stays addressable as `monitor::ConnState` (used throughout
// the monitor + classify path); only `ConnectivityMonitor` needs hoisting
// to the module root, where `lib.rs` consumes it.
pub use monitor::ConnectivityMonitor;
