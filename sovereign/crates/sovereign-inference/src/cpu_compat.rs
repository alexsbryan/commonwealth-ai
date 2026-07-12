// SPDX-License-Identifier: AGPL-3.0-or-later
//! CPU/architecture compatibility gate for chat models.
//!
//! ## Why this exists
//!
//! Recurrent / linear-attention architectures — Qwen3.5 "Gated DeltaNet"
//! (`qwen35`), Mamba/SSM, RWKV — drive an out-of-bounds write inside ggml's
//! recurrent `ggml_compute_forward_set` during multi-token prefill **on the CPU
//! backend**. In the vendored llama.cpp (llama-cpp-4 0.3.1) this SIGSEGVs the
//! process. Verified end-to-end on `qwen35` (Qwen3.5) 4B *and* 9B on an Intel
//! Mac: the shipped svrnmesh 1.20 desktop crashed on its first tool-laden query
//! (command-bridge repro, 2026-07-11). Disabling the fused chunked kernel did
//! **not** help — the OOB is inherent to the recurrent `SET`, fused or not —
//! and there is no llama.cpp toggle that avoids it. It is an upstream bug.
//!
//! These models run fine on GPU (the `SET` op lands on the Metal/CUDA kernel,
//! not the buggy CPU one), so the gate applies **only when the chat slot will
//! compute on CPU** (`n_gpu_layers == 0`, e.g. an Intel Mac where
//! `detect_gpu()` is false, or `SOVEREIGN_FORCE_CPU_CHAT`).
//!
//! ## What it does
//!
//! Rather than load such a model and crash, we detect the architecture from the
//! GGUF header ([`crate::gguf_meta`]) *before* loading and, on a CPU machine,
//! substitute a **dense** model discovered alongside it — or, if none exists,
//! report that cleanly so the app can explain it instead of dying. A crash on
//! "someone tried a cool model their machine can't run" must never be silent.

use std::path::{Path, PathBuf};

use crate::gguf_meta;

/// Architecture markers whose CPU prefill path is known (or strongly expected)
/// to hit the recurrent-`SET` OOB crash. Substring match, lowercased.
///
/// - `qwen35` — Qwen3.5 Gated DeltaNet (also matches `qwen35moe`). VERIFIED.
/// - `deltanet` / `mamba` / `rwkv` / `ssm` — explicit recurrent/linear-attention
///   markers that share the same recurrent-state `SET` mechanism.
///
/// Deliberately does NOT match dense `qwen3` / `qwen2` / `llama` / `gemma` etc.
/// (`"qwen3"` is not a substring of `"qwen35"` and vice-versa, so dense Qwen3 is
/// safe here). Kept conservative: only block what shares the crashing mechanism.
const CPU_UNSAFE_ARCH_MARKERS: &[&str] = &["qwen35", "deltanet", "mamba", "rwkv", "ssm"];

/// True when `arch` is a recurrent/linear-attention architecture that crashes
/// in ggml's CPU prefill (see [`CPU_UNSAFE_ARCH_MARKERS`]). Empty/unknown → false
/// (we only block what we positively recognize as unsafe).
pub fn is_cpu_incompatible_arch(arch: &str) -> bool {
    let a = arch.to_lowercase();
    CPU_UNSAFE_ARCH_MARKERS.iter().any(|m| a.contains(m))
}

/// Heuristic: does this filename look like a chat/instruct model (as opposed to
/// an embedding or reranker GGUF that happens to sit in the same directory)?
/// Used to avoid substituting an embedder for the chat slot.
fn looks_like_chat_model(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    !(name.contains("embed") || name.contains("rerank"))
}

/// The decision produced by [`choose_cpu_safe_chat_model`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatModelChoice {
    /// The configured model is safe on this machine — load it unchanged.
    Keep,
    /// The configured model would crash on CPU; load this dense substitute
    /// instead. Carries the arches for a human-readable explanation.
    Substitute {
        path: PathBuf,
        unsafe_arch: String,
        safe_arch: String,
    },
    /// The configured model is unsafe on CPU and no CPU-safe substitute was
    /// found alongside it. The caller must surface this, not crash.
    NoSafeModel { unsafe_arch: String },
}

/// A CPU-safe dense candidate found next to the configured model.
struct DenseCandidate {
    path: PathBuf,
    arch: String,
    size: u64,
}

/// Decide which chat model to load.
///
/// - `configured` — the model the config points at (e.g. the fast chat slot).
/// - `computes_on_cpu` — whether the slot will run on the CPU backend
///   (`n_gpu_layers == 0`). When false, we always [`Keep`](ChatModelChoice::Keep):
///   GPU is unaffected.
/// - `candidate_dir` — where to look for a dense substitute (normally the
///   directory the configured model lives in).
///
/// Reads GGUF headers only — never loads weights, never risks the crash.
pub fn choose_cpu_safe_chat_model(
    configured: &Path,
    computes_on_cpu: bool,
    candidate_dir: &Path,
) -> ChatModelChoice {
    if !computes_on_cpu {
        return ChatModelChoice::Keep; // GPU path is unaffected.
    }

    // What is the configured model's architecture? An unreadable header means
    // we can't prove it unsafe — keep it (the smoketest backstop still guards
    // the load), rather than second-guess a model we can't classify.
    let configured_arch = match gguf_meta::read_architecture(configured) {
        Ok(Some(a)) => a,
        Ok(None) | Err(_) => return ChatModelChoice::Keep,
    };
    if !is_cpu_incompatible_arch(&configured_arch) {
        return ChatModelChoice::Keep;
    }

    // The configured model is unsafe on CPU. Look for a dense substitute.
    let best = scan_dense_candidates(configured, candidate_dir)
        .into_iter()
        .max_by(|a, b| a.size.cmp(&b.size)); // largest dense ≈ most capable

    match best {
        Some(c) => ChatModelChoice::Substitute {
            path: c.path,
            unsafe_arch: configured_arch,
            safe_arch: c.arch,
        },
        None => ChatModelChoice::NoSafeModel {
            unsafe_arch: configured_arch,
        },
    }
}

/// Find CPU-safe (dense) chat-model GGUFs in `dir`, excluding `configured`
/// itself and any embedder/reranker files. Unreadable headers are skipped.
fn scan_dense_candidates(configured: &Path, dir: &Path) -> Vec<DenseCandidate> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let configured_canon = std::fs::canonicalize(configured).ok();

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
            continue;
        }
        if std::fs::canonicalize(&path).ok() == configured_canon {
            continue; // same file as configured
        }
        if !looks_like_chat_model(&path) {
            continue; // embedder / reranker — not a chat slot
        }
        let arch = match gguf_meta::read_architecture(&path) {
            Ok(Some(a)) => a,
            Ok(None) | Err(_) => continue, // can't classify → don't trust it
        };
        if is_cpu_incompatible_arch(&arch) {
            continue; // also unsafe on CPU
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        out.push(DenseCandidate { path, arch, size });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn classifier_flags_recurrent_but_not_dense() {
        for unsafe_arch in ["qwen35", "qwen35moe", "mamba2", "rwkv6", "deltanet", "ssm-hybrid"] {
            assert!(is_cpu_incompatible_arch(unsafe_arch), "{unsafe_arch} must be flagged");
        }
        for safe_arch in ["qwen3", "qwen2", "llama", "gemma3", "phi4", "qwen3moe", ""] {
            assert!(!is_cpu_incompatible_arch(safe_arch), "{safe_arch} must NOT be flagged");
        }
    }

    // Minimal GGUF with a single architecture KV — mirrors gguf_meta's builder.
    fn write_gguf(dir: &Path, name: &str, arch: &str, pad_bytes: usize) -> PathBuf {
        const MAGIC: u32 = 0x4655_4747;
        const T_STRING: u32 = 8;
        let mut b = Vec::new();
        b.extend_from_slice(&MAGIC.to_le_bytes());
        b.extend_from_slice(&3u32.to_le_bytes());
        b.extend_from_slice(&0u64.to_le_bytes()); // tensor_count
        b.extend_from_slice(&1u64.to_le_bytes()); // kv_count
        let k = "general.architecture";
        b.extend_from_slice(&(k.len() as u64).to_le_bytes());
        b.extend_from_slice(k.as_bytes());
        b.extend_from_slice(&T_STRING.to_le_bytes());
        b.extend_from_slice(&(arch.len() as u64).to_le_bytes());
        b.extend_from_slice(arch.as_bytes());
        b.extend(std::iter::repeat(0u8).take(pad_bytes)); // size differentiator
        let p = dir.join(name);
        std::fs::File::create(&p).unwrap().write_all(&b).unwrap();
        p
    }

    #[test]
    fn keep_when_gpu() {
        let d = tempfile::tempdir().unwrap();
        let m = write_gguf(d.path(), "chat.gguf", "qwen35", 0);
        assert_eq!(
            choose_cpu_safe_chat_model(&m, false, d.path()),
            ChatModelChoice::Keep
        );
    }

    #[test]
    fn keep_when_configured_is_dense_on_cpu() {
        let d = tempfile::tempdir().unwrap();
        let m = write_gguf(d.path(), "chat.gguf", "qwen3", 0);
        assert_eq!(
            choose_cpu_safe_chat_model(&m, true, d.path()),
            ChatModelChoice::Keep
        );
    }

    #[test]
    fn substitutes_largest_dense_when_unsafe_on_cpu() {
        let d = tempfile::tempdir().unwrap();
        let configured = write_gguf(d.path(), "Qwen3.5-4B.gguf", "qwen35", 0);
        let _small = write_gguf(d.path(), "qwen2.5-3b.gguf", "qwen2", 10);
        let big = write_gguf(d.path(), "Qwen3-4B.gguf", "qwen3", 5000);
        // an embedder that must be ignored even though it's "dense"
        let _embed = write_gguf(d.path(), "Qwen3-Embedding-0.6B.gguf", "qwen3", 99999);
        // another unsafe model that must be ignored
        let _unsafe2 = write_gguf(d.path(), "Qwen3.5-9B.gguf", "qwen35", 99999);

        match choose_cpu_safe_chat_model(&configured, true, d.path()) {
            ChatModelChoice::Substitute { path, unsafe_arch, safe_arch } => {
                assert_eq!(path, big, "should pick the largest dense chat model");
                assert_eq!(unsafe_arch, "qwen35");
                assert_eq!(safe_arch, "qwen3");
            }
            other => panic!("expected Substitute, got {other:?}"),
        }
    }

    #[test]
    fn no_safe_model_when_only_unsafe_and_embedders_present() {
        let d = tempfile::tempdir().unwrap();
        let configured = write_gguf(d.path(), "Qwen3.5-4B.gguf", "qwen35", 0);
        let _other_unsafe = write_gguf(d.path(), "Mamba-3B.gguf", "mamba2", 100);
        let _embed = write_gguf(d.path(), "embed.gguf", "qwen3", 100);
        assert_eq!(
            choose_cpu_safe_chat_model(&configured, true, d.path()),
            ChatModelChoice::NoSafeModel { unsafe_arch: "qwen35".to_string() }
        );
    }
}
