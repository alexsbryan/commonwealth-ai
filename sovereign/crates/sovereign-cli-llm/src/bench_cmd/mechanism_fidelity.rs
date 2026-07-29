// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench mechanism-fidelity run …` — the elicitation +
//! scoring orchestrator for the Reasoning-Fidelity Validation Harness.
//!
//! The pure logic (case schemas, structural priors, perturbation engine,
//! scorer, pools, early-stopping, the [`ReasoningClass`] registry) lives
//! in `sovereign_eval::mechanism_fidelity`; this file is the only
//! inference-coupled surface. It is **class-generic** — it drives any
//! registered reasoning class through the same loop:
//!
//!   1. resolve `--class <id>` to a [`ReasoningClass`],
//!   2. ask it to `build_probes()` — a flat list of finished, letter-
//!      anchored [`RenderedProbe`]s (base case + its perturbations ×
//!      render {full, stripped-control} × paraphrase), each carrying the
//!      structural-prior probability the scorer needs,
//!   3. elicit a forced-choice **logprob** distribution from each model in
//!      ONE forward pass per probe (no K-sampling — the candidate set
//!      rides inside `structured_output` as a sentinel the daemon's
//!      embedded path reads off the masked next-token logits), then map it
//!      to a scalar target probability via `class.target_prob()`,
//!   4. score each perturbation's `d_agent` against the structural
//!      `d_struct`, and
//!   5. emit one [`ResultRow`] per probe as JSONL for the Python verdict.
//!
//! On Train/Dev the loop runs **anytime-valid early-stopping** (empirical-
//! Bernstein confidence intervals read at a pre-registered checkpoint
//! schedule): a model that has obviously passed or failed every band is
//! resolved and its remaining cases are skipped. The sacred `test` pool
//! runs a fixed pre-registered `n` and refuses to run without
//! `--unseal-test` (burning a [`PeekBudget`] counter) — the same
//! discipline the entity-resolution holdout uses.
//!
//! Models are selected by `--models a,b` (stems advertised on the
//! daemon's `/v1/models`). `request.model_id` is pinned per call —
//! `RemoteApiProvider::build_request` routes on it, so without it every
//! model collapses onto the daemon's default slot.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};
use sovereign_eval::entity_resolution_bench::PeekBudget;
use sovereign_eval::mechanism_fidelity::{
    by_id, class_ids, decide_at, grade_class, score, Bands, BoundedMean, FidelityCard,
    GradeThresholds, Pool, RenderedProbe, ResultRow, Scores, Side, StoppingConfig, Verdict,
};
use sovereign_inference::remote::RemoteApiProvider;

use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench mechanism-fidelity",
    summary: "Metamorphic reasoning-fidelity audit: does a frozen LLM reason from the causal mechanism or from memorized label-association?",
    sections: &[
        HelpSection::Usage(
            "svrn bench mechanism-fidelity run --models <a,b> [--class <id>] [--corpus <dir>] [--pool {train|dev|test}] [--n-cases N] [--seed N] [--concurrency N] [--base-url URL] [--api-key-env VAR] [--manifest PATH] [--out PATH] [--unseal-test --reason \"…\"]",
        ),
        HelpSection::Subcommands(&[(
            "run",
            "Resolve the class, build its probe matrix, elicit a forced-choice logprob distribution from each model, score against the structural prior, and write ResultRow JSONL.",
        )]),
        HelpSection::Notes(
            "Operates against the running daemon at localhost:9741 by default; the models under test are the --models stems (see `/v1/models`). Elicitation is forced-choice logprob — ONE forward pass per probe — and requires a daemon built with the embedded forced-choice path. Train/Dev use anytime-valid early-stopping; the `test` pool is sacred (needs --unseal-test and burns a peek in baselines/mechanism_fidelity/peek_budget.json). Default --class is wealth_tax_relocation. Read the result with sovereign/bench/mechanism_fidelity/verdict.py.",
        ),
    ],
};

/// Declared context window for the daemon-backed provider.
const PROVIDER_CTX: u32 = 8192;
/// Forced-choice attempts per probe before giving up. The embedded MTP
/// path on the large slot occasionally returns a transient error or an
/// unparseable body (the §7 "MTP process(verify) failed 503" class — the
/// daemon survives, the single call doesn't). Without retry these surface
/// as NaN probes that silently shrink the instrument's effective n;
/// retry-with-backoff recovers them so a flaky slot doesn't masquerade as
/// a low-fidelity finding.
const ELICIT_ATTEMPTS: usize = 4;

pub async fn cmd_mechanism_fidelity(args: &[String]) -> i32 {
    if args.is_empty() {
        help::print(&HELP);
        return 2;
    }
    let first = args[0].as_str();
    if first == "--help" || first == "-h" || first == "help" {
        help::print(&HELP);
        return 0;
    }
    match first {
        "run" => match parse_args(&args[1..]) {
            Ok(a) => run(a).await,
            Err(e) => {
                eprintln!("error: {e}");
                eprintln!();
                help::print(&HELP);
                2
            }
        },
        other => {
            eprintln!("error: unknown mechanism-fidelity subcommand `{other}`");
            eprintln!();
            help::print(&HELP);
            2
        }
    }
}

#[derive(Debug)]
struct Args {
    models: Vec<String>,
    class: String,
    corpus: Option<PathBuf>,
    pool: Pool,
    n_cases: usize,
    seed: u64,
    concurrency: usize,
    base_url: String,
    api_key_env: Option<String>,
    manifest: PathBuf,
    out: Option<PathBuf>,
    unseal_test: bool,
    reason: Option<String>,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut models: Vec<String> = Vec::new();
    let mut class = "wealth_tax_relocation".to_string();
    let mut corpus: Option<PathBuf> = None;
    let mut pool = Pool::Dev;
    let mut n_cases: usize = 200;
    let mut seed: u64 = 0;
    let mut concurrency: usize = 8;
    let mut base_url = "http://localhost:9741".to_string();
    let mut api_key_env: Option<String> = None;
    let mut manifest = PathBuf::from("sovereign/bench/mechanism_fidelity/manifest.toml");
    let mut out: Option<PathBuf> = None;
    let mut unseal_test = false;
    let mut reason: Option<String> = None;

    // Inline value-fetch — a macro (not a closure) so it doesn't hold a
    // persistent `&mut i` borrow that would conflict with the loop's
    // `i += 1`. Defined after `i` so macro_rules hygiene resolves the
    // captured local.
    let mut i = 0;
    macro_rules! val {
        ($label:expr) => {{
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{} requires a value", $label))?
        }};
    }

    while i < args.len() {
        match args[i].as_str() {
            "--models" => {
                models = val!("--models")
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            "--class" => class = val!("--class"),
            "--corpus" => corpus = Some(PathBuf::from(val!("--corpus"))),
            "--pool" => {
                let v = val!("--pool");
                pool = Pool::parse(&v).ok_or_else(|| format!("invalid --pool `{v}`"))?;
            }
            "--n-cases" => {
                n_cases = val!("--n-cases")
                    .parse()
                    .map_err(|_| "--n-cases must be a usize")?
            }
            "--seed" => seed = val!("--seed").parse().map_err(|_| "--seed must be a u64")?,
            // The generic class path is logprob-only — `--logprob` is the
            // default and is accepted as a no-op for command compatibility.
            "--logprob" => {}
            "--no-logprob" => {
                return Err(
                    "K-sampling elicitation is deprecated; the generic class path is \
                     logprob-only (the remote/frontier sampling fallback is a later \
                     package). Drop --no-logprob."
                        .into(),
                )
            }
            "--concurrency" => {
                concurrency = val!("--concurrency")
                    .parse::<usize>()
                    .map_err(|_| "--concurrency must be a usize")?
                    .max(1);
            }
            "--base-url" => base_url = val!("--base-url"),
            "--api-key-env" => api_key_env = Some(val!("--api-key-env")),
            "--manifest" => manifest = PathBuf::from(val!("--manifest")),
            "--out" => out = Some(PathBuf::from(val!("--out"))),
            "--unseal-test" => unseal_test = true,
            "--reason" => reason = Some(val!("--reason")),
            other => return Err(format!("unknown flag `{other}`")),
        }
        i += 1;
    }

    if models.is_empty() {
        return Err("--models is required (comma-separated daemon model stems)".into());
    }
    Ok(Args {
        models,
        class,
        corpus,
        pool,
        n_cases,
        seed,
        concurrency,
        base_url,
        api_key_env,
        manifest,
        out,
        unseal_test,
        reason,
    })
}

/// Aggregated elicitation estimate for one probe. In the logprob path
/// `p_verbal == p_freq` (one deterministic forward pass; the verbalized
/// estimator is a K-sampling artifact kept only for the JSONL contract).
#[derive(Debug, Clone, Copy)]
struct Agg {
    p_freq: f64,
    p_verbal: f64,
    eff_k: u32,
    latency_ms: u64,
}

impl Agg {
    fn missing() -> Self {
        Agg {
            p_freq: f64::NAN,
            p_verbal: f64::NAN,
            eff_k: 0,
            latency_ms: 0,
        }
    }
    fn from_p(p: f64, latency_ms: u64) -> Self {
        Agg {
            p_freq: p,
            p_verbal: p,
            eff_k: 1,
            latency_ms,
        }
    }
}

/// Per-model early-stopping state: the four bounded means whose verdicts
/// gate resolution, plus the provenance stamped onto every ResultRow.
#[derive(Default)]
struct ModelStop {
    /// μ_mag — DIR-P1 magnitude_ok among large-Δ cases (AtLeast band).
    mag: BoundedMean,
    /// μ_flat_p2 — DIR-P2 saturation flat_ok (AtLeast band).
    flat_p2: BoundedMean,
    /// μ_inv — INV invariance_ok (AtLeast band).
    inv: BoundedMean,
    /// μ_ctrl — negative control directional accuracy on P1 (AtMost band).
    ctrl: BoundedMean,
    resolved: bool,
    n_drawn: usize,
    stopped_early: bool,
    cs_lower: Option<f64>,
    cs_upper: Option<f64>,
}

/// Pre-registered early-stopping parameters loaded from `[stopping]` +
/// `[negative_control]`.
struct StopParams {
    cfg: StoppingConfig,
    /// μ_mag pass-fraction (AtLeast).
    mag: f64,
    /// μ_flat_p2 pass-fraction (AtLeast).
    flat: f64,
    /// μ_inv pass-fraction (AtLeast).
    inv: f64,
    /// μ_ctrl max directional accuracy (AtMost).
    ctrl: f64,
}

async fn run(args: Args) -> i32 {
    // ── Resolve the reasoning class ──
    let class = match by_id(&args.class) {
        Some(c) => c,
        None => {
            eprintln!(
                "error: unknown --class `{}`. Registered classes: {}",
                args.class,
                class_ids().join(", ")
            );
            return 2;
        }
    };

    // ── Pool gating (the sacred test pool) ──
    if args.pool.requires_unseal() && !args.unseal_test {
        eprintln!(
            "refusing to run the sacred `test` pool without --unseal-test.\n\
             The real holdout is never an optimization target; unsealing burns a\n\
             peek in baselines/mechanism_fidelity/peek_budget.json. Re-run with\n\
             --unseal-test --reason \"<why>\" if you genuinely mean to spend a peek."
        );
        return 2;
    }
    if args.pool.requires_unseal() && args.unseal_test {
        let reason = args
            .reason
            .clone()
            .unwrap_or_else(|| "(no reason given)".to_string());
        let peek_path = PathBuf::from(
            "sovereign/bench/mechanism_fidelity/baselines/mechanism_fidelity/peek_budget.json",
        );
        let mut budget = match PeekBudget::load(&peek_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: could not read peek budget at {peek_path:?}: {e}");
                return 1;
            }
        };
        let n = budget.burn(reason, git_commit_hash());
        if let Err(e) = budget.save(&peek_path) {
            eprintln!("error: could not persist peek budget: {e}");
            return 1;
        }
        eprintln!("[unseal] burned test peek #{n} (logged to {peek_path:?})");
    }

    // ── Bands + early-stopping (pre-registration manifest, or doc defaults) ──
    let bands = load_bands(&args.manifest);
    eprintln!(
        "[manifest] bands: collapse_min={} flat_max={} inv_max={} big_struct={} small_struct={}",
        bands.collapse_min, bands.flat_max, bands.inv_max, bands.big_struct, bands.small_struct
    );
    let sp = load_stopping(&args.manifest);
    // Early-stopping is Train/Dev only — the sacred Test pool runs a fixed
    // pre-registered n. The logprob path is deterministic, which is what
    // makes peeking honest under the empirical-Bernstein construction.
    let stopping_on = !matches!(args.pool, Pool::Test);
    if stopping_on {
        eprintln!(
            "[stopping] alpha={} checkpoints={:?} mag≥{} flat≥{} inv≥{} ctrl≤{}",
            sp.cfg.alpha, sp.cfg.checkpoints, sp.mag, sp.flat, sp.inv, sp.ctrl
        );
    } else {
        eprintln!(
            "[stopping] off (sacred test pool runs fixed n={})",
            args.n_cases
        );
    }

    // ── Providers (one per model, all pointed at --base-url/v1) ──
    let api_key = args
        .api_key_env
        .as_ref()
        .and_then(|var| std::env::var(var).ok());
    let v1 = format!("{}/v1", args.base_url.trim_end_matches('/'));
    let providers: Vec<Arc<dyn InferenceProvider>> = args
        .models
        .iter()
        .map(|m| {
            Arc::new(RemoteApiProvider::new(
                &v1,
                api_key.clone(),
                m,
                PROVIDER_CTX,
            )) as Arc<dyn InferenceProvider>
        })
        .collect();

    // ── Build the probe matrix from the class ──
    let candidates = class.candidates();
    let system_prompt = class.system_prompt();
    let probes = class.build_probes(args.n_cases, args.seed, args.corpus.as_deref());
    if probes.is_empty() {
        eprintln!(
            "error: class `{}` produced no probes (n_cases={}, corpus={:?})",
            class.id(),
            args.n_cases,
            args.corpus
        );
        return 1;
    }
    let spans = case_spans(&probes);

    // ── Preflight: one parseable draw per model before spending the run ──
    if let Err(e) = preflight(
        &providers,
        &args.models,
        &probes[0],
        &candidates,
        system_prompt,
    )
    .await
    {
        eprintln!("preflight failed: {e}");
        eprintln!("  (check the daemon is up at {v1} and the --models stems appear on /v1/models)");
        return 1;
    }

    eprintln!(
        "[estimator] forced-choice logprob — ONE forward pass per probe (the K-killer), elicited \
         sequentially for determinism"
    );
    if args.concurrency > 1 {
        eprintln!(
            "[note] --concurrency {} ignored: logprob elicitation is sequential so byte-identical \
             control prompts stay deterministic (the negative-control validity invariant).",
            args.concurrency
        );
    }
    eprintln!(
        "[plan] class={} · {} models × {} cases → {} probes/model ({} total) (pool={})",
        class.id(),
        args.models.len(),
        spans.len(),
        probes.len(),
        probes.len() * args.models.len(),
        args.pool.as_str(),
    );

    // ── Checkpoint + resume ──
    // The expensive artifact is the elicitation. We append one raw
    // aggregate per probe to a `.partial.jsonl` sidecar as it completes;
    // on restart with identical generation args we reload it and skip
    // finished probes. The scored ResultRow file is always recomputed
    // (cheap, pure) from the full aggregate set — re-running with a
    // different --manifest re-scores without re-eliciting.
    let out_path = args.out.clone().unwrap_or_else(|| {
        PathBuf::from(format!(
            "target/mechanism-fidelity/{}.jsonl",
            args.pool.as_str()
        ))
    });
    let ckpt_path = checkpoint_path(&out_path);
    let sig = run_signature(&args, class.id());
    let mut aggs: HashMap<String, Agg> = match load_checkpoint(&ckpt_path, &sig) {
        Ok(loaded) => {
            if !loaded.is_empty() {
                eprintln!(
                    "[resume] loaded {} completed probes from {ckpt_path:?}",
                    loaded.len()
                );
            }
            loaded
        }
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("  (remove {ckpt_path:?} to start fresh, or restore the original args)");
            return 1;
        }
    };
    let mut ckpt = match open_checkpoint(&ckpt_path, &sig) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: could not open checkpoint {ckpt_path:?}: {e}");
            return 1;
        }
    };

    // ── Elicit, case-grouped per model, with early-stopping ──
    let mut model_stops: Vec<ModelStop> = Vec::with_capacity(providers.len());
    let total_probes = probes.len() * providers.len();
    let mut done = 0usize;
    for model_idx in 0..providers.len() {
        let model_name = args.models[model_idx].as_str();
        let mut stop = ModelStop::default();
        let mut cases_done = 0usize;
        let mut resolved = false;

        for span in &spans {
            let group = &probes[span.clone()];

            // Elicit this case's not-yet-done probes SEQUENTIALLY. This is
            // load-bearing for instrument validity, not just simplicity:
            // the negative control's "provably blind" guarantee (§7) rests
            // on the stripped base and stripped perturbed prompts being
            // byte-identical → identical logprobs → d_agent == exactly 0.
            // Concurrent same-slot requests get batched together, and the
            // daemon's batched matmul reductions are not bit-invariant to
            // batch composition — so two byte-identical prompts elicited in
            // different batches return slightly different logits, breaking
            // control == 0 and the deterministic-peeking premise of early-
            // stopping. The candidate set + system prompt come from the
            // class; the prompt is already letter-anchored by build_probes
            // (we do NOT append a legend here).
            for gi in span.start..span.end {
                if aggs.contains_key(&agg_key(model_idx, &probes[gi])) {
                    continue; // resumed from checkpoint
                }
                let (dist, lat) = elicit_logprob(
                    providers[model_idx].as_ref(),
                    model_name,
                    &probes[gi].prompt,
                    &candidates,
                    system_prompt,
                )
                .await;
                let agg = match dist {
                    Some(d) => Agg::from_p(class.target_prob(&d), lat),
                    None => Agg::missing(),
                };
                if let Err(e) = append_checkpoint(&mut ckpt, model_idx, &probes[gi], &agg) {
                    eprintln!("warning: checkpoint append failed (continuing in-memory): {e}");
                }
                aggs.insert(agg_key(model_idx, &probes[gi]), agg);
            }
            done += group.len();
            cases_done += 1;
            if done % 100 < group.len() || done == total_probes {
                eprintln!("  [elicit] {done}/{total_probes} probes");
            }

            // ── Early-stopping decision at this case ──
            if stopping_on {
                let contrib = case_contrib(group, model_idx, &aggs, &bands);
                if let Some(v) = contrib.mag {
                    stop.mag.push(v);
                }
                if let Some(v) = contrib.flat_p2 {
                    stop.flat_p2.push(v);
                }
                if let Some(v) = contrib.inv {
                    stop.inv.push(v);
                }
                if let Some(v) = contrib.ctrl {
                    stop.ctrl.push(v);
                }

                let at_cp = sp.cfg.checkpoints.contains(&cases_done);
                let at_max = cases_done >= sp.cfg.n_max();
                let vm = decide_at(&stop.mag, &sp.cfg, sp.mag, Side::AtLeast, at_cp, at_max);
                let vp2 = decide_at(
                    &stop.flat_p2,
                    &sp.cfg,
                    sp.flat,
                    Side::AtLeast,
                    at_cp,
                    at_max,
                );
                let vi = decide_at(&stop.inv, &sp.cfg, sp.inv, Side::AtLeast, at_cp, at_max);
                let vc = decide_at(&stop.ctrl, &sp.cfg, sp.ctrl, Side::AtMost, at_cp, at_max);

                // Stop the instant the OVERALL verdict is decided, not when
                // every band individually resolves: a NO-GO needs only one
                // required band to FAIL (an unfaithful model that doesn't
                // collapse is settled the moment μ_mag fails, regardless of
                // whether the flat/invariance bands have tightened yet); a GO
                // needs ALL four to PASS. The flat/invariance AtLeast-0.90
                // bands can straddle indefinitely on a good-but-imperfect
                // model, so waiting for them to resolve would defeat early-
                // stopping entirely (observed: a model whose μ_mag failed at
                // n=32 still ran to n=200). At the cap we stop unconditionally
                // — a straddling band there is Inconclusive.
                let bands_v = [vm, vp2, vi, vc];
                let any_fail = bands_v.iter().any(|v| matches!(v, Verdict::Fail));
                let all_pass = bands_v.iter().all(|v| matches!(v, Verdict::Pass));
                let resolved_by_verdict = any_fail || all_pass;
                if resolved_by_verdict || at_max {
                    resolved = true;
                    stop.resolved = true;
                    stop.stopped_early = resolved_by_verdict && cases_done < args.n_cases;
                    stop.n_drawn = cases_done;
                    let (lo, hi) = stop.mag.interval(&sp.cfg);
                    stop.cs_lower = Some(lo);
                    stop.cs_upper = Some(hi);
                    let outcome = if all_pass {
                        "GO (all bands pass)"
                    } else if any_fail {
                        "NO-GO (a band failed)"
                    } else {
                        "Inconclusive (hit cap)"
                    };
                    eprintln!(
                        "[stop] {model_name}: {outcome} at {cases_done}/{} cases  (mag={vm:?} p2={vp2:?} inv={vi:?} ctrl={vc:?})",
                        args.n_cases
                    );
                    break;
                }
            }
        }

        if !resolved {
            stop.n_drawn = cases_done;
            stop.stopped_early = false;
            if stopping_on {
                let (lo, hi) = stop.mag.interval(&sp.cfg);
                stop.cs_lower = Some(lo);
                stop.cs_upper = Some(hi);
            }
        }
        model_stops.push(stop);
    }

    // ── Score + emit ResultRows (recomputed from the full aggregate set) ──
    let rows = build_rows(&args, class.id(), &probes, &aggs, &model_stops, &bands);
    if let Err(e) = write_jsonl(&out_path, &rows) {
        eprintln!("error: could not write {out_path:?}: {e}");
        return 1;
    }
    eprintln!("[out] wrote {} rows → {out_path:?}", rows.len());
    print_glassbox_summary(&args, &rows);

    // ── Characterize once: distill each model's verdict into its card ──
    // The "read free per query" artifact. Stamped with the manifest
    // fingerprint so a reader can detect a card graded under stale bands.
    let fp = manifest_fingerprint(&args.manifest);
    let th = grade_thresholds(&bands, &sp);
    let now = chrono::Utc::now().to_rfc3339();
    let card_dir = FidelityCard::default_dir();
    for m in &args.models {
        let entry = grade_class(
            &rows,
            m,
            class.id(),
            args.pool.as_str(),
            &th,
            &bands,
            &fp,
            now.clone(),
        );
        let grade = entry.grade;
        let conf = entry.confidence;
        let mut card = FidelityCard::load_or_new(&card_dir, m);
        card.upsert(entry);
        match card.save(&card_dir) {
            Ok(p) => eprintln!(
                "[card] {m}: {grade:?} (conf {conf:.2}) on {} → {p:?}",
                class.id()
            ),
            Err(e) => eprintln!("[card] {m}: could not write card: {e}"),
        }
    }
    0
}

/// One parseable forced-choice draw per model, on the first probe, before
/// the expensive run begins. Fails fast on an unreachable daemon or a
/// model stem the daemon doesn't serve.
async fn preflight(
    providers: &[Arc<dyn InferenceProvider>],
    models: &[String],
    probe: &RenderedProbe,
    candidates: &[String],
    system_prompt: &str,
) -> Result<(), String> {
    for (p, name) in providers.iter().zip(models) {
        let (dist, _) =
            elicit_logprob(p.as_ref(), name, &probe.prompt, candidates, system_prompt).await;
        match dist {
            Some(_) => eprintln!("[preflight] {name}: ok"),
            None => {
                return Err(format!(
                    "model `{name}` returned no parseable forced-choice distribution \
                     (is the daemon built with the forced-choice embedded path?)"
                ))
            }
        }
    }
    Ok(())
}

/// Forced-choice logprob elicitation — ONE forward pass. Carries the
/// candidate set as a sentinel in `structured_output`; the daemon's
/// embedded path reads the next-token distribution over the candidates and
/// returns it as JSON in `text`. Returns the per-candidate distribution
/// (the caller maps it to a scalar via `class.target_prob`).
async fn elicit_logprob(
    model: &dyn InferenceProvider,
    model_id: &str,
    prompt: &str,
    candidates: &[String],
    system_prompt: &str,
) -> (Option<Vec<(String, f64)>>, u64) {
    let schema = serde_json::json!({
        "type": "string",
        "enum": candidates,
        "x_forced_choice": true
    });
    let req = CompletionRequest {
        prompt: prompt.to_string(),
        system_message: Some(system_prompt.to_string()),
        preferred_speed: Speed::Slow,
        max_tokens: Some(1),
        structured_output: Some(schema),
        think_budget: Some(0),
        enable_thinking: Some(false),
        // Pin the slot by name — `build_request` routes on `model_id`, NOT
        // the provider's own id, so without this every model collapses
        // onto the daemon's default slot.
        model_id: Some(model_id.to_string()),
        ..Default::default()
    };
    let start = Instant::now();
    let mut last_err = String::new();
    for attempt in 0..ELICIT_ATTEMPTS {
        match model.complete(&req).await {
            Ok(resp) => match parse_forced_choice_dist(&resp.text, candidates) {
                Some(dist) => return (Some(dist), start.elapsed().as_millis() as u64),
                None => {
                    last_err = format!("parse failed: {:?}", &resp.text[..resp.text.len().min(80)]);
                }
            },
            Err(e) => last_err = format!("inference error: {e}"),
        }
        // Exponential-ish backoff; the transient class clears in well under
        // a second, so 200/400/600ms is ample without stalling the run.
        if attempt + 1 < ELICIT_ATTEMPTS {
            tokio::time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
        }
    }
    eprintln!("    [logprob] {model_id} failed after {ELICIT_ATTEMPTS} attempts: {last_err}");
    (None, 0)
}

/// Parse the forced-choice probability JSON `{"A":..,"B":..}` into a
/// distribution over `candidates` (missing keys default to 0 — a model
/// that never puts mass on a candidate contributes nothing to it).
fn parse_forced_choice_dist(text: &str, candidates: &[String]) -> Option<Vec<(String, f64)>> {
    let m: HashMap<String, f64> = serde_json::from_str(text.trim()).ok()?;
    Some(
        candidates
            .iter()
            .map(|c| (c.clone(), m.get(c).copied().unwrap_or(0.0)))
            .collect(),
    )
}

// ── Probe grouping + keys ────────────────────────────────────────────

/// Contiguous spans of probes sharing a `case_id`. `build_probes` emits a
/// case's probes back-to-back, so a single pass groups them.
fn case_spans(probes: &[RenderedProbe]) -> Vec<std::ops::Range<usize>> {
    let mut spans = Vec::new();
    if probes.is_empty() {
        return spans;
    }
    let mut start = 0usize;
    for i in 1..=probes.len() {
        if i == probes.len() || probes[i].case_id != probes[start].case_id {
            spans.push(start..i);
            start = i;
        }
    }
    spans
}

/// The aggregate-map / checkpoint key: `model|case|render|paraphrase|variant`.
fn agg_key(model_idx: usize, rp: &RenderedProbe) -> String {
    format!(
        "{model_idx}|{}|{}|{}|{}",
        rp.case_id, rp.render, rp.paraphrase, rp.variant
    )
}

/// The base (reference) probe's key for a given context.
fn base_key(model_idx: usize, case_id: &str, render: &str, paraphrase: bool) -> String {
    format!("{model_idx}|{case_id}|{render}|{paraphrase}|base")
}

// ── Scoring ──────────────────────────────────────────────────────────

/// Signed agent + structural deltas and the metamorphic scores for one
/// probe, relative to its context's base. The single scoring authority —
/// both `build_rows` and the early-stopping contribution call it, so the
/// loop's stop decisions and the emitted rows can never disagree.
fn delta_score(
    rp: &RenderedProbe,
    p_agent: f64,
    base_p: f64,
    base_sp: f64,
    bands: &Bands,
) -> (f64, f64, Scores) {
    if rp.is_base() {
        (0.0, 0.0, Scores::none())
    } else {
        let d_agent = p_agent - base_p;
        let d_struct = rp.structural_p - base_sp;
        (d_agent, d_struct, score(rp.kind, d_agent, d_struct, bands))
    }
}

/// One case's contribution to the four early-stopping means (full,
/// non-paraphrase signals + the control's directional accuracy). Mirrors
/// `verdict.py`'s band selections so the stop decision matches the final
/// verdict.
struct CaseContrib {
    /// μ_mag — only `Some` on a large-Δ P1 case (the magnitude band applies).
    mag: Option<f64>,
    flat_p2: Option<f64>,
    inv: Option<f64>,
    ctrl: Option<f64>,
}

fn case_contrib(
    group: &[RenderedProbe],
    model_idx: usize,
    aggs: &HashMap<String, Agg>,
    bands: &Bands,
) -> CaseContrib {
    // Base (agent p, structural p) per (render, paraphrase) context.
    let mut base_p: HashMap<(String, bool), f64> = HashMap::new();
    let mut base_sp: HashMap<(String, bool), f64> = HashMap::new();
    for rp in group {
        if rp.is_base() {
            base_sp.insert((rp.render.clone(), rp.paraphrase), rp.structural_p);
            if let Some(a) = aggs.get(&agg_key(model_idx, rp)) {
                base_p.insert((rp.render.clone(), rp.paraphrase), a.p_freq);
            }
        }
    }

    let mut out = CaseContrib {
        mag: None,
        flat_p2: None,
        inv: None,
        ctrl: None,
    };
    for rp in group {
        let Some(a) = aggs.get(&agg_key(model_idx, rp)) else {
            continue;
        };
        let ctx = (rp.render.clone(), rp.paraphrase);
        let bp = base_p.get(&ctx).copied().unwrap_or(f64::NAN);
        let bsp = base_sp.get(&ctx).copied().unwrap_or(f64::NAN);
        let (d_agent, d_struct, s) = delta_score(rp, a.p_freq, bp, bsp, bands);

        if rp.render == "full" && !rp.paraphrase {
            match rp.variant.as_str() {
                "dir_p1" => {
                    if let Some(ok) = s.magnitude_ok {
                        out.mag = Some(b2f(ok));
                    }
                }
                "dir_p2" => {
                    if let Some(ok) = s.flat_ok {
                        out.flat_p2 = Some(b2f(ok));
                    }
                }
                "inv_i1" => {
                    if let Some(ok) = s.invariance_ok {
                        out.inv = Some(b2f(ok));
                    }
                }
                _ => {}
            }
        }
        // Control (stripped) directional accuracy on P1: must sit at chance.
        if rp.is_control() && rp.variant == "dir_p1" && d_agent.is_finite() && d_struct != 0.0 {
            out.ctrl = Some(b2f(sign(d_agent) == sign(d_struct)));
        }
    }
    out
}

fn build_rows(
    args: &Args,
    class_id: &str,
    probes: &[RenderedProbe],
    aggs: &HashMap<String, Agg>,
    model_stops: &[ModelStop],
    bands: &Bands,
) -> Vec<ResultRow> {
    let n_models = args.models.len();
    // Base structural p per (case, render, paraphrase) — model-independent.
    let mut base_sp: HashMap<(String, String, bool), f64> = HashMap::new();
    for rp in probes {
        if rp.is_base() {
            base_sp.insert(
                (rp.case_id.clone(), rp.render.clone(), rp.paraphrase),
                rp.structural_p,
            );
        }
    }
    // Base agent p per (model, case, render, paraphrase).
    let mut base_p: HashMap<String, f64> = HashMap::new();
    for model_idx in 0..n_models {
        for rp in probes {
            if rp.is_base() {
                if let Some(a) = aggs.get(&agg_key(model_idx, rp)) {
                    base_p.insert(
                        base_key(model_idx, &rp.case_id, &rp.render, rp.paraphrase),
                        a.p_freq,
                    );
                }
            }
        }
    }

    let mut rows = Vec::new();
    for model_idx in 0..n_models {
        let st = &model_stops[model_idx];
        for rp in probes {
            let Some(agg) = aggs.get(&agg_key(model_idx, rp)) else {
                continue;
            };
            let bp = base_p
                .get(&base_key(model_idx, &rp.case_id, &rp.render, rp.paraphrase))
                .copied()
                .unwrap_or(f64::NAN);
            let bsp = base_sp
                .get(&(rp.case_id.clone(), rp.render.clone(), rp.paraphrase))
                .copied()
                .unwrap_or(f64::NAN);
            let (d_agent, d_struct, scores) = delta_score(rp, agg.p_freq, bp, bsp, bands);

            rows.push(ResultRow {
                model_id: args.models[model_idx].clone(),
                class: class_id.to_string(),
                case_id: rp.case_id.clone(),
                pool: args.pool.as_str().to_string(),
                variant: rp.variant.clone(),
                render: rp.render.clone(),
                paraphrase: rp.paraphrase,
                control: rp.is_control(),
                expected_sign: rp.expected_sign,
                k_draws: agg.eff_k,
                p_freq: agg.p_freq,
                p_verbal: agg.p_verbal,
                d_agent,
                d_struct,
                direction_ok: scores.direction_ok,
                magnitude_ok: scores.magnitude_ok,
                flat_ok: scores.flat_ok,
                invariance_ok: scores.invariance_ok,
                seed: args.seed,
                latency_ms: agg.latency_ms,
                n_drawn: st.n_drawn,
                stopped_early: st.stopped_early,
                cs_lower: st.cs_lower,
                cs_upper: st.cs_upper,
            });
        }
    }
    rows
}

// ── Checkpoint / resume ──────────────────────────────────────────────

/// `<out>.partial.jsonl` — the resumable elicitation sidecar.
fn checkpoint_path(out: &Path) -> PathBuf {
    let mut s = out.as_os_str().to_os_string();
    s.push(".partial.jsonl");
    PathBuf::from(s)
}

/// A run is resumable only against identical generation args; otherwise
/// the case/model alignment would break. This signature is the first line
/// of the checkpoint and is checked on resume.
fn run_signature(args: &Args, class_id: &str) -> String {
    format!(
        "models={}|class={}|seed={}|n_cases={}|pool={}|corpus={}",
        args.models.join(","),
        class_id,
        args.seed,
        args.n_cases,
        args.pool.as_str(),
        args.corpus
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    )
}

/// One durable elicitation record. Keyed fields reconstruct the in-memory
/// `aggs` key on resume.
#[derive(serde::Serialize, serde::Deserialize)]
struct CheckpointAgg {
    model_idx: usize,
    case_id: String,
    variant: String,
    render: String,
    paraphrase: bool,
    p_freq: f64,
    p_verbal: f64,
    eff_k: u32,
    latency_ms: u64,
}

/// Load completed probes from the checkpoint, if any. Errors on a
/// signature mismatch (different args ⇒ unsafe to resume).
fn load_checkpoint(path: &Path, sig: &str) -> Result<HashMap<String, Agg>, String> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(out); // no checkpoint yet
    };
    let mut lines = text.lines();
    match lines.next() {
        Some(first) => {
            let header: serde_json::Value = serde_json::from_str(first)
                .map_err(|e| format!("malformed checkpoint header in {path:?}: {e}"))?;
            let found = header.get("_sig").and_then(|v| v.as_str()).unwrap_or("");
            if found != sig {
                return Err(format!(
                    "checkpoint {path:?} was written for different args\n    have: {sig}\n    file: {found}"
                ));
            }
        }
        None => return Ok(out),
    }
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let c: CheckpointAgg = serde_json::from_str(line)
            .map_err(|e| format!("bad checkpoint line in {path:?}: {e}"))?;
        let key = format!(
            "{}|{}|{}|{}|{}",
            c.model_idx, c.case_id, c.render, c.paraphrase, c.variant
        );
        out.insert(
            key,
            Agg {
                p_freq: c.p_freq,
                p_verbal: c.p_verbal,
                eff_k: c.eff_k,
                latency_ms: c.latency_ms,
            },
        );
    }
    Ok(out)
}

/// Open the checkpoint for appending, writing the signature header when
/// the file is new.
fn open_checkpoint(path: &Path, sig: &str) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let fresh = !path.exists();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    if fresh {
        writeln!(f, "{}", serde_json::json!({ "_sig": sig }))?;
        f.flush()?;
    }
    Ok(f)
}

fn append_checkpoint(
    f: &mut std::fs::File,
    model_idx: usize,
    rp: &RenderedProbe,
    agg: &Agg,
) -> std::io::Result<()> {
    let rec = CheckpointAgg {
        model_idx,
        case_id: rp.case_id.clone(),
        variant: rp.variant.clone(),
        render: rp.render.clone(),
        paraphrase: rp.paraphrase,
        p_freq: agg.p_freq,
        p_verbal: agg.p_verbal,
        eff_k: agg.eff_k,
        latency_ms: agg.latency_ms,
    };
    let line = serde_json::to_string(&rec)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    writeln!(f, "{line}")?;
    f.flush()
}

// ── Output ───────────────────────────────────────────────────────────

fn write_jsonl(path: &Path, rows: &[ResultRow]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::File::create(path)?;
    for r in rows {
        let line = serde_json::to_string(r)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        writeln!(f, "{line}")?;
    }
    Ok(())
}

/// A compact, glass-box read of the run — per model, the mean full-render
/// P1 collapse and the control's P1 movement (so a leak is visible at a
/// glance), plus the early-stopping outcome. This table is the run's
/// result, so it goes to stdout; only the pointer at the tail (what to run
/// next) stays on stderr.
fn print_glassbox_summary(args: &Args, rows: &[ResultRow]) {
    println!(
        "\n── reasoning-fidelity summary (class={}, pool={}) ──",
        args.class,
        args.pool.as_str()
    );
    for m in &args.models {
        let p1_full: Vec<f64> = rows
            .iter()
            .filter(|r| &r.model_id == m && r.variant == "dir_p1" && !r.control && !r.paraphrase)
            .map(|r| r.d_agent)
            .filter(|d| d.is_finite())
            .collect();
        let p1_ctrl: Vec<f64> = rows
            .iter()
            .filter(|r| &r.model_id == m && r.variant == "dir_p1" && r.control)
            .map(|r| r.d_agent)
            .filter(|d| d.is_finite())
            .collect();
        let p2_full: Vec<f64> = rows
            .iter()
            .filter(|r| &r.model_id == m && r.variant == "dir_p2" && !r.control && !r.paraphrase)
            .map(|r| r.d_agent.abs())
            .filter(|d| d.is_finite())
            .collect();
        let inv_full: Vec<f64> = rows
            .iter()
            .filter(|r| &r.model_id == m && r.variant == "inv_i1" && !r.control && !r.paraphrase)
            .map(|r| r.d_agent.abs())
            .filter(|d| d.is_finite())
            .collect();
        println!(
            "  {m}: P1 collapse Δ̄={:+.3} (n={})  |  control P1 Δ̄={:+.3} (n={})  |  P2 |Δ̄|={:.3}  |  INV |Δ̄|={:.3}",
            mean(&p1_full),
            p1_full.len(),
            mean(&p1_ctrl),
            p1_ctrl.len(),
            mean(&p2_full),
            mean(&inv_full),
        );
        // Early-stopping provenance (identical across a model's rows).
        if let Some(r) = rows.iter().find(|r| &r.model_id == m) {
            let cs = match (r.cs_lower, r.cs_upper) {
                (Some(lo), Some(hi)) => format!("  mag CS=[{lo:.2},{hi:.2}]"),
                _ => String::new(),
            };
            println!(
                "      cases drawn: {}{}{}",
                r.n_drawn,
                if r.stopped_early {
                    " (stopped early)"
                } else {
                    ""
                },
                cs
            );
        }
    }
    // The legend is how you read the table above, so it ships with it;
    // the "run this next" pointer is guidance and stays on stderr.
    println!(
        "  (faithful: P1 Δ̄ strongly negative, control P1 Δ̄≈0, P2/INV |Δ̄| small.)"
    );
    eprintln!(
        "  Read the power-annotated verdict with sovereign/bench/mechanism_fidelity/verdict.py."
    );
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

fn b2f(b: bool) -> f64 {
    if b {
        1.0
    } else {
        0.0
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

// ── Manifest loading ─────────────────────────────────────────────────

/// Load the `[bands]` table from the pre-registration manifest, falling
/// back to the doc defaults when the file is absent. Unknown/missing keys
/// inherit the default so a partial manifest still loads.
fn load_bands(path: &Path) -> Bands {
    let mut bands = Bands::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("[manifest] {path:?} not found — using doc default bands");
        return bands;
    };
    let Ok(val) = text.parse::<toml::Value>() else {
        eprintln!("[manifest] {path:?} is not valid TOML — using doc default bands");
        return bands;
    };
    if let Some(t) = val.get("bands").and_then(|b| b.as_table()) {
        let g = |k: &str, d: f64| t.get(k).and_then(|v| v.as_float()).unwrap_or(d);
        bands.collapse_min = g("collapse_min", bands.collapse_min);
        bands.flat_max = g("flat_max", bands.flat_max);
        bands.inv_max = g("inv_max", bands.inv_max);
        bands.big_struct = g("big_struct", bands.big_struct);
        bands.small_struct = g("small_struct", bands.small_struct);
    }
    bands
}

/// Load the `[stopping]` block + the `[negative_control]` control band,
/// falling back to the [`StoppingConfig`] / doc defaults.
fn load_stopping(path: &Path) -> StopParams {
    let mut p = StopParams {
        cfg: StoppingConfig::default(),
        mag: 0.80,
        flat: 0.90,
        inv: 0.90,
        ctrl: 0.55,
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return p;
    };
    let Ok(val) = text.parse::<toml::Value>() else {
        return p;
    };
    if let Some(t) = val.get("stopping").and_then(|b| b.as_table()) {
        if let Some(a) = t.get("alpha").and_then(|v| v.as_float()) {
            p.cfg.alpha = a;
        }
        if let Some(arr) = t.get("checkpoints").and_then(|v| v.as_array()) {
            let cps: Vec<usize> = arr
                .iter()
                .filter_map(|x| x.as_integer())
                .map(|i| i as usize)
                .collect();
            if !cps.is_empty() {
                p.cfg.checkpoints = cps;
            }
        }
        let g = |k: &str, d: f64| t.get(k).and_then(|v| v.as_float()).unwrap_or(d);
        p.mag = g("mag_pass_fraction", p.mag);
        p.flat = g("flat_pass_fraction", p.flat);
        p.inv = g("inv_pass_fraction", p.inv);
    }
    if let Some(nc) = val.get("negative_control").and_then(|b| b.as_table()) {
        if let Some(v) = nc
            .get("max_directional_accuracy")
            .and_then(|v| v.as_float())
        {
            p.ctrl = v;
        }
    }
    p
}

/// A stable fingerprint of the manifest's content — stamped onto each card
/// so a reader can tell whether the bands a card was graded under still
/// match the current pre-registration. Not cryptographic; it only needs to
/// change when the manifest changes.
fn manifest_fingerprint(path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// Assemble the card-grading thresholds from the loaded bands + stopping
/// pass-fractions (the same numbers the verdict and early-stopping use).
fn grade_thresholds(bands: &Bands, sp: &StopParams) -> GradeThresholds {
    GradeThresholds {
        collapse_min: bands.collapse_min,
        flat_max: bands.flat_max,
        mag_pass: sp.mag,
        flat_pass: sp.flat,
        inv_pass: sp.inv,
        control_max_dir_acc: sp.ctrl,
        min_cases: GradeThresholds::default().min_cases,
    }
}

fn git_commit_hash() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_forced_choice_dist_reads_distribution() {
        let cands = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let d = parse_forced_choice_dist("{\"A\":0.6,\"B\":0.3,\"C\":0.1}", &cands).unwrap();
        assert_eq!(
            d,
            vec![("A".into(), 0.6), ("B".into(), 0.3), ("C".into(), 0.1)]
        );
        // Missing keys default to 0.
        let d = parse_forced_choice_dist("{\"A\":1.0}", &cands).unwrap();
        assert_eq!(
            d,
            vec![("A".into(), 1.0), ("B".into(), 0.0), ("C".into(), 0.0)]
        );
        assert!(parse_forced_choice_dist("garbage", &cands).is_none());
    }

    #[test]
    fn case_spans_groups_contiguous_cases() {
        use sovereign_eval::mechanism_fidelity::registry;
        let cls = &registry()[0];
        let probes = cls.build_probes(3, 0, None);
        let spans = case_spans(&probes);
        assert_eq!(spans.len(), 3, "one span per base case");
        // Every probe in a span shares the same case_id.
        for span in &spans {
            let id = &probes[span.start].case_id;
            assert!(probes[span.clone()].iter().all(|p| &p.case_id == id));
        }
        // Spans tile the probe list with no gaps.
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans.last().unwrap().end, probes.len());
    }

    #[test]
    fn agg_key_matches_checkpoint_reconstruction() {
        use sovereign_eval::mechanism_fidelity::registry;
        let cls = &registry()[0];
        let probes = cls.build_probes(1, 0, None);
        let rp = &probes[0];
        let key = agg_key(1, rp);
        // The checkpoint round-trip rebuilds the same key string.
        let reconstructed = format!(
            "{}|{}|{}|{}|{}",
            1, rp.case_id, rp.render, rp.paraphrase, rp.variant
        );
        assert_eq!(key, reconstructed);
        // base_key agrees with the base probe's agg_key.
        assert_eq!(
            agg_key(1, rp),
            base_key(1, &rp.case_id, &rp.render, rp.paraphrase)
        );
    }
}
