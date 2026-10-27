// SPDX-License-Identifier: AGPL-3.0-or-later
pub use sovereign_contracts::gguf_meta;
pub use sovereign_contracts::gguf_validator;
pub mod gpu_probe;
// `remote.rs` was extracted wholesale to the `oicp-client` crate (pure-HTTP
// OICP client, no llama.cpp). Re-exported here so `sovereign_inference::remote::*`
// (RemoteApiProvider, SplitInferenceProvider, …) is unchanged for all callers.
/// OICP v0.6 §2.17: a writer's steps of model work go first.
pub mod priority_gate;
pub mod remote {
    pub use oicp_client::*;
}

pub use gguf_validator::{validate_gguf, GgufExpectation, GgufValidationError};
