// SPDX-License-Identifier: AGPL-3.0-or-later
//! Peer-work read port: live claims/observations on one scope.

use serde_json::Value;

/// How a scope string is matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeKind {
    Symbol,
    File,
}

/// TTL-filtered claims and read-time-graded observations. Empty is a real answer.
#[derive(Default, Debug)]
pub struct InFlightView {
    pub claims: Vec<Value>,
    pub observations: Vec<Value>,
}

/// Read access to what peers are working on.
pub trait PeerWork: Send + Sync {
    fn in_flight(&self, scope: &str, kind: ScopeKind, caller_token: Option<&str>) -> InFlightView;
}
