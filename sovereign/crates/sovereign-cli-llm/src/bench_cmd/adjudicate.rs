//! `svrn bench enrichment-adjudicate` — price the unmatched-atom volume (T1 P0.2).
//!
//! The eval scorer's `unmatched_count`/`unmatched_rate` (enrich_cmd/eval.rs)
//! say how much extraction earns no golden credit; this command answers the
//! question that number raises: how much of it is JUNK (page furniture,
//! malformed fragments, hallucination-shaped atoms) versus legitimate
//! extraction the golden simply doesn't cover? It samples N unmatched atoms
//! stratified across axes (deterministic — seeded hash order, no RNG state),
//! puts each to a primary-tier judge as a forced-choice A/B (SP3 economics:
//! primary-tier judging; the sample cap keeps a run in minutes), and writes
//! a calibration report: per-axis junk rate + projected junk volume
//! (`unmatched_count × junk_rate`).
//!
//! Deliberately NOT a gate: this is the measurement that makes over-
//! extraction a visible cost. Whether a threshold graduates into
//! `bench gate` is a later decision the calibration data itself funds.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use sovereign_core::traits::InferenceProvider;
use sovereign_inference::remote::RemoteApiProvider;

use sovereign_cli_shared::help::{self, Help, HelpSection};

use crate::enrich_cmd::eval::{collect_unmatched_atoms, load_golden_and_snapshot, UnmatchedAtom};

const PROVIDER_CTX: u32 = 8192;

/// Judge-failure ceiling: above this fraction of failed forced-choice
/// calls the junk rates are untrustworthy (503-burst / daemon wedge —
/// the SP3 Metal-OOM signature) and the run refuses to report them.
const MAX_JUDGE_FAILURE_RATE: f64 = 0.3;

const HELP: Help = Help {
    command: "svrn bench enrichment-adjudicate",
    summary: "Sample unmatched (zero-credit) extraction atoms and judge junk vs legitimate (T1 P0.2).",
    sections: &[
        HelpSection::Usage(
            "svrn bench enrichment-adjudicate <corpus-id> --golden <golden.toml> \
             [--sample N] [--seed N] [--model <id>] [--base-url <url>] [--output <json>]",
        ),
        HelpSection::Notes(
            "Recomputes the unmatched-atom set with the same predicates `enrich eval` \
             uses, stratified-samples --sample atoms (default 25) across axes in a \
             deterministic seeded order, and asks the judge per atom: legitimate \
             extraction, or junk? Prints the per-axis calibration table (junk rate + \
             projected junk volume per axis) and writes the full JSON artifact. Needs \
             the daemon at --base-url with the judge model resident.",
        ),
    ],
};

pub async fn cmd_adjudicate(rest: &[String]) -> i32 {
    if help::wants_help(rest) {
        help::print(&HELP);
        return 0;
    }
    run(rest).await
}

#[derive(Debug, Serialize)]
struct JudgedAtom {
    axis: String,
    kind: String,
    label: String,
    detail: String,
    evidence_previews: Vec<String>,
    /// `Some(true)` = junk, `Some(false)` = legitimate, `None` = judge failure.
    junk: Option<bool>,
    /// p(junk) − p(legitimate) from the forced-choice pass.
    margin: Option<f64>,
}

#[derive(Debug, Serialize)]
struct AxisCalibration {
    unmatched_total: usize,
    sampled: usize,
    junk: usize,
    judge_failures: usize,
    junk_rate: Option<f64>,
    /// `unmatched_total × junk_rate` — the projected junk volume this
    /// axis carries into the knowledge graph.
    projected_junk: Option<f64>,
}

#[derive(Debug, Serialize)]
struct Report {
    corpus_id: String,
    golden_path: String,
    judge_model: String,
    seed: u64,
    sample_target: usize,
    per_axis: BTreeMap<String, AxisCalibration>,
    overall_sampled: usize,
    overall_junk: usize,
    overall_junk_rate: Option<f64>,
    judged: Vec<JudgedAtom>,
}

/// Deterministic per-atom order key: FNV-1a over seed + axis + label.
/// Gives a stable, seed-steerable shuffle without RNG state.
fn order_key(seed: u64, atom: &UnmatchedAtom) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ seed;
    for b in atom.axis.bytes().chain(atom.label.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn judge_prompt(atom: &UnmatchedAtom) -> String {
    let evidence = if atom.evidence_previews.is_empty() {
        "(none recorded)".to_string()
    } else {
        atom.evidence_previews
            .iter()
            .map(|p| format!("- {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let detail = if atom.detail.is_empty() {
        String::new()
    } else {
        format!("Detail: {}\n", atom.detail)
    };
    format!(
        "An automated pipeline extracted this knowledge atom from a document corpus.\n\n\
         Kind: {kind} (axis: {axis})\n\
         Atom: {label}\n\
         {detail}\
         Source passage previews:\n{evidence}\n\n\
         Is this a well-formed, plausibly useful knowledge atom for the corpus, or \
         extraction junk (page furniture / boilerplate, a malformed fragment, a \
         trivial non-fact, or hallucination-shaped content unsupported by its own \
         evidence)?\n\
         A: legitimate atom\n\
         B: extraction junk\n",
        kind = atom.kind,
        axis = atom.axis,
        label = atom.label,
    )
}

async fn run(rest: &[String]) -> i32 {
    let mut corpus_arg: Option<String> = None;
    let mut golden: Option<PathBuf> = None;
    let mut sample: usize = 25;
    let mut seed: u64 = 17;
    let mut model = "primary".to_string();
    let mut base_url = "http://localhost:9741".to_string();
    let mut output: Option<PathBuf> = None;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            match rest.get(i).cloned() {
                Some(v) => v,
                None => {
                    eprintln!("error: {} requires a value", $l);
                    return 2;
                }
            }
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--golden" => golden = Some(PathBuf::from(val!("--golden"))),
            "--sample" => match val!("--sample").parse() {
                Ok(v) if v > 0 => sample = v,
                _ => {
                    eprintln!("error: --sample must be a positive integer");
                    return 2;
                }
            },
            "--seed" => match val!("--seed").parse() {
                Ok(v) => seed = v,
                _ => {
                    eprintln!("error: --seed must be a u64");
                    return 2;
                }
            },
            "--model" => model = val!("--model"),
            "--base-url" => base_url = val!("--base-url"),
            "--output" => output = Some(PathBuf::from(val!("--output"))),
            "--help" | "-h" => {
                help::print(&HELP);
                return 0;
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
            other => {
                if corpus_arg.is_some() {
                    eprintln!("error: unexpected argument `{other}`");
                    return 2;
                }
                corpus_arg = Some(other.to_string());
            }
        }
        i += 1;
    }
    let Some(corpus_id) = corpus_arg else {
        eprintln!("error: <corpus-id> is required");
        help::print(&HELP);
        return 2;
    };
    let Some(golden_path) = golden else {
        eprintln!("error: --golden <path> is required");
        help::print(&HELP);
        return 2;
    };

    let (golden_set, snapshot) = match load_golden_and_snapshot(&corpus_id, &golden_path) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let unmatched = collect_unmatched_atoms(&golden_set, &snapshot);
    if unmatched.is_empty() {
        println!("enrichment-adjudicate: {corpus_id} has no unmatched atoms — nothing to price.");
        return 0;
    }

    // Per-axis totals (the projection base), then a stratified sample:
    // axes sorted, atoms within an axis in seeded-hash order, round-
    // robin draw until the target. Every axis with volume gets at
    // least one draw before any axis gets its second.
    let mut per_axis_pool: BTreeMap<String, Vec<&UnmatchedAtom>> = BTreeMap::new();
    for a in &unmatched {
        per_axis_pool.entry(a.axis.clone()).or_default().push(a);
    }
    for pool in per_axis_pool.values_mut() {
        pool.sort_by_key(|a| order_key(seed, a));
    }
    let mut sampled: Vec<&UnmatchedAtom> = Vec::new();
    let mut depth = 0usize;
    while sampled.len() < sample {
        let mut drew = false;
        for pool in per_axis_pool.values() {
            if sampled.len() >= sample {
                break;
            }
            if let Some(a) = pool.get(depth) {
                sampled.push(a);
                drew = true;
            }
        }
        if !drew {
            break; // every pool exhausted
        }
        depth += 1;
    }

    // Concrete judge stem, never an alias (P0.1's dead-model lesson).
    let judge_stem = match super::model_resolve::resolve_model_attribution(&base_url, &model).await
    {
        Some(attr) => attr.file_stem,
        None if model.chars().any(|c| c.is_ascii_digit()) => model.clone(),
        None => {
            eprintln!(
                "error: could not resolve judge alias `{model}` against {base_url} — \
                 refusing to report junk rates with an unknown judge"
            );
            return 1;
        }
    };
    eprintln!(
        "enrichment-adjudicate: corpus={corpus_id} unmatched={} sampled={} judge={judge_stem}",
        unmatched.len(),
        sampled.len(),
    );

    let v1 = format!("{}/v1", base_url.trim_end_matches('/'));
    let provider: Arc<dyn InferenceProvider> =
        Arc::new(RemoteApiProvider::new(&v1, None, &model, PROVIDER_CTX));

    let mut judged: Vec<JudgedAtom> = Vec::new();
    let mut failures = 0usize;
    for (n, atom) in sampled.iter().enumerate() {
        let verdict =
            super::live_runner::forced_choice_ab(provider.as_ref(), &model, &judge_prompt(atom))
                .await;
        let (junk, margin) = match verdict {
            Some((p_a, p_b)) => (Some(p_b > p_a), Some(p_b - p_a)),
            None => {
                failures += 1;
                (None, None)
            }
        };
        judged.push(JudgedAtom {
            axis: atom.axis.clone(),
            kind: atom.kind.clone(),
            label: atom.label.clone(),
            detail: atom.detail.clone(),
            evidence_previews: atom.evidence_previews.clone(),
            junk,
            margin,
        });
        if (n + 1) % 10 == 0 {
            eprintln!("  … {}/{} judged", n + 1, sampled.len());
        }
    }

    let failure_rate = failures as f64 / sampled.len().max(1) as f64;
    if failure_rate > MAX_JUDGE_FAILURE_RATE {
        eprintln!(
            "error: {failures}/{} judge calls failed — rates untrustworthy. A 503-burst \
             here usually means the daemon's Metal backend wedged (SP3 incident) — \
             restart the daemon and re-run.",
            sampled.len()
        );
        return 5;
    }

    let mut per_axis: BTreeMap<String, AxisCalibration> = BTreeMap::new();
    for (axis, pool) in &per_axis_pool {
        let rows: Vec<&JudgedAtom> = judged.iter().filter(|j| &j.axis == axis).collect();
        let decided: Vec<&&JudgedAtom> = rows.iter().filter(|j| j.junk.is_some()).collect();
        let junk = decided.iter().filter(|j| j.junk == Some(true)).count();
        let junk_rate = if decided.is_empty() {
            None
        } else {
            Some(junk as f64 / decided.len() as f64)
        };
        per_axis.insert(
            axis.clone(),
            AxisCalibration {
                unmatched_total: pool.len(),
                sampled: rows.len(),
                junk,
                judge_failures: rows.len() - decided.len(),
                junk_rate,
                projected_junk: junk_rate.map(|r| r * pool.len() as f64),
            },
        );
    }
    let decided_total = judged.iter().filter(|j| j.junk.is_some()).count();
    let junk_total = judged.iter().filter(|j| j.junk == Some(true)).count();
    let overall_junk_rate = if decided_total == 0 {
        None
    } else {
        Some(junk_total as f64 / decided_total as f64)
    };

    println!();
    println!("  Over-extraction calibration — {corpus_id}");
    println!("  ───────────────────────────────────────────────────────────────");
    println!("  axis              unmatched  sampled  junk   junk-rate  projected-junk");
    for (axis, c) in &per_axis {
        println!(
            "  {axis:<16}  {:>9}  {:>7}  {:>4}   {:>9}  {:>14}",
            c.unmatched_total,
            c.sampled,
            c.junk,
            c.junk_rate
                .map(|r| format!("{:.0}%", r * 100.0))
                .unwrap_or_else(|| "—".into()),
            c.projected_junk
                .map(|p| format!("{p:.1}"))
                .unwrap_or_else(|| "—".into()),
        );
    }
    println!("  ───────────────────────────────────────────────────────────────");
    println!(
        "  overall: {junk_total}/{decided_total} sampled atoms judged junk ({}){}",
        overall_junk_rate
            .map(|r| format!("{:.0}%", r * 100.0))
            .unwrap_or_else(|| "n/a".into()),
        if failures > 0 {
            format!(" — {failures} judge failure(s)")
        } else {
            String::new()
        }
    );
    for j in judged.iter().filter(|j| j.junk == Some(true)).take(5) {
        println!("    junk sample [{}/{}]: {}", j.axis, j.kind, j.label);
    }

    let report = Report {
        corpus_id: corpus_id.clone(),
        golden_path: golden_path.display().to_string(),
        judge_model: judge_stem,
        seed,
        sample_target: sample,
        per_axis,
        overall_sampled: decided_total,
        overall_junk: junk_total,
        overall_junk_rate,
        judged,
    };
    let out_path = output
        .unwrap_or_else(|| PathBuf::from(format!("target/ci-bench/adjudicate-{corpus_id}.json")));
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&out_path, serde_json::to_string_pretty(&report).unwrap()) {
        Ok(_) => println!("\n  ✓ wrote {}", out_path.display()),
        Err(e) => {
            eprintln!("error: writing {}: {e}", out_path.display());
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(axis: &str, label: &str) -> UnmatchedAtom {
        UnmatchedAtom {
            axis: axis.to_string(),
            kind: "Entity".to_string(),
            label: label.to_string(),
            detail: String::new(),
            evidence_chunk_ids: vec![],
            evidence_previews: vec![],
        }
    }

    #[test]
    fn order_key_is_deterministic_and_seed_steerable() {
        let a = atom("claim", "the same atom");
        assert_eq!(order_key(17, &a), order_key(17, &a));
        assert_ne!(order_key(17, &a), order_key(18, &a));
    }

    #[test]
    fn judge_prompt_handles_missing_evidence_and_detail() {
        let a = atom("person", "Copyright 2024 The Publisher");
        let p = judge_prompt(&a);
        assert!(p.contains("(none recorded)"));
        assert!(p.contains("A: legitimate atom"));
        assert!(!p.contains("Detail:"));
    }
}
