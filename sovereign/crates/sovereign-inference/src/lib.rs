// SPDX-License-Identifier: AGPL-3.0-or-later
pub mod capacity;
pub mod cpu_compat;
pub mod embedded;
pub mod evidence_id_constraint;
pub mod fim;
pub mod gguf_meta;
pub mod gguf_validator;
pub mod hardware;
pub mod health;
pub mod hybrid;
pub mod json_grammar;
pub mod llama;
pub mod llguidance_constraint;
// `remote.rs` was extracted wholesale to the `oicp-client` crate (pure-HTTP
// OICP client, no llama.cpp). Re-exported here so `sovereign_inference::remote::*`
// (RemoteApiProvider, SplitInferenceProvider, …) is unchanged for all callers.
pub mod remote {
    pub use oicp_client::*;
}
pub mod reranker_standalone;
pub mod router_circuit;
pub mod selector;
pub mod setup_planner;
pub mod smoketest;
pub mod url_constraint;
pub mod vocab_cache;

pub use gguf_validator::{validate_gguf, GgufExpectation, GgufValidationError};
pub use sovereign_core;

/// Terminate the process with `code`, skipping C/C++ static destructors on
/// macOS. ggml-metal registers a device sweeper as a `__cxa_finalize_ranges`
/// static destructor; at a normal `exit()` it runs while the process is being
/// torn down and asserts on still-resident llama-context / Metal resources
/// (`ggml_abort` → SIGABRT → a macOS crash dialog). `_exit` hands all cleanup
/// (Metal devices, KV caches, mmap'd ggufs) to the kernel, which is correct at
/// process exit — the same shape `llama-server` uses. On non-macOS this is a
/// normal `std::process::exit` (Metal is macOS-only).
///
/// Both the daemon (`run_daemon`) and the desktop app (`RunEvent::Exit`) route
/// their final exit through here, so neither pops a crash dialog on shutdown.
/// (Confirmed 2026-05-20 in the daemon; lifted here 2026-06-16 so the desktop
/// reuses the same path instead of duplicating it.)
pub fn fast_exit_skip_destructors(code: i32) -> ! {
    #[cfg(target_os = "macos")]
    {
        unsafe {
            extern "C" {
                #[link_name = "_exit"]
                fn libc_exit_no_finalize(status: i32) -> !;
            }
            libc_exit_no_finalize(code)
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::process::exit(code)
    }
}
