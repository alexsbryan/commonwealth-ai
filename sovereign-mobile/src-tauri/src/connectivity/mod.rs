//! Connectivity state — three distinct, user-actionable states plus
//! the healthy one, surfaced to the UI so each is its own affordance
//! (not one generic "can't connect"). See `MOBILE.md` §1, §6, §7.

pub mod monitor;
pub mod reachability;

pub use monitor::{ConnState, ConnectivityMonitor};
