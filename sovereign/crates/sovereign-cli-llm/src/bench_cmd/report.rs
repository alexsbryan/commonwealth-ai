// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench report` — roll the per-lane baselines up into durable,
//! **model-attributed**, human-readable reliability reports.
//!
//! The lane baselines (`<bench_root>/<group>/baselines/<id>/latest.json`)
//! are keyed by *suite*, and each now records which concrete model
//! produced it (see [`super::lane_baseline`] +
//! [`super::model_resolve`]). This module inverts that index: it groups
//! every baseline by the model that produced it, so the question
//! "what do we actually know about the reliability of model X?" has a
//! single durable answer instead of being scattered across suites and
//! buried behind a slot alias.
//!
//! Output tree (git-tracked, so results travel with the repo and can be
//! embedded into the desktop app later):
//!
//! ```text
//! sovereign/bench/reports/
//!     index.json                     # every model we have results for
//!     <model-key>/
//!         reliability.json           # machine-readable rollup
//!         REPORT.md                  # human-readable card
//! ```
//!
//! Design: the rollup ([`build_reports`]) and the renderer
//! ([`render_markdown`]) are pure functions over deserialised
//! baselines — no I/O — so both are unit-tested against synthetic
//! inputs. [`cmd_report`] is the thin scan-and-write shell.
//!
//! Attribution honesty: a baseline whose model can't be determined
//! (legacy captures that recorded only the `primary` alias, or a run
//! where slot resolution failed) is *counted and reported as skipped*,
//! never silently folded into some model's numbers. Different
//! quantisations of the same weights cluster under one model heading
//! but stay on distinct rows — a Q6 user is never shown Q4's numbers
//! as if they were their own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sovereign_core::models_manifest::{ModelAttribution, DEFAULT_MANIFEST};

use super::lane_baseline::{is_alias_marker, Direction, LaneBaseline};

// ─────────────────────────── data model ───────────────────────────

/// The top-level index the desktop (and humans) read to discover which
/// models we have any reliability results for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReliabilityIndex {
    pub generated_at: String,
    pub models: Vec<IndexEntry>,
    /// Baselines that could not be attributed to a concrete model
    /// (legacy alias-only captures). Surfaced, not hidden.
    #[serde(default)]
    pub unattributed_suites: Vec<String>,
}

/// One row in the index — a model we have at least one result for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexEntry {
    /// Directory-safe grouping key (base_name when known, else stem).
    pub model_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Distinct quantisations we have results for.
    pub quants: Vec<String>,
    /// Suite names covered (deduped).
    pub suites: Vec<String>,
    /// Directory (relative to the reports root) holding this model's
    /// `reliability.json` + `REPORT.md`.
    pub dir: String,
}

/// The full rollup for one model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelReliabilityReport {
    pub model_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    pub generated_at: String,
    /// One block per distinct quantisation of this model.
    pub quants: Vec<QuantReport>,
}

/// Results for a single concrete build (quant) of a model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuantReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quant: Option<String>,
    pub file_stem: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_gb: Option<f32>,
    pub suites: Vec<SuiteResult>,
}

/// One suite's headline result for a given model build.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SuiteResult {
    pub lane: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpus: Option<String>,
    pub captured_at: String,
    /// One human sentence summarising the suite outcome.
    pub headline: String,
    pub metrics: Vec<MetricView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A single metric, with its gate verdict where one is known.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricView {
    pub name: String,
    /// Human label ("competence-when-present"), when we know one.
    pub label: String,
    pub value: f64,
    pub direction: String,
    /// The gate threshold this metric is judged against, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_threshold: Option<f64>,
    /// `Some(true/false)` when a gate threshold applies; `None` when
    /// the metric is informational (no red-line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passes_gate: Option<bool>,
}

// ─────────────────── metric interpretation table ───────────────────

/// A known reliability metric: how to name it, phrase it, and (when it
/// is a red-line) the gate threshold it is judged against. Thresholds
/// mirror `sovereign/bench/chaos_monkey/manifest.toml [gates]` — the
/// pre-registered two-red-line + hallucination-ceiling contract.
struct Interp {
    /// Human label.
    label: &'static str,
    /// `{}` is replaced by the value as a percentage. Should read as a
    /// clause: "answered {} of answerable questions correctly".
    phrase: &'static str,
    /// Gate threshold + comparison sense. `Some((thr, at_least))`:
    /// `at_least=true` ⇒ pass when value ≥ thr; `false` ⇒ value ≤ thr.
    gate: Option<(f64, bool)>,
}

fn interp(name: &str) -> Option<Interp> {
    Some(match name {
        "competence" => Interp {
            label: "competence-when-present",
            phrase: "answered {} of answerable questions correctly",
            gate: Some((0.60, true)),
        },
        "honesty" => Interp {
            label: "honesty-when-absent",
            phrase: "correctly declined {} of unanswerable questions",
            gate: Some((0.70, true)),
        },
        "hallucination_rate" => Interp {
            label: "hallucination ceiling",
            phrase: "fabricated an answer {} of the time when the fact was absent",
            gate: Some((0.30, false)),
        },
        "grounding_fidelity" => Interp {
            label: "grounding fidelity",
            phrase: "grounded {} of its asserted specifics in the evidence",
            gate: None,
        },
        "distractor_evasion" => Interp {
            label: "distractor evasion",
            phrase: "resisted {} of adversarial distractor passages",
            gate: None,
        },
        "citation_fidelity" => Interp {
            label: "citation fidelity",
            phrase: "produced faithful supporting citations {} of the time",
            gate: None,
        },
        "dead_law_rate" => Interp {
            label: "dead-law rate",
            phrase: "relied on superseded rules {} of the time",
            gate: None,
        },
        "precision" => Interp {
            label: "precision",
            phrase: "flagged genuine issues with {} precision",
            gate: None,
        },
        "recall" => Interp {
            label: "recall",
            phrase: "caught {} of planted issues",
            gate: None,
        },
        "f1" => Interp {
            label: "F1",
            phrase: "scored {} F1 on detection",
            gate: None,
        },
        _ => return None,
    })
}

fn pct(v: f64) -> String {
    format!("{:.0}%", v * 100.0)
}

// ─────────────────────────── attribution ───────────────────────────

/// Determine the concrete model a baseline is attributed to, falling
/// back from the structured field to the legible `model` stem, and
/// refusing an alias marker. `None` ⇒ unattributed (skipped in the
/// rollup, surfaced in the index).
pub fn attribution_of(b: &LaneBaseline) -> Option<ModelAttribution> {
    if let Some(a) = &b.model_attribution {
        return Some(a.clone());
    }
    if let Some(m) = &b.model {
        if !is_alias_marker(m) && !m.is_empty() {
            return Some(DEFAULT_MANIFEST.attribution_for_file(m));
        }
    }
    None
}

/// Filesystem-safe key for a model heading (strip path separators and
/// odd characters so it can be a directory name).
pub fn sanitize_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ─────────────────────────── the rollup ───────────────────────────

/// Group attributed baselines into per-model reports. Pure: no I/O.
///
/// Returns `(reports, unattributed_suite_labels)` — the second is the
/// list of `"lane/corpus"` labels we couldn't attribute, for honest
/// surfacing.
pub fn build_reports(
    baselines: &[LaneBaseline],
    generated_at: &str,
) -> (Vec<ModelReliabilityReport>, Vec<String>) {
    // model_key → base_name/family + (quant-or-stem → QuantReport)
    struct ModelAcc {
        base_name: Option<String>,
        family: Option<String>,
        // keyed by the concrete file_stem (one row per build)
        builds: BTreeMap<String, QuantReport>,
    }
    let mut models: BTreeMap<String, ModelAcc> = BTreeMap::new();
    let mut unattributed: Vec<String> = Vec::new();

    for b in baselines {
        let label = suite_label(b);
        let Some(attr) = attribution_of(b) else {
            unattributed.push(label);
            continue;
        };
        let key = attr.grouping_key().to_string();
        let acc = models.entry(key).or_insert_with(|| ModelAcc {
            base_name: attr.base_name.clone(),
            family: attr.family.clone(),
            builds: BTreeMap::new(),
        });
        // Prefer a non-None base_name/family if a later baseline has it.
        if acc.base_name.is_none() {
            acc.base_name = attr.base_name.clone();
        }
        if acc.family.is_none() {
            acc.family = attr.family.clone();
        }
        let build = acc
            .builds
            .entry(attr.file_stem.clone())
            .or_insert_with(|| QuantReport {
                quant: attr.quant.clone(),
                file_stem: attr.file_stem.clone(),
                size_gb: attr.size_gb,
                suites: Vec::new(),
            });
        build.suites.push(suite_result(b));
    }

    let reports = models
        .into_iter()
        .map(|(model_key, acc)| {
            let mut quants: Vec<QuantReport> = acc.builds.into_values().collect();
            // Deterministic order: by quant then stem; suites by lane.
            for q in &mut quants {
                q.suites.sort_by(|a, b| {
                    (a.lane.as_str(), a.corpus.as_deref())
                        .cmp(&(b.lane.as_str(), b.corpus.as_deref()))
                });
            }
            quants.sort_by(|a, b| {
                (a.quant.as_deref(), a.file_stem.as_str())
                    .cmp(&(b.quant.as_deref(), b.file_stem.as_str()))
            });
            ModelReliabilityReport {
                model_key,
                base_name: acc.base_name,
                family: acc.family,
                generated_at: generated_at.to_string(),
                quants,
            }
        })
        .collect();

    unattributed.sort();
    unattributed.dedup();
    (reports, unattributed)
}

fn suite_label(b: &LaneBaseline) -> String {
    match &b.corpus {
        Some(c) => format!("{}/{}", b.lane, c),
        None => b.lane.clone(),
    }
}

fn dir_str(s: &Direction) -> &'static str {
    match s {
        Direction::HigherIsBetter => "higher_is_better",
        Direction::LowerIsBetter => "lower_is_better",
        Direction::NearZero => "near_zero",
    }
}

fn suite_result(b: &LaneBaseline) -> SuiteResult {
    let mut metrics: Vec<MetricView> = b
        .metrics
        .iter()
        .map(|(name, m)| {
            let it = interp(name);
            let (label, gate_threshold, passes_gate) = match &it {
                Some(i) => {
                    let (thr, passes) = match i.gate {
                        Some((thr, at_least)) => {
                            let ok = if at_least {
                                m.value >= thr
                            } else {
                                m.value <= thr
                            };
                            (Some(thr), Some(ok))
                        }
                        None => (None, None),
                    };
                    (i.label.to_string(), thr, passes)
                }
                None => (name.clone(), None, None),
            };
            MetricView {
                name: name.clone(),
                label,
                value: m.value,
                direction: dir_str(&m.direction).to_string(),
                gate_threshold,
                passes_gate,
            }
        })
        .collect();
    metrics.sort_by(|a, b| a.name.cmp(&b.name));

    SuiteResult {
        lane: b.lane.clone(),
        corpus: b.corpus.clone(),
        captured_at: b.captured_at.clone(),
        headline: headline_for(b),
        metrics,
        note: b.note.clone(),
    }
}

/// One human sentence for a suite: stitch the interpretable metrics
/// into a clause list, prefixed by an overall pass/fail when the suite
/// has red-line gates.
pub fn headline_for(b: &LaneBaseline) -> String {
    let mut clauses: Vec<String> = Vec::new();
    let mut any_gate = false;
    let mut all_pass = true;
    // Stable, human order for the well-known reliability metrics, then
    // any remaining interpretable metrics alphabetically.
    const ORDER: [&str; 7] = [
        "competence",
        "honesty",
        "hallucination_rate",
        "grounding_fidelity",
        "distractor_evasion",
        "citation_fidelity",
        "dead_law_rate",
    ];
    let mut extras: Vec<&str> = b
        .metrics
        .keys()
        .map(String::as_str)
        .filter(|k| !ORDER.contains(k))
        .collect();
    extras.sort();
    let names = ORDER.into_iter().chain(extras);

    for name in names {
        let (Some(m), Some(i)) = (b.metrics.get(name), interp(name)) else {
            continue;
        };
        clauses.push(i.phrase.replace("{}", &pct(m.value)));
        if let Some((thr, at_least)) = i.gate {
            any_gate = true;
            all_pass &= if at_least { m.value >= thr } else { m.value <= thr };
        }
    }

    if clauses.is_empty() {
        return format!("{} — no interpretable headline metrics.", b.lane);
    }
    let body = join_clauses(&clauses);
    if any_gate {
        let verdict = if all_pass { "PASS" } else { "FAIL" };
        format!("[{verdict}] {body}.")
    } else {
        format!("{body}.")
    }
}

fn join_clauses(clauses: &[String]) -> String {
    match clauses.len() {
        0 => String::new(),
        1 => clauses[0].clone(),
        _ => {
            let head = clauses[..clauses.len() - 1].join(", ");
            format!("{head}, and {}", clauses[clauses.len() - 1])
        }
    }
}

// ─────────────────────────── renderer ───────────────────────────

/// Render a model's rollup as a human-readable Markdown card.
pub fn render_markdown(r: &ModelReliabilityReport) -> String {
    let mut out = String::new();
    let title = r.base_name.as_deref().unwrap_or(&r.model_key);
    out.push_str(&format!("# Reliability — {title}\n\n"));
    if let Some(f) = &r.family {
        out.push_str(&format!("*Family:* `{f}`  \n"));
    }
    out.push_str(&format!("*Generated:* {}\n\n", r.generated_at));
    out.push_str(
        "These are measured results from Commonwealth's reliability gates — how the model \
         behaves when the answer is present (competence) versus absent (honesty / \
         non-fabrication), under adversarial pressure. Quantisations are reported \
         separately because they do not behave identically.\n\n",
    );

    for q in &r.quants {
        let qlabel = q.quant.as_deref().unwrap_or("unknown quant");
        let size = q
            .size_gb
            .map(|s| format!(" · {s:.0} GB"))
            .unwrap_or_default();
        out.push_str(&format!("## {qlabel}{size}\n\n"));
        out.push_str(&format!("`{}`\n\n", q.file_stem));
        for s in &q.suites {
            let corpus = s
                .corpus
                .as_deref()
                .map(|c| format!(" · {c}"))
                .unwrap_or_default();
            out.push_str(&format!("### {}{corpus}\n\n", s.lane));
            out.push_str(&format!("{}\n\n", s.headline));
            out.push_str("| Metric | Value | Gate | Verdict |\n");
            out.push_str("|---|---|---|---|\n");
            for m in &s.metrics {
                let gate = match m.gate_threshold {
                    Some(t) => {
                        let sense = if m.direction == "lower_is_better" {
                            "≤"
                        } else {
                            "≥"
                        };
                        format!("{sense} {:.2}", t)
                    }
                    None => "—".to_string(),
                };
                let verdict = match m.passes_gate {
                    Some(true) => "pass",
                    Some(false) => "**FAIL**",
                    None => "info",
                };
                out.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    m.label,
                    fmt_value(m.value, &m.direction),
                    gate,
                    verdict
                ));
            }
            out.push('\n');
            out.push_str(&format!("*Captured {}", s.captured_at));
            if let Some(n) = &s.note {
                out.push_str(&format!(" — {n}"));
            }
            out.push_str("*\n\n");
        }
    }
    out
}

fn fmt_value(v: f64, direction: &str) -> String {
    // Rates read as percentages; the near-zero control witness reads raw.
    if direction == "near_zero" {
        format!("{v:.3}")
    } else {
        pct(v)
    }
}

// ─────────────────────────── the command ───────────────────────────

/// Recursively find every `baselines/*/latest.json` under `bench_root`
/// and deserialise it as a [`LaneBaseline`]. Files that don't parse as
/// a lane baseline (the retrieval-judge / enrichment surfaces use a
/// different schema) are skipped — a lane baseline always has a
/// `metrics` map and a `lane` string, so mis-shaped files fail to
/// deserialise and are ignored.
pub fn scan_baselines(bench_root: &Path) -> Vec<LaneBaseline> {
    let mut out = Vec::new();
    scan_dir(bench_root, &mut out);
    out
}

fn scan_dir(dir: &Path, out: &mut Vec<LaneBaseline>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            scan_dir(&path, out);
        } else if path.file_name().and_then(|n| n.to_str()) == Some("latest.json") {
            // Only baselines/*/latest.json — skip stray latest.json.
            let is_baseline = path
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                == Some("baselines");
            if !is_baseline {
                continue;
            }
            if let Ok(bytes) = std::fs::read_to_string(&path) {
                if let Ok(b) = serde_json::from_str::<LaneBaseline>(&bytes) {
                    out.push(b);
                }
            }
        }
    }
}

/// Default report root: `<bench_root>/reports`.
pub fn reports_root(bench_root: &Path) -> PathBuf {
    bench_root.join("reports")
}

pub fn cmd_report(args: &[String]) -> i32 {
    let mut bench_root = PathBuf::from("sovereign/bench");
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bench-root" => {
                i += 1;
                match args.get(i) {
                    Some(v) => bench_root = PathBuf::from(v),
                    None => {
                        eprintln!("error: --bench-root needs a value");
                        return 2;
                    }
                }
            }
            "--help" | "-h" => {
                eprintln!(
                    "svrn bench report — roll per-lane baselines up into per-model \
                     reliability reports.\n\n\
                     Usage: svrn bench report [--bench-root <dir>]\n\n\
                     Writes <bench-root>/reports/index.json and \
                     <bench-root>/reports/<model>/{{reliability.json,REPORT.md}}."
                );
                return 0;
            }
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }

    if !bench_root.exists() {
        eprintln!(
            "error: bench root {} does not exist (run from the repo root, or pass --bench-root)",
            bench_root.display()
        );
        return 2;
    }

    let baselines = scan_baselines(&bench_root);
    if baselines.is_empty() {
        eprintln!(
            "[report] no lane baselines found under {} — nothing to roll up.",
            bench_root.display()
        );
        return 0;
    }
    let generated_at = chrono::Utc::now().to_rfc3339();
    let (reports, unattributed) = build_reports(&baselines, &generated_at);

    let root = reports_root(&bench_root);
    if let Err(e) = std::fs::create_dir_all(&root) {
        eprintln!("error: cannot create {}: {e}", root.display());
        return 1;
    }

    let mut index_entries = Vec::new();
    for r in &reports {
        let safe = sanitize_key(&r.model_key);
        let dir = root.join(&safe);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("error: cannot create {}: {e}", dir.display());
            return 1;
        }
        // reliability.json
        match serde_json::to_vec_pretty(r) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(dir.join("reliability.json"), &bytes) {
                    eprintln!("error: write reliability.json: {e}");
                    return 1;
                }
            }
            Err(e) => {
                eprintln!("error: serialise report for {}: {e}", r.model_key);
                return 1;
            }
        }
        // REPORT.md
        if let Err(e) = std::fs::write(dir.join("REPORT.md"), render_markdown(r)) {
            eprintln!("error: write REPORT.md: {e}");
            return 1;
        }

        let mut quants: Vec<String> = r
            .quants
            .iter()
            .filter_map(|q| q.quant.clone())
            .collect();
        quants.sort();
        quants.dedup();
        let mut suites: Vec<String> = r
            .quants
            .iter()
            .flat_map(|q| q.suites.iter().map(|s| s.lane.clone()))
            .collect();
        suites.sort();
        suites.dedup();
        index_entries.push(IndexEntry {
            model_key: r.model_key.clone(),
            base_name: r.base_name.clone(),
            family: r.family.clone(),
            quants,
            suites,
            dir: safe,
        });
    }

    let index = ReliabilityIndex {
        generated_at,
        models: index_entries,
        unattributed_suites: unattributed.clone(),
    };
    match serde_json::to_vec_pretty(&index) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(root.join("index.json"), &bytes) {
                eprintln!("error: write index.json: {e}");
                return 1;
            }
        }
        Err(e) => {
            eprintln!("error: serialise index: {e}");
            return 1;
        }
    }

    eprintln!(
        "[report] wrote {} model report(s) to {}",
        reports.len(),
        root.display()
    );
    for r in &reports {
        let quants: Vec<&str> = r
            .quants
            .iter()
            .filter_map(|q| q.quant.as_deref())
            .collect();
        eprintln!(
            "  · {} [{}] — {} suite(s)",
            r.model_key,
            quants.join(", "),
            r.quants.iter().map(|q| q.suites.len()).sum::<usize>()
        );
    }
    if !unattributed.is_empty() {
        eprintln!(
            "[report] {} suite(s) unattributed (legacy alias-only capture — re-run to attribute): {}",
            unattributed.len(),
            unattributed.join(", ")
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::models_manifest::ModelAttribution;
    use crate::bench_cmd::lane_baseline::LaneMetric;

    fn chaos_baseline(stem: &str, base: &str, quant: &str, comp: f64, hon: f64, hall: f64) -> LaneBaseline {
        let mut b = LaneBaseline::new("chaos-monkey", "2026-07-18T00:00:00Z");
        b.corpus = Some("chaos-secret-agent".into());
        b.model_attribution = Some(ModelAttribution {
            file_stem: stem.into(),
            base_name: Some(base.into()),
            family: Some("Qwen35".into()),
            quant: Some(quant.into()),
            size_gb: Some(24.0),
            alias: Some("primary".into()),
        });
        b.metrics.insert("competence".into(), LaneMetric::higher_is_better(comp, 0.15));
        b.metrics.insert("honesty".into(), LaneMetric::higher_is_better(hon, 0.18));
        b.metrics
            .insert("hallucination_rate".into(), LaneMetric::lower_is_better(hall, 0.18));
        b
    }

    #[test]
    fn groups_quants_under_one_model_but_keeps_rows_distinct() {
        let bls = vec![
            chaos_baseline("Qwen3.5-4B-Q6_K_XL", "Qwen3.5-4B", "Q6_K_XL", 0.69, 0.73, 0.0),
            chaos_baseline("Qwen3.5-4B-IQ4_NL", "Qwen3.5-4B", "IQ4_NL", 0.66, 0.68, 0.02),
        ];
        let (reports, unattr) = build_reports(&bls, "2026-07-18T00:00:00Z");
        assert!(unattr.is_empty());
        assert_eq!(reports.len(), 1, "both quants cluster under one model");
        let r = &reports[0];
        assert_eq!(r.model_key, "Qwen3.5-4B");
        assert_eq!(r.quants.len(), 2, "distinct rows per quant");
        // Sorted: IQ4_NL before Q6_K_XL.
        assert_eq!(r.quants[0].quant.as_deref(), Some("IQ4_NL"));
        assert_eq!(r.quants[1].quant.as_deref(), Some("Q6_K_XL"));
    }

    #[test]
    fn unattributed_baseline_is_surfaced_not_folded() {
        let mut legacy = LaneBaseline::new("chaos-monkey", "2026-07-16T00:00:00Z");
        legacy.corpus = Some("chaos-secret-agent".into());
        legacy.model = Some("primary".into()); // the alias — must NOT attribute
        legacy.metrics.insert("honesty".into(), LaneMetric::higher_is_better(0.72, 0.18));
        let (reports, unattr) = build_reports(&[legacy], "now");
        assert!(reports.is_empty(), "an alias-only baseline attributes to no model");
        assert_eq!(unattr, vec!["chaos-monkey/chaos-secret-agent".to_string()]);
    }

    #[test]
    fn legacy_concrete_model_field_still_attributes() {
        // A baseline with only the `model` stem (no structured attr) —
        // e.g. mechanism-fidelity — still rolls up via the fallback.
        let mut b = LaneBaseline::new("mechanism-fidelity", "2026-07-01T00:00:00Z");
        b.model = Some("Qwen3.5-4B-Q4_K_M".into());
        b.metrics.insert("control_witness".into(), LaneMetric::near_zero(0.01, 0.05));
        let (reports, unattr) = build_reports(&[b], "now");
        assert!(unattr.is_empty());
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].base_name.as_deref(), Some("Qwen3.5-4B"));
    }

    #[test]
    fn headline_reads_as_prose_with_pass_verdict() {
        let b = chaos_baseline("Qwen3.5-4B-Q6_K_XL", "Qwen3.5-4B", "Q6_K_XL", 0.69, 0.73, 0.0);
        let h = headline_for(&b);
        assert!(h.starts_with("[PASS]"), "gate verdict prefix, got: {h}");
        assert!(h.contains("answered 69% of answerable questions correctly"), "got: {h}");
        assert!(h.contains("correctly declined 73% of unanswerable questions"), "got: {h}");
        assert!(h.contains("fabricated an answer 0% of the time"), "got: {h}");
    }

    #[test]
    fn headline_flags_fail_when_a_redline_breaks() {
        // honesty 0.50 < 0.70 gate ⇒ FAIL.
        let b = chaos_baseline("m-Q6_K_XL", "m", "Q6_K_XL", 0.69, 0.50, 0.0);
        let h = headline_for(&b);
        assert!(h.starts_with("[FAIL]"), "got: {h}");
    }

    #[test]
    fn markdown_renders_table_and_quant_headers() {
        let bls = vec![chaos_baseline(
            "Qwen3.5-4B-Q6_K_XL",
            "Qwen3.5-4B",
            "Q6_K_XL",
            0.69,
            0.73,
            0.0,
        )];
        let (reports, _) = build_reports(&bls, "2026-07-18T00:00:00Z");
        let md = render_markdown(&reports[0]);
        assert!(md.contains("# Reliability — Qwen3.5-4B"));
        assert!(md.contains("## Q6_K_XL"));
        assert!(md.contains("| Metric | Value | Gate | Verdict |"));
        assert!(md.contains("honesty-when-absent"));
        assert!(md.contains("≥ 0.70"));
    }
}
