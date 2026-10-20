// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a call takes with it to a thread it hands work to: the observation
//! sink ([`crate::engine_observe`]) and the request's priority
//! ([`crate::request_priority`]). Each lives in a thread-local, so a closure
//! run by `spawn_blocking` sees neither unless it is carried; this is the one
//! place a crossing names them.

/// Wrap a closure that will run on another thread so it records into the
/// call's sink and serves the call's request.
pub fn carry<R>(f: impl FnOnce() -> R) -> impl FnOnce() -> R {
    crate::request_priority::carry(crate::engine_observe::carry(f))
}
