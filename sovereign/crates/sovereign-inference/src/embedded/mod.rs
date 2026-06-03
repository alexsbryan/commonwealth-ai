//! `embedded` — local llama.cpp inference engine, split by concern (PR5b).
//! Was a single 9669-line file; one slot / concern per submodule,
//! re-exported flat so `crate::embedded::<Item>` paths are unchanged.
#![allow(unused_imports)]

mod model_slot;
mod embed_slot;
mod rerank_slot;
mod engine;
mod prompt_helpers;
mod sampler;
mod grammar;

pub use model_slot::*;
pub use embed_slot::*;
pub use rerank_slot::*;
pub use engine::*;
pub use prompt_helpers::*;
pub use sampler::*;
pub use grammar::*;
