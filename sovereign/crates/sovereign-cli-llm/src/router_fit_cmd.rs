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
    fit, parse_bank, CalibrationCase, FitReport, GateOutcome, Objective, ScoredCase,
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

The default objective encodes the asymmetry every axis documents: a false
positive hard-commits a turn down a narrowed path, a false negative merely
falls through to the cascade. `--objective accuracy` scores the way the
prior art does (both errors weighted equally) for comparison.

Nothing is written. The report names the constant and the file to edit.";

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
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut bank = None;
    let mut axes: Vec<String> = Vec::new();
    let mut embed_model = None;
    let mut json = false;
    let mut objective_name = "safe-recall".to_string();
    let mut max_fp: usize = 0;
    let mut min_precision: f64 = 1.0;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--bank" => bank = Some(PathBuf::from(next(&mut it, "--bank")?)),
            "--axis" => axes.push(next(&mut it, "--axis")?),
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

    Ok(Opts {
        bank,
        axes,
        objective,
        embed_model,
        json,
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
        dirs::home_dir()
            .unwrap_or_default()
            .join(".sovereign")
            .join("models")
            .join(&slot.file)
    });
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
            slot.file
        );
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
        // The effort axis is calibrated in the UNPREFIXED (`d:`) space;
        // every other axis uses the instruction-prefixed (`q:`) one.
        // Getting this wrong silently produces numbers from the wrong
        // vector space, which is the single easiest way to make this
        // whole command lie.
        let embedded = if c.axis == "effort" {
            cache.embed_cached(&*provider, &c.query).await
        } else {
            cache.embed_query_cached(&*provider, &c.query).await
        };
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
            }),
            "locator" => router.score_locator_from_embedding(&v).map(|s| ScoredCase {
                id: c.id.clone(),
                score: s.score,
                expect: c.expected_label().map(String::from),
                predicted: Some(s.locator),
            }),
            "scope" => scope.as_ref().and_then(|k| {
                k.score_from_embedding(&v).map(|s| ScoredCase {
                    id: c.id.clone(),
                    score: s,
                    expect: c.expected_label().map(String::from),
                    predicted: Some("personal".to_string()),
                })
            }),
            "archive" => archive.as_ref().and_then(|k| {
                k.score_from_embedding(&v).map(|s| ScoredCase {
                    id: c.id.clone(),
                    score: s,
                    expect: c.expected_label().map(String::from),
                    predicted: Some("archive".to_string()),
                })
            }),
            "current_info" => current_info.as_ref().and_then(|k| {
                k.score_from_embedding(&v).map(|s| ScoredCase {
                    id: c.id.clone(),
                    score: s,
                    expect: c.expected_label().map(String::from),
                    predicted: Some("current".to_string()),
                })
            }),
            "effort" => effort.as_ref().and_then(|k| {
                k.score_from_embedding(&v).map(|s| ScoredCase {
                    id: c.id.clone(),
                    score: s,
                    expect: c.expected_label().map(String::from),
                    predicted: Some("high".to_string()),
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
                scope.as_ref().map(|k| k.gate()).unwrap_or(AxisGate::new(0.0, 0.0)),
                "DEFAULT_MIN_PERSONAL_SIM / DEFAULT_MIN_MARGIN",
                "sovereign-core/src/scope_classifier.rs",
            ),
        ),
        (
            "archive",
            (
                archive.as_ref().map(|k| k.gate()).unwrap_or(AxisGate::new(0.0, 0.0)),
                "DEFAULT_MIN_ARCHIVE_SIM / DEFAULT_MIN_MARGIN",
                "sovereign-core/src/archive_classifier.rs",
            ),
        ),
        (
            "current_info",
            (
                current_info.as_ref().map(|k| k.gate()).unwrap_or(AxisGate::new(0.0, 0.0)),
                "DEFAULT_MIN_CURRENT_SIM / DEFAULT_MIN_MARGIN",
                "sovereign-core/src/current_info_classifier.rs",
            ),
        ),
        (
            "effort",
            (
                effort.as_ref().map(|k| k.gate()).unwrap_or(AxisGate::new(0.0, 0.0)),
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

    if opts.json {
        let payload = serde_json::json!({
            "embed_model": slot.file,
            "banks": files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>(),
            "cases_total": cases.len(),
            "skipped": skipped.iter().map(|(id, ax)| serde_json::json!({"id": id, "axis": ax})).collect::<Vec<_>>(),
            "axes": &reports,
        });
        println!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
    } else {
        print_human(&reports, &shipped, &skipped, &slot.file);
    }

    // Exit 3 when any axis could be improved — a CI-friendly signal,
    // matching `router-cache check`'s "stale" convention.
    if reports.values().any(|r| r.would_change()) {
        3
    } else {
        0
    }
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

fn print_human(
    reports: &BTreeMap<String, FitReport>,
    shipped: &BTreeMap<&str, (AxisGate, &str, &str)>,
    skipped: &[(String, String)],
    model_file: &str,
) {
    println!("\nrouter fit — thresholds measured against {model_file}\n");
    for (axis, r) in reports {
        println!("── {axis} ──────────────────────────────────────────");
        println!(
            "  {} cases · {} gates evaluated",
            r.cases_scored, r.gates_evaluated
        );
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
        "Nothing was written. A calibration that edits the constants it \n\
         measures is the opaque loop this replaces."
    );
}
