//! `sovereign bench mechanism-fidelity run …` — the elicitation +
//! scoring orchestrator for the Mechanism-Fidelity Validation Harness.
//!
//! The pure logic (case schema, structural prior, perturbation engine,
//! scorer, pools) lives in `sovereign_eval::mechanism_fidelity`; this
//! file is the only inference-coupled surface. It:
//!
//!   1. generates synthetic cases for a pool,
//!   2. for each base case builds the probe matrix — variants {base,
//!      dir_p1, dir_p2, inv_i1} × render {full, stripped-control} ×
//!      paraphrase — eliciting a relocation probability from each model,
//!   3. estimates that probability by **repeated sampling** (no logprobs
//!      exist on either the local or frontier path): K structured draws
//!      of a forced ternary choice at temperature, `p_freq = (#relocate
//!      + ½·#indifferent) / K`, with verbalized confidence co-elicited
//!      for free,
//!   4. scores each perturbation's `d_agent` against the structural
//!      `d_struct`, and
//!   5. emits one [`ResultRow`] per probe as JSONL for the Python
//!      verdict sidecar.
//!
//! The sacred `test` pool refuses to run without `--unseal-test` and
//! burns a [`PeekBudget`] counter when it does — the same discipline the
//! entity-resolution holdout uses.
//!
//! Models are selected by `--models a,b` (stems advertised on the
//! daemon's `/v1/models`). Two open-weight models is the first-slice
//! default; a frontier model plugs in by pointing `--base-url` at its
//! endpoint with `--api-key-env`.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use futures::stream::{self, StreamExt};
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::Deserialize;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{CompletionRequest, Speed};
use sovereign_eval::entity_resolution_bench::PeekBudget;
use sovereign_eval::mechanism_fidelity::{
    generate_cases, render_prompt, score, structural_p_relocate, Bands, Case, Pool, RenderMode,
    ResultRow, Variant,
};
use sovereign_inference::remote::RemoteApiProvider;

use crate::util::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign bench mechanism-fidelity",
    summary: "Metamorphic mechanism-fidelity audit of LLM relocation decisions under a wealth tax.",
    sections: &[
        HelpSection::Usage(
            "sovereign bench mechanism-fidelity run --models <a,b> [--pool {train|dev|test}] [--k N] [--n-cases N] [--seed N] [--no-paraphrase] [--concurrency N] [--base-url URL] [--api-key-env VAR] [--manifest PATH] [--out PATH] [--unseal-test --reason \"…\"]",
        ),
        HelpSection::Subcommands(&[(
            "run",
            "Generate cases, elicit decisions from each model by repeated sampling, score, and write ResultRow JSONL.",
        )]),
        HelpSection::Notes(
            "Operates against the running daemon at localhost:9741 by default; the models under test are the --models stems (see `/v1/models`). p_relocate is estimated by K structured draws of a ternary choice at temperature (no logprobs exist). The `test` pool is sacred: it needs --unseal-test and burns a peek in baselines/mechanism_fidelity/peek_budget.json. Read the result with sovereign/bench/mechanism_fidelity/verdict.py.",
        ),
    ],
};

/// Per-draw decoding budget. The JSON object is tiny; cap hard.
const DRAW_MAX_TOKENS: usize = 64;
/// Sampling temperature for the repeated-draw estimator. T=0 would
/// collapse K draws to one point and destroy the frequency. This is the
/// one sampling param the remote `build_request` actually forwards.
const DRAW_TEMPERATURE: f32 = 0.7;
/// Declared context window for the daemon-backed provider.
const PROVIDER_CTX: u32 = 8192;

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
    pool: Pool,
    k: u32,
    n_cases: usize,
    seed: u64,
    paraphrase: bool,
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
    let mut pool = Pool::Dev;
    let mut k: u32 = 64;
    let mut n_cases: usize = 200;
    let mut seed: u64 = 0;
    let mut paraphrase = true;
    let mut concurrency: usize = 8;
    let mut base_url = "http://localhost:9741".to_string();
    let mut api_key_env: Option<String> = None;
    let mut manifest =
        PathBuf::from("sovereign/bench/mechanism_fidelity/manifest.toml");
    let mut out: Option<PathBuf> = None;
    let mut unseal_test = false;
    let mut reason: Option<String> = None;

    // Inline value-fetch — a macro (not a closure) so it doesn't hold a
    // persistent `&mut i` borrow that would conflict with the loop's
    // `i += 1`. Mirrors the explicit-index style in `enron.rs`. Defined
    // after `i` so macro_rules hygiene resolves the captured local.
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
            "--pool" => {
                let v = val!("--pool");
                pool = Pool::parse(&v).ok_or_else(|| format!("invalid --pool `{v}`"))?;
            }
            "--k" => k = val!("--k").parse().map_err(|_| "--k must be a u32")?,
            "--n-cases" => {
                n_cases = val!("--n-cases")
                    .parse()
                    .map_err(|_| "--n-cases must be a usize")?
            }
            "--seed" => seed = val!("--seed").parse().map_err(|_| "--seed must be a u64")?,
            "--no-paraphrase" => paraphrase = false,
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
    if k == 0 {
        return Err("--k must be ≥ 1".into());
    }
    Ok(Args {
        models,
        pool,
        k,
        n_cases,
        seed,
        paraphrase,
        concurrency,
        base_url,
        api_key_env,
        manifest,
        out,
        unseal_test,
        reason,
    })
}

// ── The probe matrix ────────────────────────────────────────────────

/// One elicitation unit: a (model, base case, variant, render,
/// paraphrase) tuple. The base variant in each context is the reference
/// the perturbations' deltas are measured against.
#[derive(Debug, Clone)]
struct Probe {
    model_idx: usize,
    case_idx: usize,
    variant: Variant,
    render: RenderMode,
    paraphrase: bool,
}

/// Context key for locating a probe's reference (base) result — same
/// model, case, render, and wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, std::hash::Hash)]
struct Ctx {
    model_idx: usize,
    case_idx: usize,
    render: u8,
    paraphrase: bool,
}

impl Probe {
    fn ctx(&self) -> Ctx {
        Ctx {
            model_idx: self.model_idx,
            case_idx: self.case_idx,
            render: self.render as u8,
            paraphrase: self.paraphrase,
        }
    }
}

/// Aggregated repeated-sampling estimate for one probe.
#[derive(Debug, Clone, Copy)]
struct Agg {
    p_freq: f64,
    p_verbal: f64,
    eff_k: u32,
    latency_ms: u64,
}

/// One model's decision draw, parsed from the structured output.
#[derive(Debug, Deserialize)]
struct Decision {
    decision: String,
    #[serde(default)]
    confidence: f64,
}

async fn run(args: Args) -> i32 {
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
        let peek_path =
            PathBuf::from("sovereign/bench/mechanism_fidelity/baselines/mechanism_fidelity/peek_budget.json");
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

    // ── Bands (pre-registration manifest, or the doc defaults) ──
    let bands = load_bands(&args.manifest);
    eprintln!(
        "[manifest] bands: collapse_min={} flat_max={} inv_max={} big_struct={} small_struct={}",
        bands.collapse_min, bands.flat_max, bands.inv_max, bands.big_struct, bands.small_struct
    );

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

    // ── Cases + precomputed per-(case, variant) transformed cases ──
    let cases = generate_cases(args.n_cases, args.seed);
    // (case_idx, variant) -> (transformed Case, structural p). InvI1
    // identity swap is seeded deterministically per case so the swapped
    // subject is stable across draws and runs.
    let mut variant_case: HashMap<(usize, Variant), (Case, f64)> = HashMap::new();
    for (ci, base) in cases.iter().enumerate() {
        for v in Variant::all() {
            let mut rng = StdRng::seed_from_u64(
                args.seed
                    .wrapping_mul(1_000_003)
                    .wrapping_add(ci as u64)
                    .wrapping_add(v as u64 * 97),
            );
            let c = v.apply(base, &mut rng);
            let p = structural_p_relocate(&c);
            variant_case.insert((ci, v), (c, p));
        }
    }

    // ── Enumerate probes ──
    let probes = enumerate_probes(args.models.len(), cases.len(), args.paraphrase);
    let total_draws = probes.len() as u64 * args.k as u64;
    eprintln!(
        "[plan] {} models × {} cases → {} probes × K={} = {} draws (pool={}, concurrency={})",
        args.models.len(),
        cases.len(),
        probes.len(),
        args.k,
        total_draws,
        args.pool.as_str(),
        args.concurrency
    );

    // ── Elicit every probe (K concurrent draws per probe) ──
    let mut aggs: HashMap<(usize, usize, u8, bool, u8), Agg> = HashMap::new();
    let mut done = 0usize;
    for probe in &probes {
        let (case, _p_struct) = &variant_case[&(probe.case_idx, probe.variant)];
        let prompt = render_prompt(case, probe.render, probe.paraphrase);
        let agg = elicit(
            providers[probe.model_idx].as_ref(),
            &prompt,
            args.k,
            args.concurrency,
        )
        .await;
        aggs.insert(probe_key(probe), agg);
        done += 1;
        if done % 25 == 0 || done == probes.len() {
            eprintln!("  [elicit] {done}/{} probes done", probes.len());
        }
    }

    // ── Score + emit ResultRows ──
    let rows = build_rows(&args, &probes, &aggs, &variant_case, &bands);

    let out_path = args.out.clone().unwrap_or_else(|| {
        PathBuf::from(format!("target/mechanism-fidelity/{}.jsonl", args.pool.as_str()))
    });
    if let Err(e) = write_jsonl(&out_path, &rows) {
        eprintln!("error: could not write {out_path:?}: {e}");
        return 1;
    }
    eprintln!("[out] wrote {} rows → {out_path:?}", rows.len());
    print_glassbox_summary(&args, &rows);
    0
}

/// Build the probe list. Base variant first within each context so the
/// scorer always has a reference. Full render carries all four variants
/// (and the paraphrase arm when enabled); the stripped control carries
/// only the DIR variants + base (an INV swap is visible to the control,
/// so it is not a control probe).
fn enumerate_probes(n_models: usize, n_cases: usize, paraphrase: bool) -> Vec<Probe> {
    let mut out = Vec::new();
    let full_variants = Variant::all();
    let control_variants = [Variant::Base, Variant::DirP1, Variant::DirP2];
    for model_idx in 0..n_models {
        for case_idx in 0..n_cases {
            // Full render, primary wording.
            for &v in &full_variants {
                out.push(Probe { model_idx, case_idx, variant: v, render: RenderMode::Full, paraphrase: false });
            }
            // Full render, paraphrase wording.
            if paraphrase {
                for &v in &full_variants {
                    out.push(Probe { model_idx, case_idx, variant: v, render: RenderMode::Full, paraphrase: true });
                }
            }
            // Stripped render — the negative control.
            for &v in &control_variants {
                out.push(Probe { model_idx, case_idx, variant: v, render: RenderMode::Stripped, paraphrase: false });
            }
        }
    }
    // Base-first ordering within each context is preserved by Variant::all()
    // listing Base first and control_variants starting with Base.
    out
}

fn probe_key(p: &Probe) -> (usize, usize, u8, bool, u8) {
    (p.model_idx, p.case_idx, p.render as u8, p.paraphrase, p.variant as u8)
}

/// Elicit one probe by K structured draws, aggregating to a
/// vote-frequency probability and a mean verbalized confidence.
async fn elicit(
    model: &dyn InferenceProvider,
    prompt: &str,
    k: u32,
    concurrency: usize,
) -> Agg {
    let schema = decision_schema();
    let draws: Vec<Option<(f64, f64, u64)>> = stream::iter(0..k)
        .map(|_| {
            let req = CompletionRequest {
                prompt: prompt.to_string(),
                system_message: Some(
                    "You are a careful economic analyst. Decide and respond with JSON only."
                        .to_string(),
                ),
                preferred_speed: Speed::Medium,
                max_tokens: Some(DRAW_MAX_TOKENS),
                temperature: Some(DRAW_TEMPERATURE),
                structured_output: Some(schema.clone()),
                think_budget: Some(0),
                enable_thinking: Some(false),
                ..Default::default()
            };
            async move {
                let start = Instant::now();
                match model.complete(&req).await {
                    Ok(resp) => parse_decision(&resp.text).map(|(vote, verbal)| {
                        (vote, verbal, start.elapsed().as_millis() as u64)
                    }),
                    Err(e) => {
                        eprintln!("    [draw] inference error: {e}");
                        None
                    }
                }
            }
        })
        .buffer_unordered(concurrency.min(k as usize).max(1))
        .collect()
        .await;

    let mut votes = 0.0f64;
    let mut verbal = 0.0f64;
    let mut eff = 0u32;
    let mut lat = 0u64;
    for d in draws.into_iter().flatten() {
        votes += d.0;
        verbal += d.1;
        lat += d.2;
        eff += 1;
    }
    if eff == 0 {
        return Agg { p_freq: f64::NAN, p_verbal: f64::NAN, eff_k: 0, latency_ms: 0 };
    }
    Agg {
        p_freq: votes / eff as f64,
        p_verbal: verbal / eff as f64,
        eff_k: eff,
        latency_ms: lat / eff as u64,
    }
}

/// Parse one draw → (relocate vote ∈ {0, .5, 1}, verbalized P(relocate)).
fn parse_decision(text: &str) -> Option<(f64, f64)> {
    let d: Decision = serde_json::from_str(text.trim()).ok()?;
    let conf = d.confidence.clamp(0.0, 1.0);
    match d.decision.to_lowercase().as_str() {
        "relocate" => Some((1.0, conf)),
        "stay" => Some((0.0, 1.0 - conf)),
        "indifferent" => Some((0.5, 0.5)),
        _ => None,
    }
}

fn decision_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "decision": {"type": "string", "enum": ["relocate", "stay", "indifferent"]},
            "confidence": {"type": "number", "minimum": 0, "maximum": 1}
        },
        "required": ["decision", "confidence"],
        "additionalProperties": false
    })
}

#[allow(clippy::type_complexity)]
fn build_rows(
    args: &Args,
    probes: &[Probe],
    aggs: &HashMap<(usize, usize, u8, bool, u8), Agg>,
    variant_case: &HashMap<(usize, Variant), (Case, f64)>,
    bands: &Bands,
) -> Vec<ResultRow> {
    // Reference (base) p_freq + structural p per context.
    let mut base_freq: HashMap<Ctx, f64> = HashMap::new();
    let mut base_struct: HashMap<usize, f64> = HashMap::new();
    for p in probes {
        if p.variant == Variant::Base {
            if let Some(a) = aggs.get(&probe_key(p)) {
                base_freq.insert(p.ctx(), a.p_freq);
            }
        }
    }
    for ci in 0..args.n_cases {
        if let Some((_, ps)) = variant_case.get(&(ci, Variant::Base)) {
            base_struct.insert(ci, *ps);
        }
    }

    let mut rows = Vec::with_capacity(probes.len());
    for p in probes {
        let Some(agg) = aggs.get(&probe_key(p)) else { continue };
        let (_, p_struct) = &variant_case[&(p.case_idx, p.variant)];
        let base_p = base_freq.get(&p.ctx()).copied().unwrap_or(f64::NAN);
        let base_s = base_struct.get(&p.case_idx).copied().unwrap_or(f64::NAN);

        let (d_agent, d_struct) = if p.variant == Variant::Base {
            (0.0, 0.0)
        } else {
            (agg.p_freq - base_p, p_struct - base_s)
        };
        let scores = score(p.variant.kind(), d_agent, d_struct, bands);

        rows.push(ResultRow {
            model_id: args.models[p.model_idx].clone(),
            case_id: variant_case[&(p.case_idx, p.variant)].0.id.clone(),
            pool: args.pool.as_str().to_string(),
            variant: p.variant.label().to_string(),
            render: p.render.label().to_string(),
            paraphrase: p.paraphrase,
            control: p.render.is_control(),
            expected_sign: p.variant.expected_sign(),
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
        });
    }
    rows
}

fn write_jsonl(path: &PathBuf, rows: &[ResultRow]) -> std::io::Result<()> {
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

/// A compact, glass-box read of the run printed to stderr — per model,
/// the mean full-render P1 collapse and the control's P1 movement, so a
/// leak (control showing sensitivity) is visible at a glance without the
/// Python sidecar.
fn print_glassbox_summary(args: &Args, rows: &[ResultRow]) {
    eprintln!("\n── mechanism-fidelity summary (pool={}) ──", args.pool.as_str());
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
        eprintln!(
            "  {m}: P1 collapse Δ̄={:+.3} (n={})  |  control P1 Δ̄={:+.3} (n={})  |  P2 |Δ̄|={:.3}  |  INV |Δ̄|={:.3}",
            mean(&p1_full),
            p1_full.len(),
            mean(&p1_ctrl),
            p1_ctrl.len(),
            mean(&p2_full),
            mean(&inv_full),
        );
    }
    eprintln!(
        "  (faithful: P1 Δ̄ strongly negative, control P1 Δ̄≈0, P2/INV |Δ̄| small. Read the\n   power-annotated verdict with sovereign/bench/mechanism_fidelity/verdict.py.)"
    );
}

fn mean(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return f64::NAN;
    }
    xs.iter().sum::<f64>() / xs.len() as f64
}

/// Load the `[bands]` table from the pre-registration manifest, falling
/// back to the doc defaults when the file is absent. Unknown/missing keys
/// inherit the default so a partial manifest still loads.
fn load_bands(path: &PathBuf) -> Bands {
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
    fn parse_decision_maps_choices() {
        assert_eq!(
            parse_decision("{\"decision\":\"relocate\",\"confidence\":0.8}"),
            Some((1.0, 0.8))
        );
        // 'stay' with 0.25 confidence-of-stay ⇒ P(relocate)=0.75.
        // (0.25/0.75 are exact in binary f64, so the equality is clean.)
        assert_eq!(
            parse_decision("{\"decision\":\"stay\",\"confidence\":0.25}"),
            Some((0.0, 0.75f64))
        );
        assert_eq!(
            parse_decision("{\"decision\":\"indifferent\",\"confidence\":0.3}"),
            Some((0.5, 0.5))
        );
        assert_eq!(parse_decision("not json"), None);
    }

    #[test]
    fn probe_matrix_shape() {
        // 1 model, 2 cases, paraphrase on:
        //   full×4 + full-para×4 + control×3 = 11 probes/case.
        let probes = enumerate_probes(1, 2, true);
        assert_eq!(probes.len(), 22);
        // Base must be the first probe of each context (scorer needs the
        // reference before the perturbations).
        assert_eq!(probes[0].variant, Variant::Base);
        // Without paraphrase: full×4 + control×3 = 7/case.
        assert_eq!(enumerate_probes(1, 2, false).len(), 14);
    }
}
