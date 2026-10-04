// SPDX-License-Identifier: AGPL-3.0-or-later
//! The compute backend a slot reports in its load line: one decider for
//! the embed, rerank and model slots, and the only place that asks ggml
//! whether a GPU is actually there.

/// Label emitted in slot-load logs when the GPU path wasn't taken
/// (GPU context creation failed, or caller didn't request it).
/// Tells the operator which CPU math backend GGML is using.
pub(crate) fn embed_compute_backend_label() -> &'static str {
    // On Apple Silicon, GGML's CPU backend links Accelerate's SGEMM
    // — that's where real CPU-path throughput comes from. Other
    // platforms fall back to plain llama.cpp CPU kernels.
    #[cfg(target_os = "macos")]
    {
        "cpu+accelerate"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "cpu"
    }
}

/// The compute backend a slot reports. A context that built "for GPU" is
/// only on the GPU when ggml can see one: on a machine with none (a laptop,
/// a container without `/dev/dri`) llama.cpp runs it on the CPU and says so
/// only in its own startup noise, and the slot used to log `gpu` regardless.
pub(crate) fn compute_backend_label(used_gpu: bool) -> &'static str {
    let gpus = if used_gpu {
        crate::llama::local_gpu_device_count()
    } else {
        0
    };
    if used_gpu && gpus == 0 {
        tracing::warn!(
            backend = embed_compute_backend_label(),
            "asked for the GPU, but no GPU device is visible: this model runs on the CPU"
        );
    }
    backend_label(used_gpu, gpus)
}

fn backend_label(used_gpu: bool, local_gpus: usize) -> &'static str {
    if used_gpu && local_gpus > 0 {
        gpu_backend_label()
    } else {
        embed_compute_backend_label()
    }
}

/// Label emitted in slot-load logs when `wants_gpu` succeeded.
/// The specific backend (metal / rocm / vulkan) is still visible
/// in the nearby `ggml_*_init` llama.cpp output at startup.
pub(crate) fn gpu_backend_label() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "gpu+metal"
    }
    // Windows backend is feature-selected (see sovereign-inference/Cargo.toml):
    // the matrix builds CPU / vulkan / cuda variants, and those features ARE
    // visible as rustc cfgs here, so the label can name the actual backend.
    #[cfg(all(target_os = "windows", feature = "windows-cuda"))]
    {
        "gpu+cuda"
    }
    #[cfg(all(
        target_os = "windows",
        feature = "windows-vulkan",
        not(feature = "windows-cuda")
    ))]
    {
        "gpu+vulkan"
    }
    #[cfg(all(
        target_os = "windows",
        not(feature = "windows-vulkan"),
        not(feature = "windows-cuda")
    ))]
    {
        "cpu"
    }
    // Linux: vulkan is selected at the workspace level and not re-exposed as a
    // cfg here, so we just say "gpu".
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        "gpu"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A context built "for GPU" on a machine with no GPU device runs on the
    /// CPU, and says so. Seen 2026-10-03: a container without `/dev/dri`
    /// logged `ggml_vulkan: No devices found` and then `compute_backend="gpu"`.
    #[test]
    fn asking_for_the_gpu_is_not_being_on_it() {
        assert_eq!(backend_label(true, 0), embed_compute_backend_label());
        assert_eq!(backend_label(true, 1), gpu_backend_label());
        assert_eq!(backend_label(false, 1), embed_compute_backend_label());
    }
}
