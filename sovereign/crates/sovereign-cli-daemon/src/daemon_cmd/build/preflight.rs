//! VRAM capacity preflight — extracted from `run_daemon` (§3.3).
//!
//! Estimate the working set (weights + KV cache + grammar scratch) for
//! every slot the daemon would eagerly load, sum against detected VRAM
//! with a safety margin, and refuse to start if the config would
//! overcommit. Catches the 2026-05-11 L40S thrash class: 2 × Q4_K_L
//! "fit" at 38 GB idle but evicted each other under live KV pressure,
//! taking throughput to 1 slug/hr.
//!
//! Bypass via `SOVEREIGN_SKIP_VRAM_CHECK=1` when the operator knows
//! better than the planner (e.g. a slot mix where one model is
//! lazy-loaded behind a high idle_secs gate). The report still prints in
//! that case so the diagnosis stays visible in logs.

use sovereign_core::setup_config::SetupConfig;

/// Returns `false` when the config overcommits VRAM and the bypass env is
/// unset — the caller (`run_daemon`) should refuse to start (return 1).
/// Returns `true` to proceed (config fits, or the check was bypassed).
pub(crate) fn check_vram(config: &SetupConfig) -> bool {
    let hardware = sovereign_inference::hardware::HardwareProfile::detect();
    let slots = sovereign_inference::capacity::build_slots_from_config(config);
    let report = sovereign_inference::capacity::check_fit(&slots, &hardware);
    if !report.fits {
        if sovereign_inference::capacity::check_skipped_by_env() {
            tracing::warn!(
                required_mb = report.total_required_mb,
                available_mb = report.available_mb,
                "VRAM check would have refused this config — bypassed by SOVEREIGN_SKIP_VRAM_CHECK. \
                 Thrash risk accepted by operator."
            );
        } else {
            eprintln!("{}", report.refuse_message());
            eprintln!(
                "hint: edit {} and re-run, or set SOVEREIGN_SKIP_VRAM_CHECK=1 \
                 to bypass at your own risk.",
                SetupConfig::default_path().display(),
            );
            return false;
        }
    } else {
        tracing::info!(
            required_mb = report.total_required_mb,
            available_mb = report.available_mb,
            slots = report.per_slot.len(),
            "VRAM preflight: config fits"
        );
    }
    true
}
