// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which embedding INSTRUCTION should the router's classifier axes use?
//!
//! ## Why this exists (2026-08-04)
//!
//! The intent axis owned 3 of 40 calibration cases — 7.5% coverage at
//! 100% precision — and `router fit --objective max-coverage
//! --min-precision 0.85` found NOTHING better across 1640 candidate
//! gates. It was never an operating-point problem, so the geometry was
//! measured instead:
//!
//!   * leave-one-out 1-NN ON THE EXEMPLAR BANK ITSELF: ~60% — the scorer
//!     could not classify its own hand-authored training data.
//!   * margin carried NEGATIVE information: mean margin when correct was
//!     0.0029 BELOW mean margin when wrong.
//!   * between-class scatter was 12.3% of total variance.
//!
//! Cause: exemplars are embedded through `embed_query`, which applies
//! Qwen3-Embedding's shipped instruction — *"Given a search query,
//! retrieve relevant passages that answer the query"* — a RETRIEVAL
//! instruction, on an instruction-FOLLOWING model, whose output we then
//! used to classify SPEECH ACT. Topic and speech act are near-orthogonal.
//!
//! A first pass showed a speech-act instruction moving LOO 1-NN
//! 60.6% -> 76.5% and the scatter ratio 0.140 -> 0.437. This harness
//! exists to turn that hint into a decision, because LOO accuracy is a
//! PROXY and two questions it cannot answer are the ones that matter.
//!
//! ## The three questions it answers, with the metric for each
//!
//! 1. WHICH INSTRUCTION? Not by LOO, but by the product metric: how much
//!    of a HELD-OUT bank can the axis own at a precision floor. Scored
//!    with `router_calibration::fit` — the SAME sweep `router fit` runs,
//!    never a reimplementation of it (ARCH_PRINCIPLES §10.6).
//!
//! 2. ACCURACY OR GATEABILITY? `speech-act` won on LOO accuracy while
//!    `intent-request` won on margin separation. Those are different
//!    halves: accuracy is whether the RANKING is right, separation is
//!    whether the GATE can tell. Both are reported per candidate, beside
//!    the coverage that actually results.
//!
//! 3. ONE INSTRUCTION OR TWO? `router.rs:1888` and `:1898` share ONE
//!    query embedding across the intent, scope and archive axes, and
//!    `embed_query` is the same call corpus RETRIEVAL uses. So either
//!    every axis moves to one new instruction (one embed call, four
//!    recalibrations) or the router gets a dedicated one (a second embed
//!    call per turn, ~50ms). That is only decidable by measuring what a
//!    router-tuned instruction does to the OTHER axes — so all five are
//!    reported per candidate.
//!
//! ## What it does NOT do
//!
//! It writes nothing and changes nothing. It does not touch the
//! committed `router-embed-cache.json`; every embedding here is computed
//! fresh, because the cache holds vectors under the SHIPPED instruction
//! and reusing them would silently measure the baseline five times.
//!
//! Run with:
//!
//!   cargo run -p sovereign-cli-llm --example intent_instruction_probe -- \
//!       --model ~/.svrnmesh/models/Qwen3-Embedding-0.6B-Q8_0.gguf \
//!       [--min-precision 0.90] [--router-only]

use std::collections::HashMap;
use std::path::PathBuf;

use sovereign_core::model_family::ModelFamily;
use sovereign_core::router_axis::{AxisGate, AxisScore};
use sovereign_core::router_calibration::{evaluate, fit, parse_bank, Objective, ScoredCase};
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::embedded::EmbedOnlyProvider;

/// Candidate instructions, each a hypothesis about what the vector
/// should encode.
///
/// `shipped-retrieval` MUST stay first and unchanged: every run reports
/// the live baseline beside the alternatives, so a candidate that only
/// looks good in isolation cannot be mistaken for an improvement.
const CANDIDATES: &[(&str, &str)] = &[
    (
        "shipped-retrieval",
        "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: ",
    ),
    ("none", ""),
    (
        "speech-act",
        "Instruct: Classify the speech act of the user's message — what the speaker is DOING with these words, not what they are about\nMessage: ",
    ),
    (
        "intent-request",
        "Instruct: Identify what the user wants the assistant to do in response to this message, ignoring the message's subject matter\nMessage: ",
    ),
    (
        "move-not-topic",
        "Instruct: Represent the conversational move the speaker is making. Two messages about completely different subjects that make the same move should be similar\nMessage: ",
    ),
    // Blends of the two front-runners: `speech-act` led on ranking
    // accuracy, `intent-request` on margin separation. If those are
    // genuinely different halves, a phrasing carrying both should beat
    // each — and if no blend does, that is evidence they trade off and
    // the choice is a real one rather than a wording accident.
    (
        "act-and-request",
        "Instruct: Identify the speech act the user is performing and what they want done about it. Ignore the subject matter entirely — two messages on unrelated topics performing the same act should be similar\nMessage: ",
    ),
    (
        "act-terse",
        "Instruct: What is the user doing with this message?\nMessage: ",
    ),
    // Names the actual label set. Instruction-following embedders often
    // sharpen considerably when the target classes are enumerated; if
    // this wins big it also means the instruction must be kept in sync
    // with the Intent enum, which is a maintenance cost worth knowing
    // about BEFORE it is chosen.
    (
        "act-enumerated",
        "Instruct: Classify the user's message as one of: a factual lookup, a request for deep reasoning, a comparison, a question about our shared vocabulary, a question about code structure, an instruction to the assistant, a commitment the user is making, an emotional disclosure, a creative request, a multi-step task, or small talk. Encode which one, not the topic\nMessage: ",
    ),
];

/// The four non-intent classifier axes, each a one-vs-rest centroid over
/// a `[label] examples = [...]` TOML. Measured because question 3 above
/// turns entirely on whether a router-tuned instruction damages them.
const BINARY_AXES: &[(&str, &str)] = &[
    ("scope", "sovereign/router/scope_examples.toml"),
    ("effort", "sovereign/router/effort_examples.toml"),
    (
        "current_info",
        "sovereign/router/current_info_examples.toml",
    ),
    ("archive", "sovereign/router/archive_examples.toml"),
];

const BANKS: &[(&str, &str)] = &[
    (
        "axes_v1",
        "sovereign/bench/routing/calibration/axes_v1.toml",
    ),
    (
        "holdout",
        "sovereign/bench/routing/calibration/holdout/intent_frames_v1.toml",
    ),
];

struct Labelled {
    text: String,
    label: String,
}

fn main() {
    let mut model: Option<PathBuf> = None;
    let mut min_precision = 0.90f64;
    let mut router_only = false;
    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--model" => {
                model = args.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            "--min-precision" => {
                min_precision = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(0.90);
                i += 2;
            }
            "--router-only" => {
                router_only = true;
                i += 1;
            }
            other => {
                eprintln!("unknown arg {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(model) = model else {
        eprintln!("intent_instruction_probe: --model <embed-gguf> is required");
        std::process::exit(2);
    };

    let intent_rows = parse_intent_exemplars(
        &std::fs::read_to_string("sovereign/router/exemplars.toml").expect("read exemplars"),
    );
    let banks: Vec<(&str, Vec<(String, String, Option<String>)>)> = BANKS
        .iter()
        .filter_map(|(name, path)| {
            let raw = std::fs::read_to_string(path).ok()?;
            let bank = parse_bank(&raw).ok()?;
            let cases = bank
                .case
                .into_iter()
                .filter(|c| c.axis == "intent")
                .map(|c| {
                    let expect = c.expected_label().map(String::from);
                    (c.id, c.query, expect)
                })
                .collect();
            Some((*name, cases))
        })
        .collect();

    println!(
        "intent_instruction_probe — {} intent exemplars, {} held-out bank(s), \
         precision floor {:.0}%\n\
         model {}\n",
        intent_rows.len(),
        banks.len(),
        min_precision * 100.0,
        model.display()
    );

    let provider =
        EmbedOnlyProvider::load(&model, ModelFamily::Qwen3Embedding).expect("load embed model");
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let embed = |instruction: &str, texts: &[String]| -> Vec<Vec<f32>> {
        let prefixed: Vec<String> = texts.iter().map(|t| format!("{instruction}{t}")).collect();
        let mut v = rt
            .block_on(provider.embed_batch(&prefixed))
            .expect("embed batch");
        for x in v.iter_mut() {
            normalize(x);
        }
        v
    };

    // ── Q1/Q2: the intent axis, scored on HELD-OUT banks ──────────
    println!("INTENT AXIS — diagnostics on the exemplar bank, verdict on the held-out banks");
    println!(
        "{:<18} {:>8} {:>9} {:>9} {:>10} {:>22}",
        "instruction", "LOO", "scatter", "marginSep", "bank acc", "coverage@precision"
    );
    println!("{}", "-".repeat(82));

    let ex_texts: Vec<String> = intent_rows.iter().map(|r| r.text.clone()).collect();
    let ex_labels: Vec<&str> = intent_rows.iter().map(|r| r.label.as_str()).collect();

    // Scored cases per candidate per bank, kept so the cross-bank
    // validation below can fit on one and evaluate on the other without
    // re-embedding anything.
    let mut scored_by_candidate: Vec<(&str, Vec<(&str, Vec<ScoredCase>)>)> = Vec::new();

    for (name, instruction) in CANDIDATES {
        let ex_vecs = embed(instruction, &ex_texts);
        let loo = loo_1nn(&ex_vecs, &ex_labels);
        let scatter = scatter_ratio(&ex_vecs, &ex_labels);

        let mut cov = Vec::new();
        let mut accs = Vec::new();
        let mut per_bank: Vec<(&str, Vec<ScoredCase>)> = Vec::new();
        for (bank_name, cases) in &banks {
            let texts: Vec<String> = cases.iter().map(|(_, q, _)| q.clone()).collect();
            let qvecs = embed(instruction, &texts);
            let scored: Vec<ScoredCase> = cases
                .iter()
                .zip(&qvecs)
                .map(|((id, _, expect), qv)| {
                    let (pred, top, second) = rank(qv, &ex_vecs, &ex_labels);
                    ScoredCase {
                        id: id.clone(),
                        score: AxisScore::new(top, second),
                        expect: expect.clone(),
                        predicted: Some(pred.to_string()),
                        nearest: None,
                        rival: None,
                    }
                })
                .collect();

            let positives: Vec<&ScoredCase> =
                scored.iter().filter(|c| c.expect.is_some()).collect();
            let right = positives
                .iter()
                .filter(|c| c.predicted.as_deref() == c.expect.as_deref())
                .count();
            accs.push(format!("{bank_name} {right}/{}", positives.len()));

            // Same sweep `router fit` runs. The shipped gate is passed
            // only as the report's reference point; the number we read
            // is `best` under a precision floor.
            let report = fit(
                &scored,
                AxisGate::new(0.55, 0.206),
                Objective::MaxCoverage { min_precision },
            );
            // `GateOutcome::coverage`/`precision` return FRACTIONS, not
            // percentages — scale here, once, at the print site.
            let cell = match report.and_then(|r| r.best) {
                Some(b) => format!(
                    "{bank_name} {:.0}%@{:.0}%",
                    100.0 * b.coverage(),
                    100.0 * b.precision()
                ),
                None => format!("{bank_name} none"),
            };
            cov.push(cell);
            per_bank.push((bank_name, scored));
        }
        scored_by_candidate.push((name, per_bank));

        println!(
            "{name:<18} {:>7.1}% {:>9.3} {:>9.4} {:>10} {:>22}",
            100.0 * loo.accuracy,
            scatter,
            loo.margin_separation,
            accs.join(" "),
            cov.join("  ")
        );
    }

    // ── CROSS-BANK VALIDATION — the only shippable number ──────────
    //
    // Every figure above chose its gate ON the bank it reports, which is
    // in-sample selection and always flatters. A gate is only worth
    // shipping if the constants picked on one bank still hold on a bank
    // they never saw. That is what the router will actually experience,
    // because production queries are in neither bank.
    println!(
        "\nCROSS-BANK VALIDATION — gate FITTED on one bank, EVALUATED on the other.\n\
         This is the shippable number; everything above is in-sample and optimistic."
    );
    println!(
        "\n{:<18} {:>34} {:>34}",
        "instruction", "fit axes_v1 -> eval holdout", "fit holdout -> eval axes_v1"
    );
    println!("{}", "-".repeat(88));

    for (name, per_bank) in &scored_by_candidate {
        let mut cells = Vec::new();
        for (fit_on, eval_on) in [("axes_v1", "holdout"), ("holdout", "axes_v1")] {
            let src = per_bank.iter().find(|(b, _)| *b == fit_on);
            let dst = per_bank.iter().find(|(b, _)| *b == eval_on);
            let cell = match (src, dst) {
                (Some((_, sc)), Some((_, dc))) => {
                    let chosen = fit(
                        sc,
                        AxisGate::new(0.55, 0.206),
                        Objective::MaxCoverage { min_precision },
                    )
                    .and_then(|r| r.best)
                    .map(|b| b.gate());
                    match chosen {
                        Some(g) => {
                            let o = evaluate(dc, g);
                            format!(
                                "cov {:>3.0}% prec {:>3.0}% (sim>={:.3} m>={:.3})",
                                100.0 * o.coverage(),
                                100.0 * o.precision(),
                                g.min_sim,
                                g.min_margin
                            )
                        }
                        None => "no feasible gate".to_string(),
                    }
                }
                _ => "bank missing".to_string(),
            };
            cells.push(cell);
        }
        println!("{name:<18} {:>34} {:>34}", cells[0], cells[1]);
    }

    if router_only {
        return;
    }

    // ── Q3: does a router-tuned instruction damage the other axes? ──
    println!(
        "\nOTHER AXES — LOO 1-NN / scatter under each instruction.\n\
         These four ride the SAME query embedding as the intent axis\n\
         (router.rs:1888,1898), so a shared instruction is only viable if\n\
         they hold up here. A drop means the router needs its own embed call."
    );
    print!("\n{:<18}", "instruction");
    for (axis, _) in BINARY_AXES {
        print!(" {axis:>18}");
    }
    println!();
    println!("{}", "-".repeat(18 + 19 * BINARY_AXES.len()));

    let axis_data: Vec<(&str, Vec<Labelled>)> = BINARY_AXES
        .iter()
        .filter_map(|(name, path)| {
            let raw = std::fs::read_to_string(path).ok()?;
            Some((*name, parse_labelled_examples(&raw)))
        })
        .collect();

    for (name, instruction) in CANDIDATES {
        print!("{name:<18}");
        for (_, rows) in &axis_data {
            let texts: Vec<String> = rows.iter().map(|r| r.text.clone()).collect();
            let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
            let v = embed(instruction, &texts);
            let l = loo_1nn(&v, &labels);
            let s = scatter_ratio(&v, &labels);
            print!(" {:>11.1}%/{:.2}", 100.0 * l.accuracy, s);
        }
        println!();
    }

    println!(
        "\nREADING THIS: `coverage@precision` is the decision — how much of a\n\
         held-out bank the axis can OWN while holding the precision floor.\n\
         `LOO` and `scatter` are diagnostics on training data and will always\n\
         look better than the banks; do not quote them as the result.\n\
         `marginSep` near zero (or negative) means NO gate can work at any\n\
         threshold, which was the original defect."
    );
}

struct Loo {
    accuracy: f64,
    margin_separation: f32,
}

/// Rank every class by max cosine to the query. Returns
/// (winning label, its similarity, runner-up similarity).
fn rank<'a>(q: &[f32], vecs: &[Vec<f32>], labels: &[&'a str]) -> (&'a str, f32, f32) {
    let mut best: HashMap<&str, f32> = HashMap::new();
    for (v, l) in vecs.iter().zip(labels) {
        let s = dot(q, v);
        best.entry(l)
            .and_modify(|b| {
                if s > *b {
                    *b = s
                }
            })
            .or_insert(s);
    }
    let mut ranked: Vec<(&str, f32)> = best.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top = ranked[0];
    let second = ranked.get(1).map(|r| r.1).unwrap_or(0.0);
    // Re-borrow the label from `labels` so the lifetime outlives the map.
    let winner = labels
        .iter()
        .find(|l| **l == top.0)
        .expect("winning label came from labels");
    (winner, top.1, second)
}

/// Leave-one-out 1-NN over a labelled set: the CEILING for a k=1 scorer
/// on its own training data.
fn loo_1nn(vecs: &[Vec<f32>], labels: &[&str]) -> Loo {
    let n = vecs.len();
    let mut correct = 0usize;
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    for i in 0..n {
        let mut best: HashMap<&str, f32> = HashMap::new();
        for j in 0..n {
            if i == j {
                continue;
            }
            let s = dot(&vecs[i], &vecs[j]);
            best.entry(labels[j])
                .and_modify(|b| {
                    if s > *b {
                        *b = s
                    }
                })
                .or_insert(s);
        }
        let mut ranked: Vec<(&str, f32)> = best.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if ranked.is_empty() {
            continue;
        }
        let margin = ranked[0].1 - ranked.get(1).map(|r| r.1).unwrap_or(0.0);
        if ranked[0].0 == labels[i] {
            correct += 1;
            ok.push(margin);
        } else {
            bad.push(margin);
        }
    }
    let mean = |v: &Vec<f32>| {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f32>() / v.len() as f32
        }
    };
    Loo {
        accuracy: correct as f64 / n.max(1) as f64,
        margin_separation: mean(&ok) - mean(&bad),
    }
}

/// Between-class scatter over within-class scatter — the quantity a
/// linear discriminant maximises, so it estimates how much of the
/// variance is actually the label.
fn scatter_ratio(vecs: &[Vec<f32>], labels: &[&str]) -> f64 {
    let n = vecs.len();
    let dim = vecs[0].len();
    let mut mu = vec![0.0f64; dim];
    for v in vecs {
        for k in 0..dim {
            mu[k] += v[k] as f64;
        }
    }
    for m in mu.iter_mut() {
        *m /= n as f64;
    }
    let mut groups: HashMap<&str, Vec<&Vec<f32>>> = HashMap::new();
    for (v, l) in vecs.iter().zip(labels) {
        groups.entry(l).or_default().push(v);
    }
    let (mut between, mut within) = (0.0f64, 0.0f64);
    for (_, xs) in groups {
        let m = xs.len();
        let mut mc = vec![0.0f64; dim];
        for v in &xs {
            for k in 0..dim {
                mc[k] += v[k] as f64;
            }
        }
        for c in mc.iter_mut() {
            *c /= m as f64;
        }
        between += m as f64 * (0..dim).map(|k| (mc[k] - mu[k]).powi(2)).sum::<f64>();
        for v in &xs {
            within += (0..dim).map(|k| (v[k] as f64 - mc[k]).powi(2)).sum::<f64>();
        }
    }
    between / within.max(f64::EPSILON)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

/// `[[example]] intent/query` reader. Deliberately NOT the production
/// parser: this probe only needs (label, text) and must keep working if
/// the router's schema gains fields.
fn parse_intent_exemplars(raw: &str) -> Vec<Labelled> {
    #[derive(serde::Deserialize)]
    struct File {
        #[serde(default)]
        example: Vec<Ex>,
    }
    #[derive(serde::Deserialize)]
    struct Ex {
        intent: String,
        query: String,
    }
    let f: File = toml::from_str(raw).expect("parse exemplars.toml");
    f.example
        .into_iter()
        .map(|e| Labelled {
            text: e.query,
            label: e.intent,
        })
        .collect()
}

/// `[label] examples = [...]` reader — the shape every non-intent axis
/// uses (scope, effort, current_info, archive).
fn parse_labelled_examples(raw: &str) -> Vec<Labelled> {
    #[derive(serde::Deserialize)]
    struct Group {
        #[serde(default)]
        examples: Vec<String>,
    }
    let map: HashMap<String, Group> = toml::from_str(raw).expect("parse labelled examples");
    let mut out = Vec::new();
    for (label, g) in map {
        for text in g.examples {
            out.push(Labelled {
                text,
                label: label.clone(),
            });
        }
    }
    out
}
