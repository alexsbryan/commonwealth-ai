// SPDX-License-Identifier: AGPL-3.0-or-later
//! The servable-model-files reader at its historical path. Serving's part moved
//! to svrn (`sovereign_daemon::state::serving`) and the reader to the contracts
//! leaf (pb-svrn-serving-ports).

pub use sovereign_contracts::rpc_warm::ServableModelFilesReader;
