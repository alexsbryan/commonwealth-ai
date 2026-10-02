// SPDX-License-Identifier: AGPL-3.0-or-later
//! The journal placement measurements travel on.
//!
//! Moved here from `sovereign_mesh::mesh_measurements` by pb-serve-placement
//! (phase-b-22): serve publishes the records, and cw-rails' outbox guard
//! (`commonwealth_state::peer_preferences::RAIL_CARRIED_APP_IDS`) and the
//! mesh's roster derivation name the same journal, so members sync one
//! spelling.

/// The namespace measurements travel under. ONE name, and it is this one.
///
/// It was the mesh KV `app_id` and it is now also the ring-rail namespace
/// (cw-lift 2d) — deliberately the same string, because minting a second
/// spelling for the rail side would be two answers to what this data is
/// called (ARCH §10.6). It is a legal rail namespace as it stands: lowercase,
/// digits and `-`, under 64 characters.
///
/// It IS in `GOSSIP_EXCLUDED_APP_IDS` now, and that is not a privacy
/// judgement — a measurement describes hardware capability, the same class of
/// fact the mesh already gossips in `NodeCapabilities`, and it carries no
/// prompt text, no corpus content and no model size. Records still reach every
/// peer; they reach them on the rail. The exclusion is what stops a peer on an
/// older build re-creating the dead KV namespace here and a reader seeing one
/// measurement arrive twice by two transports.
pub const MEASUREMENTS_APP_ID: &str = "mesh-measurements";
