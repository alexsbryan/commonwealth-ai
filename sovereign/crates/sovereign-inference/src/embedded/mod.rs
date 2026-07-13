// SPDX-License-Identifier: AGPL-3.0-or-later
//! `embedded` — local llama.cpp inference engine, split by concern (PR5b).
//! Was a single 9669-line file; one slot / concern per submodule,
//! re-exported flat so `crate::embedded::<Item>` paths are unchanged.
#![allow(unused_imports)]

mod control_vector;
mod embed_only;
mod embed_slot;
mod engine;
pub mod ffi_trace;
mod gates;
mod grammar;
mod model_slot;
mod prefix_state;
mod prompt_helpers;
mod rerank_slot;
mod rpc_distribution;
mod rpc_warm_cache;
mod sampler;

pub use embed_only::*;
pub use embed_slot::*;
pub use engine::*;
pub(crate) use gates::*;
pub use grammar::*;
pub use model_slot::*;
pub use prompt_helpers::*;
pub use rerank_slot::*;
pub use rpc_distribution::*;
pub use rpc_warm_cache::*;
pub use sampler::*;
