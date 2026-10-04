// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pure half of `corpus-engine`'s meta-atlas substrate.
//!
//! `REVIEW-build-understanding-crate-tree` decided the module tree; this is
//! the path-preserving prefix the `dm-understanding-pure-*` batch rows fill.
//! The pure children (`classifier`, the `bridge` fold/signal/adjudication
//! logic) live here; the host children (`builder`, `index`, and the bridge's
//! `build`/`edges`/`lookup` I/O) stay in the engine's shell and are
//! re-exported at their historical paths.
//!
//! The per-corpus stability tag and the anchor injection stay with Ingest.

pub mod bridge;
pub mod classifier;
