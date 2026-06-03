//! Adaptive concurrency — the driver dials its in-flight ceiling up
//! and down based on the rolling failure rate.
//!
//! ## Why
//!
//! The recipe's `[dispatch].concurrency` is a *ceiling*, not a target.
//! Operators pick it from a gut estimate of mesh capacity, which is
//! routinely too high (thrashes a small mesh) or too low (leaves
//! throughput on the table).
//!
//! ## How
//!
//! Each completion is fed in as `Outcome::Success` or one of the two
//! failure shapes. Only **elastic** failures count toward backoff —
//! the things that get better with less load:
//!
//! - `refused`       — peer rejected (overloaded)
//! - `timeout`       — peer stalled (oversaturated)
//! - `vram_thrash`   — GPU swap-out (capacity)
//! - `gpu_vulkan`    — Vulkan runtime error (may be contention-driven)
//! - `gpu_rocm`      — ROCm runtime error (may be contention-driven)
//! - `inference_5xx` — daemon 5xx (often peer overload)
//! - `daemon_down`   — daemon was momentarily out; backing off keeps
//!                     in-flight pressure low while it recovers
//!
//! Non-elastic buckets are recorded for the failure-bucket histogram
//! but don't drive concurrency: `mismatch`, `model_missing`,
//! `stale_cache`, `inference_json_parse`, `phase_failed`,
//! `build_step_failed`, `unknown`. Lowering concurrency does not fix
//! a 404 slug, a missing cache payload, or a sampler that produces
//! malformed JSON.
//!
//! `inference_json_parse` is borderline — it *could* be contention-
//! induced if concurrent slots are degrading sampler quality — but
//! we keep it non-elastic so the metric stays a clean backend-quality
//! signal rather than getting masked by automatic backoff. If the
//! data later shows backoff helps, promote it.
//!
//! With ≥ `MIN_SAMPLES` events in the rolling window: if elastic
//! failure rate ≥ `BACKOFF_AT`, drop the ceiling by 1 (floor of 1).
//! If the rate falls below `RECOVER_AT`, climb by 1 (capped at the
//! configured maximum). A `COOLDOWN_SECS` gate prevents oscillation.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Outcome we feed into the adaptive controller. The classifier
/// bucket determines which one — see `from_bucket`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    /// A failure that backing off should help. Counts toward backoff.
    ElasticFail,
    /// A failure that backing off won't help (bad input, missing
    /// model). Recorded but doesn't drive the ceiling.
    NonElasticFail,
}

/// Map a classifier bucket name to an `Outcome` for the adaptive
/// controller. Buckets we haven't classified are treated as
/// non-elastic — concurrency-tweaking can't fix something we don't
/// yet understand the shape of.
pub fn outcome_from_bucket(bucket: &str) -> Outcome {
    match bucket {
        "timeout" | "refused" | "vram_thrash" | "gpu_vulkan" | "gpu_rocm" | "inference_5xx"
        | "daemon_down" => Outcome::ElasticFail,
        _ => Outcome::NonElasticFail,
    }
}

const WINDOW_SIZE: usize = 20;
const MIN_SAMPLES: usize = 5;
const BACKOFF_AT: f32 = 0.30;
const RECOVER_AT: f32 = 0.10;
const COOLDOWN_SECS: i64 = 30;

pub struct AdaptiveConcurrency {
    configured: u32,
    effective: AtomicU32,
    window: Mutex<VecDeque<Outcome>>,
    last_adjusted_unix: AtomicI64,
}

impl AdaptiveConcurrency {
    pub fn new(configured: u32) -> Self {
        let cur = configured.max(1);
        Self {
            configured: cur,
            effective: AtomicU32::new(cur),
            window: Mutex::new(VecDeque::with_capacity(WINDOW_SIZE)),
            last_adjusted_unix: AtomicI64::new(0),
        }
    }

    pub fn effective(&self) -> u32 {
        self.effective.load(Ordering::Relaxed)
    }

    pub fn configured(&self) -> u32 {
        self.configured
    }

    /// Record a completion and possibly adjust the ceiling. Safe to
    /// call concurrently from multiple driver tasks; internally
    /// short-lived `Mutex` + atomic store.
    pub fn record(&self, outcome: Outcome) {
        {
            let mut w = self.window.lock().unwrap();
            if w.len() >= WINDOW_SIZE {
                w.pop_front();
            }
            w.push_back(outcome);
        }
        self.maybe_adjust(unix_now());
    }

    /// Visible for tests. In production, called by `record`.
    fn maybe_adjust(&self, now: i64) {
        let last = self.last_adjusted_unix.load(Ordering::Relaxed);
        if last != 0 && now - last < COOLDOWN_SECS {
            return;
        }
        let (n, elastic_fails) = {
            let w = self.window.lock().unwrap();
            let n = w.len();
            let f = w.iter().filter(|o| **o == Outcome::ElasticFail).count();
            (n, f)
        };
        if n < MIN_SAMPLES {
            return;
        }
        let rate = elastic_fails as f32 / n as f32;
        let cur = self.effective.load(Ordering::Relaxed);
        let prev = cur;
        if rate >= BACKOFF_AT && cur > 1 {
            self.effective.store(cur - 1, Ordering::Relaxed);
            self.last_adjusted_unix.store(now, Ordering::Relaxed);
            tracing::warn!(
                effective_concurrency_was = prev,
                effective_concurrency_now = cur - 1,
                elastic_failure_rate = rate,
                window_samples = n,
                "adaptive: backing off — elastic failures climbing"
            );
        } else if rate <= RECOVER_AT && cur < self.configured {
            self.effective.store(cur + 1, Ordering::Relaxed);
            self.last_adjusted_unix.store(now, Ordering::Relaxed);
            tracing::info!(
                effective_concurrency_was = prev,
                effective_concurrency_now = cur + 1,
                elastic_failure_rate = rate,
                window_samples = n,
                "adaptive: recovering — elastic failures quiet"
            );
        }
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_configured() {
        let a = AdaptiveConcurrency::new(4);
        assert_eq!(a.effective(), 4);
    }

    #[test]
    fn zero_clamps_to_one() {
        let a = AdaptiveConcurrency::new(0);
        assert_eq!(a.effective(), 1);
    }

    #[test]
    fn outcome_mapping_uses_elastic_buckets() {
        for b in [
            "timeout",
            "refused",
            "vram_thrash",
            "gpu_vulkan",
            "gpu_rocm",
            "inference_5xx",
            "daemon_down",
        ] {
            assert_eq!(outcome_from_bucket(b), Outcome::ElasticFail, "{b}");
        }
        for b in [
            "mismatch",
            "model_missing",
            "stale_cache",
            "inference_json_parse",
            "phase_failed",
            "build_step_failed",
            "unknown",
        ] {
            assert_eq!(outcome_from_bucket(b), Outcome::NonElasticFail, "{b}");
        }
    }

    #[test]
    fn elastic_failures_trigger_backoff() {
        let a = AdaptiveConcurrency::new(4);
        // Drive elastic failure rate well above BACKOFF_AT.
        for _ in 0..10 {
            a.record(Outcome::ElasticFail);
        }
        // First adjustment should have fired.
        assert_eq!(a.effective(), 3);
    }

    #[test]
    fn non_elastic_failures_do_not_trigger_backoff() {
        let a = AdaptiveConcurrency::new(4);
        for _ in 0..20 {
            a.record(Outcome::NonElasticFail);
        }
        // No elastic failures means rate stays at 0 → recovery path
        // would fire, but we're already at the ceiling.
        assert_eq!(a.effective(), 4);
    }

    #[test]
    fn successes_after_backoff_recover() {
        let a = AdaptiveConcurrency::new(4);
        // 10 elastic fails → backoff to 3.
        for _ in 0..10 {
            a.record(Outcome::ElasticFail);
        }
        assert_eq!(a.effective(), 3);
        // Drain the window with successes. The cooldown still holds
        // during this drain — which is what we want in production:
        // the controller waits for stale failures to age out of the
        // sample before re-evaluating.
        for _ in 0..WINDOW_SIZE {
            a.record(Outcome::Success);
        }
        // Cooldown bypass now (production: 30s of real time would
        // have passed). The window is all-success, rate=0 ≤ RECOVER_AT,
        // so the recovery path fires.
        a.last_adjusted_unix.store(0, Ordering::Relaxed);
        a.record(Outcome::Success);
        assert_eq!(a.effective(), 4);
    }

    #[test]
    fn floor_is_one() {
        let a = AdaptiveConcurrency::new(2);
        // Hammer with elastic failures.
        for _ in 0..50 {
            a.last_adjusted_unix.store(0, Ordering::Relaxed);
            a.record(Outcome::ElasticFail);
        }
        // Floor — never below 1.
        assert_eq!(a.effective(), 1);
    }

    #[test]
    fn cooldown_prevents_rapid_oscillation() {
        let a = AdaptiveConcurrency::new(4);
        // First wave of elastic failures triggers one backoff.
        for _ in 0..10 {
            a.record(Outcome::ElasticFail);
        }
        assert_eq!(a.effective(), 3);
        // More failures right now should NOT trigger another
        // adjustment — we're inside the cooldown.
        for _ in 0..10 {
            a.record(Outcome::ElasticFail);
        }
        assert_eq!(a.effective(), 3);
    }

    #[test]
    fn min_samples_blocks_early_adjustment() {
        let a = AdaptiveConcurrency::new(4);
        // 4 elastic failures < MIN_SAMPLES — no decision.
        for _ in 0..(MIN_SAMPLES - 1) {
            a.record(Outcome::ElasticFail);
        }
        assert_eq!(a.effective(), 4);
    }
}
