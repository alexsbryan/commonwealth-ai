// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wall-clock helpers — re-exported from the Tier-0 `sovereign-time` leaf.
//!
//! These existed as ~40 copy-pasted private `fn unix_now()` / `now_secs()` /
//! `now_unix()` / `now_millis()` across the workspace, all computing the same
//! `SystemTime::now().duration_since(UNIX_EPOCH)` and differing only in cast and
//! error handling. The bodies here were byte-identical to `sovereign_time`'s,
//! so this module is now a re-export: there is ONE implementation to reason
//! about (ARCH principle 8). `sovereign_core::time::*` keeps resolving.
//!
//! Pick by the type your caller needs:
//!   - [`unix_now`]      — seconds, `i64` (timestamps stored as signed)
//!   - [`unix_now_u64`]  — seconds, `u64`
//!   - [`unix_millis`]   — milliseconds, `u64`
//!   - [`system_now`]    — the raw `SystemTime`, for APIs that take one

pub use sovereign_time::{system_now, unix_millis, unix_now, unix_now_u64};
