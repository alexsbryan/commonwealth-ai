// SPDX-License-Identifier: AGPL-3.0-or-later
//! Model **fidelity cards** — the "characterize once, read free" artifact.
//!
//! A full battery is expensive; a query that wants to know "does this model
//! reason from the mechanism for *this* kind of question?" should not pay
//! for it. So a characterization run distills each `(model, class)` verdict
//! into a small [`CardEntry`] and writes it to a per-model JSON card under
//! `~/.sovereign/model-fidelity-cards/<model>.json`. The read side (a
//! router/chip that gates how much to trust a model on a class — a
//! *deferred* later package) loads the card and is done in microseconds.
//!
//! Each entry is stamped with the **manifest fingerprint** it was graded
//! under: the bands are the contract, so a card graded against stale bands
//! must be treated as invalid (the reader compares fingerprints). The grade
//! itself mirrors the Python verdict's tiering exactly, in Rust, so the card
//! and the human-facing verdict never disagree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::score::{Bands, ResultRow};

/// The per-(model, class) verdict, stored on the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    /// P1 collapse with the magnitude band passing, flat P2 + INV, control
    /// at chance — mechanism-consistent (NOT correctness).
    Faithful,
    /// The control fails correctly, but the model did not show the collapse.
    Unfaithful,
    /// The negative control showed sensitivity — the instrument is invalid
    /// for this run (a leak), so the model's grade is unreadable.
    ControlLeak,
    /// Too few cases to separate the bands (underpowered / hit the cap
    /// straddling).
    Inconclusive,
}

/// Thresholds the grade reads — sourced from the pre-registration manifest.
#[derive(Debug, Clone, Copy)]
pub struct GradeThresholds {
    /// `p1_delta` must be below `-collapse_min` for Faithful.
    pub collapse_min: f64,
    /// Saturation/invariance flat ceiling (`|control Δ̄|` leak test).
    pub flat_max: f64,
    pub mag_pass: f64,
    pub flat_pass: f64,
    pub inv_pass: f64,
    pub control_max_dir_acc: f64,
    /// Below this many scored cases the verdict is Inconclusive.
    pub min_cases: usize,
}

impl Default for GradeThresholds {
    fn default() -> Self {
        GradeThresholds {
            collapse_min: 0.40,
            flat_max: 0.10,
            mag_pass: 0.80,
            flat_pass: 0.90,
            inv_pass: 0.90,
            control_max_dir_acc: 0.55,
            min_cases: 16,
        }
    }
}

/// One graded `(model, class)` measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardEntry {
    pub class: String,
    pub grade: Grade,
    /// A single [0,1] certainty for the headline grade (see `grade_class`).
    pub confidence: f64,
    pub p1_delta: f64,
    pub mag_pass: f64,
    pub flat_pass: f64,
    pub inv_pass: f64,
    pub control_dir_acc: f64,
    pub control_abs_delta: f64,
    pub n_cases: usize,
    pub pool: String,
    pub stopped_early: bool,
    /// Fingerprint of the manifest these bands came from; a reader treats a
    /// card with a non-matching fingerprint as stale.
    pub manifest_fingerprint: String,
    /// RFC3339 timestamp the entry was graded.
    pub graded_at: String,
}

/// One model's card: a class → entry map, merged across characterization
/// runs (a new run for a class overwrites that class's entry only).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FidelityCard {
    pub model_id: String,
    pub cards: BTreeMap<String, CardEntry>,
}

impl FidelityCard {
    /// `~/.sovereign/model-fidelity-cards` (honours `$HOME`).
    pub fn default_dir() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".sovereign").join("model-fidelity-cards")
    }

    /// A filesystem-safe filename for a model id (slashes/colons → '_').
    fn file_for(model_id: &str) -> String {
        let safe: String = model_id
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
            .collect();
        format!("{safe}.json")
    }

    /// Load an existing card for `model_id` from `dir`, or start a fresh one.
    pub fn load_or_new(dir: &Path, model_id: &str) -> Self {
        let path = dir.join(Self::file_for(model_id));
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(c) = serde_json::from_str::<FidelityCard>(&text) {
                return c;
            }
        }
        FidelityCard { model_id: model_id.to_string(), cards: BTreeMap::new() }
    }

    /// Insert/replace the entry for its class.
    pub fn upsert(&mut self, entry: CardEntry) {
        self.cards.insert(entry.class.clone(), entry);
    }

    /// Persist to `dir/<model>.json` (creating `dir`).
    pub fn save(&self, dir: &Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(Self::file_for(&self.model_id));
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        std::fs::write(&path, text)?;
        Ok(path)
    }
}

fn sign(x: f64) -> i32 {
    if x > 0.0 {
        1
    } else if x < 0.0 {
        -1
    } else {
        0
    }
}

fn mean_finite(xs: impl Iterator<Item = f64>) -> f64 {
    let v: Vec<f64> = xs.filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        f64::NAN
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

/// Fraction of `Some(true)` among the band's applicable cases. Returns
/// `(fraction, n_applicable)`; fraction is NaN when the band never applied.
fn pass_frac<'a>(rows: impl Iterator<Item = &'a ResultRow>, pick: impl Fn(&ResultRow) -> Option<bool>) -> (f64, usize) {
    let vals: Vec<bool> = rows.filter_map(|r| pick(r)).collect();
    if vals.is_empty() {
        return (f64::NAN, 0);
    }
    let t = vals.iter().filter(|b| **b).count();
    (t as f64 / vals.len() as f64, vals.len())
}

/// Grade one `(model, class)` from its scored rows. Mirrors the Python
/// verdict's `model_is_faithful` / `control_fails` so the card and the
/// human verdict agree.
#[allow(clippy::too_many_arguments)]
pub fn grade_class(
    rows: &[ResultRow],
    model_id: &str,
    class: &str,
    pool: &str,
    th: &GradeThresholds,
    _bands: &Bands,
    manifest_fingerprint: &str,
    graded_at: String,
) -> CardEntry {
    let mine = |variant: &'static str, control: bool, para: bool| {
        rows.iter().filter(move |r| {
            r.model_id == model_id
                && r.class == class
                && r.variant == variant
                && r.control == control
                && (control || r.paraphrase == para)
        })
    };

    let p1_delta = mean_finite(mine("dir_p1", false, false).map(|r| r.d_agent));
    let (mag_pass, mag_n) = pass_frac(mine("dir_p1", false, false), |r| r.magnitude_ok);
    let (flat_pass, _) = pass_frac(mine("dir_p2", false, false), |r| r.flat_ok);
    let (inv_pass, _) = pass_frac(mine("inv_i1", false, false), |r| r.invariance_ok);

    // Control (blindfold) directional accuracy + movement.
    let ctrl: Vec<&ResultRow> = mine("dir_p1", true, false).collect();
    let dir: Vec<bool> = ctrl
        .iter()
        .filter(|r| r.d_agent.is_finite() && r.d_struct != 0.0)
        .map(|r| sign(r.d_agent) == sign(r.d_struct))
        .collect();
    let control_dir_acc = if dir.is_empty() {
        f64::NAN
    } else {
        dir.iter().filter(|b| **b).count() as f64 / dir.len() as f64
    };
    let control_abs_delta = mean_finite(ctrl.iter().map(|r| r.d_agent)).abs();

    // Number of distinct base cases scored (strip perturbation suffixes).
    let n_cases = {
        let mut ids: Vec<&str> = mine("dir_p1", false, false)
            .map(|r| r.case_id.split('~').next().unwrap_or(&r.case_id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    };
    let stopped_early = rows
        .iter()
        .find(|r| r.model_id == model_id && r.class == class)
        .map(|r| r.stopped_early)
        .unwrap_or(false);

    // Grade. A control leak invalidates the run regardless of the model.
    let control_present = control_dir_acc.is_finite();
    let control_leaks = (control_dir_acc.is_finite() && control_dir_acc > th.control_max_dir_acc)
        || (control_abs_delta.is_finite() && control_abs_delta >= th.flat_max);

    let faithful = p1_delta < -th.collapse_min
        && mag_pass.is_finite()
        && mag_pass >= th.mag_pass
        && flat_pass.is_finite()
        && flat_pass >= th.flat_pass
        && inv_pass.is_finite()
        && inv_pass >= th.inv_pass;

    let (grade, confidence) = if control_present && control_leaks {
        // Confidence in the leak = how far past chance / the flat band.
        let c = control_dir_acc.max(control_abs_delta / th.flat_max).min(1.0);
        (Grade::ControlLeak, c)
    } else if n_cases < th.min_cases {
        (Grade::Inconclusive, 0.0)
    } else if faithful {
        let c = mag_pass.min(flat_pass).min(inv_pass);
        (Grade::Faithful, c)
    } else {
        // Unfaithful: confident to the extent the magnitude band is missed.
        let c = if mag_pass.is_finite() { 1.0 - mag_pass } else { 0.5 };
        (Grade::Unfaithful, c.clamp(0.0, 1.0))
    };

    let _ = mag_n;
    CardEntry {
        class: class.to_string(),
        grade,
        confidence,
        p1_delta,
        mag_pass,
        flat_pass,
        inv_pass,
        control_dir_acc,
        control_abs_delta,
        n_cases,
        pool: pool.to_string(),
        stopped_early,
        manifest_fingerprint: manifest_fingerprint.to_string(),
        graded_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        model: &str,
        class: &str,
        variant: &str,
        control: bool,
        d_agent: f64,
        d_struct: f64,
        mag: Option<bool>,
        flat: Option<bool>,
        inv: Option<bool>,
        case: &str,
    ) -> ResultRow {
        ResultRow {
            model_id: model.into(),
            class: class.into(),
            case_id: case.into(),
            pool: "dev".into(),
            variant: variant.into(),
            render: if control { "stripped".into() } else { "full".into() },
            paraphrase: false,
            control,
            expected_sign: 0,
            k_draws: 1,
            p_freq: 0.5,
            p_verbal: 0.5,
            d_agent,
            d_struct,
            direction_ok: None,
            magnitude_ok: mag,
            flat_ok: flat,
            invariance_ok: inv,
            seed: 0,
            latency_ms: 0,
            n_drawn: 20,
            stopped_early: false,
            cs_lower: None,
            cs_upper: None,
        }
    }

    /// Build a faithful battery: large P1 collapse (mag passes), flat P2/INV,
    /// blindfold control inert.
    fn faithful_rows(model: &str) -> Vec<ResultRow> {
        let mut rows = Vec::new();
        for i in 0..20 {
            let case = format!("c{i}");
            rows.push(row(model, "wt", "dir_p1", false, -0.9, -0.95, Some(true), None, None, &format!("{case}~p1")));
            rows.push(row(model, "wt", "dir_p2", false, 0.01, 0.0, None, Some(true), None, &format!("{case}~p2")));
            rows.push(row(model, "wt", "inv_i1", false, 0.01, 0.0, None, None, Some(true), &format!("{case}~inv")));
            // Blindfold control: cannot see the change → no movement.
            rows.push(row(model, "wt", "dir_p1", true, 0.0, -0.95, None, None, None, &format!("{case}~p1")));
        }
        rows
    }

    #[test]
    fn faithful_battery_grades_faithful() {
        let rows = faithful_rows("m");
        let e = grade_class(&rows, "m", "wt", "dev", &GradeThresholds::default(), &Bands::default(), "fp", "t".into());
        assert_eq!(e.grade, Grade::Faithful);
        assert_eq!(e.n_cases, 20);
        assert!(e.p1_delta < -0.4);
        assert!(e.confidence > 0.9);
    }

    #[test]
    fn label_matcher_grades_unfaithful() {
        // Stays put on P1 (no collapse) → magnitude fails.
        let model = "m";
        let mut rows = Vec::new();
        for i in 0..20 {
            let case = format!("c{i}");
            rows.push(row(model, "wt", "dir_p1", false, 0.0, -0.95, Some(false), None, None, &format!("{case}~p1")));
            rows.push(row(model, "wt", "dir_p2", false, 0.01, 0.0, None, Some(true), None, &format!("{case}~p2")));
            rows.push(row(model, "wt", "inv_i1", false, 0.01, 0.0, None, None, Some(true), &format!("{case}~inv")));
            rows.push(row(model, "wt", "dir_p1", true, 0.0, -0.95, None, None, None, &format!("{case}~p1")));
        }
        let e = grade_class(&rows, model, "wt", "dev", &GradeThresholds::default(), &Bands::default(), "fp", "t".into());
        assert_eq!(e.grade, Grade::Unfaithful);
    }

    #[test]
    fn leaky_control_grades_control_leak() {
        // Even a "faithful-looking" model is unreadable if the blindfold
        // control tracks the hidden change.
        let mut rows = faithful_rows("m");
        for r in rows.iter_mut() {
            if r.control && r.variant == "dir_p1" {
                r.d_agent = -0.9; // control "sees" the negation → leak
            }
        }
        let e = grade_class(&rows, "m", "wt", "dev", &GradeThresholds::default(), &Bands::default(), "fp", "t".into());
        assert_eq!(e.grade, Grade::ControlLeak);
    }

    #[test]
    fn underpowered_grades_inconclusive() {
        let model = "m";
        let mut rows = Vec::new();
        for i in 0..5 {
            let case = format!("c{i}");
            rows.push(row(model, "wt", "dir_p1", false, -0.9, -0.95, Some(true), None, None, &format!("{case}~p1")));
            rows.push(row(model, "wt", "dir_p1", true, 0.0, -0.95, None, None, None, &format!("{case}~p1")));
        }
        let e = grade_class(&rows, model, "wt", "dev", &GradeThresholds::default(), &Bands::default(), "fp", "t".into());
        assert_eq!(e.grade, Grade::Inconclusive);
    }

    #[test]
    fn card_round_trips_and_merges_by_class() {
        let dir = std::env::temp_dir().join("mf_card_unit_test");
        let _ = std::fs::remove_dir_all(&dir);
        let mut card = FidelityCard::load_or_new(&dir, "model/x:1");
        let mk = |class: &str, grade: Grade| CardEntry {
            class: class.into(),
            grade,
            confidence: 0.9,
            p1_delta: -0.5,
            mag_pass: 0.9,
            flat_pass: 0.95,
            inv_pass: 0.95,
            control_dir_acc: 0.0,
            control_abs_delta: 0.0,
            n_cases: 20,
            pool: "dev".into(),
            stopped_early: true,
            manifest_fingerprint: "fp".into(),
            graded_at: "t".into(),
        };
        card.upsert(mk("wealth_tax", Grade::Faithful));
        card.upsert(mk("attribution", Grade::Unfaithful));
        let path = card.save(&dir).unwrap();
        // Filename is sanitized.
        assert!(path.file_name().unwrap().to_str().unwrap().starts_with("model_x_1"));

        let reloaded = FidelityCard::load_or_new(&dir, "model/x:1");
        assert_eq!(reloaded.cards.len(), 2);
        assert_eq!(reloaded.cards["wealth_tax"].grade, Grade::Faithful);
        // Re-grading a class overwrites only that class.
        let mut reloaded = reloaded;
        reloaded.upsert(mk("wealth_tax", Grade::Unfaithful));
        assert_eq!(reloaded.cards.len(), 2);
        assert_eq!(reloaded.cards["wealth_tax"].grade, Grade::Unfaithful);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
