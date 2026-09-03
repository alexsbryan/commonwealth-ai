// SPDX-License-Identifier: AGPL-3.0-or-later
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
    let weights_bytes = std::fs::metadata(&plan.path)?.len();
    Ok(estimate_slot_vram_from_bytes(
        weights_bytes,
        plan.n_ctx,
        plan.n_seq_max,
    ))
}

/// The estimate for a slot whose weights size is already KNOWN, without
/// touching the filesystem.
///
/// This is where the formula lives; [`estimate_slot_vram`] is a `stat` in
/// front of it. The split exists so a machine that does not exist yet can be
/// sized — renting a GPU (`scripts/dev-pod.sh`) has to pick a card BEFORE any
/// GGUF is on it, and the loadout there declares exact byte counts. Without
/// this entry point the caller's only option is to re-derive `weights/8`, the
/// context scale and the scratch tiers in another language, which is two
/// implementations of one threshold (ARCH §10.6).
pub fn estimate_slot_vram_from_bytes(
    weights_bytes: u64,
    n_ctx: u32,
    n_seq_max: u32,
) -> VramEstimate {
    let weights_mb = weights_bytes / (1024 * 1024);

    // KV cache proxy: `weights_mb / 8` at 32K context per sequence.
    // The /8 figure is a back-fit from the L40S measurement —
    // similar-architecture transformers (40-80 layers, GQA group ≈
    // 8) land within 30% of this number, and the safety reserve
    // absorbs the rest.
    //
    // DELIBERATELY still a proxy, unlike the runtime gates. The load-path
    // gates (`shard_fits`, `local_fit_verdict`, the spawn gate) now judge
    // with llama.cpp's own three-term projection (`projected_overheads`),
    // which measured the /8 figure ~5× over on an MLA model. This preflight
    // cannot follow them: it runs before `LlamaBackend::init`, and the
    // binding treats a second init as an error, so an FFI projection here
    // would poison the engine's own startup. A warn-only boot estimate is
    // an acceptable place for a ±5× proxy; a load refusal is not.
    let ctx_factor = n_ctx as f32 / 32_768.0;
    let kv_per_seq_mb = ((weights_mb as f32 / 8.0) * ctx_factor).max(64.0) as u64;
    let kv_cache_mb = kv_per_seq_mb * n_seq_max.max(1) as u64;

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

    VramEstimate {
        weights_mb,
        kv_cache_mb,
        scratch_mb,
    }
}

/// Aggregated capacity report — one row per slot plus a verdict.
/// `fits == false` means the config overcommits detected VRAM. By
/// default the daemon warns and starts anyway; it only refuses under
/// `SOVEREIGN_STRICT_VRAM_CHECK=1` or when a slot's file is unreadable.
/// The rendered message includes the per-slot breakdown so the operator
/// can pick what to drop.
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
    /// The per-slot breakdown, header row included, in MiB.
    ///
    /// Every renderer of this report shares it — the daemon's refusal
    /// message and `svrn daemon vram-plan` alike. It is one table because a
    /// second copy of the column widths in the CLI drifted from this one the
    /// moment either was touched.
    pub fn slot_table(&self) -> String {
        let mut s = format!(
            "  {:<22} {:>8} {:>8} {:>8} {:>8}\n",
            "slot", "weights", "kv", "scratch", "total",
        );
        for (role, est) in &self.per_slot {
            s.push_str(&format!(
                "  {:<22} {:>8} {:>8} {:>8} {:>8}\n",
                role,
                est.weights_mb,
                est.kv_cache_mb,
                est.scratch_mb,
                est.total_mb(),
            ));
        }
        s
    }

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
            self.available_mb, self.safety_reserved_mb,
        ));
        s.push_str(&format!("  required:    {} MiB\n", self.total_required_mb,));
        s.push_str(&format!(
            "  overcommit:  {} MiB\n\n",
            self.total_required_mb.saturating_sub(self.available_mb),
        ));
        s.push_str("Per-slot estimates (MiB):\n");
        s.push_str(&self.slot_table());
        s.push_str("\nSuggested actions:\n");
        s.push_str("  - Reduce [models.primary_pool].copies if set\n");
        s.push_str("  - Switch primary to a smaller quant (Q4_K_M < Q4_K_L < Q5_K_M < Q6_K)\n");
        s.push_str("  - Lower [models].context_size if 32K isn't strictly required\n");
        s.push_str("  - Remove an unused slot (fast, code, or extras)\n");
        s.push_str("  - Set SOVEREIGN_SKIP_VRAM_CHECK=1 to bypass (you accept thrash risk)\n");
        s
    }

    /// Operator-facing message for the DEFAULT (advisory) posture: the
    /// config overcommits the detected VRAM budget, but the daemon is
    /// starting anyway. A concise caution rather than the full refusal
    /// breakdown — on CPU-only or low-VRAM machines this fires on every
    /// boot, so it stays short. Empty when the config fits.
    pub fn warn_message(&self) -> String {
        if self.fits {
            return String::new();
        }
        let mut s = String::new();
        s.push_str("VRAM capacity check: this configuration exceeds detected VRAM.\n");
        s.push_str(
            "Starting anyway (advisory). On CPU-only or low-VRAM machines this is \
             expected — models run on host RAM, just slower.\n",
        );
        s.push_str(&format!(
            "  available:   {} MiB (after {} MiB safety reserved)\n",
            self.available_mb, self.safety_reserved_mb,
        ));
        s.push_str(&format!("  required:    {} MiB\n", self.total_required_mb));
        s.push_str(&format!(
            "  overcommit:  {} MiB\n",
            self.total_required_mb.saturating_sub(self.available_mb),
        ));
        s
    }

    /// True when a slot's model file could not be read (missing or moved
    /// GGUF). `check_fit` flags such slots by prefixing their role with
    /// `(UNREADABLE`. This is a hard error under every posture — the file
    /// problem, not a capacity margin, is what needs fixing — so the
    /// preflight refuses even in advisory mode.
    pub fn has_unreadable_slot(&self) -> bool {
        self.per_slot
            .iter()
            .any(|(role, _)| role.contains("(UNREADABLE"))
    }
}

/// Run the planner against a slot mix and the detected hardware.
/// Returns a report; callers decide whether to honor `fits`. The
/// daemon's startup path is advisory by default — `fits == false`
/// prints a warning and starts anyway — and only hard-refuses under
/// `SOVEREIGN_STRICT_VRAM_CHECK=1` (or a genuinely unreadable model
/// file). See `daemon_cmd::build::preflight::check_vram`.
/// The safety reservation subtracted from a card's raw VRAM before any
/// fit verdict. Covers CUDA context (~300-500 MB), cuBLAS workspace
/// (~200 MB), GGML scratch we can't size from the outside, plus estimator
/// error. 8% of VRAM, floored at 768 MB so an 8 GB card still reserves
/// enough — a 1 GB floor would leave the friend's RTX 3060 at 7 GB
/// available, barely fitting a 5 GB fast slot + embed.
///
/// ONE decider: `check_fit`, `check_fit_sized` and `min_total_vram_mb` all
/// route through this rather than each spelling `/12` and `768` again.
pub fn safety_reserved_mb(available_total_mb: u64) -> u64 {
    (available_total_mb / 12).max(768)
}

/// The smallest card, in MiB of raw VRAM, on which `total_required_mb`
/// still fits once [`safety_reserved_mb`] is held back — the INVERSE of the
/// fit verdict.
///
/// This is the question a rental asks: not "does it fit on the card I have"
/// but "which card do I need". `scripts/dev-pod.sh` turns the answer into
/// the `gpu_ram>=` floor of its offer search, so the box that gets rented is
/// derived from the loadout instead of a hardcoded 46 GB that silently stops
/// being true the moment someone configures a different model.
pub fn min_total_vram_mb(total_required_mb: u64) -> u64 {
    // Two branches, because the reserve is a max() of a proportional term
    // and a floor. Try the floor branch first; if the resulting card is big
    // enough that 1/12 of it exceeds the floor, solve the proportional one
    // instead. The loop is a handful of iterations of integer correction,
    // not a search — it exists so this can never disagree with
    // `safety_reserved_mb`, whatever that rule becomes later.
    let with_floor = total_required_mb.saturating_add(768);
    if safety_reserved_mb(with_floor) == 768 {
        return with_floor;
    }
    let mut total = total_required_mb.saturating_mul(12) / 11;
    while total.saturating_sub(safety_reserved_mb(total)) < total_required_mb {
        total += 1;
    }
    total
}

/// A slot whose weights size is already KNOWN — the planning counterpart of
/// [`SlotPlan`], for sizing hardware that does not exist yet.
#[derive(Debug, Clone)]
pub struct SizedSlot {
    pub role: String,
    pub weights_bytes: u64,
    pub n_seq_max: u32,
    pub n_ctx: u32,
}

/// [`check_fit`] against a HYPOTHETICAL card of `available_total_mb` raw
/// VRAM, for slots whose sizes are declared rather than on this disk.
pub fn check_fit_sized(slots: &[SizedSlot], available_total_mb: u64) -> CapacityReport {
    let per_slot: Vec<(String, VramEstimate)> = slots
        .iter()
        .map(|s| {
            (
                s.role.clone(),
                estimate_slot_vram_from_bytes(s.weights_bytes, s.n_ctx, s.n_seq_max),
            )
        })
        .collect();
    let total_required_mb = per_slot.iter().map(|(_, e)| e.total_mb()).sum();
    finish_report(per_slot, total_required_mb, available_total_mb)
}

/// Apply the margin and the verdict. The one place either is decided.
fn finish_report(
    per_slot: Vec<(String, VramEstimate)>,
    total_required_mb: u64,
    available_total_mb: u64,
) -> CapacityReport {
    let safety_reserved_mb = safety_reserved_mb(available_total_mb);
    let available_mb = available_total_mb.saturating_sub(safety_reserved_mb);
    CapacityReport {
        per_slot,
        total_required_mb,
        available_mb,
        safety_reserved_mb,
        fits: total_required_mb <= available_mb,
    }
}

pub fn check_fit(slots: &[SlotPlan], hw: &HardwareProfile) -> CapacityReport {
    let available_total_mb = (hw.effective_vram_gb() as u64) * 1024;

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

    finish_report(per_slot, total_required_mb, available_total_mb)
}

/// Build the `Vec<SlotPlan>` the daemon would actually load from a
/// `SetupConfig`. Mirrors the slot enumeration in
/// `sovereign-mesh::daemon::register_local_model_slots` — keep them
/// in sync when adding a new slot kind. We live in
/// sovereign-inference rather than sovereign-mesh because the
/// dependency arrow runs core ← inference ← mesh; reaching the
/// other way would cycle.
pub fn build_slots_from_config(cfg: &sovereign_core::setup_config::SetupConfig) -> Vec<SlotPlan> {
    // A node with no `[models]` loads no slots, so it plans none. Empty is the
    // whole answer for a `terminal`: `check_fit` over zero slots requires zero
    // bytes and therefore FITS, which is what lets the daemon boot on a machine
    // that will never hold a GGUF. It is also why the terminal path does not
    // need `SOVEREIGN_SKIP_VRAM_CHECK` — nothing is being skipped, there is
    // genuinely nothing to weigh.
    let Some(models) = cfg.models.as_ref() else {
        return Vec::new();
    };

    // Primary's context is the operator-configured one; everything
    // else stays at 8K (fast/embed don't benefit from long ctx and
    // would balloon KV cache estimates well past their real cost).
    let primary_ctx = models.effective_context_size();

    let mut slots = Vec::new();
    slots.push(SlotPlan {
        role: "primary".into(),
        path: models.primary.clone(),
        n_seq_max: 1,
        n_ctx: primary_ctx,
    });
    // Fast subsume: when no explicit fast model is configured, the
    // primary slot serves fast-routed requests too — no separate
    // weights or context to budget for, so the fast SlotPlan is
    // skipped. (The primary SlotPlan already covers the weights;
    // primary's KV cache also handles the few extra concurrent
    // short calls fast normally would.)
    if models.has_explicit_fast() {
        slots.push(SlotPlan {
            role: "fast".into(),
            path: models.fast_path().to_path_buf(),
            n_seq_max: 8,
            n_ctx: 8_192,
        });
    }
    slots.push(SlotPlan {
        role: "embed".into(),
        path: models.embed.clone(),
        n_seq_max: 8,
        n_ctx: 8_192,
    });
    if let Some(code) = &models.code {
        slots.push(SlotPlan {
            role: "code".into(),
            path: code.clone(),
            n_seq_max: 1,
            n_ctx: primary_ctx,
        });
    }
    if let Some(pool) = &models.primary_pool {
        for i in 0..pool.copies {
            slots.push(SlotPlan {
                role: format!("primary_pool[{i}]"),
                path: pool.path.clone(),
                n_seq_max: 1,
                n_ctx: primary_ctx,
            });
        }
    }
    for (name, path) in &models.extra {
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
        let f = tempfile::Builder::new().suffix(".gguf").tempfile().unwrap();
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
            SlotPlan {
                role: "primary".into(),
                path: primary.path().into(),
                n_seq_max: 1,
                n_ctx: 32_768,
            },
            SlotPlan {
                role: "fast".into(),
                path: fast.path().into(),
                n_seq_max: 4,
                n_ctx: 8_192,
            },
            SlotPlan {
                role: "embed".into(),
                path: embed.path().into(),
                n_seq_max: 8,
                n_ctx: 8_192,
            },
        ];
        let r = check_fit(&slots, &synthetic_hw(48.0));
        assert!(
            r.fits,
            "1-copy + fast + embed should fit in 48 GB: {}",
            r.refuse_message()
        );
    }

    #[test]
    fn refuses_l40s_two_primary_copies_q4_thrash_regression() {
        // The exact incident this module was written to prevent.
        // 2 × FINAL-Bench_Darwin-36B-Opus-Q4_K_L on a 48 GB L40S
        // looked fine at idle but thrashed under the first
        // in-flight request. Planner must refuse.
        let primary = fake_gguf(21_000);
        let slots = vec![
            SlotPlan {
                role: "primary_0".into(),
                path: primary.path().into(),
                n_seq_max: 1,
                n_ctx: 32_768,
            },
            SlotPlan {
                role: "primary_1".into(),
                path: primary.path().into(),
                n_seq_max: 1,
                n_ctx: 32_768,
            },
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
        let slots = vec![SlotPlan {
            role: "primary".into(),
            path: primary.path().into(),
            n_seq_max: 1,
            n_ctx: 32_768,
        }];
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
            SlotPlan {
                role: "fast".into(),
                path: fast.path().into(),
                n_seq_max: 2,
                n_ctx: 8_192,
            },
            SlotPlan {
                role: "embed".into(),
                path: embed.path().into(),
                n_seq_max: 4,
                n_ctx: 8_192,
            },
        ];
        let r = check_fit(&slots, &synthetic_hw(8.0));
        assert!(
            r.fits,
            "8GB card should fit 9B-Q4 + embed: {}",
            r.refuse_message()
        );
    }

    #[test]
    fn unreadable_file_forces_refusal() {
        // A configured slot pointing at a non-existent file should
        // be a hard error, not a "fits because we counted 0".
        let slots = vec![SlotPlan {
            role: "primary".into(),
            path: "/nonexistent/model.gguf".into(),
            n_seq_max: 1,
            n_ctx: 32_768,
        }];
        let r = check_fit(&slots, &synthetic_hw(48.0));
        assert!(!r.fits);
        let msg = r.refuse_message();
        assert!(
            msg.contains("UNREADABLE"),
            "must surface the file error: {}",
            msg
        );
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

    #[test]
    fn min_total_vram_is_the_exact_inverse_of_the_fit_verdict() {
        // The property that makes `min_total_vram_mb` safe to spend money on:
        // the card it names FITS, and one MiB less does NOT. Checked across
        // both branches of the reserve rule (the 768 MB floor for small
        // loadouts, the 1/12 proportional term for large ones) — a rounding
        // error in the branch selection would rent a box that OOMs on first
        // request, which is the 2026-05-11 thrash incident with a bill.
        for required_mb in [500u64, 1_000, 5_000, 8_448, 8_449, 20_000, 35_000, 70_000] {
            let need = min_total_vram_mb(required_mb);
            // Drive the verdict arithmetic directly rather than through the
            // estimator, so this tests the inverse and not the KV proxy.
            let at = finish_report(vec![], required_mb, need);
            let below = finish_report(vec![], required_mb, need - 1);
            assert!(at.fits, "{required_mb} MiB should fit a {need} MiB card");
            assert!(
                !below.fits,
                "{required_mb} MiB must NOT fit {} MiB — {need} is not minimal",
                need - 1
            );
        }
    }

    #[test]
    fn the_sized_path_and_the_file_path_agree() {
        // `estimate_slot_vram_from_bytes` is the formula and
        // `estimate_slot_vram` is a stat in front of it. If these ever
        // diverge, the pod is sized by one rule and the daemon boots under
        // another — the two-implementations smell this split was made to
        // avoid (ARCH §10.6).
        let f = fake_gguf(21_000);
        let via_file = estimate_slot_vram(&SlotPlan {
            role: "primary".into(),
            path: f.path().into(),
            n_seq_max: 1,
            n_ctx: 32_768,
        })
        .unwrap();
        let via_bytes = estimate_slot_vram_from_bytes(21_000 * 1024 * 1024, 32_768, 1);
        assert_eq!(via_file.weights_mb, via_bytes.weights_mb);
        assert_eq!(via_file.kv_cache_mb, via_bytes.kv_cache_mb);
        assert_eq!(via_file.scratch_mb, via_bytes.scratch_mb);
    }

    #[test]
    fn the_founder_loadout_needs_a_48g_card_and_not_a_24g_one() {
        // The loadout `scripts/dev-pod.sh` rents for: the 35B-A3B primary,
        // the 4B fast slot and the 0.6B embed, all byte-exact with RuggedFox.
        // Named here because the pod's offer floor is derived from exactly
        // this sum — if the estimate ever drifts under a 24 GB card the
        // script would start renting boxes that cannot hold the bench.
        let slots = vec![
            SizedSlot {
                role: "primary".into(),
                weights_bytes: 30_011_242_784,
                n_seq_max: 1,
                n_ctx: 32_768,
            },
            SizedSlot {
                role: "fast".into(),
                weights_bytes: 4_261_908_800,
                n_seq_max: 1,
                n_ctx: 32_768,
            },
            SizedSlot {
                role: "embed".into(),
                weights_bytes: 639_150_592,
                n_seq_max: 1,
                n_ctx: 32_768,
            },
        ];
        let need_mb = check_fit_sized(&slots, 0).total_required_mb;
        let min_mb = min_total_vram_mb(need_mb);
        assert!(
            min_mb > 24 * 1024,
            "loadout claimed to fit a 24 GB card ({min_mb} MiB) — estimate drifted"
        );
        assert!(
            min_mb <= 48 * 1024,
            "loadout no longer fits the 48 GB class ({min_mb} MiB) — dev-pod's \
             offer search would find nothing"
        );
        assert!(check_fit_sized(&slots, 48 * 1024).fits);
        assert!(!check_fit_sized(&slots, 24 * 1024).fits);
    }
}
