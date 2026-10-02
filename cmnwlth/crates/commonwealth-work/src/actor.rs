// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ActorKey`] — who signed an act. The type lives in `kernel-types` since
//! pb-work-doors (FIVE_PROGRAMS §12 3a rung 2) and is re-exported here at its
//! historical path; reading it off an admitted op stays with the rail.

use commonwealth_rail_core::AdmittedOp;

pub use kernel_types::actor::{ActorKey, InvalidActorKey};

/// The key of whoever signed this admitted op.
///
/// Admission has already verified the signature and placed the key in the
/// roster, so this only fails on a build that admitted something this one
/// cannot spell — reported by the caller as an unreadable line, never
/// panicked on.
pub fn of_op(op: &AdmittedOp) -> Result<ActorKey, InvalidActorKey> {
    ActorKey::parse(&op.actor)
}
