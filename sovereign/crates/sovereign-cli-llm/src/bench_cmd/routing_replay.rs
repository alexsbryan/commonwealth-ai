// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench routing-replay` — replay a routing bank through a
//! live desktop's production chat path and score the intent that
//! ACTUALLY fired, read back from each turn's glassbox metadata
//! (`provenance.intent`).
//!
//! This simultaneously regression-tests routing under the desktop
//! surface AND the glassbox surface itself: a turn whose metadata
//! carries no intent fails the row even if the answer was fine.
//!
//! Scoring is exact-match accuracy per category — the same contract
//! `bench all --routing-only` applies to these banks via the eval
//! runner, projected onto the desktop transport. Routing banks carry
//! `category` labels; the category→Intent mapping below mirrors the
//! bank header's documentation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use super::desktop_bridge::{run_bridge_live, BridgeClient, DEFAULT_BRIDGE_URL};
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench routing-replay",
    summary: "Replay a routing bank through a live desktop (command bridge) and score provenance.intent against the bank's expected categories.",
    sections: &[
        HelpSection::Usage(
            "svrn bench routing-replay --bank <bank.toml> [--bridge-url <url>] [--limit N] [--out <json>]",
        ),
        HelpSection::Notes(
            "Requires a desktop running with SOVEREIGN_COMMAND_BRIDGE=1. Each question runs the FULL \
             production turn (routing + retrieval + synthesis) — slower than the routing-only eval, \
             but it scores what the desktop user actually experiences, and proves the glassbox \
             metadata carries the fired intent.",
        ),
    ],
};

/// Bank categories → the Intent name the runtime records in
/// `provenance.intent`. Mirrors the documented mapping in
/// `sovereign/bench/routing/cells_v1.toml`'s header.
fn expected_intents(category: &str) -> &'static [&'static str] {
    match category {
        "conation" => &["ConationQuery"],
        "commissive" => &["CommissiveQuery"],
        "expressive" => &["ExpressiveQuery"],
        "metalingual" => &["MetalingualQuery"],
        "comparative" => &["ComparisonQuery"],
        "factual_recall" => &["KnowledgeQuery", "SimpleQuery"],
        "multi_article_synthesis" => &["DeepQuery"],
        _ => &[],
    }
}

#[derive(Debug, Deserialize)]
struct Bank {
    #[serde(default)]
    questions: Vec<BankQuestion>,
}

#[derive(Debug, Deserialize)]
struct BankQuestion {
    id: String,
    #[serde(alias = "prompt", alias = "question")]
    question: String,
    #[serde(default)]
    category: String,
}

pub async fn cmd_routing_replay(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        help::print(&HELP);
        return 0;
    }
    let mut bank_path: Option<PathBuf> = None;
    let mut bridge_url = DEFAULT_BRIDGE_URL.to_string();
    let mut limit: Option<usize> = None;
    let mut out: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bank" => {
                i += 1;
                bank_path = args.get(i).map(PathBuf::from);
            }
            "--bridge-url" => {
                i += 1;
                bridge_url = args.get(i).cloned().unwrap_or(bridge_url);
            }
            "--limit" => {
                i += 1;
                limit = args.get(i).and_then(|v| v.parse().ok());
            }
            "--out" => {
                i += 1;
                out = args.get(i).map(PathBuf::from);
            }
            other => {
                eprintln!("error: unknown flag `{other}`");
                return 2;
            }
        }
        i += 1;
    }
    let Some(bank_path) = bank_path else {
        eprintln!("error: --bank is required");
        return 2;
    };

    let text = match std::fs::read_to_string(&bank_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: read {}: {e}", bank_path.display());
            return 1;
        }
    };
    let bank: Bank = match toml::from_str(&text) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: parse bank: {e}");
            return 1;
        }
    };
    let known: Vec<&BankQuestion> = bank
        .questions
        .iter()
        .filter(|q| !expected_intents(&q.category).is_empty())
        .collect();
    let skipped = bank.questions.len() - known.len();
    let take = limit.unwrap_or(known.len()).min(known.len());

    let client = BridgeClient::new(&bridge_url);
    if let Err(e) = client.healthz().await {
        eprintln!("error: {e}");
        return 1;
    }
    for ev in ["message-complete", "message-error"] {
        if let Err(e) = client.listen(ev).await {
            eprintln!("error: {e}");
            return 1;
        }
    }
    eprintln!(
        "[routing-replay] bank={} questions={} (skipped {} with unmapped categories) bridge={}",
        bank_path.display(),
        take,
        skipped,
        bridge_url,
    );

    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut per_category: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (qi, q) in known.iter().take(take).enumerate() {
        let turn = run_bridge_live(&client, None, &q.question, "bench:routing-replay").await;
        let (fired, glassbox_ok) = match &turn {
            Ok(t) => {
                // Two metadata contracts coexist: referential handlers
                // attach a full ResponseProvenance (`provenance.intent`),
                // speech-act handlers (conation/commissive/expressive/
                // metalingual) attach a top-level `intent` only.
                let fired = t.metadata["provenance"]["intent"]
                    .as_str()
                    .or_else(|| t.metadata["intent"].as_str())
                    .unwrap_or("")
                    .to_string();
                let ok = !fired.is_empty();
                (fired, ok)
            }
            Err(e) => {
                eprintln!("  [{}] turn failed: {e}", q.id);
                (String::new(), false)
            }
        };
        let expected = expected_intents(&q.category);
        let pass = glassbox_ok && expected.contains(&fired.as_str());
        let bucket = per_category.entry(q.category.clone()).or_insert((0, 0));
        bucket.1 += 1;
        if pass {
            bucket.0 += 1;
        }
        eprintln!(
            "  [{:>2}/{}] {:<28} {:<14} → {:<16} {}",
            qi + 1,
            take,
            q.id,
            q.category,
            if fired.is_empty() { "(no intent!)" } else { &fired },
            if pass { "PASS" } else { "FAIL" },
        );
        rows.push(serde_json::json!({
            "id": q.id,
            "category": q.category,
            "expected": expected,
            "fired": fired,
            "glassbox_intent_present": glassbox_ok,
            "pass": pass,
        }));
    }

    let total = rows.len();
    let passed = rows.iter().filter(|r| r["pass"] == true).count();
    eprintln!("\n── routing-replay (desktop transport) ──");
    for (cat, (p, t)) in &per_category {
        eprintln!("  {:<26} {:>2}/{:<2}", cat, p, t);
    }
    eprintln!(
        "  overall: {passed}/{total} ({:.0}%)",
        if total == 0 { 0.0 } else { 100.0 * passed as f64 / total as f64 }
    );

    if let Some(out) = out {
        let doc = serde_json::json!({
            "bank": bank_path.display().to_string(),
            "transport": "desktop-bridge",
            "passed": passed,
            "total": total,
            "rows": rows,
        });
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&out, serde_json::to_string_pretty(&doc).unwrap()) {
            eprintln!("error: write {}: {e}", out.display());
            return 1;
        }
        eprintln!("  wrote {}", out.display());
    }
    if passed == total {
        0
    } else {
        1
    }
}
