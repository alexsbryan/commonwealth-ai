// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `/v1/mesh/measurements` answer shapes, beside the policy that owns
//! what a measurement is.
//!
//! These two were defined in `sovereign_daemon::mesh_http`, where the route
//! that fills them lives. That is the right place for a ROUTE and the wrong
//! place for a TYPE: the CLI's measurement-travel reader (`svrn mesh bench`
// / `mesh plan`) pins its own wire read against exactly these two — it
//! serialises the route's shape and parses its own reader from the bytes —
//! and a pin cannot reach a type that only exists behind the daemon's link.
//!
//! Why not `sovereign_contracts::daemon_wire`, the usual home for wire
//! shapes: [`MemberMeasurementDto`] carries
//! [`MeasurementRecord`](crate::mesh_measurements::MeasurementRecord),
//! whose home is this crate (`mesh_measurements` owns what may travel and
//! under what key), and a leaf cannot name a package crate. Beside the
//! record is the rule that module itself states — a DTO that closes over a
//! type stays with that type's home. The daemon re-exports both at their
//! historical `mesh_http::` paths, so the route and its tests are unchanged.

use serde::Serialize;

/// One peer's measurement, as `GET /v1/mesh/measurements` returns it.
#[derive(Debug, Serialize)]
pub struct MemberMeasurementDto {
    /// Hex node id of the publisher, resolved from the journal line's `actor`
    /// through the ring roster — not from anything inside the payload, which
    /// the publisher controls. The `actor` is the public key that SIGNED the
    /// line, which is the one field a writer cannot forge for someone else
    /// (ARCH §18.1).
    pub origin_node: String,
    /// Friendly mesh name, as the roster resolved the signing key. Absent only
    /// when this build could not name the node behind an admitted key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_name: Option<String>,
    /// What they measured.
    pub record: crate::mesh_measurements::MeasurementRecord,
}

/// Response body for `GET /v1/mesh/measurements`.
#[derive(Debug, Serialize)]
pub struct MemberMeasurementsResponse {
    /// Peer records, newest first. Excludes this node's own — the CLI already
    /// holds those on disk, and they are the authoritative copy — unless
    /// `include_self` was set on the query.
    pub records: Vec<MemberMeasurementDto>,
    /// Journal lines that were admitted but could not be read as measurements,
    /// usually a peer on an incompatible schema, PLUS everything admission
    /// could not account for at all — an unplaceable signer, a hole in a peer's
    /// sequence, a torn line. Reported rather than swallowed: a reader seeing
    /// `records: []` deserves to know whether the ring is quiet or whether this
    /// answer covers a subset (ARCH §18.3).
    pub unreadable: usize,
}
