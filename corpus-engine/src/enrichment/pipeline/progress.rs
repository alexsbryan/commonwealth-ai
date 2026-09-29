// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `enrich build` progress events and their stdout wire — defined in
//! `sovereign_contracts::daemon_wire::enrich_progress` so a host that reads a
//! build's progress links no engine (pb-ingest-dial-tools).

pub use sovereign_contracts::daemon_wire::enrich_progress::*; // shim: moved by pb-ingest-dial-tools
