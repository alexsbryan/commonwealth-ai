//! Inference-binding shim.
//!
//! `crate::llama::cpp` re-exports the underlying llama.cpp Rust binding.
//! All sovereign-inference consumers (embedded.rs, json_constraint.rs,
//! hardware.rs, smoketest.rs, reranker_standalone.rs) import via this shim —
//! never directly from `llama_cpp_4::…`. That keeps the binding-swap radius
//! small: switching the underlying crate is one `pub use` line here plus the
//! handful of adapter shapes below.
//!
//! ## Why this exists
//!
//! On 2026-05-17 we migrated from `llama-cpp-2 0.1.146` to
//! `llama-cpp-4 0.2.57` to pick up MTP (Multi-Token Prediction, upstream
//! PR #22673 in llama.cpp release b9180) and a newer ggml backend. The
//! 0.2.x line broke:
//!   * Renames: `token_to_piece` → `token_to_str`, `size` → `model_size`,
//!     `chat_template` → `get_chat_template` (with shape change).
//!   * Retired surface: `LlamaContextParams::with_n_seq_max`,
//!     `with_op_offload`, the entire `openai` chat-template-oaicompat
//!     module, and the standalone `list_llama_ggml_backend_devices()`.
//!   * Signature changes: `token_to_piece` lost its `encoding_rs::Decoder`
//!     streaming wrapper and now returns single-token bytes/string only.
//!     `LlamaSampler::dry` became a method on an existing sampler
//!     (takes `&self`) instead of a free constructor.
//!
//! ## Adapter shapes here
//!
//! * **Pure renames + retained call shape** live on extension traits
//!   ([`LlamaModelExt`], [`LlamaContextExt`]). Consumer code reads as
//!   it did pre-migration (`model.token_to_piece(t, &mut decoder, true, None)`),
//!   the trait dispatches to the 0.2.x byte-level API and runs the
//!   streaming decode through `encoding_rs` here so the consumer
//!   keeps its existing decoder loop intact.
//! * **Retired concepts** (chat-template-oaicompat,
//!   list_llama_ggml_backend_devices) live as free functions with
//!   TODO breadcrumbs.
//!
//! ## Switching back
//!
//! Branch `llama-cpp-4-mtp` was created from `main` to make the swap
//! reversible at the git level — `git checkout main` returns to the
//! llama-cpp-2 era. To swap to a different crate version on this
//! branch: change the two `pub use` lines below and update the
//! adapter bodies — consumer code does not move.

pub use llama_cpp_4 as cpp;
pub use llama_cpp_sys_4 as sys;

use cpp::token::LlamaToken;
use cpp::TokenToStringError;

/// Extension methods restoring 0.1.x-compatible names + signatures on
/// `LlamaModel`. Each method documents its default-on-error policy
/// so non-trivial call sites can rely on the invariant rather than
/// re-discovering it.
pub trait LlamaModelExt {
    /// 0.1.x `token_to_piece(token, &mut Decoder, special, max_bytes)
    /// -> Result<String, _>`. 0.2.x retired the streaming-decoder
    /// shape; we re-implement it here so existing per-token-decode
    /// loops keep working. Internally fetches the token bytes via
    /// 0.2.x's `token_to_bytes` and feeds them through the supplied
    /// `encoding_rs::Decoder`, which preserves partial UTF-8 state
    /// across calls — required for emoji and other multi-byte chars
    /// that can split across BPE token boundaries.
    fn token_to_piece(
        &self,
        token: LlamaToken,
        decoder: &mut encoding_rs::Decoder,
        special: bool,
        _max_bytes: Option<usize>,
    ) -> Result<String, TokenToStringError>;

    /// 0.1.x `token_to_piece_bytes(token, lstrip, special, max_bytes)
    /// -> Result<Vec<u8>, _>`. 0.2.x simplified to
    /// `token_to_bytes(token, Special) -> Result<Vec<u8>, _>`. We
    /// ignore `lstrip` and `max_bytes` — the original semantics
    /// (strip first `lstrip` chars after decode, cap output at
    /// `max_bytes`) are call-site-controlled in the JsonConstraint
    /// flow and don't need to be expressed here.
    fn token_to_piece_bytes(
        &self,
        token: LlamaToken,
        _lstrip: usize,
        special: bool,
        _max_bytes: Option<usize>,
    ) -> Result<Vec<u8>, TokenToStringError>;

    /// 0.1.x `size() -> u64`. 0.2.x renamed to `model_size`.
    fn size(&self) -> u64;
}

impl LlamaModelExt for cpp::model::LlamaModel {
    fn token_to_piece(
        &self,
        token: LlamaToken,
        decoder: &mut encoding_rs::Decoder,
        special: bool,
        _max_bytes: Option<usize>,
    ) -> Result<String, TokenToStringError> {
        let special_enum = special_bool_to_enum(special);
        let bytes = self.token_to_bytes(token, special_enum)?;
        // Decode through encoding_rs's streaming UTF-8 decoder. The
        // `last=false` arg keeps the decoder in streaming mode —
        // partial codepoints stay buffered for the next call rather
        // than emitting U+FFFD. That matches the 0.1.x semantics our
        // call sites rely on.
        let mut out = String::with_capacity(bytes.len() * 2);
        let (_result, _consumed, _had_errors) = decoder.decode_to_string(&bytes, &mut out, false);
        Ok(out)
    }

    fn token_to_piece_bytes(
        &self,
        token: LlamaToken,
        _lstrip: usize,
        special: bool,
        _max_bytes: Option<usize>,
    ) -> Result<Vec<u8>, TokenToStringError> {
        self.token_to_bytes(token, special_bool_to_enum(special))
    }

    fn size(&self) -> u64 {
        self.model_size()
    }
}

fn special_bool_to_enum(special: bool) -> cpp::model::Special {
    if special {
        cpp::model::Special::Tokenize
    } else {
        cpp::model::Special::Plaintext
    }
}

/// Extension method restoring 0.1.x `LlamaContext::token_data_array_ith`.
/// 0.2.x renamed to `candidates_ith` and returns an iterator; we
/// materialize into a `LlamaTokenDataArray` for compatibility with
/// the existing JsonConstraint flow which expects a slice view for
/// in-place logit-mask updates.
pub trait LlamaContextExt<'a> {
    fn token_data_array_ith(&'a self, i: i32) -> cpp::token::data_array::LlamaTokenDataArray;
}

impl<'a> LlamaContextExt<'a> for cpp::context::LlamaContext<'a> {
    fn token_data_array_ith(&'a self, i: i32) -> cpp::token::data_array::LlamaTokenDataArray {
        let candidates: Vec<_> = self.candidates_ith(i).collect();
        // sorted=false matches 0.1.x default for fresh logit arrays.
        cpp::token::data_array::LlamaTokenDataArray::new(candidates, false)
    }
}

/// 0.1.x `model.chat_template(None) -> Result<LlamaChatTemplate, _>`.
///
/// 0.2.x retired `LlamaChatTemplate` and exposes a string-returning
/// `get_chat_template(buf_size)`. Returns `None` when the model gguf
/// lacks `tokenizer.chat_template` metadata. `buf_size=8192` covers
/// every Jinja-style template we ship (Qwen, Gemma, Darwin all
/// 2-4 KiB encoded).
pub fn chat_template(model: &cpp::model::LlamaModel) -> Option<String> {
    const BUF_SIZE: usize = 8 * 1024;
    model.get_chat_template(BUF_SIZE).ok()
}

/// Stand-in for 0.1.x's `list_llama_ggml_backend_devices()`.
///
/// 0.2.x dropped this from the safe-Rust surface. We re-implement
/// via direct FFI into `llama_cpp_sys_4` (which exposes the raw
/// `ggml_backend_dev_*` C functions). The first daemon smoke after
/// the migration revealed that returning empty here caused the
/// hardware probe to report "GPU: none" → slot loader picked the
/// CPU compute backend, defeating the entire point of the swap on
/// a GPU box. The FFI walk below restores parity.
///
/// Safety: the ggml backend registry is initialized lazily on
/// first `ggml_backend_dev_count` call by llama.cpp itself
/// (`ggml_backend_load_all`). Calls are thread-safe per
/// upstream's contract. The C strings the FFI returns are owned
/// by ggml (static or registry-owned); we copy them into Rust
/// `String`s before returning, so callers can outlive the call.
pub struct BackendDevice {
    pub name: String,
    pub memory_total: u64,
}
/// Route llama.cpp + ggml's internal log stream into our `tracing`
/// subscriber. By default llama.cpp prints diagnostic messages via
/// its own log callback (which goes to stderr or is silently dropped
/// when the Rust wrapper captures stdio). When `LlamaModel::load_from_file`
/// returns a `null` result, the operator-relevant error message is
/// in that log stream — *not* in any Rust-side error chain.
///
/// Wiring it through `tracing` makes load failures debuggable.
/// Idempotent: subsequent calls replace the callback. Safe to call
/// once at daemon startup.
///
/// Maps ggml log levels to tracing levels (DEBUG/INFO unchanged,
/// WARN/ERROR straight; CONT is a continuation of the prior line and
/// gets logged at DEBUG with a marker).
pub fn install_log_tracing() {
    use std::ffi::CStr;

    unsafe extern "C" fn cb(
        level: sys::ggml_log_level,
        text: *const std::os::raw::c_char,
        _user_data: *mut std::os::raw::c_void,
    ) {
        if text.is_null() {
            return;
        }
        let msg = unsafe { CStr::from_ptr(text) }.to_string_lossy();
        let msg = msg.trim_end_matches('\n');
        if msg.is_empty() {
            return;
        }
        match level {
            sys::GGML_LOG_LEVEL_ERROR => tracing::error!(target: "llama_cpp", "{}", msg),
            sys::GGML_LOG_LEVEL_WARN => tracing::warn!(target: "llama_cpp", "{}", msg),
            sys::GGML_LOG_LEVEL_INFO => tracing::info!(target: "llama_cpp", "{}", msg),
            sys::GGML_LOG_LEVEL_DEBUG | sys::GGML_LOG_LEVEL_CONT => {
                tracing::debug!(target: "llama_cpp", "{}", msg)
            }
            _ => tracing::trace!(target: "llama_cpp", level = level, "{}", msg),
        }
    }

    unsafe {
        cpp::log_set(Some(cb), std::ptr::null_mut());
    }
}

pub fn list_llama_ggml_backend_devices() -> Vec<BackendDevice> {
    use std::ffi::CStr;
    let mut out = Vec::new();
    unsafe {
        let count = sys::ggml_backend_dev_count();
        for i in 0..count {
            let dev = sys::ggml_backend_dev_get(i);
            if dev.is_null() {
                continue;
            }
            let name_ptr = sys::ggml_backend_dev_name(dev);
            let name = if name_ptr.is_null() {
                String::from("unknown")
            } else {
                CStr::from_ptr(name_ptr).to_string_lossy().into_owned()
            };
            let mut free: usize = 0;
            let mut total: usize = 0;
            sys::ggml_backend_dev_memory(dev, &mut free as *mut usize, &mut total as *mut usize);
            out.push(BackendDevice {
                name,
                memory_total: total as u64,
            });
        }
    }
    out
}
