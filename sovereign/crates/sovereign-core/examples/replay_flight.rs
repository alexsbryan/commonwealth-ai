// SPDX-License-Identifier: AGPL-3.0-or-later
//! drb1-t1 flight-replay harness — the admission stage (M0, order
//! drb1-t1, campaign drb1-race). The logged flight is the tuning
//! fixture: this driver replays the ADMISSION stage over every
//! recorded (question, result-row) of the 9 logged t7a tasks through
//! PRODUCTION code paths — `web_hit_relevance` (the web-admission
//! decider) and `triage_hits` — never a reimplementation (the
//! rescan_render.rs precedent). Stage-shaped: `--stage admission` is
//! the one implemented stage; anything else refuses loudly (later
//! rungs add fetch-decision, audit, render).
//!
//! Per run dir (charter.json, fetch-list-N.json, skip-ledger-N.json):
//! reconstruct the round's ranked rows (admitted rows from the fetch
//! list in rank order, skipped rows from the ledger by rank — the
//! parity gate asserts the recorded admitted set reproduces from the
//! recorded scores), then re-score every row and re-run triage at the
//! production thresholds. Zero web, zero API, zero daemon.
//!
//! NAMED SUBSTITUTIONS (the logs carry less than production saw —
//! §18.3; each is per-row in the CSV's `query_source` /
//! `snippet_source` columns):
//! - skipped rows carry no snippet (SkipEntry never recorded one):
//!   scored on title+url, or the marked gold overlay when one exists;
//! - skipped rows carry no query_id: scored against EVERY round
//!   query, max (an upper bound);
//! - phantom rows the pre-fix id collision un-ledgered are excluded
//!   (their presence could only displace admitted rows, never add).
//!
//! Usage:
//!   replay_flight <flight-root> <out-dir> [--stage admission]
//!   flight-root — the dir holding drb-<task>/dr-<run>/ (e.g.
//!   research/deep-research/arms/runs-t7a/std)
//!
//! Outputs in out-dir:
//!   admission-rows.csv    — per row: task, round, rank, url, title,
//!                           logged/replayed scores, decisions, the
//!                           substitution provenance
//!   admission-summary.json— per task/round parity + phantom counts,
//!                           before/after admitted sets, threshold
//!                           moves, gold rows' fate, the k sweep
//!   admission-labels.csv  — the labeling sheet for the seat (label
//!                           column EMPTY; 3-class on-topic /
//!                           adjacent / off)
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sovereign_core::deep_research::acquisition::{
    triage_hits, web_hit_relevance, DEFAULT_CODE_SET_K, DEFAULT_EPS_QUOTA,
};
use sovereign_core::deep_research::icd::{FetchList, SearchHit, SkipLedger};

/// The gold-overlay snippets (task 56): the search snippets the
/// flight saw were never persisted, and the recorded titles of two
/// gold rows are degenerate (a PDF's `<title>` is its filename). The
/// overlay carries the paper's own title as the reconstructed
/// snippet — public metadata, marked `overlay` in every output. The
/// only reconstruction in the harness.
fn gold_overlay(task: &str, url: &str) -> Option<&'static str> {
    if task == "56" && url == "https://brocku.ca/repec/pdf/0504.pdf" {
        Some("A Simple Approach to Analyzing Asymmetric First Price Auctions")
    } else {
        None
    }
}

/// The order's named gold rows for task 56 (exact-topic papers the
/// logged flight skipped below-cut at 0.0).
const GOLD_56: &[&str] = &[
    "https://brocku.ca/repec/pdf/0504.pdf",
    "https://kasberger.github.io/assets/pdf/fpa_robust.pdf",
    "https://www.researchgate.net/publication/228319685_Linear_Bid_in_Asymmetric_First-Price_Auctions",
    "https://www.sciencedirect.com/science/article/abs/pii/S0165176511002473",
    "https://eml.berkeley.edu/~mcfadden/eC103_f03/auctionlect.pdf",
];

fn is_gold(task: &str, url: &str) -> bool {
    task == "56" && GOLD_56.contains(&url)
}

struct Row {
    url: String,
    title: String,
    snippet: String,
    query_texts: Vec<String>,
    snippet_source: &'static str,
    query_source: &'static str,
    recorded_score: f64,
    recorded_admitted: bool,
    rank: usize,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let raw = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

/// The same reconstruction the red test pins (its parity assertion is
/// this harness's validity gate): admitted rows fill the ranks the
/// ledger does not carry, in order; holes that remain are the phantom
/// rows the id collision un-ledgered.
fn reconstruct_round(task: &str, fetch_list: &FetchList, ledger: &SkipLedger) -> (Vec<Row>, usize) {
    fn ensure<T>(rank: usize, rows: &mut Vec<Option<T>>) {
        while rows.len() < rank {
            rows.push(None);
        }
    }
    let queries: Vec<String> = fetch_list.queries.iter().map(|q| q.text.clone()).collect();
    let mut by_rank: Vec<Option<&_>> = Vec::new();
    let mut max_rank = 0usize;
    for e in &ledger.entries {
        max_rank = max_rank.max(e.rank);
        ensure(e.rank, &mut by_rank);
        by_rank[e.rank - 1] = Some(e);
    }
    let total = (fetch_list.search_hits.len() + ledger.entries.len()).max(max_rank);
    let mut slots: Vec<Option<Row>> = Vec::new();
    ensure(total, &mut slots);
    let mut admitted = fetch_list.search_hits.iter();
    for rank in 1..=total {
        if let Some(entry) = by_rank.get(rank - 1).copied().flatten() {
            let overlay = gold_overlay(task, &entry.url);
            let snippet = overlay.map(str::to_string).unwrap_or_default();
            slots[rank - 1] = Some(Row {
                url: entry.url.clone(),
                title: entry.title.clone(),
                snippet: snippet.clone(),
                query_texts: queries.clone(),
                snippet_source: if snippet.is_empty() {
                    "absent"
                } else {
                    "overlay"
                },
                query_source: "max-over-round-queries",
                recorded_score: entry.score,
                recorded_admitted: false,
                rank,
            });
        } else if let Some(hit) = admitted.next() {
            let q = fetch_list
                .queries
                .iter()
                .find(|q| q.id == hit.query_id)
                .map(|q| q.text.clone())
                .unwrap_or_default();
            slots[rank - 1] = Some(Row {
                url: hit.url.clone(),
                title: hit.title.clone(),
                snippet: hit.snippet.clone(),
                query_texts: vec![q],
                snippet_source: "recorded",
                query_source: "recorded-query",
                recorded_score: hit.score,
                recorded_admitted: true,
                rank,
            });
        }
    }
    let phantoms = slots.iter().filter(|s| s.is_none()).count();
    (slots.into_iter().flatten().collect(), phantoms)
}

fn rescored_hits(rows: &[Row]) -> Vec<SearchHit> {
    rows.iter()
        .map(|r| SearchHit {
            id: format!("row-{}", r.rank),
            query_id: String::new(),
            url: r.url.clone(),
            title: r.title.clone(),
            snippet: r.snippet.clone(),
            content: None,
            engine: "replay".to_string(),
            score: r
                .query_texts
                .iter()
                .map(|q| web_hit_relevance(q, &r.title, &r.snippet, &r.url))
                .fold(0.0_f64, f64::max),
            custody: String::new(),
        })
        .collect()
}

fn admitted_urls(
    triaged: &sovereign_core::deep_research::acquisition::TriageResult,
) -> Vec<String> {
    triaged.ranked.iter().map(|h| h.url.clone()).collect()
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut stage = "admission".to_string();
    args.retain(|a| {
        if let Some(s) = a.strip_prefix("--stage=") {
            stage = s.to_string();
            false
        } else {
            true
        }
    });
    if stage != "admission" {
        eprintln!(
            "stage `{stage}` is not implemented (this rung ships admission; later rungs add fetch-decision/audit/render)"
        );
        std::process::exit(2);
    }
    if args.len() != 2 {
        eprintln!("usage: replay_flight <flight-root> <out-dir> [--stage admission]");
        std::process::exit(2);
    }
    let root = PathBuf::from(&args[0]);
    let out = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&out).expect("create out dir");
    if let Err(e) = run(&root, &out) {
        eprintln!("replay failed: {e}");
        std::process::exit(1);
    }
}

fn run(root: &Path, out: &Path) -> Result<(), String> {
    let mut task_dirs: Vec<(u32, PathBuf)> = std::fs::read_dir(root)
        .map_err(|e| format!("{}: {e}", root.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().to_string();
            let task = name.strip_prefix("drb-")?.parse().ok()?;
            Some((task, p))
        })
        .collect();
    task_dirs.sort();
    if task_dirs.is_empty() {
        return Err(format!("no drb-* task dirs under {}", root.display()));
    }

    let mut rows_csv = String::from(
        "task,round,rank,url,title,logged_score,replayed_score,snippet_source,query_source,recorded_decision,replayed_decision,gold\n",
    );
    let mut labels_csv = String::from(
        "task,url,title,rank,logged_score,replayed_score,label,round,snippet_source\n",
    );
    let mut summary = serde_json::json!({});
    let mut parity_failures = 0usize;
    let mut total_phantoms = 0usize;
    let mut gold_fate: Vec<serde_json::Value> = Vec::new();

    for (task, dir) in &task_dirs {
        let run_dir = std::fs::read_dir(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().starts_with("dr-"))
                    .unwrap_or(false)
            })
            .ok_or_else(|| format!("{}: no dr-* run dir", dir.display()))?;
        let charter: serde_json::Value = read_json(&run_dir.join("charter.json"))?;
        let recorded_k = charter["charter"]["triage"]["code_set_k"]
            .as_u64()
            .unwrap_or(0) as usize;
        let recorded_eps = charter["charter"]["triage"]["eps_quota"]
            .as_f64()
            .unwrap_or(0.0);
        let run_id = charter["run_id"].as_str().unwrap_or_default().to_string();
        let mut rounds = vec![];
        for entry in 1.. {
            let fl_path = run_dir.join(format!("fetch-list-{entry}.json"));
            let sl_path = run_dir.join(format!("skip-ledger-{entry}.json"));
            if !fl_path.exists() {
                break;
            }
            let fetch_list: FetchList = read_json(&fl_path)?;
            let ledger: SkipLedger = read_json(&sl_path)?;
            let (rows, phantoms) = reconstruct_round(&task.to_string(), &fetch_list, &ledger);
            total_phantoms += phantoms;

            // Parity gate: the recorded admitted set must reproduce
            // from the recorded scores (the instrument's validity).
            let parity_hits: Vec<SearchHit> = rows
                .iter()
                .map(|r| SearchHit {
                    id: format!("row-{}", r.rank),
                    query_id: String::new(),
                    url: r.url.clone(),
                    title: r.title.clone(),
                    snippet: r.snippet.clone(),
                    content: None,
                    engine: "replay".to_string(),
                    score: r.recorded_score,
                    custody: String::new(),
                })
                .collect();
            let parity = triage_hits(
                &run_id,
                "hash",
                entry as u32,
                parity_hits,
                recorded_k,
                recorded_eps,
            );
            let replayed_recorded: Vec<String> =
                parity.ranked.iter().map(|h| h.url.clone()).collect();
            let recorded: Vec<String> = fetch_list
                .search_hits
                .iter()
                .map(|h| h.url.clone())
                .collect();
            let parity_ok = replayed_recorded == recorded;
            if !parity_ok {
                parity_failures += 1;
            }

            // The after picture: production scorer + production
            // thresholds, plus the k sweep for the tune.
            let hits = rescored_hits(&rows);
            let mut sweep = serde_json::Map::new();
            for k in [recorded_k, DEFAULT_CODE_SET_K, DEFAULT_CODE_SET_K + 1] {
                let t = triage_hits(
                    &run_id,
                    "hash",
                    entry as u32,
                    hits.clone(),
                    k,
                    DEFAULT_EPS_QUOTA,
                );
                sweep.insert(
                    format!("k{k}"),
                    serde_json::json!({
                        "admitted": admitted_urls(&t),
                        "threshold": t.outcome.threshold,
                        "gold_admitted": admitted_urls(&t)
                            .iter()
                            .filter(|u| is_gold(&task.to_string(), u))
                            .cloned()
                            .collect::<Vec<_>>(),
                    }),
                );
            }
            let after = triage_hits(
                &run_id,
                "hash",
                entry as u32,
                hits.clone(),
                DEFAULT_CODE_SET_K,
                DEFAULT_EPS_QUOTA,
            );
            let after_admitted = admitted_urls(&after);
            let after_set: HashSet<&String> = after_admitted.iter().collect();

            for r in &rows {
                let after_decision = if after_set.contains(&r.url) {
                    // url-level: a duplicate url admitted once admits
                    // all its rows (fetch dedups later)
                    "admit"
                } else {
                    "skip"
                };
                let replayed_score = hits
                    .iter()
                    .find(|h| h.id == format!("row-{}", r.rank))
                    .map(|h| h.score)
                    .unwrap_or(0.0);
                let title = r
                    .title
                    .replace(['"', ',', '\n', '\r'], " ")
                    .trim()
                    .to_string();
                rows_csv.push_str(&format!(
                    "{},{},{},{},{},{:.4},{:.4},{},{},{},{},{}\n",
                    task,
                    entry,
                    r.rank,
                    r.url,
                    title,
                    r.recorded_score,
                    replayed_score,
                    r.snippet_source,
                    r.query_source,
                    if r.recorded_admitted { "admit" } else { "skip" },
                    after_decision,
                    is_gold(&task.to_string(), &r.url),
                ));
                labels_csv.push_str(&format!(
                    "{},{},{},{},{:.4},{:.4},,{},{},\n",
                    task,
                    r.url,
                    title,
                    r.rank,
                    r.recorded_score,
                    replayed_score,
                    entry,
                    r.snippet_source,
                ));
                if is_gold(&task.to_string(), &r.url) {
                    gold_fate.push(serde_json::json!({
                        "task": task,
                        "round": entry,
                        "url": r.url,
                        "logged_score": r.recorded_score,
                        "replayed_score": replayed_score,
                        "snippet_source": r.snippet_source,
                        "recorded_decision": if r.recorded_admitted { "admit" } else { "skip" },
                        "replayed_decision": after_decision,
                    }));
                }
            }
            rounds.push(serde_json::json!({
                "round": entry,
                "rows": rows.len(),
                "phantoms": phantoms,
                "parity_ok": parity_ok,
                "recorded_admitted": recorded,
                "after_admitted": after_admitted,
                "after_threshold": after.outcome.threshold,
                "sweep": sweep,
            }));
        }
        summary[&task.to_string()] = serde_json::json!({
            "run_id": run_id,
            "recorded_k": recorded_k,
            "recorded_eps": recorded_eps,
            "rounds": rounds,
        });
    }

    let full = serde_json::json!({
        "flight_root": root.display().to_string(),
        "stage": "admission",
        "production_defaults": {
            "code_set_k": DEFAULT_CODE_SET_K,
            "eps_quota": DEFAULT_EPS_QUOTA,
        },
        "parity_failures": parity_failures,
        "total_phantom_rows": total_phantoms,
        "gold_rows": gold_fate,
        "tasks": summary,
    });
    std::fs::write(out.join("admission-rows.csv"), rows_csv)
        .map_err(|e| format!("admission-rows.csv: {e}"))?;
    std::fs::write(out.join("admission-labels.csv"), labels_csv)
        .map_err(|e| format!("admission-labels.csv: {e}"))?;
    std::fs::write(
        out.join("admission-summary.json"),
        serde_json::to_string_pretty(&full).unwrap(),
    )
    .map_err(|e| format!("admission-summary.json: {e}"))?;
    println!(
        "admission replay: parity_failures={parity_failures} phantoms={total_phantoms} — rows/labels/summary written to {}",
        out.display()
    );
    Ok(())
}
