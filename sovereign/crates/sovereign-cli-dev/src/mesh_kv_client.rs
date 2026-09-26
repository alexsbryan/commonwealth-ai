// SPDX-License-Identifier: AGPL-3.0-or-later
//! The work atlas's store — dialed at the mesh's rails daemon (`cw-rails`)
//! through the ONE sync `ReplicatedKv` client the daemon dials with,
//! [`sovereign_turn_client::rails_kv::RailsKv`] (pb-atlas-kv, phase-b-7).
//!
//! Until pb-atlas-kv this module was that client's twin: a
//! `reqwest::blocking` client on the daemon's `/v1/mesh/kv` proxy, which
//! panicked when built inside the async `svrn tools` path. A second process
//! never opens the mesh's store, it dials the process that owns it (fp-33).
//!
//! Absence is reported, never defaulted (principle 6): a cw-rails that is down
//! surfaces on the first call as `ReplicatedKvError::Backend` naming the URL.
//! Nothing here brings cw-rails up.

use sovereign_contracts::setup_config::SetupConfig;
use sovereign_turn_client::rails_kv::{resolve_rails_base, RailsKv};

/// The `RailsKv` at the base `[daemon] rails_base` names, through that key's
/// one reader. Construction checks no presence and cannot fail.
pub(crate) fn atlas_kv() -> RailsKv {
    let daemon = match SetupConfig::load() {
        Ok(c) => c.daemon,
        Err(e) => {
            tracing::warn!(error = %e, "work atlas: no setup config; the rails base is the default");
            SetupConfig::unconfigured().daemon
        }
    };
    RailsKv::new(resolve_rails_base(&daemon))
}
