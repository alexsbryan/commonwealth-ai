// SPDX-License-Identifier: AGPL-3.0-or-later
//! Serving policy — the [cmnwlth] serving cluster's name for the shared
//! serving-policy arithmetic.
//!
//! Both modules moved to the `serving-policy-core` vocabulary leaf
//! (FIVE_PROGRAMS fp-17), which the daemon names directly; this crate
//! re-exports them at their historical paths so `sovereign-inference` and
//! `sovereign-serving-host` (the cluster's own consumers) are unchanged.
//! A re-export, never a twin — ARCH §10.6.

pub use serving_policy_core::fair_sched;
pub use serving_policy_core::pipeline_aliases;
