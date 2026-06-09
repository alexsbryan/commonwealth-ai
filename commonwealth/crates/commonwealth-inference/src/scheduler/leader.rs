// SPDX-License-Identifier: AGPL-3.0-or-later
//! Leader election lives in `commonwealth-core::partition` so other
//! replicated-state daemons (e.g. corpus-engine freshness watchers) can
//! reuse it without depending on the inference crate. This module is a
//! thin shim so existing scheduler call-sites compile unchanged.
pub use commonwealth_core::partition::{elect_leader, is_leader};
