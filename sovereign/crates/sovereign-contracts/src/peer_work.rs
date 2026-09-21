// SPDX-License-Identifier: AGPL-3.0-or-later
//! Peer-work read port: live claims/observations on one scope.

use serde_json::Value;

/// How a scope string is matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    /// Match SCIP symbol ids and explicit claims.
    Symbol,
    /// Match file paths, prefix-wise.
    File,
}

/// TTL-filtered claims and read-time-graded observations. Empty is a real answer.
#[derive(Default, Debug)]
pub struct InFlightView {
    /// Live explicit claims, TTL-filtered.
    pub claims: Vec<Value>,
    /// Watcher observations, graded at read time.
    pub observations: Vec<Value>,
}

/// Read access to what peers are working on.
pub trait PeerWork: Send + Sync {
    /// Claims and observations overlapping `scope`. Empty is a real answer.
    fn in_flight(&self, scope: &str, kind: ScopeKind, caller_token: Option<&str>) -> InFlightView;
}
