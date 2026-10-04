// SPDX-License-Identifier: AGPL-3.0-or-later
//! What one slot's KV cache actually costs, computed from the gguf.
//!
//! # Why this exists
//!
//! On 2026-08-25 a peer measured 17.7 GB of KV + compute across this host's
//! four contexts and could not get the PER-CONTEXT split without raising
//! llama.cpp's own log level and rebooting the daemon to read it off the load
//! banner. That is a measurement the daemon should publish about itself
//! (ARCH §0.1 glassbox, principle 1): a cost that only exists in another
//! process's stderr is a cost nobody budgets against.
//!
//! The number is not a mystery — it is arithmetic over metadata the gguf
//! already carries:
//!
//! ```text
//! bytes = 2 (K and V) × n_ctx × n_layer × n_kv_head × head_dim × dtype_bytes
//! ```
//!
//! Every term but `n_ctx` is fixed by the model. `n_ctx` is the term this
//! daemon chooses, which is what makes it the lever: KV scales LINEARLY in the
//! window, so a slot carrying a window it never fills is paying full price for
//! it.
//!
//! # What this deliberately does not do
//!
//! It does not guess. When the gguf does not carry the keys — an architecture
//! that names them differently, or a stripped conversion — [`kv_footprint`]
//! returns `None` and the caller logs that it could not compute rather than
//! logging a plausible wrong number (ARCH §18.3, principle 6). An absent
//! measurement is reportable; a fabricated one silently becomes a budget.
//!
//! # Two reasons this is a CEILING, not a reading
//!
//! Both matter, because a number that reads as "allocated" but means "would be
//! allocated if fully used" quietly becomes a budget.
//!
//! **1. Allocation may be lazy.** This computes what a full window COSTS, not
//! what llama.cpp has committed at the moment you read the line. Measured on
//! the Halo 2026-08-25 (note `05cbffed`): the 27B showed ~6.6 GB of KV despite
//! carrying twice the layers of the 4B companion set, which is the wrong way
//! round for an eager allocator and points at KV being committed as sequences
//! are actually used. That asymmetry is NOT yet root-caused, so treat this
//! figure as the ceiling a slot can reach — which is the right number for
//! sizing a window, and the wrong one for answering "what is resident now".
//! For the latter, read GTT/RSS from the OS, not from here.
//!
//! **2. Hybrid architectures.**
//!
//! `n_layer` is the model's TOTAL block count, and the formula assumes every
//! block holds token-indexed KV. For a uniform attention model that is exact.
//! For a hybrid — Qwen3-Next-style gated-delta layers, Mamba/SSM blocks,
//! sliding-window interleaves — only a minority of layers contribute a growing
//! cache, so the figure is an UPPER BOUND and [`KvFootprint::upper_bound_only`]
//! says so. Reporting a hybrid's cache at its dense-equivalent size without
//! flagging it would overstate the cost of exactly the architectures that were
//! chosen to reduce it.

use crate::llama::cpp::model::LlamaModel;

/// Bytes per element of an f16 KV cache. llama.cpp's default for both K and V.
const F16_BYTES: u64 = 2;

/// Architectures whose blocks are not uniformly attention layers, so a
/// per-layer KV formula over the total block count is an upper bound.
///
/// Matched as substrings of `general.architecture`. Kept deliberately short:
/// a name that is not here yields an exact-looking figure, so the list is a
/// claim about what we have verified, not a taxonomy.
const NON_UNIFORM_KV_ARCHS: &[&str] = &["mamba", "rwkv", "ssm", "deltanet", "jamba", "recurrent"];

/// One slot's KV cache cost, with the terms that produced it.
///
/// The terms are carried rather than folded away so a reader can check the
/// arithmetic against llama.cpp's own banner instead of trusting this.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KvFootprint {
    pub(crate) n_ctx: u32,
    pub(crate) n_layer: u64,
    pub(crate) n_kv_head: u64,
    pub(crate) head_dim_k: u64,
    pub(crate) head_dim_v: u64,
    pub(crate) bytes: u64,
    /// `true` when the architecture's blocks are not uniformly attention
    /// layers — see the module docs. `bytes` is then an upper bound.
    pub(crate) upper_bound_only: bool,
}

impl KvFootprint {
    /// The ceiling in MiB. Named `ceiling` at every emit site rather than
    /// `kv_mib`, because a reader who sees `kv_mib` will budget against it as
    /// if it were a reading — see the module docs' two caveats.
    pub(crate) fn mib(&self) -> u64 {
        self.bytes / (1024 * 1024)
    }
}

fn meta_u64(model: &LlamaModel, key: &str) -> Option<u64> {
    model.meta_val_str(key, 64).ok()?.trim().parse::<u64>().ok()
}

/// Compute a slot's KV cache size, or `None` when the gguf does not carry the
/// metadata to do it honestly.
///
/// `n_ctx` is the window the slot was BUILT with, not the model's trained
/// maximum — the whole point is to price the choice this daemon made.
pub(crate) fn kv_footprint(model: &LlamaModel, n_ctx: u32) -> Option<KvFootprint> {
    let arch = model.meta_val_str("general.architecture", 64).ok()?;
    let n_layer = meta_u64(model, &format!("{arch}.block_count"))?;
    // GQA/MQA models publish a KV head count distinct from the attention head
    // count, and it is the KV one that sizes the cache. A model that publishes
    // neither is not one we can price.
    let n_kv_head = meta_u64(model, &format!("{arch}.attention.head_count_kv"))
        .or_else(|| meta_u64(model, &format!("{arch}.attention.head_count")))?;

    // Preferred: the explicit per-head key/value lengths. These are the only
    // correct source for MLA and other models where K and V differ in width.
    let explicit_k = meta_u64(model, &format!("{arch}.attention.key_length"));
    let explicit_v = meta_u64(model, &format!("{arch}.attention.value_length"));
    let (head_dim_k, head_dim_v) = match (explicit_k, explicit_v) {
        (Some(k), Some(v)) => (k, v),
        _ => {
            // Fallback: embedding width divided by the ATTENTION head count
            // (not the KV head count — under GQA the head dim is set by the
            // query heads and the KV heads share it).
            let n_embd = meta_u64(model, &format!("{arch}.embedding_length"))?;
            let n_head = meta_u64(model, &format!("{arch}.attention.head_count"))?;
            if n_head == 0 {
                return None;
            }
            let d = n_embd / n_head;
            (d, d)
        }
    };

    let per_token = n_layer * n_kv_head * (head_dim_k + head_dim_v) * F16_BYTES;
    let bytes = per_token * u64::from(n_ctx);
    let lower = arch.to_ascii_lowercase();
    Some(KvFootprint {
        n_ctx,
        n_layer,
        n_kv_head,
        head_dim_k,
        head_dim_v,
        bytes,
        upper_bound_only: NON_UNIFORM_KV_ARCHS.iter().any(|a| lower.contains(a)),
    })
}

/// Emit one slot's KV cost at construction.
///
/// Called from every site that builds a `LlamaContext`, so the daemon's total
/// is the sum of its own log lines rather than a number someone has to derive
/// from the outside.
pub(crate) fn trace_kv_footprint(model: &LlamaModel, slot: &str, n_ctx: u32) {
    match kv_footprint(model, n_ctx) {
        Some(f) => tracing::info!(
            slot,
            n_ctx = f.n_ctx,
            kv_ceiling_mib = f.mib(),
            n_layer = f.n_layer,
            n_kv_head = f.n_kv_head,
            head_dim_k = f.head_dim_k,
            head_dim_v = f.head_dim_v,
            upper_bound_only = f.upper_bound_only,
            "kv budget: slot context built"
        ),
        // Absence is reported, never defaulted (ARCH §18.3). A slot whose cost
        // we cannot compute must not silently contribute 0 to a total.
        None => tracing::info!(
            slot,
            n_ctx,
            "kv budget: gguf does not carry the metadata to price this slot"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The formula, checked against a hand-computed case rather than against
    /// itself. Llama-3-8B: 32 layers, 8 KV heads (GQA), head dim 128, f16.
    /// Per token = 32 × 8 × (128+128) × 2 = 131072 bytes = 128 KiB.
    /// At 8192 tokens that is exactly 1024 MiB.
    #[test]
    fn the_formula_matches_a_hand_computed_case() {
        let per_token = 32u64 * 8 * (128 + 128) * F16_BYTES;
        assert_eq!(per_token, 131_072);
        assert_eq!(per_token * 8192 / (1024 * 1024), 1024);
    }

    /// THE LEVER, as an assertion: KV is linear in the window, so a slot
    /// carrying 4× the context it needs costs 4× the cache. This is the whole
    /// reason per-slot sizing is worth doing, and it belongs in a test rather
    /// than in a comment (ARCH §7.2 — an assertion in prose is not a test).
    #[test]
    fn kv_is_linear_in_the_window() {
        let per_token = 32u64 * 8 * 256 * F16_BYTES;
        let at_16k = per_token * 16_384;
        let at_64k = per_token * 65_536;
        assert_eq!(at_64k, at_16k * 4);
    }

    #[test]
    fn hybrid_architectures_are_flagged_as_upper_bounds() {
        for arch in ["mamba2", "qwen3-deltanet", "rwkv6", "jamba"] {
            assert!(
                NON_UNIFORM_KV_ARCHS
                    .iter()
                    .any(|a| arch.to_ascii_lowercase().contains(a)),
                "{arch} should be flagged: its blocks are not uniformly attention layers"
            );
        }
        for arch in ["llama", "qwen2", "gemma3"] {
            assert!(
                !NON_UNIFORM_KV_ARCHS
                    .iter()
                    .any(|a| arch.to_ascii_lowercase().contains(a)),
                "{arch} is uniform attention; flagging it would overstate the caveat"
            );
        }
    }
}
