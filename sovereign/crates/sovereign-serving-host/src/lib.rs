// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ports and the knot.
//!
//! Opens connections, holds the HTTP surface, and receives every candidate
//! through a port. Receives `peer_inference`, `inference_adapter`,
//! `oicp_synthesis`, `guest_lender`, `pinned_worker_source`, `entry_endpoint`
//! and `sovereign-api`'s `admission` (`sovereign/SERVING_BOUNDARY.md` "The two
//! tiers").

pub mod slot_select;
