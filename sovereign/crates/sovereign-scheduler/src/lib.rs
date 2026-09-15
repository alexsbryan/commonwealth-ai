// SPDX-License-Identifier: AGPL-3.0-or-later
//! Arithmetic over the published language.
//!
//! The serving package's ranker: the ranking decision reads no clock and does
//! no I/O. Receives the scheduler half of `sovereign-mesh` —
//! `scheduler_core`, `oicp_select`, `predicted_time`, `tier`, `decision_log`,
//! `decision_replay`, `decision_trace`, `slot_aliases`,
//! `yield_backoff` — while the recorder sink, the local slot pick and the
//! throughput stream observer stay with `sovereign-serving-host`
//! (`sovereign/SERVING_BOUNDARY.md` "The two tiers").

pub mod decision_log;
pub mod decision_replay;
pub mod decision_trace;
pub mod oicp_select;
pub mod predicted_time;
pub mod scheduler_core;
pub mod slot_aliases;
pub mod tier;
pub mod yield_backoff;
