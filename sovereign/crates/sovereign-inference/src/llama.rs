// SPDX-License-Identifier: AGPL-3.0-or-later
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
//! PR #22673) and a newer ggml backend. The
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
/// `get_chat_template(buf_size)`. Returns `None` only when the gguf
/// truly lacks `tokenizer.chat_template` metadata (`MissingTemplate`).
/// On `BuffSizeError(needed)` we retry once with the size the binding
/// asked for — Gemma 4's tool-call template is ~12 KiB, well past the
/// 8 KiB the upstream wrapper docs as the "longest known". Without the
/// retry the lookup silently failed and the daemon fell through to
/// plain-text concat (no role markers, no `<|tool>` declarations) —
/// observed 2026-05-19 with `gemma-4-E4B-it-Q6_K`: the model generated
/// 14k tokens of role-play and tripped the inference deadline.
pub fn chat_template(model: &cpp::model::LlamaModel) -> Option<String> {
    chat_template_with(|buf_size| model.get_chat_template(buf_size))
}

/// The retry policy behind [`chat_template`], generic over the lookup
/// so the BuffSizeError-retry contract is pinned by weight-free tests
/// (the Gemma-4 P0: a silent lookup failure fell through to plain-text
/// concat and the model role-played for 14k tokens).
pub(crate) fn chat_template_with(
    mut get: impl FnMut(usize) -> Result<String, cpp::ChatTemplateError>,
) -> Option<String> {
    use cpp::ChatTemplateError;
    const INITIAL_BUF: usize = 8 * 1024;
    const MAX_BUF: usize = 256 * 1024;
    match get(INITIAL_BUF) {
        Ok(t) => Some(t),
        Err(ChatTemplateError::BuffSizeError(needed)) => {
            let retry = needed.min(MAX_BUF);
            match get(retry) {
                Ok(t) => Some(t),
                Err(e) => {
                    tracing::warn!(
                        needed,
                        retry,
                        error = %e,
                        "chat_template lookup retry failed after BuffSizeError; \
                         falling back to plain-text concat"
                    );
                    None
                }
            }
        }
        Err(ChatTemplateError::MissingTemplate(_)) => None,
        Err(e) => {
            tracing::warn!(error = %e, "chat_template lookup failed");
            None
        }
    }
}

#[cfg(test)]
mod chat_template_tests {
    use super::chat_template_with;
    use super::cpp::ChatTemplateError;

    #[test]
    fn first_try_success_does_not_retry() {
        let mut calls = 0;
        let t = chat_template_with(|buf| {
            calls += 1;
            assert_eq!(buf, 8 * 1024, "initial buffer is 8 KiB");
            Ok("template".to_string())
        });
        assert_eq!(t.as_deref(), Some("template"));
        assert_eq!(calls, 1);
    }

    #[test]
    fn buff_size_error_retries_with_requested_size() {
        // The Gemma-4 P0: tool-call template ~12 KiB crosses the 8 KiB
        // initial buffer; pre-fix the lookup silently returned None and
        // the daemon fell through to plain-text concat.
        let mut sizes = Vec::new();
        let t = chat_template_with(|buf| {
            sizes.push(buf);
            if buf < 12 * 1024 {
                Err(ChatTemplateError::BuffSizeError(12 * 1024))
            } else {
                Ok("gemma tool template".to_string())
            }
        });
        assert_eq!(t.as_deref(), Some("gemma tool template"));
        assert_eq!(sizes, vec![8 * 1024, 12 * 1024]);
    }

    #[test]
    fn retry_size_is_capped_at_256_kib() {
        let mut sizes = Vec::new();
        let _ = chat_template_with(|buf| {
            sizes.push(buf);
            Err(ChatTemplateError::BuffSizeError(usize::MAX))
        });
        assert_eq!(sizes, vec![8 * 1024, 256 * 1024], "cap, then give up");
    }

    #[test]
    fn missing_template_is_none_without_retry() {
        // gguf genuinely lacks tokenizer.chat_template — None is the
        // honest answer, not an error to retry.
        let mut calls = 0;
        let t = chat_template_with(|_| {
            calls += 1;
            Err(ChatTemplateError::MissingTemplate(-1))
        });
        assert!(t.is_none());
        assert_eq!(calls, 1);
    }

    #[test]
    fn retry_failure_falls_back_to_none() {
        let t = chat_template_with(|buf| {
            if buf == 8 * 1024 {
                Err(ChatTemplateError::BuffSizeError(16 * 1024))
            } else {
                Err(ChatTemplateError::MissingTemplate(-1))
            }
        });
        assert!(t.is_none(), "retry failure → plain-text concat fallback");
    }
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
/// Two modes share one callback, selected by `LLAMA_LOG_ERRORS_ONLY`:
///   - full (`install_log_tracing`): DEBUG/INFO/WARN/ERROR map straight
///     through; CONT continuations log at DEBUG.
///   - errors-only (`install_log_tracing_errors_only`): WARN/ERROR still
///     surface, but INFO/DEBUG/CONT are demoted to TRACE so routine
///     load chatter stays hidden under a normal `info` subscriber while
///     the lines that explain a *failed* load still show. This is the
///     daemon default — it replaces the old `void_logs()`, which hid the
///     failure reason too and left the operator with a bare "null result
///     from llama cpp".
static LLAMA_LOG_ERRORS_ONLY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

unsafe extern "C" fn ggml_log_cb(
    level: sys::ggml_log_level,
    text: *const std::os::raw::c_char,
    _user_data: *mut std::os::raw::c_void,
) {
    use std::ffi::CStr;
    if text.is_null() {
        return;
    }
    let msg = unsafe { CStr::from_ptr(text) }.to_string_lossy();
    let msg = msg.trim_end_matches('\n');
    if msg.is_empty() {
        return;
    }
    let errors_only = LLAMA_LOG_ERRORS_ONLY.load(std::sync::atomic::Ordering::Relaxed);
    match level {
        sys::GGML_LOG_LEVEL_ERROR => tracing::error!(target: "llama_cpp", "{}", msg),
        sys::GGML_LOG_LEVEL_WARN => tracing::warn!(target: "llama_cpp", "{}", msg),
        sys::GGML_LOG_LEVEL_INFO if !errors_only => tracing::info!(target: "llama_cpp", "{}", msg),
        sys::GGML_LOG_LEVEL_DEBUG | sys::GGML_LOG_LEVEL_CONT if !errors_only => {
            tracing::debug!(target: "llama_cpp", "{}", msg)
        }
        _ => tracing::trace!(target: "llama_cpp", level = level, "{}", msg),
    }
}

/// Route llama.cpp/ggml logs to `tracing` at their native levels (verbose).
pub fn install_log_tracing() {
    LLAMA_LOG_ERRORS_ONLY.store(false, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        cpp::log_set(Some(ggml_log_cb), std::ptr::null_mut());
    }
}

/// Route only ggml WARN/ERROR to `tracing`; demote INFO/DEBUG to TRACE.
/// The daemon default: quiet in normal operation, but a failed model load
/// still explains itself instead of surfacing as a bare null result.
pub fn install_log_tracing_errors_only() {
    LLAMA_LOG_ERRORS_ONLY.store(true, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        cpp::log_set(Some(ggml_log_cb), std::ptr::null_mut());
    }
}

/// Turn a `LlamaModel::load_from_file` failure into an operator-actionable
/// error. The underlying llama.cpp text (often just "null result from
/// llama cpp") is opaque on its own; the real reason rides the ggml log
/// stream, which the daemon now surfaces by default. This adds the model
/// path, its on-disk size against detected host RAM, and the most common
/// causes so the message stands on its own even if logs are missed.
pub fn describe_model_load_failure(
    role: &str,
    model_path: &std::path::Path,
    err: impl std::fmt::Display,
) -> String {
    let mut s = format!(
        "failed to load {role} model {}: {err}",
        model_path.display()
    );
    // A MISSING SHARD IS NOT A GUESS — say it first and stop guessing.
    // Pointing `[models].primary` at shard `00001` of a split is the supported
    // gesture, and the common way it goes wrong is that not every sibling made
    // it onto disk. Shard `00001` is often ~10 MB of header, so the generic
    // advice below ("didn't fit", "pick a smaller quant") is actively wrong
    // here and sends the reader to the one place the problem is not.
    let missing = crate::embedded::missing_shards(model_path);
    if !missing.is_empty() {
        s.push_str(&format!(
            "\n  THIS IS ONE SHARD OF A SPLIT GGUF AND {} SIBLING(S) ARE MISSING FROM {}:\n    {}\
             \n  A split model needs every shard in the same directory; llama.cpp is pointed at \
             shard 00001 and opens the rest itself. Download the missing file(s) into that \
             directory — nothing else needs to change, and `[models].primary` stays pointed at \
             shard 00001.",
            missing.len(),
            model_path.parent().unwrap_or(model_path).display(),
            missing.join("\n    "),
        ));
        return s;
    }

    // Sum every shard: for a split, the path we were handed is a ~10 MB header
    // and reporting its size next to host RAM makes an unrelated diagnosis look
    // obvious ("0.0 GB on disk … likely didn't fit in memory").
    let size_gb = Some(crate::embedded::total_model_bytes(model_path) as f64 / 1_000_000_000.0)
        .filter(|g| *g > 0.0);
    let ram_gb =
        crate::hardware::HardwareProfile::detect().system_ram_bytes as f64 / 1_000_000_000.0;
    if let Some(g) = size_gb {
        s.push_str(&format!(
            "\n  model file: {g:.1} GB on disk; detected host RAM: {ram_gb:.1} GB"
        ));
    }
    s.push_str(
        "\n  likely causes: it didn't fit in memory (pick a smaller quant or the low_mem \
         profile via `svrn setup`), the GGUF is incomplete or corrupt (re-download), or the \
         quant/arch isn't supported by this build. The underlying llama.cpp error is logged \
         just above on the daemon's stderr; SOVEREIGN_LLAMA_LOGS=1 makes it fully verbose, \
         =0 silences it.",
    );
    s
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

#[cfg(test)]
mod load_failure_message_tests {
    use super::describe_model_load_failure;

    /// The message a friend actually reads when they copied shard 1 and not the
    /// rest. It must NAME the missing files and must NOT offer the memory/quant
    /// advice, which is wrong for this cause and sends them somewhere useless.
    #[test]
    fn a_missing_shard_is_named_and_the_memory_advice_is_withheld() {
        let d = tempfile::tempdir().unwrap();
        let first = d.path().join("m-00001-of-00004.gguf");
        std::fs::write(&first, b"x").unwrap();
        std::fs::write(d.path().join("m-00003-of-00004.gguf"), b"x").unwrap();

        let msg = describe_model_load_failure("primary", &first, "some llama.cpp error");

        assert!(
            msg.contains("m-00002-of-00004.gguf"),
            "names the gap: {msg}"
        );
        assert!(msg.contains("m-00004-of-00004.gguf"), "names both: {msg}");
        assert!(
            !msg.contains("pick a smaller quant"),
            "must not offer the wrong diagnosis when the cause is known: {msg}"
        );
        assert!(
            !msg.contains("GB on disk"),
            "must not print a header shard's size as the model size: {msg}"
        );
    }

    /// A complete split still reports the WHOLE model's size, not shard 1's.
    /// Before this, a 104 GB model read as "0.0 GB on disk" beside the host's
    /// RAM — which makes "it didn't fit in memory" look obviously true.
    #[test]
    fn a_complete_split_reports_the_summed_size_not_the_header_shard() {
        let d = tempfile::tempdir().unwrap();
        let first = d.path().join("m-00001-of-00002.gguf");
        // SPARSE, not a 4 GB allocation: `total_model_bytes` stats the files, it
        // never reads them, and a unit test that materialises 4 GB of zeroes can
        // OOM a loaded box during a full suite run.
        std::fs::write(&first, vec![0u8; 1000]).unwrap();
        std::fs::File::create(d.path().join("m-00002-of-00002.gguf"))
            .unwrap()
            .set_len(4_000_000_000)
            .unwrap();

        let msg = describe_model_load_failure("primary", &first, "boom");

        assert!(msg.contains("4.0 GB on disk"), "sums every shard: {msg}");
        assert!(
            msg.contains("pick a smaller quant"),
            "generic advice returns when the cause is genuinely unknown: {msg}"
        );
    }
}
