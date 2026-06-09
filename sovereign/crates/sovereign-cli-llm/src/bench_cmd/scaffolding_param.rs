// SPDX-License-Identifier: AGPL-3.0-or-later
//! The typed write-back surface for the Fidelity-Flywheel promotion loop.
//!
//! A [`ScaffoldingParam`] is the ONLY thing the loop may change — and it is
//! decoupled from `atoms.json` (the verifier's oracle) **by construction**:
//! there is no atlas/enrichment variant here, so the loop structurally cannot
//! touch the thing the oracle reads. The first (and v1-only) channel is
//! retrieval reranking, applied in-process via the env vars `build_session`
//! already reads (`SOVEREIGN_RERANK_*`), so a candidate change needs only a new
//! bench-process arm — no daemon restart.
//!
//! Higher-blast-radius channels (routing thresholds, then enrichment) will join
//! as new variants with [`AutoApplyPolicy::ProposeOnly`]; rerank is
//! [`AutoApplyPolicy::AutoOnPass`] because it is atoms-decoupled, in-process,
//! and trivially reversible.
//!
//! The pure decision logic ([`decide`]) and the settings struct are unit-tested
//! here; the live arms + gate reuse live in [`super::promote`].

use serde::{Deserialize, Serialize};

use super::lane_baseline::{diff, LaneBaseline, LaneDiff};

/// The persisted retrieval-rerank settings — the loop's tunable state for the
/// first channel. Serialized to `candidate/rerank.toml` on an accepted change;
/// applied to a bench arm by setting the env vars `build_session` reads.
///
/// Only the knobs effective WITHOUT a local cross-encoder model are exposed
/// (the `SOVEREIGN_RERANK_DEDUP_ONLY` path): `enabled` (overfetch + per-article
/// dedup on the fusion ordering) and `candidates_k` (the overfetch pool). The
/// cross-encoder `alpha`/`atlas_weight` knobs are deliberately omitted until a
/// reranker model is in scope — exposing them here would silently no-op.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RerankSettings {
    pub enabled: bool,
    pub candidates_k: usize,
}

impl Default for RerankSettings {
    fn default() -> Self {
        // Matches the live default: reranking off; pool size 50 when on.
        Self { enabled: false, candidates_k: 50 }
    }
}

impl RerankSettings {
    /// Load from a `candidate/rerank.toml` artifact; missing file = default
    /// (the loop's first run starts from the live default).
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(t) => toml::from_str(&t).map_err(|e| format!("parse {path:?}: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("read {path:?}: {e}")),
        }
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(path, text).map_err(|e| format!("write {path:?}: {e}"))
    }

    /// Set the `SOVEREIGN_RERANK_*` env vars `build_session` reads so the NEXT
    /// in-process session is built with these settings. `corpus` is added to the
    /// dedup allowlist so the knob actually bites on the corpus under test (the
    /// live default allowlist is SEP-only).
    ///
    /// # Safety
    /// Sequential use only — the bench runs one arm at a time, with no
    /// concurrent readers of these env vars between `set_env` and the
    /// subsequent `build_session`.
    pub fn set_env(&self, corpus: &str) {
        set_var("SOVEREIGN_RERANK_DEDUP_ONLY", if self.enabled { "1" } else { "0" });
        set_var("SOVEREIGN_RERANK_CANDIDATES_K", &self.candidates_k.to_string());
        // Apply dedup to the corpus under test (default allowlist is SEP-only,
        // which would make the knob a no-op on any other corpus).
        set_var("SOVEREIGN_RERANK_DEDUP_CORPORA", corpus);
    }
}

fn set_var(k: &str, v: &str) {
    // `std::env::set_var` is safe on this edition; isolated in one place so a
    // future edition bump (where it becomes `unsafe`) touches a single call.
    std::env::set_var(k, v);
}

/// A single proposed change to the rerank settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RerankParam {
    Enabled(bool),
    CandidatesK(usize),
}

/// Whether an accepted change auto-applies, or only emits a proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoApplyPolicy {
    /// Auto-apply on a passing gate (rerank: atoms-decoupled, in-process, reversible).
    AutoOnPass,
    /// Propose only; a human applies (reserved for routing / enrichment channels).
    ProposeOnly,
}

/// The typed write-back surface. No atlas/enrichment variant exists, so the
/// loop structurally cannot target `atoms.json`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScaffoldingParam {
    Rerank(RerankParam),
}

impl ScaffoldingParam {
    /// Parse a `--param` spec, e.g. `rerank.enabled=true` / `rerank.candidates_k=80`.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let (key, val) = spec
            .split_once('=')
            .ok_or_else(|| format!("--param must be key=value, got `{spec}`"))?;
        match key.trim() {
            "rerank.enabled" => {
                let b = parse_bool(val.trim())?;
                Ok(Self::Rerank(RerankParam::Enabled(b)))
            }
            "rerank.candidates_k" => {
                let n = val
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| format!("rerank.candidates_k must be a usize, got `{val}`"))?;
                Ok(Self::Rerank(RerankParam::CandidatesK(n)))
            }
            other => Err(format!(
                "unknown --param key `{other}` (supported: rerank.enabled, rerank.candidates_k)"
            )),
        }
    }

    /// Stable id for the glassbox / write-back, e.g. `rerank.enabled`.
    pub fn id(&self) -> &'static str {
        match self {
            Self::Rerank(RerankParam::Enabled(_)) => "rerank.enabled",
            Self::Rerank(RerankParam::CandidatesK(_)) => "rerank.candidates_k",
        }
    }

    pub fn auto_apply_policy(&self) -> AutoApplyPolicy {
        match self {
            // Rerank is atoms-decoupled, in-process, reversible → safe to auto-apply.
            Self::Rerank(_) => AutoApplyPolicy::AutoOnPass,
        }
    }

    /// Apply the proposed change to a settings struct (returns the candidate).
    pub fn apply(&self, base: RerankSettings) -> RerankSettings {
        let mut s = base;
        match self {
            Self::Rerank(RerankParam::Enabled(b)) => s.enabled = *b,
            Self::Rerank(RerankParam::CandidatesK(n)) => s.candidates_k = *n,
        }
        s
    }
}

fn parse_bool(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(format!("expected a bool, got `{other}`")),
    }
}

/// The promotion verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromoteDecision {
    /// Candidate improved ≥1 metric past tolerance with NO regression — promote.
    Accept,
    /// Candidate regressed ≥1 red line past tolerance — reject.
    Reject,
    /// All movement within tolerance (noise) — don't thrash the baseline.
    NoChange,
}

/// The decision rule (pure, the substrate's heart): diff the candidate arm
/// against the baseline arm with the reused gate, then:
///   - any regression → Reject (a red line crossed),
///   - else any improvement past tolerance → Accept (a ≥2–3-item real move),
///   - else NoChange (sub-tolerance = noise; the ≥3-item-collapse discipline
///     that stops the loop thrashing on run-to-run nondeterminism).
pub fn decide(baseline_arm: &LaneBaseline, candidate_arm: &LaneBaseline) -> (PromoteDecision, LaneDiff) {
    let d = diff(Some(baseline_arm), candidate_arm);
    let decision = if d.n_regressed() > 0 {
        PromoteDecision::Reject
    } else if d.improvements().count() > 0 {
        PromoteDecision::Accept
    } else {
        PromoteDecision::NoChange
    };
    (decision, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench_cmd::lane_baseline::LaneMetric;

    #[test]
    fn parse_and_apply_rerank_params() {
        let base = RerankSettings::default();
        assert!(!base.enabled);

        let p = ScaffoldingParam::parse("rerank.enabled=true").unwrap();
        assert_eq!(p.id(), "rerank.enabled");
        assert_eq!(p.auto_apply_policy(), AutoApplyPolicy::AutoOnPass);
        assert!(p.apply(base).enabled);

        let p = ScaffoldingParam::parse("rerank.candidates_k=80").unwrap();
        assert_eq!(p.apply(base).candidates_k, 80);

        assert!(ScaffoldingParam::parse("rerank.enabled").is_err(), "missing =value");
        assert!(ScaffoldingParam::parse("bogus=1").is_err(), "unknown key");
        assert!(ScaffoldingParam::parse("rerank.candidates_k=nope").is_err());
    }

    fn lane(competence: f64, honesty: f64, hallu: f64) -> LaneBaseline {
        LaneBaseline::new("flywheel", "t")
            .with("competence", LaneMetric::higher_is_better(competence, 0.15))
            .with("honesty", LaneMetric::higher_is_better(honesty, 0.18))
            .with("hallucination_rate", LaneMetric::lower_is_better(hallu, 0.18))
    }

    #[test]
    fn decide_accepts_improvement_rejects_regression_holds_noise() {
        let base = lane(0.50, 0.50, 0.50);

        // Competence +0.20 (past 0.15 tol), nothing regressed → Accept.
        let (d, _) = decide(&base, &lane(0.70, 0.50, 0.50));
        assert_eq!(d, PromoteDecision::Accept);

        // Honesty -0.30 (regression past 0.18 tol) → Reject, even with a gain elsewhere.
        let (d, _) = decide(&base, &lane(0.70, 0.20, 0.50));
        assert_eq!(d, PromoteDecision::Reject);

        // All within tolerance (one-item wobble) → NoChange (don't thrash).
        let (d, _) = decide(&base, &lane(0.55, 0.45, 0.55));
        assert_eq!(d, PromoteDecision::NoChange);
    }

    #[test]
    fn settings_round_trip_toml() {
        let dir = std::env::temp_dir().join("flywheel_rerank_settings_unit");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("rerank.toml");
        let s = RerankSettings { enabled: true, candidates_k: 64 };
        s.save(&path).unwrap();
        assert_eq!(RerankSettings::load(&path).unwrap(), s);
        // Missing file → default.
        assert_eq!(
            RerankSettings::load(&dir.join("absent.toml")).unwrap(),
            RerankSettings::default()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
