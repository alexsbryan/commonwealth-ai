// SPDX-License-Identifier: AGPL-3.0-or-later
//! shim: `SovereignConfig` & co. moved to the `sovereign-contracts` leaf
//! (`sovereign_contracts::config`) to close the corpus-engine boundary edge;
//! this module keeps the historical `corpus_engine::sovereign_config` path.
//! corpus-engine already depends on the leaf, and only reads the config
//! through the shim, so the edge closes without a new dependency.
pub use sovereign_contracts::config::*;
