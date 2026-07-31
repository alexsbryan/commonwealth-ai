// SPDX-License-Identifier: AGPL-3.0-or-later
//! Signal-source generators. I1 (corpus self-supervision) and I2 (adversarial
//! corruption, the Stream B core) today; I3–I5 drop in here as new
//! [`crate::flywheel::Generator`] impls + one line in
//! [`crate::flywheel::registry`].

pub mod adversarial;
pub mod corpus;
