// SPDX-License-Identifier: AGPL-3.0-or-later
//! How much llama.cpp/ggml says — decided once, for every backend.
//!
//! # Why one decider (ARCH §10.6, principle 1)
//!
//! Five sites read `SOVEREIGN_LLAMA_LOGS` and they did not agree:
//!
//! | site | when the var is unset | honours `GGML_RPC_DEBUG` |
//! |---|---|---|
//! | `embedded::engine` (the primary) | errors only | yes |
//! | `embedded::embed_only` | **silence** | no |
//! | `smoketest` | **silence** | no |
//! | `reranker_standalone` | **silence** | no |
//! | `sovereign-cli-daemon`'s tracing filter | (verbose = var is `"1"`) | yes |
//!
//! Two consequences, both live:
//!
//! 1. **`GGML_RPC_DEBUG` half-worked.** An operator debugging an RPC worker
//!    got llama output from the primary engine and silence from the embed
//!    slot, the crash smoketest and the reranker — three backends that could
//!    be the thing failing. The daemon's own doc comment promises "one env
//!    var, all three gates"; there were five.
//! 2. **The silence was contagious.** `install_log_tracing*` sets a
//!    **process-global** ggml callback (`llama.rs::log_set`) and
//!    `void_logs()` disables it globally, so the LAST backend to initialise
//!    decided for all of them. A daemon that loads the primary (errors only)
//!    and then the embed slot (silence) ends up silent everywhere — defeating
//!    the stated default that "a failed model load still explains itself
//!    instead of surfacing as a bare null result".
//!
//! Resolving to one value and applying it at every init makes the ordering
//! irrelevant, because every backend installs the same thing.

/// What the ggml log callback should do, for this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlamaLogs {
    /// Nothing at all. Only an explicit `SOVEREIGN_LLAMA_LOGS=0` asks for
    /// this — asking for silence is unambiguous and outranks a debug var.
    Void,
    /// WARN/ERROR to `tracing`, INFO/DEBUG demoted to TRACE. The default:
    /// quiet in normal operation, loud when a load fails.
    ErrorsOnly,
    /// Everything ggml emits.
    Full,
}

impl LlamaLogs {
    /// Resolve from the environment. Cheap and idempotent, so a backend that
    /// initialises late calls it again and lands on the same answer.
    pub fn from_env() -> Self {
        Self::resolve(
            std::env::var("SOVEREIGN_LLAMA_LOGS").ok().as_deref(),
            std::env::var_os("GGML_RPC_DEBUG").is_some(),
        )
    }

    /// THE decider, with the environment split off so the disagreements in
    /// the module docs are testable without racing another test's `set_var`.
    ///
    /// `GGML_RPC_DEBUG` is llama.cpp's own documented knob. Honouring it here
    /// is what makes it mean upstream what it says: the var alone gates
    /// `LOG_DBG` inside `ggml-rpc.cpp`, but those lines are `GGML_LOG_DEBUG`
    /// and would still die at our callback.
    pub fn resolve(knob: Option<&str>, ggml_rpc_debug: bool) -> Self {
        match knob.map(str::trim) {
            Some("0") => Self::Void,
            Some("1") => Self::Full,
            _ if ggml_rpc_debug => Self::Full,
            _ => Self::ErrorsOnly,
        }
    }

    /// Apply to the process. `backend` is taken because silence goes through
    /// llama-cpp-2's own backend-level disable; the two verbose modes set the
    /// global callback.
    pub fn install(self, backend: &mut crate::llama::cpp::llama_backend::LlamaBackend) {
        match self {
            Self::Void => backend.void_logs(),
            // One body for the global half, so a caller who reaches it through
            // either door lands on the same callback.
            _ => {
                let _ = self.install_global();
            }
        }
    }

    /// Apply the process-global half, for a caller that touches ggml WITHOUT
    /// owning a [`LlamaBackend`].
    ///
    /// `svrn setup` is the case that forced this, and it was a sixth site
    /// outside this module's one decision. Its in-process daemon builds a
    /// capability manifest, which calls `detect_hardware()`
    /// (`sovereign-mesh/src/capabilities.rs`), which initialises the Metal
    /// device — about thirty lines of `ggml_metal_library_compile_all:
    /// compiled 'fa' library in 0.036 sec` straight to stderr, in the middle
    /// of an onboarding flow written for someone who has never seen a GPU log.
    /// No backend is constructed anywhere on that path, so
    /// [`install`](Self::install) could not be called at all.
    ///
    /// [`Void`](Self::Void) cannot be honoured in full here: silencing goes
    /// through llama-cpp-2's backend-level disable and there is no backend to
    /// disable. The tracing callback is installed regardless — that routes
    /// ggml into `tracing`, where the process's own filter decides, which is
    /// strictly quieter than raw stderr. The shortfall is REPORTED rather than
    /// papered over (§18.3), which is what the return value is for.
    ///
    /// Returns whether the policy was applied in full.
    #[must_use = "Void cannot be fully honoured without a backend; check or explicitly ignore"]
    pub fn install_global(self) -> bool {
        match self {
            Self::Full => {
                crate::llama::install_log_tracing();
                true
            }
            Self::ErrorsOnly => {
                crate::llama::install_log_tracing_errors_only();
                true
            }
            Self::Void => {
                crate::llama::install_log_tracing_errors_only();
                false
            }
        }
    }

    /// True when the operator asked for verbose ggml output — the daemon's
    /// tracing filter widens on this.
    pub fn is_verbose(self) -> bool {
        matches!(self, Self::Full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The behaviour four of the five sites did not have: unset means the
    /// default, not silence. A voided backend cannot explain a failed load.
    #[test]
    fn unset_is_errors_only_not_silence() {
        assert_eq!(LlamaLogs::resolve(None, false), LlamaLogs::ErrorsOnly);
        assert_eq!(LlamaLogs::resolve(Some(""), false), LlamaLogs::ErrorsOnly);
        assert_eq!(
            LlamaLogs::resolve(Some("yes"), false),
            LlamaLogs::ErrorsOnly
        );
    }

    /// The half-working knob: `GGML_RPC_DEBUG` reached the primary engine and
    /// no other backend.
    #[test]
    fn ggml_rpc_debug_turns_every_backend_verbose() {
        assert_eq!(LlamaLogs::resolve(None, true), LlamaLogs::Full);
        assert!(LlamaLogs::resolve(None, true).is_verbose());
    }

    /// Asking for silence outranks a debug var — the one precedence rule the
    /// primary engine already documented, now the only one.
    #[test]
    fn an_explicit_zero_beats_the_debug_var() {
        assert_eq!(LlamaLogs::resolve(Some("0"), true), LlamaLogs::Void);
        assert!(!LlamaLogs::resolve(Some("0"), true).is_verbose());
    }

    #[test]
    fn an_explicit_one_is_full() {
        assert_eq!(LlamaLogs::resolve(Some("1"), false), LlamaLogs::Full);
        assert_eq!(LlamaLogs::resolve(Some(" 1 "), false), LlamaLogs::Full);
    }
}
