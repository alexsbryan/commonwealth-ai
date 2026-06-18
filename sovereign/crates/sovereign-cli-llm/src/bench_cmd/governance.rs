// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign bench governance` — FR-9 Lane A: governance tension
//! detector precision/recall against an exhaustive truth manifest.
//!
//! Reads the corpus's enriched atlas (`atoms.json` + `edges.json`) + the
//! chapter manifest, maps each `EdgeType::Tension` edge to the pair of
//! source sections it connects, and scores against the recipe's
//! `truth.json` via the pure `sovereign_eval::governance_bench` scorer.
//!
//! - `run`      prints the report (precision/recall/F1, per type) and,
//!              with `--out`, writes the `DetectorReport` JSON the
//!              `bench gate governance` lane consumes.
//! - `diagnose` adds the glassbox: per-tension hit/miss + every flagged
//!              decoy and unlabeled false positive.
//! - `qa`       drives the FR-9 Lane B Q&A bench: the chaos two-red-line
//!              path over the governance corpus. Because the corpus carries
//!              a `governance_oplog.jsonl`, the live turns automatically get
//!              the active-set retrieval filter (dead law dropped) + the
//!              `GateSurface::Governance` cite-or-abstain gate, so the bank's
//!              SupersededTrap rows measure RL-3 (no dead law) alongside
//!              RL-1 (no confabulation) / RL-2 (honest abstention).
//!
//! This lane is the *tracked* (advisory absolute-verdict) half; the
//! paired hard `gate` lane re-reads the artifact and fails only on
//! regression vs the committed baseline (the chaos/mechanism pattern).

use std::collections::HashMap;
use std::path::Path;

use corpus_engine::enrichment::atlas::{
    edges::EdgeType, read_atlas_atoms, read_atlas_edges, AtomEnvelope, ATLAS_DIRNAME,
};
use serde::Deserialize;
use sovereign_cli_shared::help::{self, Help, HelpSection};
use sovereign_eval::governance_bench::{
    score_detector, DetectorReport, GovernanceTruth, PairKey, SectionKey, Split, ALL_SPLITS,
};

const HELP: Help = Help {
    command: "sovereign bench governance",
    summary: "Governance tension-detector precision/recall vs an exhaustive truth manifest (FR-9 Lane A).",
    sections: &[
        HelpSection::Usage(
            "sovereign bench governance <run|diagnose> <corpus-id> [--truth <path>] [--split <test|all>] [--out <report.json>]",
        ),
        HelpSection::Flags(&[
            (
                "--truth <path>",
                "Ground-truth manifest. Default: ~/.sovereign/recipes/<corpus-id>/truth.json.",
            ),
            (
                "--split <test|all>",
                "Which split to score recall over. Default: test (the gated metric); `all` scores every split.",
            ),
            (
                "--out <path>",
                "Write the DetectorReport JSON (consumed by `bench gate governance`).",
            ),
        ]),
        HelpSection::Examples(&[
            (
                "sovereign bench governance run maple-house --split test --out target/gov.json",
                "Score the detector on the test split; emit the gate artifact.",
            ),
            (
                "sovereign bench governance diagnose maple-house --split all",
                "Per-tension hit/miss + per-decoy false-positive listing.",
            ),
            (
                "sovereign bench governance qa maple-house --out target/gov-qa.jsonl",
                "Lane B: run the QA chaos bank over the governance corpus (active-set + governance gate apply); emit ResultRow JSONL for `bench gate governance-qa`.",
            ),
        ]),
    ],
};

/// FR-9 Lane B: the Q&A bench. Delegates to the chaos two-red-line runner
/// over the governance corpus. The corpus's `governance_oplog.jsonl` makes
/// the live turns governance turns (active-set filter + the
/// `GateSurface::Governance` gate), so the bank's SupersededTrap rows
/// exercise RL-3 (dead law) on top of RL-1/RL-2 — no bespoke orchestrator,
/// the chaos scorer already computes `cited_obsolete` for those rows.
///
/// `<corpus-id>` is the first argument; bank / manifest / out default to the
/// committed governance fixture but any chaos flag (e.g. `--limit`,
/// `--judge-model`, `--naked`) passes straight through.
async fn qa(args: &[String]) -> i32 {
    let Some((corpus, rest)) = args.split_first() else {
        eprintln!("error: usage: sovereign bench governance qa <corpus-id> [--bank <t>] [--manifest <t>] [--out <jsonl>] [chaos flags]");
        return 2;
    };
    if corpus.starts_with("--") {
        eprintln!("error: the first argument to `qa` must be the corpus id, not a flag");
        return 2;
    }
    let present = |flag: &str| rest.iter().any(|a| a == flag);
    let mut chaos: Vec<String> = vec!["run".into(), "--corpus".into(), corpus.clone()];
    if !present("--bank") {
        chaos.push("--bank".into());
        chaos.push("sovereign/bench/governance/maple_house.toml".into());
    }
    if !present("--manifest") {
        chaos.push("--manifest".into());
        chaos.push("sovereign/bench/governance/manifest.toml".into());
    }
    if !present("--out") {
        chaos.push("--out".into());
        chaos.push("target/governance-qa/results.jsonl".into());
    }
    // Measure the SAME hardened turn `govern ask` ships: pin the intent to a
    // factual lookup (governance Qs never need the router) + carry the
    // governance answering discipline. Lane defaults; overridable for A/Bs.
    if !present("--pin-intent") {
        chaos.push("--pin-intent".into());
        chaos.push("knowledge_query".into());
    }
    if !present("--custom-instructions") {
        chaos.push("--custom-instructions".into());
        chaos.push(crate::govern_cmd::ask::GOVERN_ASK_DISCIPLINE.to_string());
    }
    chaos.extend(rest.iter().cloned());
    eprintln!(
        "[governance qa] → chaos-monkey over `{corpus}` (active-set filter + GateSurface::Governance apply: the corpus carries a governance oplog)"
    );
    super::chaos_monkey::cmd_chaos_monkey(&chaos).await
}

pub async fn cmd_governance(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    match args.first().map(|s| s.as_str()) {
        Some("run") => run(&args[1..], false),
        Some("diagnose") => run(&args[1..], true),
        Some("qa") => qa(&args[1..]).await,
        Some(other) => {
            eprintln!("error: unknown governance subcommand `{other}`");
            help::print(&HELP);
            2
        }
        None => {
            help::print(&HELP);
            1
        }
    }
}

struct ParsedArgs {
    corpus_id: String,
    truth: Option<String>,
    splits: Vec<Split>,
    out: Option<String>,
}

fn parse(args: &[String]) -> Result<ParsedArgs, String> {
    let mut corpus_id = None;
    let mut truth = None;
    let mut splits = vec![Split::Test];
    let mut out = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--truth" => {
                truth = Some(args.get(i + 1).ok_or("--truth needs a path")?.clone());
                i += 2;
            }
            "--split" => {
                let v = args.get(i + 1).ok_or("--split needs a value")?;
                splits = match v.as_str() {
                    "test" => vec![Split::Test],
                    "dev" => vec![Split::Dev],
                    "train" => vec![Split::Train],
                    "all" => ALL_SPLITS.to_vec(),
                    other => return Err(format!("unknown split `{other}`")),
                };
                i += 2;
            }
            "--out" => {
                out = Some(args.get(i + 1).ok_or("--out needs a path")?.clone());
                i += 2;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag {other}")),
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else {
                    return Err(format!("unexpected argument {other}"));
                }
                i += 1;
            }
        }
    }
    Ok(ParsedArgs {
        corpus_id: corpus_id.ok_or("missing <corpus-id>")?,
        truth,
        splits,
        out,
    })
}

fn run(args: &[String], diagnose: bool) -> i32 {
    let parsed = match parse(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            help::print(&HELP);
            return 2;
        }
    };

    let index_root = crate::enrich_cmd::paths::index_root(&parsed.corpus_id);
    let detected = match load_detected(&index_root) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: reading enriched atlas for `{}`: {e}", parsed.corpus_id);
            eprintln!("  run `sovereign enrich build {} --full` first.", parsed.corpus_id);
            return 1;
        }
    };

    let truth_path = parsed.truth.clone().map(std::path::PathBuf::from).unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".sovereign/recipes")
            .join(&parsed.corpus_id)
            .join("truth.json")
    });
    let truth = match GovernanceTruth::load(&truth_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: reading {}: {e}", truth_path.display());
            return 1;
        }
    };

    let report = score_detector(&truth, &detected, &parsed.splits);
    print_report(&report, &parsed.corpus_id, &parsed.splits);
    if diagnose {
        print_diagnose(&report);
    }

    if let Some(out) = &parsed.out {
        match serde_json::to_vec_pretty(&report)
            .map_err(|e| e.to_string())
            .and_then(|b| std::fs::write(out, b).map_err(|e| e.to_string()))
        {
            Ok(()) => println!("  ✓ wrote {out}"),
            Err(e) => {
                eprintln!("error: writing {out}: {e}");
                return 1;
            }
        }
    }
    0
}

// ── IO: enriched atlas → detected section-pairs ─────────────

#[derive(Deserialize)]
struct ChaptersFile {
    #[serde(default)]
    chapters: Vec<ChapterRow>,
}
#[derive(Deserialize)]
struct ChapterRow {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    chunk_ids: Vec<serde_json::Value>,
}

/// Map every `Tension` edge to the unordered pair of source sections it
/// connects (claim → section via evidence chunk id → chapter title →
/// `SectionKey`). Same-section and unmappable edges are dropped.
fn load_detected(index_root: &Path) -> Result<Vec<PairKey>, String> {
    let atlas = index_root.join(ATLAS_DIRNAME);
    let atoms = read_atlas_atoms(&atlas).map_err(|e| format!("atoms.json: {e}"))?;
    let edges = read_atlas_edges(&atlas).map_err(|e| format!("edges.json: {e}"))?;

    let chapters: ChaptersFile = {
        let bytes = std::fs::read(index_root.join("chapters.json"))
            .map_err(|e| format!("chapters.json: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("chapters.json: {e}"))?
    };
    // Section id AND chunk id → key (atom evidence keys on the section
    // id; the integer chunk ids are a fallback).
    let mut chunk2key: HashMap<String, SectionKey> = HashMap::new();
    for c in &chapters.chapters {
        let key = SectionKey::from_title(&c.title);
        chunk2key.insert(c.id.clone(), key.clone());
        for ci in &c.chunk_ids {
            let s = match ci {
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::String(s) => s.clone(),
                _ => continue,
            };
            chunk2key.insert(s, key.clone());
        }
    }
    // Claim atom → section key.
    let mut claim2key: HashMap<String, SectionKey> = HashMap::new();
    for env in &atoms.atoms {
        if let AtomEnvelope::Claim(c) = env {
            if let Some(ev) = c.evidence.first() {
                if let Some(k) = chunk2key.get(&ev.chunk_id) {
                    claim2key.insert(c.id.as_str().to_string(), k.clone());
                }
            }
        }
    }
    // Tension edges → section pairs.
    let mut out = Vec::new();
    for e in &edges.edges {
        if e.edge_type != EdgeType::Tension {
            continue;
        }
        let (Some(a), Some(b)) = (
            claim2key.get(e.source.as_str()),
            claim2key.get(e.target.as_str()),
        ) else {
            continue;
        };
        if a != b {
            out.push(PairKey::new(a.clone(), b.clone()));
        }
    }
    Ok(out)
}

// ── rendering ───────────────────────────────────────────────

fn split_label(splits: &[Split]) -> String {
    if splits.len() == ALL_SPLITS.len() {
        "all".into()
    } else {
        splits
            .iter()
            .map(|s| format!("{s:?}").to_lowercase())
            .collect::<Vec<_>>()
            .join("+")
    }
}

fn print_report(r: &DetectorReport, corpus: &str, splits: &[Split]) {
    println!("=== bench governance — {corpus} (recall split: {}) ===", split_label(splits));
    println!(
        "  precision {:.2}  recall {:.2}  f1 {:.2}",
        r.overall.precision, r.overall.recall, r.overall.f1
    );
    println!(
        "  detected: {} edges over {} distinct pairs",
        r.n_detected_edges, r.n_detected_pairs
    );
    if !r.recall_by_type.is_empty() {
        println!("  recall by type:");
        for (t, (found, total)) in &r.recall_by_type {
            println!("    {t:24} {found}/{total}");
        }
    }
}

fn print_diagnose(r: &DetectorReport) {
    println!("--- diagnose ---");
    println!("  planted FOUND  ({}): {}", r.planted_found.len(), r.planted_found.join(", "));
    println!("  planted MISSED ({}): {}", r.planted_missed.len(), r.planted_missed.join(", "));
    if !r.flagged_decoys.is_empty() {
        println!("  !! flagged expected-non/decoys: {}", r.flagged_decoys.join(", "));
    }
    if !r.flagged_other.is_empty() {
        println!("  flagged unlabeled pairs ({}):", r.flagged_other.len());
        for pk in &r.flagged_other {
            println!("     {:?} <-> {:?}", pk.0, pk.1);
        }
    }
}
