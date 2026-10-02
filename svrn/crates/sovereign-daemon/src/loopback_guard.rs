// SPDX-License-Identifier: AGPL-3.0-or-later
//! The loopback guard lives in the host kit's server shell
//! (`host_kit::shell::guard`, phase-b pb-shell); every path it was reached
//! by here still resolves through these re-exports.

pub(crate) use host_kit::shell::guard::{enforce_localhost, LoopbackRouter};
pub use host_kit::shell::guard::{loopback_only, LocalOnly};
