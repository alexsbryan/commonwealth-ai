//! VRAM working-set planner — refuse-to-load if the configured slot
//! mix wouldn't fit on the detected GPU with safety margin.
//!
//! This module exists because the load-time test ("does the model
//! file fit in VRAM bytes?") is not the right question. The real
//! question is "does the *working set* fit?" — weights plus KV cache
//! per concurrent sequence plus compute scratch plus the grammar
//! workspace. Each layer adds GB of live VRAM that the disk size
//! doesn't reveal.
//!
//! Empirical anchor (2026-05-11 L40S thrash incident):
//!   - 2 × FINAL-Bench_Darwin-36B-Opus-Q4_K_L (file size 21 GB)
//!     loaded comfortably at 37.8 GB idle on a 48 GB L40S.
//!   - First inference request pushed past 45 GB → CUDA OOM →
//!     one copy evicted → next request faulted it back in → ~3 min
//!     reload cycle for the next hour → 1 successful slug out of
//!     1235 attempts.
//!   - Working-set per slot above weights ≈ 3-4 GB (KV cache for
//!     32K context + 1.5 GB grammar/compute scratch).
//!
//! The estimates here intentionally err conservative. The cost of
//! refusing a config that would have *just* fit is the operator
//! editing `primary_pool.copies` or `context_size`. The cost of
//! letting a thrashing config load is what we lived through above.

use crate::hardware::HardwareProfile;
use std::path::PathBuf;

/// One configured slot the daemon would eagerly load at boot.
/// Built from `SetupConfig` (or a synthetic test config) — this
/// module doesn't reach into setup_config directly so the
/// dependency arrow stays inference → core.
#[derive(Debug, Clone)]
pub struct SlotPlan {
    /// Operator-facing label — `primary`, `fast`, `embed`,
    /// `primary_1`, `extras:coder`, etc. Surfaced in the refuse
    /// message so the operator knows which slot to edit out.
    pub role: String,
    pub path: PathBuf,
    /// `n_seq_max` for this slot. Primary slots set 1 (long-context
    /// preserved); embed slots can be 8-16 to amortise batches.
    pub n_seq_max: u32,
    /// Context window the slot will be configured with. Larger
    /// windows linearly scale KV cache. Default 32K for primary,
    /// 8K for embed.
    pub n_ctx: u32,
}

/// VRAM cost breakdown for one slot. All values in MiB so they
/// add cleanly into a single number without floating-point drift.
#[derive(Debug, Clone, Copy, Default)]
pub struct VramEstimate {
    /// Disk size of the GGUF, used directly. Q4-Q8 quants land
    /// in VRAM within ~5% of file size; for the planner this is
    /// close enough that the extra rounding is absorbed by the
    /// safety margin.
    pub weights_mb: u64,
    /// KV cache for the slot. Scales with `n_seq_max × n_ctx`.
    /// The constant out front is calibrated to the L40S
    /// observation (~2 GB per seq at 32K on a 36B Q4): roughly
    /// `n_layers × 2 × n_kv_heads × head_dim × n_ctx × 2 bytes`
    /// at FP16. We use a multiplicative proxy keyed on weights
    /// size to avoid parsing GGUF headers — same-class models
    /// have similar KV needs per byte of weights.
    pub kv_cache_mb: u64,
    /// Compute scratch (matmul workspace, activation buffers,
    /// grammar mask buffer for in-house constrained decoding).
    /// Constant per slot regardless of model size — bounded by
    /// the largest intermediate tensor, not the weights.
    pub scratch_mb: u64,
}

impl VramEstimate {
    pub fn total_mb(&self) -> u64 {
        self.weights_mb + self.kv_cache_mb + self.scratch_mb
    }
}

/// Per-slot working-set estimate. Conservative; meant to catch
/// the over-commit case, not to be a precise VRAM accountant.
///
/// **Calibration anchor** (2026-05-11 L40S thrash):
///   - 2 × Q4_K_L 36B (21 GB file) idled at 37.8 GB on a 48 GB
///     card. Per-slot **idle** working set ≈ weights only;
///     llama.cpp lazy-allocates KV on demand.
///   - Each in-flight request added ~3-4 GB → thrash threshold
///     at total ≈ 45 GB.
///   - So per-slot **under-load** working set above weights ≈
///     2-3 GB (KV at 32K, 1 seq) + 1 GB (scratch + grammar).
///
/// The KV proxy below is `weights_mb / 8` at the reference 32K
/// context, scaled linearly by actual `n_ctx`. For a 21 GB primary
/// at 32K, 1 seq that's 2.6 GB — matches observation.
pub fn estimate_slot_vram(plan: &SlotPlan) -> std::io::Result<VramEstimate> {
    let weights_mb = std::fs::metadata(&plan.path)?.len() / (1024 * 1024);

    // KV cache proxy: `weights_mb / 8` at 32K context per sequence.
    // The /8 figure is a back-fit from the L40S measurement —
    // similar-architecture transformers (40-80 layers, GQA group ≈
    // 8) land within 30% of this number, and the safety reserve
    // absorbs the rest.
    let ctx_factor = plan.n_ctx as f32 / 32_768.0;
    let kv_per_seq_mb = ((weights_mb as f32 / 8.0) * ctx_factor).max(64.0) as u64;
    let kv_cache_mb = kv_per_seq_mb * plan.n_seq_max.max(1) as u64;

    // Scratch: compute buffers + grammar mask + activation peak.
    // Scales with model size since intermediate activations are
    // proportional to hidden_dim². Three tiers cover the realistic
    // span without needing per-arch detail.
    let scratch_mb = if weights_mb < 2_000 {
        // <2 GB: tiny models, no grammar (embed slots), bounded.
        200
    } else if weights_mb < 8_000 {
        // 2-8 GB: 7-9B-class chat models, includes grammar mask.
        500
    } else {
        // >8 GB: 27B+ where activations dominate.
        1_000
    };

    Ok(VramEstimate {
        weights_mb,
        kv_cache_mb,
        scratch_mb,
    })
}

/// Aggregated capacity report — one row per slot plus a verdict.
/// `fits == false` means the daemon will refuse to start under
/// this config; the rendered message includes the per-slot
/// breakdown so the operator can pick what to drop.
#[derive(Debug, Clone)]
pub struct CapacityReport {
    pub per_slot: Vec<(String, VramEstimate)>,
    pub total_required_mb: u64,
    pub available_mb: u64,
    /// Safety reservation we subtract from raw VRAM before
    /// declaring fit. Covers everything the planner can't see —
    /// CUDA context, cuBLAS workspace, GGML scratch buffers
    /// allocated during the first real request — plus a buffer
    /// against estimation error. 8% of detected VRAM, floored at
    /// 1024 MB.
    pub safety_reserved_mb: u64,
    pub fits: bool,
}

impl CapacityReport {
    /// Operator-facing message describing why a config was refused,
    /// listing each slot's contribution and suggesting concrete
    /// actions. Empty when the config fits.
    pub fn refuse_message(&self) -> String {
        if self.fits {
            return String::new();
        }
        let mut s = String::new();
        s.push_str("VRAM capacity check refused this configuration:\n");
        s.push_str(&format!(
            "  available:   {} MiB (after {} MiB safety reserved)\n",
            self.available_mb,
            self.safety_reserved_mb,
        ));
        s.push_str(&format!(
            "  required:    {} MiB\n",
            self.total_required_mb,
        ));
        s.push_str(&format!(
            "  overcommit:  {} MiB\n\n",
            self.total_required_mb.saturating_sub(self.available_mb),
        ));
        s.push_str("Per-slot estimates (MiB):\n");
        s.push_str(&format!(
            "  {:<22} {:>8} {:>8} {:>8} {:>8}\n",
            "slot", "weights", "kv", "scratch", "total",
        ));
        for (role, est) in &self.per_slot {
            s.push_str(&format!(
                "  {:<22} {:>8} {:>8} {:>8} {:>8}\n",
                role, est.weights_mb, est.kv_cache_mb, est.scratch_mb, est.total_mb(),
            ));
        }
        s.push_str("\nSuggested actions:\n");
        s.push_str("  - Reduce [models.primary_pool].copies if set\n");
        s.push_str("  - Switch primary to a smaller quant (Q4_K_M < Q4_K_L < Q5_K_M < Q6_K)\n");
        s.push_str("  - Lower [models].context_size if 32K isn't strictly required\n");
        s.push_str("  - Remove an unused slot (fast, code, or extras)\n");
        s.push_str("  - Set SOVEREIGN_SKIP_VRAM_CHECK=1 to bypass (you accept thrash risk)\n");
        s
    }
}

/// Run the planner against a slot mix and the detected hardware.
/// Returns a report; callers decide whether to honor `fits`. The
/// daemon's startup path treats `fits == false` as a hard error
/// unless `SOVEREIGN_SKIP_VRAM_CHECK` is set.
pub fn check_fit(slots: &[SlotPlan], hw: &HardwareProfile) -> CapacityReport {
    let available_total_mb = (hw.effective_vram_gb() as u64) * 1024;
    // Safety reserve covers CUDA context (~300-500 MB), cuBLAS
    // workspace (~200 MB), GGML scratch we can't size from the
    // outside, plus estimator error. 8% of detected VRAM, floored
    // at 768 MB so an 8 GB card still reserves enough — a 1 GB
    // floor would leave the friend's RTX 3060 at 7 GB available,
    // barely fitting a 5 GB fast slot + embed.
    let safety_reserved_mb = (available_total_mb / 12).max(768);
    let available_mb = available_total_mb.saturating_sub(safety_reserved_mb);

    let mut per_slot = Vec::with_capacity(slots.len());
    let mut total_required_mb: u64 = 0;
    for plan in slots {
        // Missing-file errors are common during config drift
        // (renamed GGUF, moved disk, etc.). Surface them in the
        // report rather than silently zero-budgeting the slot —
        // a missing file is its own refusal-grade problem.
        let est = match estimate_slot_vram(plan) {
            Ok(e) => e,
            Err(e) => {
                per_slot.push((
                    format!("{} (UNREADABLE: {})", plan.role, e),
                    VramEstimate::default(),
                ));
                // Force refusal so the operator sees the file
                // problem rather than the planner pretending the
                // slot is free.
                total_required_mb = total_required_mb.saturating_add(u64::MAX / 2);
                continue;
            }
        };
        total_required_mb += est.total_mb();
        per_slot.push((plan.role.clone(), est));
    }

    let fits = total_required_mb <= available_mb;
    CapacityReport {
        per_slot,
        total_required_mb,
        available_mb,
        safety_reserved_mb,
        fits,
    }
}

/// Build the `Vec<SlotPlan>` the daemon would actually load from a
/// `SetupConfig`. Mirrors the slot enumeration in
/// `sovereign-mesh::daemon::register_local_model_slots` — keep them
/// in sync when adding a new slot kind. We live in
/// sovereign-inference rather than sovereign-mesh because the
/// dependency arrow runs core ← inference ← mesh; reaching the
/// other way would cycle.
pub fn build_slots_from_config(
    cfg: &sovereign_core::setup_config::SetupConfig,
) -> Vec<SlotPlan> {
    // Primary's context is the operator-configured one; everything
    // else stays at 8K (fast/embed don't benefit from long ctx and
    // would balloon KV cache estimates well past their real cost).
    let primary_ctx = cfg.models.effective_context_size();

    let mut slots = Vec::new();
    slots.push(SlotPlan {
        role: "primary".into(),
        path: cfg.models.primary.clone(),
        n_seq_max: 1,
        n_ctx: primary_ctx,
    });
    // Fast subsume: when no explicit fast model is configured, the
    // primary slot serves fast-routed requests too — no separate
    // weights or context to budget for, so the fast SlotPlan is
    // skipped. (The primary SlotPlan already covers the weights;
    // primary's KV cache also handles the few extra concurrent
    // short calls fast normally would.)
    if cfg.models.has_explicit_fast() {
        slots.push(SlotPlan {
            role: "fast".into(),
            path: cfg.models.fast_path().to_path_buf(),
            n_seq_max: 8,
            n_ctx: 8_192,
        });
    }
    slots.push(SlotPlan {
        role: "embed".into(),
        path: cfg.models.embed.clone(),
        n_seq_max: 8,
        n_ctx: 8_192,
    });
    if let Some(code) = &cfg.models.code {
        slots.push(SlotPlan {
            role: "code".into(),
            path: code.clone(),
            n_seq_max: 1,
            n_ctx: primary_ctx,
        });
    }
    if let Some(pool) = &cfg.models.primary_pool {
        for i in 0..pool.copies {
            slots.push(SlotPlan {
                role: format!("primary_pool[{i}]"),
                path: pool.path.clone(),
                n_seq_max: 1,
                n_ctx: primary_ctx,
            });
        }
    }
    for (name, path) in &cfg.models.extra {
        slots.push(SlotPlan {
            role: format!("extras:{name}"),
            path: path.clone(),
            n_seq_max: 4,
            n_ctx: 8_192,
        });
    }
    slots
}

/// True iff the operator has opted out of the planner via env.
/// Honored at the daemon-start call site, not inside `check_fit`
/// itself — the planner always produces a report so logs/diagnostics
/// stay useful even when enforcement is off.
pub fn check_skipped_by_env() -> bool {
    std::env::var("SOVEREIGN_SKIP_VRAM_CHECK")
        .map(|v| !v.is_empty() && v != "0" && v.to_lowercase() != "false")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_hw(vram_gb: f32) -> HardwareProfile {
        HardwareProfile {
            system_ram_bytes: 64 * 1024 * 1024 * 1024,
            gpu_available: vram_gb > 0.0,
            gpu_name: Some("synthetic".into()),
            gpu_memory_bytes: Some((vram_gb * 1_073_741_824.0) as u64),
            recommended_gpu_layers: 999,
            is_unified_memory: false,
        }
    }

    /// Write a tempfile with the given size in MiB so we can probe
    /// the file-size-based weight estimate without shipping real
    /// GGUFs. Uses `set_len` so the file is sparse on Linux/macOS
    /// — a 21 GB "model" costs zero disk bytes and finishes
    /// instantly, only the inode's reported length matters for
    /// the planner.
    fn fake_gguf(size_mb: u64) -> tempfile::NamedTempFile {
        let f = tempfile::Builder::new()
            .suffix(".gguf")
            .tempfile()
            .unwrap();
        let len_bytes = size_mb * 1024 * 1024;
        f.as_file().set_len(len_bytes).unwrap();
        f
    }

    #[test]
    fn fits_in_comfortable_headroom() {
        // L40S 48 GB with a single 21 GB Q4 36B + 9B fast + embed.
        // Matches the eventual "drop to 1 copy" config we settled
        // on after the thrash incident.
        let primary = fake_gguf(21_000);
        let fast = fake_gguf(10_000);
        let embed = fake_gguf(700);
        let slots = vec![
            SlotPlan { role: "primary".into(), path: primary.path().into(), n_seq_max: 1, n_ctx: 32_768 },
            SlotPlan { role: "fast".into(),    path: fast.path().into(),    n_seq_max: 4, n_ctx: 8_192 },
            SlotPlan { role: "embed".into(),   path: embed.path().into(),   n_seq_max: 8, n_ctx: 8_192 },
        ];
        let r = check_fit(&slots, &synthetic_hw(48.0));
        assert!(r.fits, "1-copy + fast + embed should fit in 48 GB: {}", r.refuse_message());
    }

    #[test]
    fn refuses_l40s_two_primary_copies_q4_thrash_regression() {
        // The exact incident this module was written to prevent.
        // 2 × FINAL-Bench_Darwin-36B-Opus-Q4_K_L on a 48 GB L40S
        // looked fine at idle but thrashed under the first
        // in-flight request. Planner must refuse.
        let primary = fake_gguf(21_000);
        let slots = vec![
            SlotPlan { role: "primary_0".into(), path: primary.path().into(), n_seq_max: 1, n_ctx: 32_768 },
            SlotPlan { role: "primary_1".into(), path: primary.path().into(), n_seq_max: 1, n_ctx: 32_768 },
        ];
        let r = check_fit(&slots, &synthetic_hw(48.0));
        assert!(
            !r.fits,
            "2 × Q4 36B at 32K must refuse on 48 GB L40S — that's the regression. report:\n{}",
            r.refuse_message()
        );
    }

    #[test]
    fn refuses_8gb_card_with_36b_q4_primary() {
        // Friend's 8 GB RTX 3060 with a 36B Q4 primary — way over.
        // Confirms the planner also catches the obvious case, not
        // just the subtle tight-margin case.
        let primary = fake_gguf(21_000);
        let slots = vec![
            SlotPlan { role: "primary".into(), path: primary.path().into(), n_seq_max: 1, n_ctx: 32_768 },
        ];
        let r = check_fit(&slots, &synthetic_hw(8.0));
        assert!(!r.fits);
        assert!(r.total_required_mb > r.available_mb);
    }

    #[test]
    fn fits_9b_q4_on_friend_3060() {
        // What the 3060 *should* be running: a 5 GB Q4 9B for
        // fast-class work + the embed slot. Both fit easily.
        let fast = fake_gguf(5_000);
        let embed = fake_gguf(700);
        let slots = vec![
            SlotPlan { role: "fast".into(),  path: fast.path().into(),  n_seq_max: 2, n_ctx: 8_192 },
            SlotPlan { role: "embed".into(), path: embed.path().into(), n_seq_max: 4, n_ctx: 8_192 },
        ];
        let r = check_fit(&slots, &synthetic_hw(8.0));
        assert!(r.fits, "8GB card should fit 9B-Q4 + embed: {}", r.refuse_message());
    }

    #[test]
    fn unreadable_file_forces_refusal() {
        // A configured slot pointing at a non-existent file should
        // be a hard error, not a "fits because we counted 0".
        let slots = vec![
            SlotPlan { role: "primary".into(), path: "/nonexistent/model.gguf".into(), n_seq_max: 1, n_ctx: 32_768 },
        ];
        let r = check_fit(&slots, &synthetic_hw(48.0));
        assert!(!r.fits);
        let msg = r.refuse_message();
        assert!(msg.contains("UNREADABLE"), "must surface the file error: {}", msg);
    }

    #[test]
    fn skip_env_recognised_only_when_truthy() {
        std::env::remove_var("SOVEREIGN_SKIP_VRAM_CHECK");
        assert!(!check_skipped_by_env());

        std::env::set_var("SOVEREIGN_SKIP_VRAM_CHECK", "1");
        assert!(check_skipped_by_env());

        std::env::set_var("SOVEREIGN_SKIP_VRAM_CHECK", "0");
        assert!(!check_skipped_by_env());

        std::env::set_var("SOVEREIGN_SKIP_VRAM_CHECK", "false");
        assert!(!check_skipped_by_env());

        std::env::set_var("SOVEREIGN_SKIP_VRAM_CHECK", "");
        assert!(!check_skipped_by_env());

        std::env::remove_var("SOVEREIGN_SKIP_VRAM_CHECK");
    }
}
