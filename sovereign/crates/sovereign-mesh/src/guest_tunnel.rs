// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest half of an iroh-reached lend moved to `mesh_reach::guest`
//! (pb-reach-guest, phase-b-35); re-exported at its historical path until
//! pb-mesh-dissolve deletes this crate.

pub use mesh_reach::guest::GuestTunnel;
