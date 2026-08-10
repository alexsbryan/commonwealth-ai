// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn router fit` — measure the router's twelve threshold constants
//! against a calibration bank instead of against a source comment.
//!
//! ## Why
//!
//! Six embedding gates ship twelve hand-picked constants, every one
//! calibrated by hand against Qwen3-Embedding-0.6B and justified in
//! prose. Two of those decisions turned on thousandths — an archive
//! negative held out "by only 0.002 of margin", a tool gate hijacked
//! by "0.011 of cosine noise" — and both were found days late, by
//! hand, from a bench regression. Nothing tells you which of the
//! remaining constants is one embedding-model change away from the
//! same fate.
//!
//! ## How
//!
//! Scoring a query costs an embedding; gating it costs two
//! comparisons. So ONE embedding pass over the bank makes the whole
//! threshold space searchable by arithmetic —
//! [`sovereign_core::router_calibration`] sweeps it exhaustively and
//! reports the shipped gate beside the best reachable one.
//!
//! It embeds through [`EmbedOnlyProvider`] against the model
//! `models.toml` prescribes, driving the same `from_toml_str_cached`
//! path the runtime uses, so the vectors sit in production's space.
//! Exemplar embeddings hit the boot cache; only the bank's own queries
//! are new work. The cache is never flushed — a measurement must not
//! mutate the artifact it measures.
//!
//! ## It does not edit anything
//!
//! The report names the constant and the file to change and stops
//! there. A calibrator that silently rewrote the constants it measures
//! would reproduce the opacity it exists to remove.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sovereign_core::archive_classifier::ConversationArchiveClassifier;
use sovereign_core::current_info_classifier::CurrentInfoClassifier;
use sovereign_core::effort_classifier::EffortClassifier;
use sovereign_core::models_manifest::ModelsManifest;
use sovereign_core::router_axis::{normalize, AxisGate};
use sovereign_core::router_calibration::{
    attribute, fit, parse_bank, verdict_changes, CalibrationCase, CaseAttribution, CaseVerdict,
    FitReport, GateOutcome, Objective, ScoredCase,
};
use sovereign_core::router_drift::{
    bank_digest, compare, AxisChange, AxisDelta, DriftReport, FitSnapshot,
};
use sovereign_core::router_embed::{intent_label, EmbedRouter};
use sovereign_core::router_embed_cache::BootEmbedCache;
use sovereign_core::scope_classifier::PersonalScopeClassifier;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::embedded::EmbedOnlyProvider;

const DEFAULT_BANK_DIR: &str = "sovereign/bench/routing/calibration";

const HELP: &str = "\
sovereign router fit — calibrate the router's embedding thresholds

USAGE:
  sovereign router fit [OPTIONS]

OPTIONS:
  --bank <path>              Calibration bank file, or a directory of them.
                             Default: sovereign/bench/routing/calibration/
  --axis <name>              Only fit this axis (intent, locator, scope,
                             archive, current_info, effort). Repeatable.
  --objective <name>         safe-recall (default) | accuracy | max-coverage
  --max-false-positives <n>  Ceiling for safe-recall. Default 0.
  --min-precision <f>        Floor for max-coverage. Default 1.0.
  --embed-model <path>       Override the prescribed embed GGUF.
  --format <fmt>             human (default) | json
  --save-baseline            Record THIS run under
                             sovereign/bench/routing/baselines/<bank>-fit/
                             so a later run can be diffed against it.
  --baseline-dir <path>      Override that directory.
  --no-drift                 Skip the comparison against the baseline.
  --explain                  Name the cases behind the counts: every error
                             under the shipped gate, and every case whose
                             verdict the best gate would flip.

The default objective encodes the asymmetry every axis documents: a false
positive hard-commits a turn down a narrowed path, a false negative merely
falls through to the cascade. `--objective accuracy` scores the way the
prior art does (both errors weighted equally) for comparison.

DRIFT. When a baseline exists, every run diffs the shipped gate's headroom
against it — the check that catches an encoder or exemplar change closing in
on a threshold BEFORE any bench regression shows it. Deltas are only called a
regression when the encoder AND the bank are unchanged; otherwise they are
printed as evidence and left to you.

EXPLAIN. `--explain` answers the question the counts cannot: the report says
two false positives — WHICH two? It lists them by case id with the scores and
the per-case cushion, plus what moving to the fitted gate would actually flip.
`--format json` always carries the full per-case attribution for both gates.

EXIT: 0 clean · 2 usage · 3 a gate is movable · 4 drift regression.

No constant is ever written. Only the measurement is, and only on
--save-baseline: a calibrator that edits what it measures is the opaque loop
this replaces.";

pub async fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("fit") => cmd_fit(&args[1..]).await,
        Some("--help") | Some("-h") | Some("help") | None => {
            println!("{HELP}");
            0
        }
        Some(other) => {
            eprintln!("router: unknown subcommand '{other}'\n");
            println!("{HELP}");
            2
        }
    }
}

struct Opts {
    bank: Option<PathBuf>,
    axes: Vec<String>,
    objective: Objective,
    embed_model: Option<PathBuf>,
    json: bool,
    save_baseline: bool,
    baseline_dir: Option<PathBuf>,
    no_drift: bool,
    explain: bool,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut bank = None;
    let mut axes: Vec<String> = Vec::new();
    let mut embed_model = None;
    let mut json = false;
    let mut objective_name = "safe-recall".to_string();
    let mut max_fp: usize = 0;
    let mut min_precision: f64 = 1.0;
    let mut save_baseline = false;
    let mut baseline_dir = None;
    let mut no_drift = false;
    let mut explain = false;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bank" => bank = Some(PathBuf::from(next(&mut it, "--bank")?)),
            "--axis" => axes.push(next(&mut it, "--axis")?),
            "--save-baseline" => save_baseline = true,
            "--no-drift" => no_drift = true,
            "--explain" => explain = true,
            "--baseline-dir" => {
                baseline_dir = Some(PathBuf::from(next(&mut it, "--baseline-dir")?))
            }
            "--objective" => objective_name = next(&mut it, "--objective")?,
            "--max-false-positives" => {
                max_fp = next(&mut it, "--max-false-positives")?
                    .parse()
                    .map_err(|e| format!("--max-false-positives: {e}"))?
            }
            "--min-precision" => {
                min_precision = next(&mut it, "--min-precision")?
                    .parse()
                    .map_err(|e| format!("--min-precision: {e}"))?
            }
            "--embed-model" => embed_model = Some(PathBuf::from(next(&mut it, "--embed-model")?)),
            "--format" => json = next(&mut it, "--format")? == "json",
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }

    let objective = match objective_name.as_str() {
        "safe-recall" | "safe" => Objective::SafeRecall {
            max_false_positives: max_fp,
        },
        "accuracy" => Objective::Accuracy,
        "max-coverage" | "coverage" => Objective::MaxCoverage { min_precision },
        other => {
            return Err(format!(
                "unknown objective '{other}' (safe-recall | accuracy | max-coverage)"
            ))
        }
    };

    // A run restricted to one axis measures one axis. Saving it as
    // THE baseline would silently retire the other five — the next
    // full run would then report them as newly appeared and have
    // nothing to diff them against.
    if save_baseline && !axes.is_empty() {
        return Err(
            "--save-baseline needs a full run; drop --axis (a partial baseline \
             would retire the axes it omits)"
                .into(),
        );
    }

    Ok(Opts {
        bank,
        axes,
        objective,
        embed_model,
        json,
        save_baseline,
        baseline_dir,
        no_drift,
        explain,
    })
}

fn next(it: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<String, String> {
    it.next()
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value"))
}

/// Collect `.toml` calibration banks from a file or directory path.
fn collect_banks(path: &Path) -> Result<Vec<PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    if !path.is_dir() {
        return Err(format!("no calibration bank at {}", path.display()));
    }
    let mut out: Vec<PathBuf> = std::fs::read_dir(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    out.sort();
    if out.is_empty() {
        return Err(format!("no .toml banks under {}", path.display()));
    }
    Ok(out)
}

async fn cmd_fit(args: &[String]) -> i32 {
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("router fit: {e}\n");
            println!("{HELP}");
            return 2;
        }
    };

    let Some(root) = crate::router_cache_cmd::repo_root() else {
        eprintln!("router fit: not inside a sovereign checkout (no sovereign/models.toml found)");
        return 2;
    };
    let tree = match crate::router_cache_cmd::read_tree(&root) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("router fit: reading router/models files: {e}");
            return 2;
        }
    };

    // ── Load the bank(s) ─────────────────────────────────────────
    let bank_path = opts.bank.unwrap_or_else(|| root.join(DEFAULT_BANK_DIR));
    let files = match collect_banks(&bank_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("router fit: {e}");
            return 2;
        }
    };
    let mut cases: Vec<CalibrationCase> = Vec::new();
    // Kept verbatim for the drift digest: it is the BANK CONTENT that
    // decides whether two runs measured the same thing, not the paths.
    let mut raw_banks: Vec<String> = Vec::new();
    for f in &files {
        let raw = match std::fs::read_to_string(f) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("router fit: read {}: {e}", f.display());
                return 2;
            }
        };
        match parse_bank(&raw) {
            Ok(b) => cases.extend(b.case),
            Err(e) => {
                eprintln!("router fit: {}: {e}", f.display());
                return 2;
            }
        }
        raw_banks.push(raw);
    }
    if !opts.axes.is_empty() {
        cases.retain(|c| opts.axes.contains(&c.axis));
        if cases.is_empty() {
            eprintln!("router fit: no cases match --axis {:?}", opts.axes);
            return 2;
        }
    }

    // ── Resolve + load the prescribed embed model ────────────────
    let manifest = match ModelsManifest::from_toml_str(&tree.models) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("router fit: parse models.toml: {e}");
            return 2;
        }
    };
    let Some(slot) = manifest.prescribed_embed_slot() else {
        eprintln!("router fit: models.toml declares no `default`-profile embed slot");
        return 2;
    };
    let embed_family = manifest
        .embed_family_for_file(&slot.file)
        .unwrap_or(sovereign_core::model_family::ModelFamily::Unknown);
    let model_path = opts.embed_model.unwrap_or_else(|| {
        sovereign_contracts::rebrand::svrnmesh_root()
            .join("models")
            .join(&slot.file)
    });
    // What was ACTUALLY loaded, which `--embed-model` can make
    // different from what models.toml prescribes. Every number below
    // is a property of this file, so this — not `slot.file` — is what
    // the report and the drift baseline must carry: two runs against
    // two different GGUFs recorded under one prescribed name would be
    // declared comparable, and their cosines are not.
    //
    // The file NAME rather than the path: an absolute path would make
    // every cross-machine comparison unattributable, and by this
    // repo's convention one filename is one model.
    let measured_model = model_path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| slot.file.clone());
    let is_override = measured_model != slot.file;

    if !model_path.is_file() {
        eprintln!(
            "router fit: prescribed embed model not found:\n  {}\n\
             Download it from {} (file {}), or pass --embed-model <path>.",
            model_path.display(),
            slot.hf_url,
            slot.file,
        );
        return 2;
    }
    if !opts.json {
        eprintln!(
            "router fit: {} cases from {} bank(s), embedding with {} …",
            cases.len(),
            files.len(),
            measured_model
        );
        if is_override {
            eprintln!(
                "router fit: NOTE --embed-model overrides the prescribed {}. \
                 These numbers describe {}, not production.",
                slot.file, measured_model
            );
        }
    }
    let provider: Arc<dyn InferenceProvider> =
        match EmbedOnlyProvider::load(&model_path, embed_family) {
            Ok(p) => Arc::new(p),
            Err(e) => {
                eprintln!("router fit: load embed model: {e}");
                return 1;
            }
        };

    // ── Build every classifier off the same cache ────────────────
    //
    // NOT flushed: a calibration run must not mutate the artifact it
    // is measuring. Exemplar embeddings hit; only the bank's own
    // queries are new work.
    let mut cache = BootEmbedCache::open(&*provider).await;
    let built = build_all(&tree, Arc::clone(&provider), &mut cache).await;
    let (router, scope, archive, current_info, effort) = match built {
        Ok(v) => v,
        Err(e) => {
            eprintln!("router fit: building classifiers: {e}");
            return 1;
        }
    };

    // ── Score every case against its axis ────────────────────────
    let mut by_axis: BTreeMap<String, Vec<ScoredCase>> = BTreeMap::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    for c in &cases {
        // Each axis is calibrated in a specific vector space, and getting
        // this wrong silently produces numbers from the wrong one — the
        // single easiest way to make this whole command lie. So the mapping
        // is NOT re-derived here: `axis_space` is the one decider the
        // classifiers and the cache freshness gate also read
        // (ARCH_PRINCIPLES §10.6). An axis it doesn't know is skipped and
        // reported, never embedded in a guessed space (§18.3).
        let Some(space) = sovereign_core::router_instruction::axis_space(&c.axis) else {
            eprintln!(
                "router fit: case `{}` declares axis `{}`, which has no registered embedding \
                 space — skipping rather than guessing one. Add it to \
                 `router_instruction::axis_space`.",
                c.id, c.axis
            );
            skipped.push((c.id.clone(), c.axis.clone()));
            continue;
        };
        let embedded = cache.embed_in_space(space, &*provider, &c.query).await;
        let mut v = match embedded {
            Ok(v) => v,
            Err(e) => {
                eprintln!("router fit: embedding case `{}`: {e}", c.id);
                return 1;
            }
        };
        normalize(&mut v);

        let scored = match c.axis.as_str() {
            "intent" => router.score_intent_from_embedding(&v).map(|s| ScoredCase {
                id: c.id.clone(),
                score: s.score,
                expect: c.expected_label().map(String::from),
                predicted: Some(intent_label(&s.top_intent).to_string()),
                nearest: Some(s.nearest_exemplar),
                rival: s.rival_exemplar,
            }),
            "locator" => router.score_locator_from_embedding(&v).map(|s| ScoredCase {
                id: c.id.clone(),
                score: s.score,
                expect: c.expected_label().map(String::from),
                predicted: Some(s.locator),
                nearest: Some(s.nearest_exemplar),
                rival: s.rival_exemplar,
            }),
            "scope" => scope.as_ref().and_then(|k| {
                k.score_from_embedding(&v).map(|s| ScoredCase {
                    id: c.id.clone(),
                    score: s,
                    expect: c.expected_label().map(String::from),
                    predicted: Some("personal".to_string()),
                    // Centroid axis: the positive class is a mean over
                    // ~20 rows, so no single exemplar is responsible.
                    nearest: None,
                    rival: None,
                })
            }),
            "archive" => archive.as_ref().and_then(|k| {
                k.score_from_embedding(&v).map(|s| ScoredCase {
                    id: c.id.clone(),
                    score: s,
                    expect: c.expected_label().map(String::from),
                    predicted: Some("archive".to_string()),
                    nearest: None,
                    rival: None,
                })
            }),
            "current_info" => current_info.as_ref().and_then(|k| {
                k.score_from_embedding(&v).map(|s| ScoredCase {
                    id: c.id.clone(),
                    score: s,
                    expect: c.expected_label().map(String::from),
                    predicted: Some("current".to_string()),
                    nearest: None,
                    rival: None,
                })
            }),
            "effort" => effort.as_ref().and_then(|k| {
                k.score_from_embedding(&v).map(|s| ScoredCase {
                    id: c.id.clone(),
                    score: s,
                    expect: c.expected_label().map(String::from),
                    predicted: Some("high".to_string()),
                    nearest: None,
                    rival: None,
                })
            }),
            _ => None,
        };
        match scored {
            Some(s) => by_axis.entry(c.axis.clone()).or_default().push(s),
            None => skipped.push((c.id.clone(), c.axis.clone())),
        }
    }

    // ── Fit each axis ────────────────────────────────────────────
    let shipped: BTreeMap<&str, (AxisGate, &str, &str)> = [
        (
            "intent",
            (
                router.intent_gate(),
                "DEFAULT_MIN_TOP_SIM / DEFAULT_MIN_MARGIN",
                "sovereign-core/src/router_embed.rs",
            ),
        ),
        (
            "locator",
            (
                router.locator_gate(),
                "DEFAULT_LOCATOR_MIN_SIM / DEFAULT_LOCATOR_MIN_MARGIN",
                "sovereign-core/src/router_embed.rs",
            ),
        ),
        (
            "scope",
            (
                scope
                    .as_ref()
                    .map(|k| k.gate())
                    .unwrap_or(AxisGate::new(0.0, 0.0)),
                "DEFAULT_MIN_PERSONAL_SIM / DEFAULT_MIN_MARGIN",
                "sovereign-core/src/scope_classifier.rs",
            ),
        ),
        (
            "archive",
            (
                archive
                    .as_ref()
                    .map(|k| k.gate())
                    .unwrap_or(AxisGate::new(0.0, 0.0)),
                "DEFAULT_MIN_ARCHIVE_SIM / DEFAULT_MIN_MARGIN",
                "sovereign-core/src/archive_classifier.rs",
            ),
        ),
        (
            "current_info",
            (
                current_info
                    .as_ref()
                    .map(|k| k.gate())
                    .unwrap_or(AxisGate::new(0.0, 0.0)),
                "DEFAULT_MIN_CURRENT_SIM / DEFAULT_MIN_MARGIN",
                "sovereign-core/src/current_info_classifier.rs",
            ),
        ),
        (
            "effort",
            (
                effort
                    .as_ref()
                    .map(|k| k.gate())
                    .unwrap_or(AxisGate::new(0.0, 0.0)),
                "DEFAULT_MIN_HIGH_SIM / DEFAULT_MIN_MARGIN",
                "sovereign-core/src/effort_classifier.rs",
            ),
        ),
    ]
    .into_iter()
    .collect();

    let mut reports: BTreeMap<String, FitReport> = BTreeMap::new();
    for (axis, axis_cases) in &by_axis {
        let Some((gate, _, _)) = shipped.get(axis.as_str()) else {
            continue;
        };
        // The intent axis is multi-class and its value is COVERAGE —
        // owning decisions the LLM would otherwise be woken for. The
        // binary axes are asymmetric one-shot commits, so they take
        // the caller's objective (safe-recall by default).
        let obj = if axis.as_str() == "intent"
            && matches!(opts.objective, Objective::SafeRecall { .. })
        {
            Objective::MaxCoverage { min_precision: 1.0 }
        } else {
            opts.objective
        };
        if let Some(r) = fit(axis_cases, *gate, obj) {
            reports.insert(axis.clone(), r);
        }
    }

    // ── Drift against the last recorded run ──────────────────────
    // Repo-relative: this snapshot gets committed, and an absolute
    // path would bake one developer's home directory into a shared
    // artifact and re-diff on every other machine. Comparability keys
    // on `bank_digest`, so these paths are for the human reading the
    // JSON — which is exactly why they should read the same everywhere.
    let bank_names: Vec<String> = files
        .iter()
        .map(|f| f.strip_prefix(&root).unwrap_or(f).display().to_string())
        .collect();

    let snapshot = FitSnapshot {
        embed_model: measured_model.clone(),
        banks: bank_names.clone(),
        bank_digest: bank_digest(&raw_banks),
        axes: reports.clone(),
    };
    let dir = opts
        .baseline_dir
        .clone()
        .unwrap_or_else(|| baseline_dir_for_bank(&root, &bank_path));

    let drift = if opts.no_drift {
        None
    } else {
        match crate::bench_cmd::baselines::read_latest_at::<FitSnapshot>(&dir) {
            Ok(Some(mut base)) => {
                // An `--axis` run measured a subset. Diffing it whole
                // would report every axis it did not ask for as
                // vanished, which is a fact about the flag, not the
                // router.
                if !opts.axes.is_empty() {
                    base.axes.retain(|k, _| opts.axes.contains(k));
                }
                Some(compare(&base, &snapshot))
            }
            Ok(None) => None,
            Err(e) => {
                eprintln!("router fit: baseline unreadable, skipping drift ({e})");
                None
            }
        }
    };

    // Written AFTER the comparison, so `--save-baseline` on a drifting
    // run still shows you the drift it is about to overwrite.
    let saved = if opts.save_baseline {
        match crate::bench_cmd::baselines::write_dated_and_update_latest_at(&dir, &snapshot) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("router fit: write baseline into {}: {e}", dir.display());
                return 1;
            }
        }
    } else {
        None
    };

    if opts.json {
        let payload = serde_json::json!({
            "embed_model": measured_model,
            "prescribed_embed_model": slot.file,
            "banks": bank_names,
            "bank_digest": snapshot.bank_digest,
            "cases_total": cases.len(),
            "skipped": skipped.iter().map(|(id, ax)| serde_json::json!({"id": id, "axis": ax})).collect::<Vec<_>>(),
            "axes": &reports,
            "attribution": attribution_json(&reports, &by_axis),
            "drift": drift.as_ref().map(drift_json),
            "baseline_written": saved.as_ref().map(|p| p.display().to_string()),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else {
        print_human(
            &reports,
            &shipped,
            &skipped,
            &measured_model,
            &by_axis,
            opts.explain,
        );
        match &drift {
            Some(d) => print_drift(d, &dir),
            None if !opts.no_drift => println!(
                "No baseline under {} — nothing to diff against yet.\n\
                 `--save-baseline` records this run as the first one.\n",
                dir.display()
            ),
            None => {}
        }
        if let Some(p) = &saved {
            println!("baseline written: {}", p.display());
        }
    }

    // 4 beats 3: "the ground moved under a constant you did not touch"
    // is a different, more urgent fact than "this constant could be
    // tuned", and CI should be able to fail on one without the other.
    if drift.as_ref().is_some_and(DriftReport::is_regression) {
        4
    } else if reports.values().any(|r| r.would_change()) {
        // Exit 3 when any axis could be improved — a CI-friendly
        // signal, matching `router-cache check`'s "stale" convention.
        3
    } else {
        0
    }
}

/// `sovereign/bench/routing/baselines/<bank>-fit/`.
///
/// `<bank>` is the bank file's stem, or the directory's name when the
/// whole calibration directory was swept — so a default run lands in
/// `calibration-fit/`, alongside the `<bench>-routing` / `<bench>-synth`
/// directories the same tree already uses.
fn baseline_dir_for_bank(root: &Path, bank_path: &Path) -> PathBuf {
    let stem = if bank_path.is_file() {
        bank_path.file_stem()
    } else {
        bank_path.file_name()
    }
    .and_then(|s| s.to_str())
    .unwrap_or("calibration");
    crate::bench_cmd::baselines::baseline_dir(
        &root.join("sovereign").join("bench"),
        "routing",
        &format!("{stem}-fit"),
    )
}

type Built = (
    EmbedRouter,
    Option<PersonalScopeClassifier>,
    Option<ConversationArchiveClassifier>,
    Option<CurrentInfoClassifier>,
    Option<EffortClassifier>,
);

/// Build every classifier through the SAME cached path the runtime
/// uses. A per-classifier failure degrades to `None` (its cases are
/// reported as skipped) rather than aborting the whole run — one
/// unparseable side bank should not cost you the other five axes.
async fn build_all(
    tree: &crate::router_cache_cmd::TreeFiles,
    provider: Arc<dyn InferenceProvider>,
    cache: &mut BootEmbedCache,
) -> Result<Built, String> {
    // `Some(&mut *cache)` reborrows for each call; `Some(cache)` would
    // move the mutable reference into the first one.
    let router =
        EmbedRouter::from_toml_str_cached(&tree.router, Arc::clone(&provider), Some(&mut *cache))
            .await
            .map_err(|e| format!("exemplars.toml: {e}"))?;
    let scope = PersonalScopeClassifier::from_toml_str_cached(
        &tree.scope,
        Arc::clone(&provider),
        Some(&mut *cache),
    )
    .await
    .ok();
    let archive = ConversationArchiveClassifier::from_toml_str_cached(
        &tree.archive,
        Arc::clone(&provider),
        Some(&mut *cache),
    )
    .await
    .ok();
    let current_info = CurrentInfoClassifier::from_toml_str_cached(
        &tree.current_info,
        Arc::clone(&provider),
        Some(&mut *cache),
    )
    .await
    .ok();
    let effort = EffortClassifier::from_toml_str_cached(
        &tree.effort,
        Arc::clone(&provider),
        Some(&mut *cache),
    )
    .await
    .ok();
    Ok((router, scope, archive, current_info, effort))
}

fn fmt_cushion(c: Option<f32>) -> String {
    match c {
        Some(v) => format!("{v:+.3}"),
        None => "  n/a".into(),
    }
}

fn print_outcome(label: &str, o: &GateOutcome) {
    println!(
        "  {label:<14} sim >= {:.3}   margin >= {:.3}",
        o.min_sim, o.min_margin
    );
    println!(
        "                 fired {} correct · {} mislabelled · {} false-positive",
        o.fired_correct, o.mislabelled, o.false_positive
    );
    println!(
        "                 abstained {} correct · {} missed",
        o.abstained_correct, o.missed
    );
    println!(
        "                 coverage {:.1}%  precision {:.1}%  accuracy {:.1}%",
        o.coverage() * 100.0,
        o.precision() * 100.0,
        o.accuracy() * 100.0
    );
    println!(
        "                 headroom weakest-accept {} · nearest-miss {} · separation {:.3}",
        fmt_cushion(o.weakest_accept),
        fmt_cushion(o.nearest_miss),
        o.separation()
    );
}

/// Full per-case attribution for both gates, per axis.
///
/// Unconditional in JSON — unlike the human report, which filters to
/// errors to stay readable. A machine consumer wants every row, and
/// this is the surface a future regression check would key on ("the FP
/// set changed", not just "the FP count changed").
fn attribution_json(
    reports: &BTreeMap<String, FitReport>,
    by_axis: &BTreeMap<String, Vec<ScoredCase>>,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (axis, r) in reports {
        let Some(cases) = by_axis.get(axis) else {
            continue;
        };
        let shipped_rows = attribute(cases, r.current.gate());
        let best_rows = r.best.as_ref().map(|b| attribute(cases, b.gate()));
        let flips = r.best.as_ref().map(|b| {
            verdict_changes(cases, r.current.gate(), b.gate())
                .into_iter()
                .map(|(id, before, after)| {
                    serde_json::json!({"id": id, "before": before, "after": after})
                })
                .collect::<Vec<_>>()
        });
        out.insert(
            axis.clone(),
            serde_json::json!({
                "shipped": shipped_rows,
                "best": best_rows,
                "flips": flips,
            }),
        );
    }
    serde_json::Value::Object(out)
}

/// One attribution row, aligned so a column of them reads as a table.
///
/// `expect`/`predicted` are spelled out rather than abbreviated: on the
/// multi-class intent axis "fired the wrong label" and "fired when it
/// should have abstained" are different bugs, and the row has to say
/// which without the reader consulting the bank.
fn print_case_row(r: &CaseAttribution) {
    let want = r.expect.as_deref().unwrap_or("abstain");
    let would = r.predicted.as_deref().unwrap_or("-");
    println!(
        "    {:<15} {:<34} sim {:.3}  margin {:+.3}  cushion {:+.3}   want {} · would fire {}",
        r.verdict.label(),
        r.id,
        r.sim_positive,
        r.margin,
        r.cushion,
        want,
        would,
    );
    print_case_exemplars(r);
}

/// The k=1 axes can say WHICH exemplars produced the two similarities;
/// the centroid axes cannot, and print nothing here.
///
/// This is the line that turns "margin -0.133" from a number into a
/// fix. `won by` names the row that outscored the positive class — on
/// a missed case with a negative margin, that row is the defect.
fn print_case_exemplars(r: &CaseAttribution) {
    let (nearest, rival) = (r.nearest.as_deref(), r.rival.as_deref());
    if nearest.is_none() && rival.is_none() {
        return;
    }
    // Which side actually won tells the reader which row to look at.
    let verb = if r.margin < 0.0 { "LOST to" } else { "beat" };
    println!(
        "                    nearest \"{}\"",
        clip(nearest.unwrap_or("<none>"), 62)
    );
    println!(
        "                    {:<7} \"{}\"",
        verb,
        clip(rival.unwrap_or("<none>"), 62)
    );
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n.saturating_sub(1)).collect();
    format!("{head}…")
}

/// The per-case half of the report: which cases are behind the counts.
///
/// Prints ERRORS only, expensive first (false positive → mislabelled →
/// missed), then what moving to the fitted gate would flip. Correct
/// cases are omitted deliberately — `weakest_accept` already summarises
/// how close the healthy ones sit to the boundary, and a 74-row dump
/// buries the two rows a human is looking for. `--format json` carries
/// every case for both gates.
fn print_attribution(r: &FitReport, cases: &[ScoredCase]) {
    let rows = attribute(cases, r.current.gate());
    let mut errors: Vec<&CaseAttribution> = rows.iter().filter(|x| x.verdict.is_error()).collect();
    // Expensive errors first; within a bucket, closest to the boundary
    // first — that is the one a threshold move would reach soonest.
    errors.sort_by(|a, b| {
        let rank = |v: CaseVerdict| match v {
            CaseVerdict::FalsePositive => 0,
            CaseVerdict::Mislabelled => 1,
            _ => 2,
        };
        rank(a.verdict).cmp(&rank(b.verdict)).then(
            b.cushion
                .partial_cmp(&a.cushion)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });

    if errors.is_empty() {
        println!("  every case decided correctly under the shipped gate.");
    } else {
        println!("  the {} error(s) behind those counts:", errors.len());
        for e in errors {
            print_case_row(e);
        }
    }

    if let Some(b) = &r.best {
        let flips = verdict_changes(cases, r.current.gate(), b.gate());
        if !flips.is_empty() {
            println!(
                "  moving to the fitted gate would flip {} case(s):",
                flips.len()
            );
            for (id, before, after) in flips {
                println!("    {:<34} {} → {}", id, before.label(), after.label());
            }
        }
    }
}

fn print_human(
    reports: &BTreeMap<String, FitReport>,
    shipped: &BTreeMap<&str, (AxisGate, &str, &str)>,
    skipped: &[(String, String)],
    model_file: &str,
    by_axis: &BTreeMap<String, Vec<ScoredCase>>,
    explain: bool,
) {
    println!("\nrouter fit — thresholds measured against {model_file}\n");
    for (axis, r) in reports {
        println!("── {axis} ──────────────────────────────────────────");
        println!(
            "  {} cases ({} must fire · {} must abstain) · {} gates evaluated",
            r.cases_scored, r.positives, r.negatives, r.gates_evaluated
        );
        if r.underpowered() {
            println!(
                "  ! UNDERPOWERED — fewer than {} cases in one class. Read the\n    \
                 shipped numbers, not the fitted gate: an optimum found on this\n    \
                 few cases describes the sample, not the axis. Add cases first.",
                sovereign_core::router_calibration::MIN_CASES_PER_CLASS
            );
        }
        print_outcome("shipped", &r.current);
        match &r.best {
            Some(b) => {
                print_outcome("best", b);
                if r.would_change() {
                    let d_owned = b.fired_correct as i64 - r.current.fired_correct as i64;
                    let d_wrong = b.wrong() as i64 - r.current.wrong() as i64;
                    println!(
                        "  → MOVABLE: {d_owned:+} correct fires, {d_wrong:+} errors.\n\
                             edit {} in {}",
                        shipped.get(axis.as_str()).map(|s| s.1).unwrap_or("?"),
                        shipped.get(axis.as_str()).map(|s| s.2).unwrap_or("?"),
                    );
                } else {
                    println!(
                        "  → optimal on this bank. Headroom is the number to watch: {:.3}",
                        r.current.separation()
                    );
                }
            }
            None => println!("  best           <no gate satisfies the objective on this bank>"),
        }
        if explain {
            match by_axis.get(axis) {
                Some(cases) => print_attribution(r, cases),
                // Unreachable in practice — a report only exists for an
                // axis that scored cases — but silence here would read
                // as "no errors", which is the one thing it must not say.
                None => println!("  (no scored cases retained for this axis — cannot attribute)"),
            }
        }
        println!();
    }
    if !skipped.is_empty() {
        println!(
            "skipped {} case(s) whose axis was unavailable or unscoreable:",
            skipped.len()
        );
        for (id, ax) in skipped {
            println!("  {id} ({ax})");
        }
        println!();
    }
    println!(
        "No constant was written. A calibration that edits the constants it \n\
         measures is the opaque loop this replaces."
    );
    println!();
}

/// The before/after block.
///
/// Deltas print whether or not the run is attributable — they are the
/// operator's evidence and withholding them would be the opposite of
/// the point — but the header says plainly which of the two this is,
/// and only an attributable run is allowed to say "REGRESSED".
fn print_drift(d: &DriftReport, dir: &Path) {
    let when = match crate::bench_cmd::baselines::baseline_age(dir) {
        Some((date, days)) => format!("{date} ({days}d ago)"),
        None => "an undated baseline".to_string(),
    };
    println!("── drift vs {when} ──────────────────────");
    println!("  baseline  {}", dir.display());
    if d.same_model() {
        println!("  encoder   {} (unchanged)", d.current_model);
    } else {
        println!(
            "  encoder   {} → {}   CHANGED",
            d.baseline_model, d.current_model
        );
    }
    if d.same_bank() {
        println!("  bank      {} (unchanged)", d.current_digest);
    } else {
        println!(
            "  bank      {} → {}   CHANGED",
            d.baseline_digest, d.current_digest
        );
    }

    if !d.attributable() {
        // Name the reason that actually applies. Listing both every
        // time would teach the reader to skip the line, which is how a
        // caveat stops being read at all.
        let why = match (d.same_model(), d.same_bank()) {
            (false, false) => {
                "the encoder AND the bank both changed — two encoders' cosines are\n  \
                 coordinates in different spaces, and a different bank asks different\n  \
                 questions"
            }
            (false, true) => {
                "the encoder changed — cosines from two models are coordinates in\n  \
                 different spaces, so subtracting them yields a number, not a\n  \
                 measurement"
            }
            _ => {
                "the bank changed — adding or editing cases moves separation\n  \
                 legitimately, which is better measurement rather than drift"
            }
        };
        println!(
            "\n  ! NOT ATTRIBUTABLE: {why}.\n  \
               What follows are real differences between two runs, but not between\n  \
               two measurements of the same thing, so nothing below is called a\n  \
               regression. Once the change is deliberate, re-record the baseline\n  \
               with --save-baseline."
        );
    }
    println!();

    for a in &d.axes {
        match &a.change {
            AxisChange::Appeared => {
                println!("  {:<13} not in the baseline — nothing to diff", a.axis)
            }
            AxisChange::Vanished => println!(
                "  {:<13} SCORED IN THE BASELINE, ABSENT NOW — a classifier failed\n\
                 {:<15} to build or a bank section was removed",
                a.axis, ""
            ),
            AxisChange::Compared(x) => print_axis_delta(&a.axis, x, d.attributable()),
        }
    }

    println!();
    let regressions = d.regressions();
    if regressions.is_empty() {
        if d.attributable() {
            println!(
                "No axis lost headroom. Encoder, bank and all twelve constants still\n\
                 agree with the last recorded run.\n"
            );
        }
    } else {
        let names: Vec<&str> = regressions.iter().map(|a| a.axis.as_str()).collect();
        println!(
            "→ REGRESSED on {}: {}.\n  \
               Same encoder, same bank — so what moved is the score distribution,\n  \
               not the question. Read the cushions above before touching a constant:\n  \
               the fix for a closing cushion is usually an exemplar, not a threshold.\n",
            if names.len() == 1 {
                "1 axis"
            } else {
                "several axes"
            },
            names.join(", ")
        );
    }
}

fn print_axis_delta(axis: &str, x: &AxisDelta, attributable: bool) {
    let flag = if attributable && x.regressed() {
        "   REGRESSED"
    } else {
        ""
    };
    println!(
        "  {axis:<13} separation {:.3} → {:.3} ({:+.3}) · errors {} → {} · coverage {:.0}% → {:.0}%{flag}",
        x.separation_before,
        x.separation_after,
        x.d_separation(),
        x.errors_before,
        x.errors_after,
        x.coverage_before * 100.0,
        x.coverage_after * 100.0,
    );
    // The cushion pair IS the early warning — separation is their
    // difference, and a shrinking one can come from either end.
    println!(
        "                weakest-accept {} → {} · nearest-miss {} → {}",
        fmt_cushion(x.weakest_accept_before),
        fmt_cushion(x.weakest_accept_after),
        fmt_cushion(x.nearest_miss_before),
        fmt_cushion(x.nearest_miss_after),
    );
    if x.gate_moved() {
        println!(
            "                GATE MOVED sim {:.3}→{:.3} margin {:.3}→{:.3} — a human\n\
             \x20               edited this, so the separation change follows from it",
            x.gate_before.min_sim,
            x.gate_after.min_sim,
            x.gate_before.min_margin,
            x.gate_after.min_margin,
        );
    }
    if x.cases_changed() {
        println!(
            "                bank cases {} → {} — the axis is being asked a\n\
             \x20               different question than it was",
            x.cases_before, x.cases_after
        );
    }
}

fn drift_json(d: &DriftReport) -> serde_json::Value {
    let axes: Vec<serde_json::Value> = d
        .axes
        .iter()
        .map(|a| match &a.change {
            AxisChange::Appeared => serde_json::json!({"axis": a.axis, "status": "appeared"}),
            AxisChange::Vanished => serde_json::json!({"axis": a.axis, "status": "vanished"}),
            AxisChange::Compared(x) => serde_json::json!({
                "axis": a.axis,
                "status": "compared",
                "separation_before": x.separation_before,
                "separation_after": x.separation_after,
                "d_separation": x.d_separation(),
                "errors_before": x.errors_before,
                "errors_after": x.errors_after,
                "d_coverage": x.d_coverage(),
                "cases_before": x.cases_before,
                "cases_after": x.cases_after,
                "gate_moved": x.gate_moved(),
                "regressed": attributable_regression(x, d.attributable()),
            }),
        })
        .collect();
    serde_json::json!({
        "attributable": d.attributable(),
        "same_model": d.same_model(),
        "same_bank": d.same_bank(),
        "baseline_model": d.baseline_model,
        "current_model": d.current_model,
        "baseline_digest": d.baseline_digest,
        "current_digest": d.current_digest,
        "regressed": d.is_regression(),
        "axes": axes,
    })
}

/// A per-axis regression flag that agrees with the report-level one.
///
/// `AxisDelta::regressed` answers "did this axis get worse", which is
/// only a claim about the router when the run is attributable. Emitting
/// the raw per-axis value next to `"regressed": false` at the top would
/// let a JSON consumer read a regression the tool explicitly refused to
/// assert.
fn attributable_regression(x: &AxisDelta, attributable: bool) -> bool {
    attributable && x.regressed()
}
