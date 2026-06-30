// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench enron run …` — Phase 5 measurement command for
//! the architecture-over-Enron substrate.
//!
//! Reads the corpus's `atlas/atoms.json` (produced by the regular
//! atlas-enrichment pass over an Enron-recipe corpus), folds Entity
//! atoms into a [`Clustering`] keyed by surface form, scores against
//! `sovereign/bench/enron/ground_truth_entities.jsonl` via
//! `sovereign_eval::entity_resolution_score::score`, and persists the
//! outcome under `sovereign/bench/enron/baselines/enron-entity-resolution/`.
//!
//! Two policies:
//!   - `pre_reconciliation` — every Entity atom is its own cluster
//!     (the intentionally-bad floor every Phase 4 tuning move must
//!     beat).
//!   - `tuned` — re-runs the multi-origin merger over the atoms with
//!     the recipe's `[enrichment.reconciliation]` policy, folds the
//!     [`ReconciledEntity`] result, and computes the delta against
//!     `pre_reconciliation.json`.
//!
//! Split discipline (Phase 3): the runner refuses to score the
//! `holdout` split without `--unseal-holdout`, which burns a counter
//! in `peek_budget.json`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::atoms::{AtomEnvelope, Entity};
use corpus_engine::enrichment::reconciliation::{reconcile, ReconciliationPolicy};
use serde::{Deserialize, Serialize};
use sovereign_eval::entity_resolution_bench::{BenchGroundTruth, PeekBudget, Split};
use sovereign_eval::entity_resolution_score::{score, Clustering, EntityResolutionReport};

use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench enron",
    summary:
        "Phase 5 measurement loop for the architecture-over-Enron substrate.",
    sections: &[
        HelpSection::Usage(
            "svrn bench enron run --corpus <id> --split {train|test|holdout} [--policy {pre_reconciliation|tuned}] [--judge-trials N] [--name-similarity-threshold <0..1>] [--unseal-holdout] [--bench-dir <path>] [--out <path>]",
        ),
        HelpSection::Subcommands(&[
            ("run", "Score a corpus's reconciled atoms against the ground-truth split."),
            ("diagnose", "Glass-box: per-gold coverage + cluster spread + over-merge bridges for the tuned policy."),
        ]),
        HelpSection::Notes(
            "Reads atlas/atoms.json from ~/.sovereign/indexes/<corpus>. Run \
             `svrn corpus install enron-sample-onemailbox` and let the \
             daemon enrich it before measuring. `--policy pre_reconciliation` \
             skips the multi-origin merger; `--policy tuned` (default) re-runs \
             reconciliation with the recipe's policy and computes the delta \
             vs pre_reconciliation.json.",
        ),
    ],
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    PreReconciliation,
    Tuned,
}

impl Policy {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "pre_reconciliation" | "pre-reconciliation" | "floor" => {
                Some(Policy::PreReconciliation)
            }
            "tuned" | "reconciled" => Some(Policy::Tuned),
            _ => None,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Policy::PreReconciliation => "pre_reconciliation",
            Policy::Tuned => "tuned",
        }
    }
}

#[derive(Debug)]
struct Args {
    corpus: String,
    split: Split,
    policy: Policy,
    judge_trials: u8,
    unseal_holdout: bool,
    bench_dir: PathBuf,
    out: Option<PathBuf>,
    indexes_dir: PathBuf,
    /// Optional override for `ReconciliationPolicy.name_similarity_threshold`.
    /// `None` falls back to the policy's compile-time default (0.85).
    /// Lets the operator sweep thresholds against an already-enriched
    /// corpus without re-running ingest — the recon pass is purely a
    /// function of the existing `atoms.json` entities.
    name_similarity_threshold: Option<f32>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut corpus: Option<String> = None;
    let mut split: Option<Split> = None;
    let mut policy = Policy::Tuned;
    let mut judge_trials: u8 = 3;
    let mut unseal_holdout = false;
    let mut bench_dir: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut indexes_dir: Option<PathBuf> = None;
    let mut name_similarity_threshold: Option<f32> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus" => {
                i += 1;
                corpus = Some(
                    args.get(i)
                        .ok_or_else(|| "--corpus requires a value".to_string())?
                        .clone(),
                );
            }
            "--split" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--split requires a value".to_string())?;
                split = Some(match v.as_str() {
                    "train" => Split::Train,
                    "test" => Split::Test,
                    "holdout" => Split::Holdout,
                    other => return Err(format!("unknown split: {other}")),
                });
            }
            "--policy" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--policy requires a value".to_string())?;
                policy = Policy::parse(v).ok_or_else(|| {
                    format!("unknown policy: {v}; expected pre_reconciliation|tuned")
                })?;
            }
            "--judge-trials" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--judge-trials requires a value".to_string())?;
                judge_trials = v
                    .parse::<u8>()
                    .map_err(|e| format!("--judge-trials: {e}"))?;
            }
            "--unseal-holdout" => {
                unseal_holdout = true;
            }
            "--bench-dir" => {
                i += 1;
                bench_dir = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--bench-dir requires a value".to_string())?,
                ));
            }
            "--out" => {
                i += 1;
                out = Some(PathBuf::from(
                    args.get(i)
                        .ok_or_else(|| "--out requires a value".to_string())?,
                ));
            }
            "--indexes-dir" => {
                i += 1;
                indexes_dir =
                    Some(PathBuf::from(args.get(i).ok_or_else(|| {
                        "--indexes-dir requires a value".to_string()
                    })?));
            }
            "--name-similarity-threshold" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| "--name-similarity-threshold requires a value".to_string())?;
                let parsed = v
                    .parse::<f32>()
                    .map_err(|e| format!("--name-similarity-threshold: {e}"))?;
                if !(0.0..=1.0).contains(&parsed) {
                    return Err(format!(
                        "--name-similarity-threshold must be in [0.0, 1.0]; got {parsed}"
                    ));
                }
                name_similarity_threshold = Some(parsed);
            }
            "--help" | "-h" => {
                help::print(&HELP);
                return Err("__HELP__".into());
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    let corpus = corpus.ok_or_else(|| "--corpus is required".to_string())?;
    let split = split.ok_or_else(|| "--split is required".to_string())?;
    let bench_dir = bench_dir.unwrap_or_else(default_bench_dir);
    let indexes_dir = indexes_dir.unwrap_or_else(default_indexes_dir);
    Ok(Args {
        corpus,
        split,
        policy,
        judge_trials,
        unseal_holdout,
        bench_dir,
        out,
        indexes_dir,
        name_similarity_threshold,
    })
}

fn default_bench_dir() -> PathBuf {
    // Walk up from current_exe / current_dir for the workspace
    // root. Falls back to CWD/sovereign/bench/enron.
    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("sovereign/bench/enron");
        if candidate.exists() {
            return candidate;
        }
        // Maybe we're inside sovereign/ — walk up one.
        if let Some(parent) = cwd.parent() {
            let candidate = parent.join("sovereign/bench/enron");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("sovereign/bench/enron")
}

fn default_indexes_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".sovereign/indexes")
    } else {
        PathBuf::from(".sovereign/indexes")
    }
}

// ── Outcome record persisted to JSON ─────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EnronBenchOutcome {
    schema_version: u32,
    bench_id: String,
    policy_kind: String,
    split: String,
    corpus: String,
    captured_ts_unix: i64,
    policy: Option<RecordedPolicy>,
    b_cubed: serde_json::Value,
    pairwise: serde_json::Value,
    signal_histogram: BTreeMap<String, usize>,
    surface_form_collapse_rate: Option<f64>,
    delta_from_pre_reconciliation_f1: Option<f64>,
    notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecordedPolicy {
    name_similarity_threshold: f32,
    cross_origin_required_signals: u8,
    judge_when_uncertain: bool,
    judge_trials: u8,
}

// ── Entry point ──────────────────────────────────────────────

pub async fn cmd_enron(args: &[String]) -> i32 {
    if args.is_empty() {
        help::print(&HELP);
        return 2;
    }
    match args[0].as_str() {
        "run" => match cmd_run(&args[1..]).await {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        "diagnose" => match cmd_diagnose(&args[1..]).await {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        "--help" | "-h" | "help" => {
            help::print(&HELP);
            0
        }
        other => {
            eprintln!("error: unknown enron subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}

async fn cmd_run(args: &[String]) -> Result<i32, String> {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) if e == "__HELP__" => return Ok(0),
        Err(e) => return Err(e),
    };

    // ── Holdout discipline: refuse without --unseal-holdout ────
    if parsed.split == Split::Holdout && !parsed.unseal_holdout {
        eprintln!(
            "error: --split holdout is sealed. Pass --unseal-holdout to \
             burn a peek-budget counter. This is intentional — every \
             unseal weakens the holdout as a generalisation estimate."
        );
        return Ok(2);
    }

    // ── Load ground truth + (optionally) unsealed holdout ──────
    let gt_path = parsed.bench_dir.join("ground_truth_entities.jsonl");
    let mut gt = BenchGroundTruth::load(&gt_path).map_err(|e| {
        format!(
            "load ground_truth_entities.jsonl ({}): {e}",
            gt_path.display()
        )
    })?;

    if parsed.split == Split::Holdout && parsed.unseal_holdout {
        let private_holdout_path = default_private_holdout_path();
        let added = gt
            .merge_unsealed_holdout(&private_holdout_path)
            .map_err(|e| {
                format!(
                    "merge unsealed holdout ({}): {e}",
                    private_holdout_path.display()
                )
            })?;
        if added == 0 {
            eprintln!(
                "warning: --unseal-holdout passed but no entries found at {}. \
                 Sealed entries will score as 0.",
                private_holdout_path.display()
            );
        }
        // Burn the peek counter regardless — the flag was passed,
        // the intent was registered.
        let budget_path = parsed
            .bench_dir
            .join("baselines/enron-entity-resolution/peek_budget.json");
        let mut budget = PeekBudget::load(&budget_path)
            .map_err(|e| format!("load peek_budget ({}): {e}", budget_path.display()))?;
        let n = budget.burn(
            format!(
                "--unseal-holdout from `svrn bench enron run`; corpus={}, policy={}",
                parsed.corpus,
                parsed.policy.as_str()
            ),
            git_head_short(),
        );
        budget
            .save(&budget_path)
            .map_err(|e| format!("save peek_budget: {e}"))?;
        eprintln!("peek_budget: holdout_peeks = {n} (burned this run)");
    }

    let gold: Clustering = gt.as_gold_clustering(parsed.split);
    if gold.is_empty() {
        eprintln!(
            "warning: no gold-cluster entries for split {} after loading {} (sealed?)",
            parsed.split.as_str(),
            gt_path.display()
        );
    }

    // ── Load the corpus's atoms.json ────────────────────────────
    let atlas_dir = parsed.indexes_dir.join(&parsed.corpus).join("atlas");
    let atoms_path = atlas_dir.join("atoms.json");
    if !atoms_path.exists() {
        eprintln!(
            "error: no atoms.json at {}. Run `svrn corpus install {}` \
             and let the daemon enrich it before measuring.",
            atoms_path.display(),
            parsed.corpus
        );
        return Ok(2);
    }
    let atoms_file = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("read atoms.json ({}): {e}", atoms_path.display()))?;

    let entities: Vec<Entity> = atoms_file
        .atoms
        .into_iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => Some(e),
            _ => None,
        })
        .collect();
    if entities.is_empty() {
        eprintln!("warning: 0 Entity atoms in {}.", atoms_path.display());
    }

    // ── Build predicted Clustering per policy ───────────────────
    let (predicted, signal_hist, recorded_policy) = match parsed.policy {
        Policy::PreReconciliation => {
            let mut predicted = Clustering::new();
            for e in &entities {
                let cluster_id = e.id.as_str().to_string();
                for sf in surface_forms_of(e) {
                    predicted.insert(sf, cluster_id.clone());
                }
            }
            (predicted, BTreeMap::new(), None)
        }
        Policy::Tuned => {
            let mut policy = ReconciliationPolicy::default();
            policy.judge_trials = parsed.judge_trials;
            if let Some(t) = parsed.name_similarity_threshold {
                policy.name_similarity_threshold = t;
            }
            let outcome = reconcile(entities.clone(), &policy);
            let mut hist: BTreeMap<String, usize> = BTreeMap::new();
            for re in &outcome.entities {
                for s in &re.signals_fired {
                    *hist.entry(s.as_str().to_string()).or_insert(0) += 1;
                }
            }
            // Append the reconciler's oplog entries to the on-disk
            // oplog so the audit trail survives the process exiting.
            if !outcome.oplog_entries.is_empty() {
                let oplog =
                    corpus_engine::enrichment::reconciliation::OplogWriter::new(atlas_dir.clone());
                for entry in &outcome.oplog_entries {
                    let _ = oplog.append(entry);
                }
            }
            let mut predicted = Clustering::new();
            for re in &outcome.entities {
                let cluster_id = re.canonical_id.as_str().to_string();
                // The surface forms recorded on the reconciled
                // entity ARE the surface forms (verbatim
                // canonical_names from the input atoms). Each maps
                // to the same canonical_id.
                for (sf, _provenance) in &re.surface_forms {
                    predicted.insert(sf.clone(), cluster_id.clone());
                }
                // Each atom's own aliases also belong under the
                // canonical cluster id.
                for atom_id in &re.source_atom_ids {
                    if let Some(orig) = entities.iter().find(|e| &e.id == atom_id) {
                        for alias in &orig.aliases {
                            predicted.insert(alias.clone(), cluster_id.clone());
                        }
                    }
                }
            }
            (
                predicted,
                hist,
                Some(RecordedPolicy {
                    name_similarity_threshold: policy.name_similarity_threshold,
                    cross_origin_required_signals: policy.cross_origin_required_signals,
                    judge_when_uncertain: policy.judge_when_uncertain,
                    judge_trials: policy.judge_trials,
                }),
            )
        }
    };

    // ── Score ───────────────────────────────────────────────────
    let report: EntityResolutionReport = score(&predicted, &gold);
    let surface_form_collapse_rate = if predicted.is_empty() {
        None
    } else {
        let n_predicted = predicted.len() as f64;
        let n_clusters = predicted
            .values()
            .collect::<std::collections::BTreeSet<_>>()
            .len() as f64;
        Some(1.0 - (n_clusters / n_predicted))
    };
    let delta = match parsed.policy {
        Policy::Tuned => compute_delta_from_floor(&parsed.bench_dir, report.b_cubed.f1),
        Policy::PreReconciliation => None,
    };

    // ── Print summary ──────────────────────────────────────────
    println!("─── Enron entity-resolution ───");
    println!("  corpus           : {}", parsed.corpus);
    println!("  split            : {}", parsed.split.as_str());
    println!("  policy           : {}", parsed.policy.as_str());
    println!("  entity atoms     : {}", entities.len());
    println!("  predicted        : {} surface forms", predicted.len());
    println!("  gold             : {} surface forms", gold.len());
    println!(
        "  B³ precision/recall/F1 : {:.3} / {:.3} / {:.3} (n_aligned={})",
        report.b_cubed.precision,
        report.b_cubed.recall,
        report.b_cubed.f1,
        report.b_cubed.n_aligned
    );
    println!(
        "  pairwise P/R/F1        : {:.3} / {:.3} / {:.3} (pairs={})",
        report.pairwise.precision,
        report.pairwise.recall,
        report.pairwise.f1,
        report.pairwise.n_aligned_pairs
    );
    if let Some(rate) = surface_form_collapse_rate {
        println!("  surface-form collapse  : {:.1}%", rate * 100.0);
    }
    if !signal_hist.is_empty() {
        println!("  merge signal histogram :");
        for (k, v) in &signal_hist {
            println!("    {k:<24} {v}");
        }
    }
    if let Some(d) = delta {
        println!("  delta vs pre-recon F1  : {:+.3}", d);
    }
    if !report.b_cubed.unmatched_predicted.is_empty() {
        println!(
            "  unmatched predicted    : {} (first 5: {})",
            report.b_cubed.unmatched_predicted.len(),
            sample_first_n(&report.b_cubed.unmatched_predicted, 5)
        );
    }
    if !report.b_cubed.unmatched_gold.is_empty() {
        println!(
            "  unmatched gold         : {} (first 5: {})",
            report.b_cubed.unmatched_gold.len(),
            sample_first_n(&report.b_cubed.unmatched_gold, 5)
        );
    }

    // ── Persist to JSON ────────────────────────────────────────
    // The B³ `unmatched_*` arrays hold every noise surface form (25k+
    // on a wide corpus), bloating the committed baseline to MBs of
    // un-reviewable diff. Cap them to a sample, preserving the true
    // counts as `*_total`, before serialising.
    let mut b_cubed_value =
        serde_json::to_value(&report.b_cubed).unwrap_or(serde_json::Value::Null);
    cap_unmatched_arrays(&mut b_cubed_value, UNMATCHED_SAMPLE_CAP);
    let outcome = EnronBenchOutcome {
        schema_version: 1,
        bench_id: "enron-entity-resolution".into(),
        policy_kind: parsed.policy.as_str().to_string(),
        split: parsed.split.as_str().to_string(),
        corpus: parsed.corpus.clone(),
        captured_ts_unix: now_secs(),
        policy: recorded_policy,
        b_cubed: b_cubed_value,
        pairwise: serde_json::to_value(&report.pairwise).unwrap_or(serde_json::Value::Null),
        signal_histogram: signal_hist,
        surface_form_collapse_rate,
        delta_from_pre_reconciliation_f1: delta,
        notes: format!(
            "Written by `svrn bench enron run`. Reads {}; ground truth at {}.",
            atoms_path.display(),
            gt_path.display()
        ),
    };

    let out_path = parsed.out.unwrap_or_else(|| {
        let default = match parsed.policy {
            Policy::PreReconciliation => "pre_reconciliation.json",
            Policy::Tuned => "latest.json",
        };
        parsed
            .bench_dir
            .join("baselines/enron-entity-resolution")
            .join(default)
    });
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let json =
        serde_json::to_string_pretty(&outcome).map_err(|e| format!("serialise outcome: {e}"))?;
    std::fs::write(&out_path, json).map_err(|e| format!("write {}: {e}", out_path.display()))?;
    println!("  → {}", out_path.display());

    Ok(0)
}

/// `svrn bench enron diagnose` — glass-box view of WHY the tuned
/// B³ is what it is, without re-deriving it by hand. For each gold
/// entity in the split it reports coverage (forms present verbatim in
/// the atoms) and how many predicted clusters those forms scatter
/// across (under-merge); then it flags every predicted cluster that
/// bridges more than one gold entity (over-merge — the precision
/// killer). Reads the same atoms + runs the same tuned reconciliation
/// as `run --policy tuned`, so its verdict matches the scored number.
async fn cmd_diagnose(args: &[String]) -> Result<i32, String> {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) if e == "__HELP__" => return Ok(0),
        Err(e) => return Err(e),
    };

    let atlas_dir = parsed.indexes_dir.join(&parsed.corpus).join("atlas");
    let atoms_path = atlas_dir.join("atoms.json");
    if !atoms_path.exists() {
        eprintln!("error: no atoms.json at {}", atoms_path.display());
        return Ok(2);
    }
    let atoms_file = corpus_engine::enrichment::atlas::read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("read atoms.json ({}): {e}", atoms_path.display()))?;
    let entities: Vec<Entity> = atoms_file
        .atoms
        .into_iter()
        .filter_map(|env| match env {
            AtomEnvelope::Entity(e) => Some(e),
            _ => None,
        })
        .collect();

    // Same tuned reconciliation `run --policy tuned` performs.
    let mut policy = ReconciliationPolicy::default();
    policy.judge_trials = parsed.judge_trials;
    if let Some(t) = parsed.name_similarity_threshold {
        policy.name_similarity_threshold = t;
    }
    let outcome = reconcile(entities.clone(), &policy);

    // form -> predicted cluster id (last write wins, matching cmd_run).
    let mut form_cluster: BTreeMap<String, String> = BTreeMap::new();
    for re in &outcome.entities {
        let cid = re.canonical_id.as_str().to_string();
        for (sf, _) in &re.surface_forms {
            form_cluster.insert(sf.clone(), cid.clone());
        }
        for atom_id in &re.source_atom_ids {
            if let Some(orig) = entities.iter().find(|e| &e.id == atom_id) {
                for alias in &orig.aliases {
                    form_cluster.insert(alias.clone(), cid.clone());
                }
            }
        }
    }

    let gt_path = parsed.bench_dir.join("ground_truth_entities.jsonl");
    let gt = BenchGroundTruth::load(&gt_path)
        .map_err(|e| format!("load ground truth ({}): {e}", gt_path.display()))?;
    let gold = gt.by_split(parsed.split);

    println!(
        "─── enron diagnose: {} / {} ───",
        parsed.corpus,
        parsed.split.as_str()
    );
    println!("  entity atoms     : {}", entities.len());
    println!("  reconciled into  : {} clusters", outcome.entities.len());
    println!();
    println!("  per-gold coverage + cluster spread:");
    let mut cluster_to_gold: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let (mut total_forms, mut total_aligned) = (0usize, 0usize);
    for g in &gold {
        if g.is_sealed() {
            continue;
        }
        let mut aligned = 0usize;
        let mut clusters: BTreeSet<String> = BTreeSet::new();
        for sf in &g.surface_forms {
            if let Some(cid) = form_cluster.get(sf) {
                aligned += 1;
                clusters.insert(cid.clone());
                cluster_to_gold
                    .entry(cid.clone())
                    .or_default()
                    .insert(g.canonical_id.clone());
            }
        }
        total_forms += g.surface_forms.len();
        total_aligned += aligned;
        let flag = if clusters.len() > 1 {
            "  ⚠ UNDER-MERGE"
        } else {
            ""
        };
        println!(
            "    {:<24} {}/{} forms aligned, {} cluster(s){}",
            g.canonical_id,
            aligned,
            g.surface_forms.len(),
            clusters.len(),
            flag
        );
    }
    println!();
    println!(
        "  coverage: {total_aligned}/{total_forms} gold forms present verbatim (extraction ceiling)"
    );

    let bridges: Vec<(&String, &BTreeSet<String>)> = cluster_to_gold
        .iter()
        .filter(|(_, gs)| gs.len() > 1)
        .collect();
    println!();
    if bridges.is_empty() {
        println!("  ✓ precision-safe: no predicted cluster bridges >1 gold entity");
    } else {
        println!(
            "  ✗ OVER-MERGE — {} cluster(s) bridge multiple gold entities:",
            bridges.len()
        );
        for (cid, gs) in bridges {
            let re = outcome
                .entities
                .iter()
                .find(|re| re.canonical_id.as_str() == cid.as_str());
            let size = re.map(|re| re.source_atom_ids.len()).unwrap_or(0);
            let sample: Vec<String> = re
                .map(|re| {
                    re.surface_forms
                        .iter()
                        .take(8)
                        .map(|(s, _)| s.clone())
                        .collect()
                })
                .unwrap_or_default();
            println!("    cluster {cid} ({size} atoms) holds gold {gs:?}");
            println!("       sample members: {sample:?}");
        }
    }
    Ok(0)
}

fn surface_forms_of(e: &Entity) -> Vec<String> {
    let mut out = Vec::with_capacity(1 + e.aliases.len());
    out.push(e.canonical_name.clone());
    out.extend(e.aliases.iter().cloned());
    out
}

fn compute_delta_from_floor(bench_dir: &Path, tuned_f1: f64) -> Option<f64> {
    let floor_path = bench_dir.join("baselines/enron-entity-resolution/pre_reconciliation.json");
    let bytes = std::fs::read(&floor_path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let floor_f1 = value
        .get("b_cubed")
        .and_then(|b| b.get("f1"))
        .and_then(|v| v.as_f64())?;
    Some(tuned_f1 - floor_f1)
}

fn default_private_holdout_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".sovereign/bench/enron/holdout.jsonl")
    } else {
        PathBuf::from(".sovereign/bench/enron/holdout.jsonl")
    }
}

fn git_head_short() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn sample_first_n(v: &[String], n: usize) -> String {
    v.iter().take(n).cloned().collect::<Vec<_>>().join(", ")
}

/// How many `unmatched_*` surface forms to keep verbatim in a persisted
/// baseline. The rest are summarised by a `*_total` count.
const UNMATCHED_SAMPLE_CAP: usize = 50;

/// Truncate the `unmatched_predicted` / `unmatched_gold` arrays in a
/// serialised `B3Outcome` to a reviewable sample, recording the true
/// length as a sibling `*_total` field first. Keeps committed baselines
/// small and diff-friendly without losing the counts that matter (the
/// arrays themselves are full of extractor noise — 25k+ forms on a wide
/// corpus). No-op for arrays already within `cap`.
fn cap_unmatched_arrays(value: &mut serde_json::Value, cap: usize) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    for key in ["unmatched_predicted", "unmatched_gold"] {
        let total = match obj.get(key).and_then(|v| v.as_array()) {
            Some(arr) if arr.len() > cap => arr.len(),
            _ => continue,
        };
        let sample: Vec<serde_json::Value> = obj
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().take(cap).cloned().collect())
            .unwrap_or_default();
        obj.insert(format!("{key}_total"), serde_json::json!(total));
        obj.insert(key.to_string(), serde_json::Value::Array(sample));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_minimum() {
        let args: Vec<String> = ["--corpus", "enron-sample-onemailbox", "--split", "train"]
            .into_iter()
            .map(String::from)
            .collect();
        let a = parse_args(&args).unwrap();
        assert_eq!(a.corpus, "enron-sample-onemailbox");
        assert_eq!(a.split, Split::Train);
        assert_eq!(a.policy, Policy::Tuned);
    }

    #[test]
    fn parse_args_rejects_unknown_split() {
        let args: Vec<String> = ["--corpus", "c", "--split", "weird"]
            .into_iter()
            .map(String::from)
            .collect();
        let err = parse_args(&args).unwrap_err();
        assert!(err.contains("unknown split"), "{err}");
    }

    #[test]
    fn parse_args_policy_aliases() {
        for alias in ["floor", "pre-reconciliation", "pre_reconciliation"] {
            let args: Vec<String> = ["--corpus", "c", "--split", "train", "--policy", alias]
                .into_iter()
                .map(String::from)
                .collect();
            let a = parse_args(&args).unwrap();
            assert_eq!(a.policy, Policy::PreReconciliation, "{alias}");
        }
    }

    #[test]
    fn holdout_without_unseal_is_an_error() {
        // Driver test: build args invoking holdout without --unseal-holdout.
        // We don't exercise the full async run here — the CLI's gate
        // check fires before any IO. Just verify the parser accepts
        // the args and the policy/split bits are right; the gate is
        // verified by the actual run_cmd_run integration.
        let args: Vec<String> = ["--corpus", "c", "--split", "holdout"]
            .into_iter()
            .map(String::from)
            .collect();
        let a = parse_args(&args).unwrap();
        assert_eq!(a.split, Split::Holdout);
        assert!(!a.unseal_holdout);
    }
}
