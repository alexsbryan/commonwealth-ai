// SPDX-License-Identifier: AGPL-3.0-or-later
//! Anytime-valid early-stopping for the reasoning-fidelity verdict.
//!
//! The instrument's per-class verdict reduces to a handful of **bounded
//! means in [0,1]** — the fraction of large-Δ DIR cases that collapse,
//! the saturation/invariance flat-fractions, the control's directional
//! accuracy. To hit "minutes, not hours" we want to STOP drawing cases
//! the instant the answer is obvious, without inflating the false-decision
//! rate by peeking.
//!
//! The construction here makes peeking honest with two pre-registered
//! ingredients:
//!
//!   * an **empirical-Bernstein** half-width (Maurer–Pontil) — it shrinks
//!     with the observed *variance*, so a model that collapses on *every*
//!     case (near-zero variance) resolves far faster than a worst-case
//!     Hoeffding bound would allow; and
//!   * a frozen **checkpoint schedule** — the verdict is only read at a
//!     finite, pre-registered set of sample sizes, with the confidence
//!     level Bonferroni-split across them. The union bound over those
//!     checkpoints controls the family-wise false-decision rate at
//!     `alpha` regardless of which checkpoint you stop at.
//!
//! Only `n` is adaptive; `alpha`, the checkpoints, and the bands are
//! frozen in `manifest.toml` (the `[stopping]` block). This composes with
//! the three-pool discipline untouched: Train/Dev draw fresh cases from
//! the infinite generator, while the sacred Test pool runs a *fixed*
//! pre-registered `n` (you never optimize `n` against the holdout).

use serde::{Deserialize, Serialize};

/// Pre-registered stopping parameters (frozen in the manifest).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoppingConfig {
    /// Family-wise false-decision rate across all checkpoints.
    pub alpha: f64,
    /// Ascending sample-size checkpoints, e.g. `[16, 32, 64, 128, 200]`.
    /// The first is the `n_min` floor (no verdict before it); the last is
    /// the `n_max` cap (a still-straddling interval there → Inconclusive).
    pub checkpoints: Vec<usize>,
}

impl Default for StoppingConfig {
    fn default() -> Self {
        StoppingConfig {
            alpha: 0.05,
            checkpoints: vec![16, 32, 64, 128, 200],
        }
    }
}

impl StoppingConfig {
    pub fn n_min(&self) -> usize {
        self.checkpoints.first().copied().unwrap_or(0)
    }
    pub fn n_max(&self) -> usize {
        self.checkpoints.last().copied().unwrap_or(0)
    }
    /// Bonferroni-split per-checkpoint confidence level.
    fn alpha_per_checkpoint(&self) -> f64 {
        self.alpha / (self.checkpoints.len().max(1) as f64)
    }
}

/// A running estimator of a bounded mean in [0,1] with an
/// empirical-Bernstein confidence interval.
#[derive(Debug, Clone, Default)]
pub struct BoundedMean {
    n: usize,
    sum: f64,
    sum_sq: f64,
}

impl BoundedMean {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one observation (clamped to [0,1]).
    pub fn push(&mut self, x: f64) {
        let x = x.clamp(0.0, 1.0);
        self.n += 1;
        self.sum += x;
        self.sum_sq += x * x;
    }

    pub fn n(&self) -> usize {
        self.n
    }

    pub fn mean(&self) -> f64 {
        if self.n == 0 {
            f64::NAN
        } else {
            self.sum / self.n as f64
        }
    }

    /// Unbiased sample variance; the max-entropy 0.25 until determinable.
    fn variance(&self) -> f64 {
        if self.n < 2 {
            return 0.25;
        }
        let n = self.n as f64;
        let m = self.mean();
        ((self.sum_sq - n * m * m) / (n - 1.0)).max(0.0)
    }

    /// Empirical-Bernstein (Maurer–Pontil) half-width at confidence
    /// `1 - alpha_pc`.
    fn half_width(&self, alpha_pc: f64) -> f64 {
        if self.n < 2 {
            return 1.0;
        }
        let n = self.n as f64;
        let l = (2.0 / alpha_pc).ln();
        (2.0 * self.variance() * l / n).sqrt() + 7.0 * l / (3.0 * (n - 1.0))
    }

    /// Confidence interval `[lo, hi]` for the mean at the Bonferroni
    /// per-checkpoint level, clamped to [0,1].
    pub fn interval(&self, cfg: &StoppingConfig) -> (f64, f64) {
        let h = self.half_width(cfg.alpha_per_checkpoint());
        ((self.mean() - h).max(0.0), (self.mean() + h).min(1.0))
    }
}

/// Which side of `threshold` is the *passing* side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Pass when the mean is at least `threshold` (e.g. magnitude-pass ≥ 0.80).
    AtLeast,
    /// Pass when the mean is at most `threshold` (e.g. control accuracy ≤ 0.55).
    AtMost,
}

/// The verdict for one bounded mean against one pre-registered band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    /// Hit the `n_max` cap with the interval still straddling the band.
    Inconclusive,
    /// Not at a checkpoint, or undecided — draw more cases.
    Continue,
}

impl Verdict {
    /// True once no more cases will change the outcome.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Verdict::Continue)
    }
}

/// Decide the verdict for `m` against `threshold`/`side`. A verdict is only
/// returned AT a pre-registered checkpoint (otherwise `Continue`); at the
/// `n_max` cap a straddling interval yields `Inconclusive`. The schedule is
/// keyed on the mean's own observation count `m.n()`.
pub fn decide(m: &BoundedMean, cfg: &StoppingConfig, threshold: f64, side: Side) -> Verdict {
    let at_checkpoint = cfg.checkpoints.contains(&m.n());
    let at_max = m.n() >= cfg.n_max();
    decide_at(m, cfg, threshold, side, at_checkpoint, at_max)
}

/// Like [`decide`], but the caller supplies whether this read is at a
/// checkpoint / the cap. Use this when the checkpoint schedule is driven by
/// an *external* counter — e.g. the number of synthetic **cases** drawn —
/// rather than the mean's own observation count. The two diverge for a
/// conditionally-updated mean like the magnitude band (`μ_mag` only takes an
/// observation on a large-Δ case, so its `n()` lags the case counter and
/// would skip the exact checkpoint values entirely). Driving every per-model
/// mean off the shared case counter keeps them resolving on the same
/// pre-registered schedule.
pub fn decide_at(
    m: &BoundedMean,
    cfg: &StoppingConfig,
    threshold: f64,
    side: Side,
    at_checkpoint: bool,
    at_max: bool,
) -> Verdict {
    if !at_checkpoint && !at_max {
        return Verdict::Continue;
    }
    // A mean with < 2 observations has no usable variance estimate; never
    // resolve it on data alone (only the cap can force a read, as
    // Inconclusive). Guards the NaN-mean edge when μ_mag has seen no
    // large-Δ case yet.
    if m.n() < 2 && !at_max {
        return Verdict::Continue;
    }
    let (lo, hi) = m.interval(cfg);
    let (pass, fail) = match side {
        Side::AtLeast => (lo >= threshold, hi < threshold),
        Side::AtMost => (hi <= threshold, lo > threshold),
    };
    if pass {
        Verdict::Pass
    } else if fail {
        Verdict::Fail
    } else if at_max {
        Verdict::Inconclusive
    } else {
        Verdict::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> StoppingConfig {
        StoppingConfig::default()
    }

    fn feed(m: &mut BoundedMean, value: f64, n: usize) {
        for _ in 0..n {
            m.push(value);
        }
    }

    #[test]
    fn no_verdict_before_n_min() {
        let c = cfg();
        let mut m = BoundedMean::new();
        // 15 perfect samples — below the first checkpoint (16) → Continue.
        feed(&mut m, 1.0, 15);
        assert_eq!(decide(&m, &c, 0.80, Side::AtLeast), Verdict::Continue);
    }

    #[test]
    fn obvious_fail_resolves_early() {
        let c = cfg();
        // A clearly-unfaithful model: never collapses → mean 0 vs an
        // AtLeast-0.80 band. The empirical-Bernstein additive term is wide
        // at n=16 (warmup), so the interval clears below the band by the
        // n=32 checkpoint — still 6× sooner than the n=200 cap.
        let mut m = BoundedMean::new();
        feed(&mut m, 0.0, 16);
        assert_eq!(
            decide(&m, &c, 0.80, Side::AtLeast),
            Verdict::Continue,
            "n=16 is a warmup — too wide to decide"
        );
        feed(&mut m, 0.0, 16); // now at n=32
        assert_eq!(decide(&m, &c, 0.80, Side::AtLeast), Verdict::Fail);
    }

    #[test]
    fn flat_control_passes_at_most_band_fast() {
        let c = cfg();
        // Control directional accuracy ≈ 0 (perfectly blind) vs ≤ 0.55 →
        // passes by the n=32 checkpoint.
        let mut m = BoundedMean::new();
        feed(&mut m, 0.0, 32);
        assert_eq!(decide(&m, &c, 0.55, Side::AtMost), Verdict::Pass);
    }

    #[test]
    fn zero_variance_pass_resolves_before_n_max() {
        let c = cfg();
        // Every case collapses (value 1.0, zero variance). Empirical-
        // Bernstein should clear AtLeast-0.80 well before the 200 cap —
        // and strictly faster than a worst-case Hoeffding bound.
        let mut m = BoundedMean::new();
        feed(&mut m, 1.0, 64);
        assert_eq!(decide(&m, &c, 0.80, Side::AtLeast), Verdict::Pass);
    }

    #[test]
    fn borderline_runs_to_cap_then_inconclusive() {
        let c = cfg();
        // Mean hovering right at the 0.80 band with real spread → never
        // separates; at the cap it must read Inconclusive, not a forced
        // Pass/Fail.
        let mut m = BoundedMean::new();
        for i in 0..200 {
            m.push(if i % 5 == 0 { 0.0 } else { 1.0 }); // mean 0.8, real variance
        }
        assert_eq!(decide(&m, &c, 0.80, Side::AtLeast), Verdict::Inconclusive);
    }

    #[test]
    fn interval_tightens_with_n() {
        let c = cfg();
        let mut a = BoundedMean::new();
        feed(&mut a, 0.5, 16);
        let mut b = BoundedMean::new();
        feed(&mut b, 0.5, 128);
        let (alo, ahi) = a.interval(&c);
        let (blo, bhi) = b.interval(&c);
        assert!((ahi - alo) > (bhi - blo), "more samples ⇒ tighter interval");
    }
}
