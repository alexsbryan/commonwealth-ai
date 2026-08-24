// SPDX-License-Identifier: AGPL-3.0-or-later
//! drb1-t1 flight-replay harness — the admission stage (M0, order
//! drb1-t1, campaign drb1-race), extended at drb1-t2 with the FETCH
//! stage. The logged flight is the tuning fixture: this driver
//! replays the ADMISSION stage over every recorded (question,
//! result-row) of the 9 logged t7a tasks through PRODUCTION code
//! paths — `web_hit_relevance` (the web-admission decider) and
//! `triage_hits` — never a reimplementation (the rescan_render.rs
//! precedent). Stage-shaped: `--stage admission` replays admission
//! alone; `--stage fetch` (drb1-t2) replays admission and then the
//! fetch walk (the permissive queue, the round fetch cap, fallbacks,
//! the health/retry classes, and the post-fetch content admission
//! over the RECORDED page contents); anything else refuses loudly
//! (later rungs add audit/render).
//!
//! Per run dir (charter.json, fetch-list-N.json, skip-ledger-N.json;
//! the fetch stage also reads evidence-window-N.json and
//! budget-ledger.json): reconstruct the round's ranked rows (admitted
//! rows from the fetch list in rank order, skipped rows from the
//! ledger by rank — the parity gate asserts the recorded admitted set
//! reproduces from the recorded scores), then re-score every row and
//! re-run triage at the production thresholds. Zero web, zero API,
//! zero daemon.
//!
//! NAMED SUBSTITUTIONS (the logs carry less than production saw —
//! §18.3; each is per-row in the CSV's `query_source` /
//! `snippet_source` columns):
//! - skipped rows carry no snippet (SkipEntry never recorded one):
//!   scored on title+url, or the marked gold overlay when one exists;
//! - skipped rows carry no query_id: scored against EVERY round
//!   query, max (an upper bound);
//! - phantom rows the pre-fix id collision un-ledgered are excluded
//!   (their presence could only displace admitted rows, never add);
//! - FETCH stage: rows the logged flight never fetched carry no
//!   content — the walk SPENDS on them but cannot content-judge them
//!   (`content-unknown`, never a fabricated admit/refuse); only rows
//!   with a recorded outcome (window chunk, failure, refusal)
//!   contribute to the surviving-fetch and content-rejection counts.
//!   The end-to-end content path is the mock-deck battery's (the
//!   seat's).
//!
//! Usage:
//!   replay_flight <flight-root> <out-dir> [--stage admission|fetch]
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
//!   label-input.jsonl     — the labeling SURFACE: one line per row
//!                           carrying the charter question, the round
//!                           queries and the snippet the CSVs drop;
//!                           row order matches admission-rows.csv
//!   admission-labels.csv  — the labeling sheet for the seat (label
//!                           column EMPTY; 3-class on-topic /
//!                           adjacent / off)
//!   fetch-rows.csv        — (fetch stage) per row: the replayed walk
//!                           outcome, spend, health class, content
//!                           verdict
//!   fetch-summary.json    — (fetch stage) per task: queue sizes,
//!                           spend shape, surviving-fetch rate,
//!                           content rejections with reasons, gold
//!                           queue membership, the registry shape
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sovereign_core::deep_research::acquisition::{
    judge_content, noise_class, triage_hits, web_hit_relevance, DEFAULT_CODE_SET_K,
    DEFAULT_CONTENT_COVERAGE_FLOOR, DEFAULT_EPS_QUOTA, DEFAULT_PROSE_LINE_FLOOR,
};
use sovereign_core::deep_research::fetch::{classify_fetch_error, source_type_of, RetryClass};
use sovereign_core::deep_research::icd::{
    EvidenceWindow, FetchList, SearchHit, SkipLedger, SourceRegistryRow, SourceType,
};

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

impl Clone for Row {
    fn clone(&self) -> Self {
        Self {
            url: self.url.clone(),
            title: self.title.clone(),
            snippet: self.snippet.clone(),
            query_texts: self.query_texts.clone(),
            snippet_source: self.snippet_source,
            query_source: self.query_source,
            recorded_score: self.recorded_score,
            recorded_admitted: self.recorded_admitted,
            rank: self.rank,
        }
    }
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
    if stage != "admission" && stage != "fetch" {
        eprintln!(
            "stage `{stage}` is not implemented (this rung ships admission + fetch; later rungs add audit/render)"
        );
        std::process::exit(2);
    }
    if args.len() != 2 {
        eprintln!("usage: replay_flight <flight-root> <out-dir> [--stage admission|fetch]");
        std::process::exit(2);
    }
    let root = PathBuf::from(&args[0]);
    let out = PathBuf::from(&args[1]);
    std::fs::create_dir_all(&out).expect("create out dir");
    if let Err(e) = run(&root, &out, &stage) {
        eprintln!("replay failed: {e}");
        std::process::exit(1);
    }
}

fn run(root: &Path, out: &Path, stage: &str) -> Result<(), String> {
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
    // The labeling SURFACE (acquisition tune, 2026-08-24): the two CSVs
    // carry the decision columns but drop the three fields a labeler —
    // human or model — actually judges topicality from (the charter
    // question, the round's queries, the hit's snippet). Emitting them
    // here keeps ONE reconstruction (§10.6): the rank interleave of
    // fetch-list ∪ skip-ledger is pinned by the parity gate above, and a
    // second implementation in the labeling script would be a second
    // decider. Row order matches admission-rows.csv line-for-line.
    let mut label_input = String::new();
    let mut summary = serde_json::json!({});
    let mut parity_failures = 0usize;
    let mut total_phantoms = 0usize;
    let mut gold_fate: Vec<serde_json::Value> = Vec::new();
    // drb1-t2 fetch stage: the raw materials per (task, round) — the
    // reconstructed rows, the round's queries, and the RECORDED window
    // (chunks carry the only real page contents the logs hold).
    let mut fetch_materials: Vec<FetchMaterial> = Vec::new();
    let mut fetch_allowance = 0u32;

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
        let question = charter["question"].as_str().unwrap_or_default().to_string();
        fetch_allowance = charter["charter"]["budget"]["web_fetch_pages"]
            .as_u64()
            .unwrap_or(12) as u32;
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
            if stage == "fetch" {
                let w_path = run_dir.join(format!("evidence-window-{entry}.json"));
                let window: EvidenceWindow = if w_path.exists() {
                    read_json(&w_path)?
                } else {
                    EvidenceWindow {
                        icd: "evidence_window".to_string(),
                        version: fetch_list.version,
                        run_id: fetch_list.run_id.clone(),
                        charter_hash: fetch_list.charter_hash.clone(),
                        round: entry as u32,
                        chunks: Vec::new(),
                        fetch_failures: Vec::new(),
                        dedup_refused: Vec::new(),
                        content_refused: Vec::new(),
                        derived_custody: String::new(),
                    }
                };
                fetch_materials.push(FetchMaterial {
                    task: task.to_string(),
                    round: entry,
                    queries: fetch_list
                        .queries
                        .iter()
                        .map(|q| (q.id.clone(), q.text.clone()))
                        .collect(),
                    rows: rows.clone(),
                    window,
                });
            }

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
                label_input.push_str(
                    &serde_json::to_string(&serde_json::json!({
                        "task": task,
                        "round": entry,
                        "rank": r.rank,
                        "question": question,
                        "queries": r.query_texts,
                        "url": r.url,
                        "title": r.title,
                        "snippet": r.snippet,
                        "snippet_source": r.snippet_source,
                        "query_source": r.query_source,
                        "logged_score": r.recorded_score,
                        "replayed_score": replayed_score,
                        "recorded_decision": if r.recorded_admitted { "admit" } else { "skip" },
                        "replayed_decision": after_decision,
                        "gold": is_gold(&task.to_string(), &r.url),
                    }))
                    .unwrap(),
                );
                label_input.push('\n');
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
    std::fs::write(out.join("label-input.jsonl"), label_input)
        .map_err(|e| format!("label-input.jsonl: {e}"))?;
    std::fs::write(
        out.join("admission-summary.json"),
        serde_json::to_string_pretty(&full).unwrap(),
    )
    .map_err(|e| format!("admission-summary.json: {e}"))?;
    println!(
        "admission replay: parity_failures={parity_failures} phantoms={total_phantoms} — rows/labels/summary written to {}",
        out.display()
    );
    if stage == "fetch" {
        run_fetch_stage(&fetch_materials, fetch_allowance, out)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// drb1-t2 — the fetch stage (order drb1-t2). Replays the new
// fetch-then-judge walk over the recorded flight: permissive triage
// (noise demoted, every non-noise row queued), the round fetch cap
// (the r2b split), per-query fallback promotion past failures,
// permanent/transient retry classes, and the post-fetch content
// admission judged over the RECORDED page contents. Zero web: a row
// the flight never fetched replays as `content-unknown` (spend
// simulated, outcome never fabricated — §18.3).
// ---------------------------------------------------------------------

/// One round's replay materials.
struct FetchMaterial {
    task: String,
    round: usize,
    queries: Vec<(String, String)>,
    rows: Vec<Row>,
    window: EvidenceWindow,
}

/// The recorded outcome for a url in one round.
enum Recorded {
    /// The window holds the fetched content.
    Success(String),
    /// The window records the failure (with the error text).
    Failure(String),
    /// Refused as already-fetched.
    Dedup,
    /// No recorded outcome — the flight never fetched this url.
    Unknown,
}

/// The production scorer over a reconstructed row, for ranking the
/// queue (the same substitution the admission stage scores with).
fn row_score(r: &Row) -> f64 {
    r.query_texts
        .iter()
        .map(|q| web_hit_relevance(q, &r.title, &r.snippet, &r.url))
        .fold(0.0_f64, f64::max)
}

/// The row's best query text (the content-judge surface): its OWN
/// query when recorded, else the round query with the best coverage
/// (the named upper-bound substitution).
fn row_query(r: &Row) -> String {
    let mut best = String::new();
    let mut best_score = -1.0_f64;
    for q in &r.query_texts {
        let s = web_hit_relevance(q, &r.title, &r.snippet, &r.url);
        if s > best_score {
            best_score = s;
            best = q.clone();
        }
    }
    best
}

fn run_fetch_stage(materials: &[FetchMaterial], allowance: u32, out: &Path) -> Result<(), String> {
    use sovereign_core::deep_research::budget::round_allowance_cap;

    let mut rows_csv = String::from(
        "task,round,rank,url,noise_class,queue,recorded,replayed_outcome,spend,health,content_verdict,coverage,prose_line,reason,gold\n",
    );
    let mut tasks_json = serde_json::Map::new();
    let mut registry_all: Vec<SourceRegistryRow> = Vec::new();

    let tasks: Vec<String> = materials
        .iter()
        .map(|m| m.task.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    let mut tasks_sorted = tasks;
    tasks_sorted.sort();

    for task in &tasks_sorted {
        let rounds: Vec<&FetchMaterial> = materials.iter().filter(|m| &m.task == task).collect();
        let n_rounds = rounds.len() as u32;
        let mut remaining = allowance;
        let mut dead: HashSet<String> = HashSet::new();
        let mut already: HashSet<String> = HashSet::new();
        let mut round_json: Vec<serde_json::Value> = Vec::new();
        // Per-task measurement accumulators.
        let mut attempts_recorded = 0usize;
        let mut surviving = 0usize;
        let mut content_rejected: Vec<serde_json::Value> = Vec::new();
        let mut gold_queued: Vec<serde_json::Value> = Vec::new();
        let mut spend_total = 0u32;

        for m in &rounds {
            // Production shape: rescore, triage (noise demoted, all
            // non-noise rows queued), walk under the round cap.
            let mut hits: Vec<SearchHit> = m
                .rows
                .iter()
                .enumerate()
                .map(|(i, r)| SearchHit {
                    id: format!("row-{}", i + 1),
                    query_id: String::new(),
                    url: r.url.clone(),
                    title: r.title.clone(),
                    snippet: r.snippet.clone(),
                    content: None,
                    engine: "replay".to_string(),
                    score: row_score(r),
                    custody: String::new(),
                })
                .collect();
            let triaged = triage_hits(
                "replay",
                "hash",
                m.round as u32,
                std::mem::take(&mut hits),
                DEFAULT_CODE_SET_K,
                DEFAULT_EPS_QUOTA,
            );
            let noise_demoted = triaged
                .skip_ledger
                .entries
                .iter()
                .filter(|e| e.reason.starts_with("noise-demoted"))
                .count();
            let queue: Vec<(usize, &Row)> = triaged
                .candidates
                .iter()
                .map(|h| {
                    let idx: usize = h.id.strip_prefix("row-").unwrap().parse().unwrap();
                    (idx, &m.rows[idx - 1])
                })
                .collect();
            let rounds_left = n_rounds.saturating_sub(m.round as u32).saturating_add(1);
            let round_cap = round_allowance_cap(remaining, rounds_left) as usize;
            let mut spent_round = 0usize;

            let recorded_of = |url: &str| -> Recorded {
                if let Some(c) = m.window.chunks.iter().find(|c| c.source_url == url) {
                    return Recorded::Success(c.content.clone());
                }
                if let Some(f) = m.window.fetch_failures.iter().find(|f| f.url == url) {
                    return Recorded::Failure(f.error.clone());
                }
                if m.window.dedup_refused.iter().any(|u| u == url) {
                    return Recorded::Dedup;
                }
                Recorded::Unknown
            };

            for (idx, r) in &queue {
                let noise = noise_class(&r.url).map(|c| c.as_str().to_string());
                let recorded = if noise.is_some() {
                    "noise-demoted".to_string()
                } else if dead.contains(&r.url) {
                    "dead-refused".to_string()
                } else if already.contains(&r.url) {
                    "dedup-refused".to_string()
                } else {
                    match recorded_of(&r.url) {
                        Recorded::Success(_) => "fetched".to_string(),
                        Recorded::Failure(_) => "failed".to_string(),
                        Recorded::Dedup => "dedup-refused".to_string(),
                        Recorded::Unknown => "never-fetched".to_string(),
                    }
                };
                // Replay the walk decision + spend.
                let (outcome, spend, health, verdict, coverage, prose_line, reason) =
                    if let Some(nc) = &noise {
                        (
                            format!("noise-demoted:{nc}"),
                            0u32,
                            "none".to_string(),
                            "n/a".to_string(),
                            0.0,
                            0usize,
                            String::new(),
                        )
                    } else if dead.contains(&r.url) {
                        (
                            "dead-refused".to_string(),
                            0,
                            "dead".to_string(),
                            "n/a".to_string(),
                            0.0,
                            0,
                            String::new(),
                        )
                    } else if already.contains(&r.url) {
                        (
                            "dedup-refused".to_string(),
                            0,
                            "dedup".to_string(),
                            "n/a".to_string(),
                            0.0,
                            0,
                            String::new(),
                        )
                    } else if spent_round >= round_cap {
                        (
                            "not-attempted-round-cap".to_string(),
                            0,
                            "round-cap".to_string(),
                            "n/a".to_string(),
                            0.0,
                            0,
                            String::new(),
                        )
                    } else {
                        match recorded_of(&r.url) {
                            Recorded::Success(content) => {
                                spent_round += 1;
                                attempts_recorded += 1;
                                let q = row_query(r);
                                let v = judge_content(
                                    &q,
                                    &r.title,
                                    &content,
                                    &r.url,
                                    DEFAULT_CONTENT_COVERAGE_FLOOR,
                                    DEFAULT_PROSE_LINE_FLOOR,
                                );
                                already.insert(r.url.clone());
                                if v.admits {
                                    surviving += 1;
                                } else {
                                    content_rejected.push(serde_json::json!({
                                        "round": m.round,
                                        "url": r.url,
                                        "coverage": v.coverage,
                                        "prose_line": v.prose_line,
                                        "reason": v.reason,
                                    }));
                                }
                                registry_all.push(SourceRegistryRow {
                                    url: r.url.clone(),
                                    title: r.title.clone(),
                                    source_type: source_type_of(&r.url),
                                    round: m.round as u32,
                                    admitted: v.admits,
                                });
                                let verdict = if v.admits { "admit" } else { "content-refused" };
                                (
                                    verdict.to_string(),
                                    1,
                                    "ok".to_string(),
                                    verdict.to_string(),
                                    v.coverage,
                                    v.prose_line,
                                    v.reason.clone(),
                                )
                            }
                            Recorded::Failure(err) => {
                                // ONE page per URL, whatever the retry
                                // ladder does — mirroring the production
                                // gate in `fetch::fetch_round` since the
                                // acquisition tune of 2026-08-24 (red:
                                // `one_dead_url_does_not_eat_the_rounds_fetch_allowance`).
                                //
                                // KNOWN DEBT (§10.6): this harness
                                // re-implements production's walk and
                                // spend model instead of calling
                                // `fetch_round` behind a replay port, so
                                // the accounting rule lives in two
                                // places and they can drift silently.
                                // They agree as of this edit. The fix is
                                // a `ResearchPort` that answers
                                // `web_fetch` from the recorded chunk or
                                // the recorded error; then this whole
                                // branch is deleted.
                                let attempts = 1u32;
                                spent_round += attempts as usize;
                                attempts_recorded += 1;
                                dead.insert(r.url.clone());
                                let h = match classify_fetch_error(&err) {
                                    RetryClass::Permanent(h) => h.as_str(),
                                    RetryClass::Transient => "dead",
                                };
                                (
                                    "fetch-failed".to_string(),
                                    attempts,
                                    h.to_string(),
                                    "n/a".to_string(),
                                    0.0,
                                    0,
                                    err.clone(),
                                )
                            }
                            Recorded::Dedup => (
                                "dedup-refused".to_string(),
                                0,
                                "dedup".to_string(),
                                "n/a".to_string(),
                                0.0,
                                0,
                                String::new(),
                            ),
                            Recorded::Unknown => {
                                // NAMED SUBSTITUTION: the flight never
                                // fetched this url — spend simulated,
                                // outcome never fabricated.
                                spent_round += 1;
                                (
                                    "fetched-content-unknown".to_string(),
                                    1,
                                    "ok".to_string(),
                                    "content-unknown".to_string(),
                                    0.0,
                                    0,
                                    "no recorded content".to_string(),
                                )
                            }
                        }
                    };
                spend_total += spend;
                rows_csv.push_str(&format!(
                    "{},{},{},{},{},{},{},{},{},{},{},{:.4},{},{},{}\n",
                    task,
                    m.round,
                    r.rank,
                    r.url,
                    noise.clone().unwrap_or_default(),
                    queue
                        .iter()
                        .position(|(i, _)| i == idx)
                        .map(|p| p + 1)
                        .unwrap_or(0),
                    recorded,
                    outcome,
                    spend,
                    health,
                    verdict,
                    coverage,
                    prose_line,
                    reason.replace(',', ";").replace('\n', " "),
                    is_gold(task, &r.url),
                ));
                if is_gold(task, &r.url) {
                    gold_queued.push(serde_json::json!({
                        "round": m.round,
                        "url": r.url,
                        "noise_demoted": noise.is_some(),
                        "in_queue": noise.is_none(),
                        "recorded": recorded,
                        "replayed_outcome": outcome,
                    }));
                }
            }
            remaining = remaining.saturating_sub(spent_round as u32);
            round_json.push(serde_json::json!({
                "round": m.round,
                "noise_demoted": noise_demoted,
                "queue_len": queue.len(),
                "round_cap": round_cap,
                "spent_round": spent_round,
                "remaining_after": remaining,
            }));
        }
        let surviving_rate = if attempts_recorded == 0 {
            0.0
        } else {
            surviving as f64 / attempts_recorded as f64
        };
        tasks_json.insert(
            task.clone(),
            serde_json::json!({
                "rounds": round_json,
                "recorded_fetch_attempts": attempts_recorded,
                "content_admitted": surviving,
                "content_rejected": content_rejected,
                "surviving_fetch_rate": surviving_rate,
                "spend_total": spend_total,
                "allowance": allowance,
                "gold_queue_membership": gold_queued,
            }),
        );
    }

    let full = serde_json::json!({
        "stage": "fetch",
        "production_defaults": {
            "code_set_k": DEFAULT_CODE_SET_K,
            "eps_quota": DEFAULT_EPS_QUOTA,
            "content_coverage_floor": DEFAULT_CONTENT_COVERAGE_FLOOR,
            "prose_line_floor": DEFAULT_PROSE_LINE_FLOOR,
        },
        "registry_rows": registry_all.len(),
        "registry_type_counts": {
            "web": registry_all.iter().filter(|r| r.source_type == SourceType::Web).count(),
            "pdf": registry_all.iter().filter(|r| r.source_type == SourceType::Pdf).count(),
            "estate": registry_all.iter().filter(|r| r.source_type == SourceType::Estate).count(),
        },
        "tasks": tasks_json,
    });
    std::fs::write(out.join("fetch-rows.csv"), rows_csv)
        .map_err(|e| format!("fetch-rows.csv: {e}"))?;
    std::fs::write(
        out.join("fetch-summary.json"),
        serde_json::to_string_pretty(&full).unwrap(),
    )
    .map_err(|e| format!("fetch-summary.json: {e}"))?;
    // The registry shape, emitted per run (the seat's citation-
    // whitelist surface).
    let registry = sovereign_core::deep_research::icd::SourceRegistry {
        icd: "source_registry".to_string(),
        version: sovereign_core::deep_research::icd::ICD_VERSION,
        run_id: "replay".to_string(),
        charter_hash: "replay".to_string(),
        sources: registry_all,
    };
    std::fs::write(
        out.join("fetch-registry.json"),
        serde_json::to_string_pretty(&registry).unwrap(),
    )
    .map_err(|e| format!("fetch-registry.json: {e}"))?;
    println!(
        "fetch replay written to {} (fetch-rows.csv, fetch-summary.json, fetch-registry.json)",
        out.display()
    );
    Ok(())
}
