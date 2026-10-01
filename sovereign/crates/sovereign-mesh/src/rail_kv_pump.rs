// SPDX-License-Identifier: AGPL-3.0-or-later
//! The two plane seal arms moved to `commonwealth_rails::plane_seal`
//! (pb-mesh-exit-mesh): cw-rails seals `mesh-measurements` and `work` over its
//! own rail on its kv pump's tick, and the svrn daemon spawns no pump. The
//! namespace vocabulary stays reachable at its historical path until
//! pb-mesh-dissolve.
pub use commonwealth_rails::plane_seal::{
    projector_for, Projector, MEASUREMENTS_NAMESPACE, SEAL_AFTER_OWN_OPS, WORK_NAMESPACE,
};
