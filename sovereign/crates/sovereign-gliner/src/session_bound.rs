// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every ONNX session this crate builds, built the same way.
//!
//! # What `ort 2.0.0-rc.9` does NOT give us
//!
//! **There is no way to bound the CPU arena's SIZE.** Checked against the
//! vendored source on 2026-09-12:
//!
//! - `OrtApi::CreateArenaCfg(..., max_mem, arena_extend_strategy, ...)`
//!   exists in `ort-sys-2.0.0-rc.9/src/lib.rs:1421` as a raw function
//!   pointer, and `ort` wraps it **nowhere**. The only `OrtArenaCfg`
//!   mention anywhere in `ort-2.0.0-rc.9/src/` is
//!   `RocmExecutionProvider::with_default_memory_arena_cfg`
//!   (`execution_providers/rocm.rs:83`), which takes a raw
//!   `*mut ort_sys::OrtArenaCfg` the caller must build itself and applies
//!   to ROCm devices.
//! - The memory-limit knobs that do exist are device-side only:
//!   `CANNExecutionProvider::with_memory_limit`, and CUDA/ROCm's
//!   `gpu_mem_limit`. There is no CPU counterpart, and this workload is
//!   CPU-only.
//! - `SessionBuilder::add_config_entry` reaches ORT's session config keys
//!   (`session/builder/impl_config_keys.rs`); none of the arena-related
//!   ones takes a size. `session.use_env_allocators` shares the ENV's
//!   allocator between sessions, which changes who owns the arena, not how
//!   big it may get.
//!
//! So **guard 3 cannot cap the arena, and the bound on this pass is
//! guard 2** — the input bound in [`crate::bounded_input`]. That is stated
//! rather than papered over (ARCH 6): an unbounded allocator fed bounded
//! inputs is bounded; an unbounded allocator fed a whole conversation is
//! what took the daemon to 79.9 GB on 2026-09-12.
//!
//! # What it DOES give us, and is applied here
//!
//! - **The CPU arena is switched OFF.** `CPUExecutionProvider::default()`
//!   (i.e. without `with_arena_allocator()`) calls `DisableCpuMemArena` on
//!   the session options (`execution_providers/cpu.rs:48-51`). ORT's
//!   default is the arena ENABLED, and neither backend was registering a
//!   CPU provider at all, so both ran with it on. The arena is a pooling
//!   allocator that grows in power-of-two buckets and does not return them
//!   — which is exactly the 1, 2, 2, 8 and 32 GB stackless blocks
//!   `malloc_history` showed on pid 47944. With it off, transient
//!   inference buffers go to the system allocator and are freed on
//!   release. This trades some allocator throughput for a bounded
//!   footprint on a background pass; the throughput cost has NOT been
//!   measured, and `sovereign/DEFAULTS_LEDGER.md` says so.
//! - **Memory-pattern planning off** on the sessions we build ourselves.
//!   It pre-plans one contiguous buffer from the first run's shapes; with
//!   a varying batch dimension that plan is re-made at the largest shape
//!   seen and retained.
//! - **Explicit thread counts**, so the count is a stated number rather
//!   than whatever the host's core count implies.
//!
//! # Two backends, two builders, one policy
//!
//! `gliner2.rs` builds its own `ort::Session` and gets all three. The v1
//! path does NOT own its builder — `orp::Model::new`
//! (`orp-0.9.2/src/model.rs:20-24`) calls `Session::builder()`,
//! `with_intra_threads`, `with_execution_providers` and
//! `with_optimization_level` itself, and the ONLY lever a caller has is
//! `RuntimeParameters`. Its default is `threads: 4, execution_providers:
//! []`, and an empty provider list means `DisableCpuMemArena` is never
//! called. [`v1_runtime_parameters`] is that lever, so the path the daemon
//! actually runs (`labeled::configured_model_id` resolves to
//! `DEFAULT_MODEL_ID`, a V1 model) gets the arena switch too;
//! `with_memory_pattern` is simply not reachable from there.
//!
//! `tests/session_bound_census.rs` is the ratchet: no other site in this
//! crate may call `Session::builder()` or hand `orp` a bare
//! `RuntimeParameters`.

use orp::params::RuntimeParameters;
use ort::execution_providers::{CPUExecutionProvider, ExecutionProviderDispatch};
use ort::session::builder::SessionBuilder;
use sovereign_core::error::{Error, Result};

/// Intra-op thread count for every session this crate builds.
///
/// 4 is `orp::params::RuntimeParameters::default()`'s value
/// (`orp-0.9.2/src/params.rs`, `Default` impl), kept deliberately: this
/// commit changes the ALLOCATOR, and changing the thread count in the same
/// breath would make the next measurement unattributable (ARCH 2 — one
/// dimension at a time). It is stated here rather than inherited so a
/// future change to it is a visible edit.
pub const INTRA_THREADS: usize = 4;

/// Inter-op thread count. One: these graphs are a single sequential
/// encoder, so parallel node execution buys nothing and each extra pool
/// thread is another allocation arena.
pub const INTER_THREADS: usize = 1;

/// The CPU execution provider with the **arena allocator off**.
///
/// `CPUExecutionProvider::default()` has `use_arena: false`, and its
/// `register` calls `DisableCpuMemArena` in that case
/// (`ort-2.0.0-rc.9/src/execution_providers/cpu.rs:48-51`). Registering
/// no provider at all — what both backends did before 2026-09-12 — leaves
/// ORT's default, which is the arena ENABLED.
pub fn cpu_provider_arena_off() -> ExecutionProviderDispatch {
    CPUExecutionProvider::default().build()
}

/// `RuntimeParameters` for the gline-rs (v1) path — the ONLY session knob
/// `orp::Model::new` leaves to its caller. Carries the arena switch and
/// the stated thread count.
pub fn v1_runtime_parameters() -> RuntimeParameters {
    RuntimeParameters::default()
        .with_threads(INTRA_THREADS)
        .with_execution_providers([cpu_provider_arena_off()])
}

/// A `SessionBuilder` carrying every bound `ort 2.0.0-rc.9` exposes for a
/// CPU session. The caller adds `commit_from_file`.
///
/// Errors are mapped into this crate's error type naming the option that
/// failed, so an `ort` upgrade that drops or renames one surfaces as
/// "GLiNER session bound: …" rather than a bare FFI status.
pub fn bounded_session_builder() -> Result<SessionBuilder> {
    let opt = |what: &'static str, r: ort::Result<SessionBuilder>| {
        r.map_err(move |e| Error::Storage(format!("GLiNER session bound: {what}: {e}")))
    };
    let b = opt("Session::builder", ort::session::Session::builder())?;
    let b = opt(
        "with_execution_providers(CPU, arena off)",
        b.with_execution_providers([cpu_provider_arena_off()]),
    )?;
    let b = opt("with_memory_pattern(false)", b.with_memory_pattern(false))?;
    let b = opt("with_intra_threads", b.with_intra_threads(INTRA_THREADS))?;
    opt("with_inter_threads", b.with_inter_threads(INTER_THREADS))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every option we apply is still accepted by the pinned `ort`. This
    /// is the half of guard 3 that CAN run without a model file: it fails
    /// the day an `ort` bump drops, renames or rejects one of them, which
    /// is the realistic way this bound would silently stop being applied.
    ///
    /// It is NOT a claim that the arena is bounded — nothing in rc.9 can
    /// make that claim (see the module docs). See
    /// `tests/session_bound_census.rs` for the structural half.
    #[test]
    fn every_bound_is_accepted_by_the_pinned_ort() {
        bounded_session_builder().expect("rc.9 accepts every option in the bound");
    }

    /// The v1 lever carries the two things `orp` will read back out of it.
    /// Threads are readable; the provider list is only observable by
    /// length, because `ExecutionProviderDispatch` exposes no identity.
    #[test]
    fn the_v1_lever_carries_the_arena_switch() {
        let params = v1_runtime_parameters();
        assert_eq!(params.threads(), INTRA_THREADS);
        assert_eq!(
            params.execution_providers().len(),
            1,
            "orp's default is an EMPTY provider list, which never calls \
             DisableCpuMemArena — the whole point of this lever"
        );
    }
}
