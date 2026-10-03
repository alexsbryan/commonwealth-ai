// SPDX-License-Identifier: AGPL-3.0-or-later
//! The tests that assert a daemon route answered by serve's router or
//! adapter, moved from sovereign-daemon's test tree by
//! pb-serve-ranks-tests-stock (phase-b-72): this distribution is the one root
//! that links both halves. One binary, so cargo links the daemon once.
//!
//! `#[path]` keeps the sources in `tests/serve_routes/`, a directory cargo
//! does not scan for targets. Serve's library is named by
//! sovereign-serving-host's own paths; the daemon's seed records come from
//! `sovereign_daemon::double`, so nothing here names commonwealth-core.

/// What the moved tests reached as `crate::common` in the daemon's tree.
mod common {
    pub use sovereign_contracts::double::TestProvider;
    pub use sovereign_daemon::double::*;
}

#[path = "serve_routes/chat_completion_e2e.rs"]
mod chat_completion_e2e;
#[path = "serve_routes/guest_lender_routing.rs"]
mod guest_lender_routing;
#[path = "serve_routes/load_awareness_e2e.rs"]
mod load_awareness_e2e;
#[path = "serve_routes/peer_preference_manifest.rs"]
mod peer_preference_manifest;
#[path = "serve_routes/peer_tally_status_e2e.rs"]
mod peer_tally_status_e2e;
#[path = "serve_routes/peer_turn_reaches_serve_e2e.rs"]
mod peer_turn_reaches_serve_e2e;
#[path = "serve_routes/status_answers_from_serve.rs"]
mod status_answers_from_serve;
