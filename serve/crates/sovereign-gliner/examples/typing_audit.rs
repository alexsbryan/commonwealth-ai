// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The P2.1 typing gate.** Runs v1 and GLiNER2 over the SAME chunks,
//! through the SAME production seam (`LabeledEntityExtractor`), and asks
//! the one question the throughput and RSS numbers cannot answer: does
//! GLiNER2 assign the RIGHT LABEL?
//!
//! Why this exists. The P2.1 plan claims GLiNER2 "fixes type-collapse by
//! extracting types jointly". The only evidence that existed on
//! 2026-08-02 pointed the other way — a 50-chunk eyeball found GLiNER2
//! typing `BonJour` and `Sosa` as `Work` where v1 said `Person`. An
//! aggregate mention count cannot see that; it looks like *more*
//! extraction, which reads as better. So this audit reports per LABEL,
//! and scores against a named oracle rather than a vibe.
//!
//! Three sections, cheapest signal first:
//!
//! 1. **Volume per label** — how many mentions each backend produces of
//!    each type. Establishes whether a difference is breadth or typing.
//! 2. **Head-to-head on shared surface forms** — for every string BOTH
//!    backends found, do they agree on the label? This needs no ground
//!    truth at all and is where type-collapse shows up as a block of
//!    `Person → Work` cells.
//! 3. **Oracle** — names with a known correct label (`expect`) and names
//!    with a known WRONG label (`never`, the anti-tests). Lives in
//!    `sovereign/bench/gliner/` (see its README); the obsidian one is
//!    transcribed from `bench/obsidian/golden.toml`'s
//!    `expected_person_atoms` / `forbidden_person_atoms`, which the
//!    operator already reviewed, plus SEP philosopher surnames.
//!
//! Run (both models must resolve under `models_root()`):
//!
//! ```text
//! cargo run --release -p sovereign-gliner \
//!     --features corpus-engine/treesitter --example typing_audit -- \
//!     --fixture research/enrichment-spikes/data/chunks_50.jsonl \
//!     --oracle  sovereign/bench/gliner/typing_oracle_sep.json \
//!     --out     research/enrichment-spikes/findings/typing_audit.json
//! ```
//!
//! Exit codes: 0 report produced, 2 bad arguments, 1 a backend failed to
//! load or a fixture could not be read. A run that scores zero oracle
//! entries exits 1 — an audit that checked nothing is not a pass
//! (ARCH_PRINCIPLES §18.1).

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use sovereign_gliner::gliner_ner::{DEFAULT_MODEL_ID, GLINER2_MODEL_ID};
use sovereign_gliner::labeled::{load_labeled_extractor, LabeledEntityExtractor};

/// One backend's view of the fixture: surface form → label → count.
type LabelCounts = BTreeMap<String, BTreeMap<String, usize>>;

struct Args {
    fixtures: Vec<PathBuf>,
    oracle: Option<PathBuf>,
    out: Option<PathBuf>,
    limit: Option<usize>,
}

fn parse_args() -> Result<Args, String> {
    let mut fixtures = Vec::new();
    let mut oracle = None;
    let mut out = None;
    let mut limit = None;
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let need = |i: usize| -> Result<String, String> {
            argv.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{} needs a value", argv[i]))
        };
        match argv[i].as_str() {
            "--fixture" => {
                fixtures.push(PathBuf::from(need(i)?));
                i += 2;
            }
            "--oracle" => {
                oracle = Some(PathBuf::from(need(i)?));
                i += 2;
            }
            "--out" => {
                out = Some(PathBuf::from(need(i)?));
                i += 2;
            }
            "--limit" => {
                limit = Some(need(i)?.parse().map_err(|_| "--limit wants a number")?);
                i += 2;
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if fixtures.is_empty() {
        return Err("at least one --fixture <jsonl> is required".into());
    }
    Ok(Args {
        fixtures,
        oracle,
        out,
        limit,
    })
}

/// Pull chunk text out of a JSONL fixture. Accepts the three field names
/// `scripts/dump_chunks.py` may emit depending on the source corpus's
/// Lance schema; a line with none of them is skipped LOUDLY, because a
/// silently-empty fixture would make every number below a zero that
/// looks like agreement.
fn read_fixture(path: &PathBuf) -> Result<Vec<String>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut texts = Vec::new();
    let mut skipped = 0usize;
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        match ["content", "text", "chunk"]
            .iter()
            .find_map(|k| v.get(*k).and_then(|t| t.as_str()))
        {
            Some(t) if !t.trim().is_empty() => texts.push(t.to_string()),
            _ => skipped += 1,
        }
    }
    if skipped > 0 {
        eprintln!(
            "  warn: {} line(s) in {} had no content/text/chunk field",
            skipped,
            path.display()
        );
    }
    Ok(texts)
}

struct OracleEntry {
    name: String,
    label: String,
    source: String,
}

struct Oracle {
    expect: Vec<OracleEntry>,
    never: Vec<OracleEntry>,
}

/// Hand-parsed rather than derived: this crate has no `serde` derive dep,
/// and an oracle is ~40 lines of JSON whose shape is stated right here.
/// An entry missing `name` or `label` is an ERROR, not a skip — a
/// silently-dropped oracle row is a check that never ran.
fn parse_oracle(v: &serde_json::Value, key: &str) -> Result<Vec<OracleEntry>, String> {
    let Some(arr) = v.get(key) else {
        return Ok(Vec::new());
    };
    let arr = arr
        .as_array()
        .ok_or_else(|| format!("oracle `{key}` must be an array"))?;
    arr.iter()
        .enumerate()
        .map(|(i, e)| {
            let name = e
                .get("name")
                .and_then(|s| s.as_str())
                .ok_or_else(|| format!("oracle {key}[{i}] has no string `name`"))?;
            let label = e
                .get("label")
                .and_then(|s| s.as_str())
                .ok_or_else(|| format!("oracle {key}[{i}] has no string `label`"))?;
            Ok(OracleEntry {
                name: name.to_string(),
                label: label.to_string(),
                source: e
                    .get("source")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect()
}

/// Chunks per inference call. Matches
/// `corpus_extract_entities_cmd::BATCH_SIZE`, deliberately: an audit that
/// batches differently from production is measuring a different thing.
///
/// It also has to be bounded at all. Handing v1's gline-rs stack all
/// 3,175 vault chunks in ONE call pads every text to the longest and
/// gets the process SIGKILLed — observed 2026-08-03, exit 137, after
/// the first version of this file did exactly that.
const BATCH_SIZE: usize = 8;

/// Run one backend over every chunk, folding into surface → label → count.
fn run_backend(
    extractor: &Arc<dyn LabeledEntityExtractor>,
    texts: &[String],
) -> (LabelCounts, usize) {
    let mut counts: LabelCounts = BTreeMap::new();
    let mut total = 0usize;
    let mut failed_batches = 0usize;
    for (i, group) in texts.chunks(BATCH_SIZE).enumerate() {
        let refs: Vec<&str> = group.iter().map(|s| s.as_str()).collect();
        let batches = match extractor.extract_mentions_batch(&refs) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("  warn: {} batch {i} failed: {e}", extractor.model_id());
                failed_batches += 1;
                continue;
            }
        };
        for mentions in batches {
            for m in mentions {
                total += 1;
                *counts
                    .entry(m.text.to_lowercase())
                    .or_default()
                    .entry(m.label)
                    .or_insert(0) += 1;
            }
        }
        if i % 50 == 0 && i > 0 {
            eprint!(
                "\r  {} … {}/{} chunks",
                extractor.model_id(),
                i * BATCH_SIZE,
                texts.len()
            );
        }
    }
    if failed_batches > 0 {
        // Never silent: a dropped batch is missing evidence, and every
        // rate below is computed over what survived.
        eprintln!(
            "\n  WARN: {failed_batches} batch(es) of {} failed for {} — rates below exclude them",
            texts.len().div_ceil(BATCH_SIZE),
            extractor.model_id()
        );
    }
    eprint!("\r");
    (counts, total)
}

/// The label a backend assigns a surface form most often. `None` when the
/// backend never saw it.
fn dominant_label(counts: &LabelCounts, surface: &str) -> Option<String> {
    counts.get(surface).and_then(|by_label| {
        by_label
            .iter()
            .max_by_key(|(label, n)| (**n, std::cmp::Reverse(label.as_str().to_string())))
            .map(|(label, _)| label.clone())
    })
}

/// `(mentions carrying `want`, mentions of this surface at all)`.
///
/// The mention-level view. `dominant_label` answers "what does this
/// backend mostly think X is"; this answers "how many rows did it get
/// wrong", which is the number `chunk_entities` actually stores.
fn mention_split(counts: &LabelCounts, surface: &str, want: &str) -> (usize, usize) {
    match counts.get(surface) {
        None => (0, 0),
        Some(by_label) => (
            by_label.get(want).copied().unwrap_or(0),
            by_label.values().sum(),
        ),
    }
}

/// The labels a backend gave this surface OTHER than `want`, as
/// `Label×n` — so a disagreement row names what it actually said.
fn other_labels(counts: &LabelCounts, surface: &str, want: &str) -> String {
    counts
        .get(surface)
        .map(|by_label| {
            by_label
                .iter()
                .filter(|(l, _)| l.as_str() != want)
                .map(|(l, n)| format!("{l}×{n}"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        100.0 * n as f64 / d as f64
    }
}

fn per_label_totals(counts: &LabelCounts) -> BTreeMap<String, usize> {
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for by_label in counts.values() {
        for (label, n) in by_label {
            *out.entry(label.clone()).or_insert(0) += n;
        }
    }
    out
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("typing_audit: {msg}");
            eprintln!("usage: --fixture <jsonl> [--fixture <jsonl>…] [--oracle <json>] [--out <json>] [--limit N]");
            std::process::exit(2);
        }
    };

    let mut texts = Vec::new();
    for f in &args.fixtures {
        match read_fixture(f) {
            Ok(mut t) => {
                println!("fixture {}: {} chunk(s)", f.display(), t.len());
                texts.append(&mut t);
            }
            Err(e) => {
                eprintln!("typing_audit: {e}");
                std::process::exit(1);
            }
        }
    }
    if let Some(n) = args.limit {
        texts.truncate(n);
    }
    if texts.is_empty() {
        eprintln!("typing_audit: fixtures yielded 0 chunks — nothing to audit");
        std::process::exit(1);
    }
    println!("auditing {} chunk(s)\n", texts.len());

    // Parse the oracle BEFORE loading a single model. It is the cheapest
    // input and the easiest to get wrong (a moved path, a typo), and
    // reading it after the inference passes means a bad path costs a
    // full run — which is exactly what happened on 2026-08-03: 15
    // minutes of v1 and 15 of GLiNER2, then `No such file or directory`,
    // and the report never written. Validate what is cheap to validate
    // first.
    let oracle = match &args.oracle {
        None => None,
        Some(path) => {
            let raw = match std::fs::read_to_string(path) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("typing_audit: oracle {}: {e}", path.display());
                    std::process::exit(1);
                }
            };
            let parsed: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(o) => o,
                Err(e) => {
                    eprintln!(
                        "typing_audit: oracle {} is not valid JSON: {e}",
                        path.display()
                    );
                    std::process::exit(1);
                }
            };
            let oracle = match (
                parse_oracle(&parsed, "expect"),
                parse_oracle(&parsed, "never"),
            ) {
                (Ok(expect), Ok(never)) => Oracle { expect, never },
                (Err(e), _) | (_, Err(e)) => {
                    eprintln!("typing_audit: {e}");
                    std::process::exit(1);
                }
            };
            println!(
                "oracle {}: {} positive(s), {} anti-test(s)",
                path.display(),
                oracle.expect.len(),
                oracle.never.len()
            );
            Some(oracle)
        }
    };

    let backends: Vec<(&str, &str)> = vec![("v1", DEFAULT_MODEL_ID), ("g2", GLINER2_MODEL_ID)];
    let mut results: BTreeMap<&str, (LabelCounts, usize)> = BTreeMap::new();
    for (arm, model_id) in &backends {
        let extractor = match load_labeled_extractor(model_id, None) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("typing_audit: {arm} ({model_id}) failed to load: {e}");
                std::process::exit(1);
            }
        };
        let started = std::time::Instant::now();
        let (counts, total) = run_backend(&extractor, &texts);
        println!(
            "{arm} ({model_id}, threshold {:.2}): {} mentions, {} distinct surfaces, {:.1?}",
            extractor.threshold(),
            total,
            counts.len(),
            started.elapsed()
        );
        results.insert(arm, (counts, total));
    }

    let (v1_counts, v1_total) = results.remove("v1").expect("v1 arm ran");
    let (g2_counts, g2_total) = results.remove("g2").expect("g2 arm ran");

    // ── 1. Volume per label ──────────────────────────────────────────
    println!("\n── mentions per label ──");
    let v1_by_label = per_label_totals(&v1_counts);
    let g2_by_label = per_label_totals(&g2_counts);
    let all_labels: BTreeSet<&String> = v1_by_label.keys().chain(g2_by_label.keys()).collect();
    println!("  {:<16} {:>8} {:>8}", "label", "v1", "g2");
    for label in &all_labels {
        println!(
            "  {:<16} {:>8} {:>8}",
            label,
            v1_by_label.get(*label).copied().unwrap_or(0),
            g2_by_label.get(*label).copied().unwrap_or(0)
        );
    }
    println!("  {:<16} {:>8} {:>8}", "TOTAL", v1_total, g2_total);
    println!(
        "  {:<16} {:>8.2} {:>8.2}",
        "per chunk",
        v1_total as f64 / texts.len() as f64,
        g2_total as f64 / texts.len() as f64
    );

    // ── 2. Head-to-head on shared surface forms ──────────────────────
    // No ground truth needed: disagreement on a string both backends
    // found IS the type-collapse signal, and it is symmetric — the table
    // does not assume v1 is right.
    let shared: Vec<&String> = v1_counts
        .keys()
        .filter(|k| g2_counts.contains_key(*k))
        .collect();
    let mut confusion: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for surface in &shared {
        let a = dominant_label(&v1_counts, surface).unwrap_or_default();
        let b = dominant_label(&g2_counts, surface).unwrap_or_default();
        confusion
            .entry((a, b))
            .or_default()
            .push((*surface).clone());
    }
    let agree: usize = confusion
        .iter()
        .filter(|((a, b), _)| a == b)
        .map(|(_, v)| v.len())
        .sum();
    println!(
        "\n── head-to-head on {} shared surface form(s): {} agree ({:.1}%), {} disagree ──",
        shared.len(),
        agree,
        if shared.is_empty() {
            0.0
        } else {
            100.0 * agree as f64 / shared.len() as f64
        },
        shared.len() - agree
    );
    for ((a, b), surfaces) in confusion.iter().filter(|((a, b), _)| a != b) {
        let mut sample: Vec<&str> = surfaces.iter().map(|s| s.as_str()).collect();
        sample.sort();
        sample.truncate(8);
        println!(
            "  v1 {a:<14} → g2 {b:<14} {:>4}  e.g. {}",
            surfaces.len(),
            sample.join(", ")
        );
    }

    // ── 3. Oracle ────────────────────────────────────────────────────
    let mut oracle_report = serde_json::json!(null);
    if let Some(oracle) = &oracle {
        let mut rows = Vec::new();
        let mut scored = 0usize;
        let (mut v1_ok, mut g2_ok) = (0usize, 0usize);
        let (mut v1_mentions_ok, mut v1_mentions_total) = (0usize, 0usize);
        let (mut g2_mentions_ok, mut g2_mentions_total) = (0usize, 0usize);
        println!("\n── oracle: names with a known-correct label ──");
        println!(
            "  {:<26} {:<12} {:<12} {:<12}  {}",
            "surface", "expected", "v1", "g2", "per-mention"
        );
        for e in &oracle.expect {
            let key = e.name.to_lowercase();
            let v1 = dominant_label(&v1_counts, &key);
            let g2 = dominant_label(&g2_counts, &key);
            if v1.is_none() && g2.is_none() {
                // Absent from the fixture entirely — report it, never
                // silently drop it, but do not score it either way.
                println!("  {:<26} {:<12} {:<12} {:<12}", e.name, e.label, "—", "—");
                rows.push(serde_json::json!({
                    "kind": "expect", "name": e.name, "expected": e.label,
                    "v1": null, "g2": null, "scored": false, "source": e.source,
                }));
                continue;
            }
            scored += 1;
            let v1_hit = v1.as_deref() == Some(e.label.as_str());
            let g2_hit = g2.as_deref() == Some(e.label.as_str());
            v1_ok += usize::from(v1_hit);
            g2_ok += usize::from(g2_hit);
            // Dominant label is itself an aggregate, and a minority
            // mistyping still writes wrong `chunk_entities` rows. So
            // score MENTIONS too, and print the distribution whenever
            // the two disagree with each other.
            let (v1_right, v1_n) = mention_split(&v1_counts, &key, &e.label);
            let (g2_right, g2_n) = mention_split(&g2_counts, &key, &e.label);
            v1_mentions_ok += v1_right;
            v1_mentions_total += v1_n;
            g2_mentions_ok += g2_right;
            g2_mentions_total += g2_n;
            println!(
                "  {:<26} {:<12} {:<12} {:<12}  {}",
                e.name,
                e.label,
                format!(
                    "{}{}",
                    v1.clone().unwrap_or_else(|| "—".into()),
                    if v1_hit { " ✓" } else { "" }
                ),
                format!(
                    "{}{}",
                    g2.clone().unwrap_or_else(|| "—".into()),
                    if g2_hit { " ✓" } else { "" }
                ),
                format!(
                    "mentions v1 {v1_right}/{v1_n} g2 {g2_right}/{g2_n}{}",
                    if g2_right < g2_n {
                        format!("  g2 also: {}", other_labels(&g2_counts, &key, &e.label))
                    } else {
                        String::new()
                    }
                ),
            );
            rows.push(serde_json::json!({
                "kind": "expect", "name": e.name, "expected": e.label,
                "v1": v1, "g2": g2, "v1_correct": v1_hit, "g2_correct": g2_hit,
                "v1_mentions_correct": v1_right, "v1_mentions": v1_n,
                "g2_mentions_correct": g2_right, "g2_mentions": g2_n,
                "v1_dist": v1_counts.get(&key), "g2_dist": g2_counts.get(&key),
                "scored": true, "source": e.source,
            }));
        }

        println!("\n── oracle anti-tests: names that must NOT carry this label ──");
        let (mut v1_viol, mut g2_viol) = (0usize, 0usize);
        let mut anti_scored = 0usize;
        for e in &oracle.never {
            let key = e.name.to_lowercase();
            let v1 = dominant_label(&v1_counts, &key);
            let g2 = dominant_label(&g2_counts, &key);
            if v1.is_none() && g2.is_none() {
                rows.push(serde_json::json!({
                    "kind": "never", "name": e.name, "forbidden": e.label,
                    "v1": null, "g2": null, "scored": false, "source": e.source,
                }));
                continue;
            }
            anti_scored += 1;
            let v1_bad = v1.as_deref() == Some(e.label.as_str());
            let g2_bad = g2.as_deref() == Some(e.label.as_str());
            v1_viol += usize::from(v1_bad);
            g2_viol += usize::from(g2_bad);
            if v1_bad || g2_bad {
                println!(
                    "  {:<26} must not be {:<12} v1={} g2={}",
                    e.name,
                    e.label,
                    v1.clone().unwrap_or_else(|| "—".into()),
                    g2.clone().unwrap_or_else(|| "—".into())
                );
            }
            rows.push(serde_json::json!({
                "kind": "never", "name": e.name, "forbidden": e.label,
                "v1": v1, "g2": g2, "v1_violates": v1_bad, "g2_violates": g2_bad,
                "scored": true, "source": e.source,
            }));
        }
        if v1_viol == 0 && g2_viol == 0 {
            println!("  none violated (of {anti_scored} present in the fixture)");
        }

        println!("\n── oracle verdict ──");
        println!(
            "  positives present in fixture: {scored} of {}",
            oracle.expect.len()
        );
        println!("  correct label   v1 {v1_ok}/{scored}   g2 {g2_ok}/{scored}   (entity level, dominant label)");
        println!(
            "  correct mention v1 {v1_mentions_ok}/{v1_mentions_total} ({:.1}%)   g2 {g2_mentions_ok}/{g2_mentions_total} ({:.1}%)   (row level — what chunk_entities stores)",
            pct(v1_mentions_ok, v1_mentions_total),
            pct(g2_mentions_ok, g2_mentions_total),
        );
        println!(
            "  anti-tests present: {anti_scored} of {}",
            oracle.never.len()
        );
        println!("  violations      v1 {v1_viol}/{anti_scored}   g2 {g2_viol}/{anti_scored}");

        if scored == 0 && anti_scored == 0 {
            eprintln!(
                "\ntyping_audit: the oracle scored NOTHING — no oracle name appears in this \
                 fixture. That is a could-not-judge, not a pass (ARCH_PRINCIPLES §18.1). \
                 Use a fixture drawn from a corpus the oracle was written against."
            );
            std::process::exit(1);
        }

        oracle_report = serde_json::json!({
            "positives_scored": scored,
            "positives_total": oracle.expect.len(),
            "v1_correct": v1_ok,
            "g2_correct": g2_ok,
            "v1_mentions_correct": v1_mentions_ok,
            "v1_mentions_total": v1_mentions_total,
            "g2_mentions_correct": g2_mentions_ok,
            "g2_mentions_total": g2_mentions_total,
            "anti_scored": anti_scored,
            "anti_total": oracle.never.len(),
            "v1_violations": v1_viol,
            "g2_violations": g2_viol,
            "rows": rows,
        });
    }

    if let Some(out) = &args.out {
        let report = serde_json::json!({
            "chunks": texts.len(),
            "fixtures": args.fixtures.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "v1": { "model_id": DEFAULT_MODEL_ID, "mentions": v1_total, "per_label": v1_by_label },
            "g2": { "model_id": GLINER2_MODEL_ID, "mentions": g2_total, "per_label": g2_by_label },
            "shared_surfaces": shared.len(),
            "shared_agree": agree,
            "confusion": confusion
                .iter()
                .map(|((a, b), v)| serde_json::json!({ "v1": a, "g2": b, "count": v.len(), "surfaces": v }))
                .collect::<Vec<_>>(),
            "oracle": oracle_report,
        });
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(
            out,
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        ) {
            Ok(()) => println!("\nreport written: {}", out.display()),
            Err(e) => eprintln!("\nwarn: could not write {}: {e}", out.display()),
        }
    }
}
