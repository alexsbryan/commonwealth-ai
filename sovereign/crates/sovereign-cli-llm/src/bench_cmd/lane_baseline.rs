// SPDX-License-Identifier: AGPL-3.0-or-later
//! Lane baselines — promote an *absolute-verdict* bench into a
//! *baseline-relative* CI gate.
//!
//! Some benches return an **absolute** verdict that is a true finding for the
//! current system, not a regression signal: chaos-monkey is designed to break
//! the present agent (NO-GO), and mechanism-fidelity returns NO-GO for any
//! model that isn't mechanism-faithful. Gating CI on their pass/fail would
//! pin the build permanently red. The fix is to gate on **change vs a captured
//! baseline** instead: fail only when a headline metric moves in the wrong
//! direction by more than its tolerance.
//!
//! This module is the small, pure, self-describing primitive that makes that
//! uniform across lanes. A [`LaneBaseline`] is a flat bag of named
//! [`LaneMetric`]s; each metric carries its own [`Direction`] (which way is
//! "worse") and `tolerance` (how much movement is noise), so the baseline JSON
//! is legible on its own — a reader can see exactly what would count as a
//! regression without consulting code. [`diff`] applies those per-metric rules;
//! the comparison logic lives in one place and is reused by every lane adapter
//! in [`super::gate`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A slot alias, not a concrete model — the historical placeholder a
/// baseline recorded before resolution existed. We refuse to attribute
/// a baseline to one of these: an alias is not a model.
pub(crate) fn is_alias_marker(s: &str) -> bool {
    let s = s.trim().trim_start_matches("commonwealth/");
    matches!(s, "primary" | "fast" | "embed" | "code" | "reasoning")
}

/// Which direction of movement counts as a **regression** for a metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Bigger is better (competence, honesty, judge coverage, first-failure
    /// turn). A *drop* past tolerance is a regression.
    HigherIsBetter,
    /// Smaller is better (hallucination rate, latency). A *rise* past
    /// tolerance is a regression.
    LowerIsBetter,
    /// A witness that must stay near zero (the mechanism-fidelity control
    /// Δ̄ — "the scoring join is intact"). Drift *away from zero* in either
    /// sign, past tolerance, is a regression. Compared on absolute value.
    NearZero,
}

/// One headline scalar in a lane baseline, fully self-describing: the value,
/// which way is worse, and how much movement is noise.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LaneMetric {
    pub value: f64,
    pub direction: Direction,
    /// Movement strictly within ±tolerance is treated as noise (not a
    /// regression and not an improvement). Chosen per metric by the adapter
    /// — e.g. coarser for small-n fractions (one-item GPU-nondeterminism
    /// flips), tight for the deterministic control witness.
    pub tolerance: f64,
}

impl LaneMetric {
    pub fn higher_is_better(value: f64, tolerance: f64) -> Self {
        Self {
            value,
            direction: Direction::HigherIsBetter,
            tolerance,
        }
    }
    pub fn lower_is_better(value: f64, tolerance: f64) -> Self {
        Self {
            value,
            direction: Direction::LowerIsBetter,
            tolerance,
        }
    }
    pub fn near_zero(value: f64, tolerance: f64) -> Self {
        Self {
            value,
            direction: Direction::NearZero,
            tolerance,
        }
    }
}

/// A captured set of headline metrics for one lane — the on-disk baseline
/// (serialised to `<bench_root>/<group>/baselines/<id>/latest.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneBaseline {
    pub lane: String,
    /// When this baseline was captured (RFC-3339). Provenance only.
    pub captured_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpus: Option<String>,
    /// Legacy / legible model field — the concrete GGUF stem when
    /// known (was historically the slot alias `"primary"`, which is
    /// why `model_attribution` now carries the real provenance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Structured, human-readable model provenance: the concrete stem
    /// plus manifest-derived `base_name`/`family`/`quant`. Populated by
    /// [`Self::attribute`] from the resolved model stem. `None` on
    /// legacy baselines and when slot resolution failed at capture
    /// time — the report rollup buckets those as "unattributed".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_attribution: Option<sovereign_core::models_manifest::ModelAttribution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub metrics: BTreeMap<String, LaneMetric>,
}

impl LaneBaseline {
    pub fn new(lane: impl Into<String>, captured_at: impl Into<String>) -> Self {
        Self {
            lane: lane.into(),
            captured_at: captured_at.into(),
            corpus: None,
            model: None,
            model_attribution: None,
            note: None,
            metrics: BTreeMap::new(),
        }
    }

    /// Attribute this baseline to the concrete model that produced it,
    /// given the model stem recorded on the run's transcript rows (or
    /// any concrete GGUF stem). Sets both the legible `model` field
    /// (the stem) and the structured `model_attribution`, enriched
    /// deterministically from the bundled manifest — no daemon needed,
    /// so the gate re-score path attributes correctly from the
    /// transcript alone.
    ///
    /// A `None` or alias-shaped stem (`"primary"`/`"fast"` — the legacy
    /// unresolved marker) leaves attribution empty rather than
    /// recording the alias as if it were a model.
    pub fn attribute(&mut self, model_stem: Option<&str>) {
        let Some(stem) = model_stem.filter(|s| !s.is_empty() && !is_alias_marker(s)) else {
            return;
        };
        self.model = Some(stem.to_string());
        self.model_attribution = Some(
            sovereign_core::models_manifest::DEFAULT_MANIFEST.attribution_for_file(stem),
        );
    }
    pub fn with(mut self, name: impl Into<String>, metric: LaneMetric) -> Self {
        self.metrics.insert(name.into(), metric);
        self
    }
}

/// How a single metric moved between baseline and current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Movement {
    Regressed,
    Improved,
    Unchanged,
}

/// Per-metric comparison outcome — the glassbox row.
#[derive(Debug, Clone)]
pub struct MetricDelta {
    pub name: String,
    pub baseline: f64,
    pub current: f64,
    /// Signed `current − baseline` (raw, for display). The regression
    /// decision uses `direction`-aware comparison, not this raw sign.
    pub delta: f64,
    pub tolerance: f64,
    pub direction: Direction,
    pub movement: Movement,
}

/// The result of comparing a current run against a baseline.
#[derive(Debug, Clone, Default)]
pub struct LaneDiff {
    /// True when there was no baseline to compare against — first run.
    pub first_run: bool,
    pub deltas: Vec<MetricDelta>,
    /// Metrics present in the baseline but absent from the current run
    /// (schema drift / a metric stopped being emitted). Reported, not gated.
    pub missing: Vec<String>,
}

impl LaneDiff {
    pub fn regressions(&self) -> impl Iterator<Item = &MetricDelta> {
        self.deltas
            .iter()
            .filter(|d| d.movement == Movement::Regressed)
    }
    pub fn improvements(&self) -> impl Iterator<Item = &MetricDelta> {
        self.deltas
            .iter()
            .filter(|d| d.movement == Movement::Improved)
    }
    pub fn n_regressed(&self) -> usize {
        self.regressions().count()
    }
}

/// Classify one metric's movement, honouring its direction + tolerance.
///
/// A non-finite *current* value is always a regression: the bench produced an
/// undefined metric (e.g. an empty population → NaN), which we can never
/// certify as "no worse than baseline".
fn classify(prev: f64, cur: &LaneMetric) -> Movement {
    if !cur.value.is_finite() {
        return Movement::Regressed;
    }
    let tol = cur.tolerance.abs();
    match cur.direction {
        Direction::HigherIsBetter => {
            if cur.value < prev - tol {
                Movement::Regressed
            } else if cur.value > prev + tol {
                Movement::Improved
            } else {
                Movement::Unchanged
            }
        }
        Direction::LowerIsBetter => {
            if cur.value > prev + tol {
                Movement::Regressed
            } else if cur.value < prev - tol {
                Movement::Improved
            } else {
                Movement::Unchanged
            }
        }
        Direction::NearZero => {
            let (pa, ca) = (prev.abs(), cur.value.abs());
            if ca > pa + tol {
                Movement::Regressed
            } else if ca < pa - tol {
                Movement::Improved
            } else {
                Movement::Unchanged
            }
        }
    }
}

/// Compare a `current` run against an optional `baseline`. The **current**
/// metric is authoritative for direction + tolerance (it reflects the present
/// adapter's intent), so editing a tolerance takes effect immediately.
pub fn diff(baseline: Option<&LaneBaseline>, current: &LaneBaseline) -> LaneDiff {
    let Some(prev) = baseline else {
        return LaneDiff {
            first_run: true,
            deltas: Vec::new(),
            missing: Vec::new(),
        };
    };
    let mut deltas = Vec::new();
    for (name, cur) in &current.metrics {
        // A metric with no baseline counterpart is new — report it as an
        // unchanged row at its own value (informational; never a regression).
        let prev_val = prev.metrics.get(name).map(|m| m.value);
        let movement = match prev_val {
            Some(p) => classify(p, cur),
            None => Movement::Unchanged,
        };
        deltas.push(MetricDelta {
            name: name.clone(),
            baseline: prev_val.unwrap_or(f64::NAN),
            current: cur.value,
            delta: cur.value - prev_val.unwrap_or(cur.value),
            tolerance: cur.tolerance,
            direction: cur.direction,
            movement,
        });
    }
    let missing = prev
        .metrics
        .keys()
        .filter(|k| !current.metrics.contains_key(*k))
        .cloned()
        .collect();
    LaneDiff {
        first_run: false,
        deltas,
        missing,
    }
}

fn dir_glyph(d: Direction) -> &'static str {
    match d {
        Direction::HigherIsBetter => "↑",
        Direction::LowerIsBetter => "↓",
        Direction::NearZero => "≈0",
    }
}

/// Render a glassbox table of the diff and return the CI exit code.
///
/// - first run (no baseline) → `0`, with a clear "capture with
///   `--update-baseline`" line and a `first-run` marker the CI script reads as
///   a setup gap, not a pass-by-regression.
/// - any regression → `1`.
/// - otherwise → `0`.
///
/// Always prints an `N regressed` line so the CI runner's existing scoreboard
/// parser (shared with `bench all`) sees a consistent vocabulary.
pub fn render_and_exit_code(diff: &LaneDiff, lane: &str) -> i32 {
    eprintln!("\n── lane gate: {lane} (baseline-relative) ──");
    if diff.first_run {
        eprintln!("  no baseline yet — first-run. Capture one with --update-baseline.");
        eprintln!("  0 regressed (first-run)");
        return 0;
    }
    eprintln!(
        "  {:<28} {:>10} {:>10} {:>9} {:>8}  dir  status",
        "metric", "baseline", "current", "Δ", "tol"
    );
    for d in &diff.deltas {
        let status = match d.movement {
            Movement::Regressed => "REGRESSED",
            Movement::Improved => "improved",
            Movement::Unchanged => "ok",
        };
        eprintln!(
            "  {:<28} {:>10.4} {:>10.4} {:>+9.4} {:>8.4}  {:<3}  {}",
            d.name,
            d.baseline,
            d.current,
            d.delta,
            d.tolerance,
            dir_glyph(d.direction),
            status,
        );
    }
    for m in &diff.missing {
        eprintln!(
            "  {m:<28} {:>10} (in baseline, absent now — schema drift?)",
            "—"
        );
    }
    let n_reg = diff.n_regressed();
    let n_imp = diff.improvements().count();
    eprintln!(
        "  ── {} regressed · {} improved · {} ok ──",
        n_reg,
        n_imp,
        diff.deltas.len().saturating_sub(n_reg + n_imp),
    );
    if n_reg == 0 {
        eprintln!("  VERDICT: PASS ✓  — no metric regressed past tolerance vs baseline.");
        0
    } else {
        eprintln!("  VERDICT: FAIL ✗  — {n_reg} metric(s) regressed vs baseline.");
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> LaneBaseline {
        LaneBaseline::new("chaos", "2026-06-07")
            .with("competence", LaneMetric::higher_is_better(0.57, 0.10))
            .with("honesty", LaneMetric::higher_is_better(0.36, 0.10))
            .with(
                "hallucination_rate",
                LaneMetric::lower_is_better(0.64, 0.10),
            )
            .with("control_delta", LaneMetric::near_zero(0.0, 0.05))
    }

    #[test]
    fn first_run_when_no_baseline() {
        let d = diff(None, &base());
        assert!(d.first_run);
        assert_eq!(d.n_regressed(), 0);
    }

    #[test]
    fn identical_run_has_no_regression() {
        let d = diff(Some(&base()), &base());
        assert!(!d.first_run);
        assert_eq!(d.n_regressed(), 0);
        assert_eq!(d.improvements().count(), 0);
    }

    #[test]
    fn higher_is_better_drop_past_tolerance_regresses() {
        // competence 0.57 → 0.40 (drop 0.17 > tol 0.10) is a regression.
        let cur = LaneBaseline::new("chaos", "now")
            .with("competence", LaneMetric::higher_is_better(0.40, 0.10));
        let prev = LaneBaseline::new("chaos", "old")
            .with("competence", LaneMetric::higher_is_better(0.57, 0.10));
        let d = diff(Some(&prev), &cur);
        assert_eq!(d.n_regressed(), 1);
    }

    #[test]
    fn one_item_flip_within_tolerance_is_noise() {
        // honesty 0.36 → 0.27 (one of 11 items flips ≈0.09 < tol 0.10): ok.
        let cur = LaneBaseline::new("chaos", "now")
            .with("honesty", LaneMetric::higher_is_better(0.27, 0.10));
        let prev = LaneBaseline::new("chaos", "old")
            .with("honesty", LaneMetric::higher_is_better(0.36, 0.10));
        let d = diff(Some(&prev), &cur);
        assert_eq!(
            d.n_regressed(),
            0,
            "single-item nondeterminism must not gate"
        );
    }

    #[test]
    fn lower_is_better_rise_regresses() {
        let cur = LaneBaseline::new("chaos", "now").with(
            "hallucination_rate",
            LaneMetric::lower_is_better(0.80, 0.10),
        );
        let prev = LaneBaseline::new("chaos", "old").with(
            "hallucination_rate",
            LaneMetric::lower_is_better(0.64, 0.10),
        );
        assert_eq!(diff(Some(&prev), &cur).n_regressed(), 1);
        // ...and a drop is an improvement, not a regression.
        let better = LaneBaseline::new("chaos", "now").with(
            "hallucination_rate",
            LaneMetric::lower_is_better(0.40, 0.10),
        );
        let d = diff(Some(&prev), &better);
        assert_eq!(d.n_regressed(), 0);
        assert_eq!(d.improvements().count(), 1);
    }

    #[test]
    fn near_zero_drift_either_sign_regresses() {
        let prev = LaneBaseline::new("mech", "old")
            .with("control_delta", LaneMetric::near_zero(0.00, 0.05));
        // +0.20 drift away from zero → regression (scoring join broke).
        let up = LaneBaseline::new("mech", "now")
            .with("control_delta", LaneMetric::near_zero(0.20, 0.05));
        assert_eq!(diff(Some(&prev), &up).n_regressed(), 1);
        // −0.20 drift is equally bad.
        let down = LaneBaseline::new("mech", "now")
            .with("control_delta", LaneMetric::near_zero(-0.20, 0.05));
        assert_eq!(diff(Some(&prev), &down).n_regressed(), 1);
        // staying near zero is fine.
        let flat = LaneBaseline::new("mech", "now")
            .with("control_delta", LaneMetric::near_zero(0.02, 0.05));
        assert_eq!(diff(Some(&prev), &flat).n_regressed(), 0);
    }

    #[test]
    fn nan_current_is_a_regression() {
        let prev = LaneBaseline::new("chaos", "old")
            .with("honesty", LaneMetric::higher_is_better(0.36, 0.10));
        let cur = LaneBaseline::new("chaos", "now")
            .with("honesty", LaneMetric::higher_is_better(f64::NAN, 0.10));
        assert_eq!(diff(Some(&prev), &cur).n_regressed(), 1);
    }

    #[test]
    fn missing_metric_is_reported_not_gated() {
        let prev = base();
        let cur = LaneBaseline::new("chaos", "now")
            .with("competence", LaneMetric::higher_is_better(0.57, 0.10));
        let d = diff(Some(&prev), &cur);
        assert_eq!(d.n_regressed(), 0);
        assert!(d.missing.contains(&"honesty".to_string()));
    }

    #[test]
    fn attribute_refuses_alias_and_accepts_concrete_stem() {
        // The alias placeholder must never become an attribution.
        let mut b = LaneBaseline::new("chaos-monkey", "now");
        b.attribute(Some("primary"));
        assert!(b.model.is_none(), "'primary' is a slot alias, not a model");
        assert!(b.model_attribution.is_none());

        b.attribute(Some("commonwealth/primary"));
        assert!(b.model_attribution.is_none(), "namespaced alias also refused");

        // A concrete stem sets both the legible field and the structured
        // attribution, enriched from the bundled manifest.
        b.attribute(Some("Qwen3.5-4B-Q4_K_M"));
        assert_eq!(b.model.as_deref(), Some("Qwen3.5-4B-Q4_K_M"));
        let a = b.model_attribution.expect("concrete stem attributes");
        assert_eq!(a.base_name.as_deref(), Some("Qwen3.5-4B"));
        assert_eq!(a.quant.as_deref(), Some("Q4_K_M"));
    }

    #[test]
    fn is_alias_marker_matches_canonical_slots() {
        for a in ["primary", "fast", "embed", "code", "commonwealth/primary"] {
            assert!(is_alias_marker(a), "{a} is an alias");
        }
        assert!(!is_alias_marker("Qwen3.5-4B-Q4_K_M"), "a real stem is not an alias");
    }
}
