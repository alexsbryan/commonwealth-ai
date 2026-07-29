// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench parity-compare` — the desktop-vs-bench **enrichment-parity gate**.
//!
//! The benches validate chat over the full enrichment variety because they build
//! their Runtime from the same `chat_cmd::bootstrap` the CLI uses, which wires
//! every enrichment-consuming provider. The desktop runtime wires its own chain
//! (`sovereign-desktop/.../state.rs`) — and historically wired FEWER seams, so a
//! fully-built corpus could have enrichment legs silently dropped in desktop
//! inference (the load-bearing example: `apply_atlas_grounding` hard-returns when
//! `atlas_context_provider` is `None`, and the desktop never wired it — atlas
//! grounding was entirely dead in the desktop until 2026-06).
//!
//! This harness makes that class of regression impossible to ship silently. For
//! each `(corpus, question)` it runs BOTH paths —
//!   - the **bench** path (`run_live_pinned`, an in-process Runtime delegating
//!     inference to the daemon), and
//!   - the **desktop** path (`run_bridge_live`, the real `#[tauri::command]`
//!     surface over the debug command bridge) —
//! extracts each side's **enrichment-signal set** from the SAME persisted
//! `message.metadata` glassbox channel, and **fails when desktop ⊊ bench** (the
//! desktop surfaced strictly fewer enrichment legs than the bench did).
//!
//! ## Signals
//! All read off `message.metadata` (no log parsing — works identically across the
//! process boundary). The `retrieved_chunks[].metadata.source` field cleanly
//! discriminates four enrichment legs, set by the shared retrieval pipeline:
//!   - `atom-enum`         → atlas atom enumeration (`retrieval.rs`)
//!   - `code_intel_summary`→ code-intel call-graph hits (`code_trace.rs`)
//!   - `raptor`            → tiered/RAPTOR summaries (`retrieval.rs`)
//!   - `bridge_boost`      → cross-corpus meta-atlas bridge (`retrieval.rs`)
//! plus `atom_type=claim` (the overview-claim path) and any `atlas:*` tag keys,
//! plus the `knowledge_view_digests` channel (field_model landscape digests; the
//! Phase 3 ambient-field_model step persists its view ids there).
//!
//! ## Preconditions
//! - The **daemon** is up (`--base-url`, default :9741) — the bench session
//!   delegates inference to it.
//! - The **desktop** is up with `SOVEREIGN_COMMAND_BRIDGE=1` (`--bridge-url`,
//!   default :9745), with the same corpus installed.
//! - Run against **fresh** corpora — a stale build (declared enrichment absent on
//!   disk) confounds the diff. Phase 4's readiness gate flags those.
//!
//! Like `--warm-atlas` in the chaos bench, the in-process session is warmed
//! (`atlas_mgr.warm_one`) so the bench side actually surfaces its atlas instead
//! of silently measuring base retrieval. Disable with `--no-warm-atlas`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use sovereign_eval::chaos_monkey::ChaosBank;

use crate::bench_cmd::desktop_bridge::{run_bridge_live, BridgeClient, DEFAULT_BRIDGE_URL};
use crate::bench_cmd::live_runner::run_live_pinned;
use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "svrn bench parity-compare",
    summary: "Diff the enrichment legs the desktop surfaces vs the bench, per question; fail when desktop ⊊ bench.",
    sections: &[
        HelpSection::Usage(
            "svrn bench parity-compare --bank <bank.toml> [--corpus <id>] [--bridge-url <url>] [--spec <s>] [--limit N] [--no-warm-atlas] [--out <json>]",
        ),
        HelpSection::Notes(
            "Requires the daemon up (--base-url, default :9741) AND the desktop up with \
             SOVEREIGN_COMMAND_BRIDGE=1 (--bridge-url, default :9745), same corpus installed on \
             both. Reuses the chaos-monkey bank TOML ([meta].corpus + [[questions]]). Reads \
             enrichment signals off message.metadata (atom-enum / code_intel_summary / raptor / \
             bridge_boost / atlas: tags / knowledge_view_digests). Exit 0 when every question has \
             desktop ⊇ bench; exit 1 on any desktop-deficient question (or a desktop turn error).",
        ),
    ],
};

/// The enrichment-signal set extracted from one turn's persisted message
/// metadata. Each token names an enrichment leg that demonstrably surfaced. The
/// parity gate requires `desktop ⊇ bench` per question, i.e.
/// `bench.difference(desktop)` is empty.
///
/// Pure over the JSON — unit-tested below; the two transports feed it the
/// identical `message.metadata` shape so one extractor judges both.
fn extract_signals(meta: &serde_json::Value) -> BTreeSet<String> {
    let mut sig = BTreeSet::new();
    if let Some(chunks) = meta.get("retrieved_chunks").and_then(|v| v.as_array()) {
        for c in chunks {
            // `source` discriminates four enrichment legs. project_retrieved_chunks
            // copies it to the top level AND keeps the full map under `metadata`;
            // read the map first, fall back to the top-level mirror.
            let map = c.get("metadata").and_then(|v| v.as_object());
            let source = map
                .and_then(|m| m.get("source"))
                .and_then(|v| v.as_str())
                .or_else(|| c.get("source").and_then(|v| v.as_str()));
            match source {
                Some("atom-enum") => {
                    sig.insert("atom_enum".to_string());
                }
                Some("code_intel_summary") => {
                    sig.insert("code_intel".to_string());
                }
                Some("raptor") => {
                    sig.insert("raptor".to_string());
                }
                Some("bridge_boost") => {
                    sig.insert("cross_corpus_bridge".to_string());
                }
                _ => {}
            }
            if let Some(m) = map {
                if m.get("atom_type").and_then(|v| v.as_str()) == Some("claim") {
                    sig.insert("atom_claim".to_string());
                }
                for k in m.keys() {
                    if let Some(rest) = k.strip_prefix("atlas:") {
                        sig.insert(format!("atlas_tag:{rest}"));
                    }
                }
            }
        }
    }
    // field_model / knowledge_view landscape digests — not chunks. The shared
    // runtime persists the spliced view ids here (Phase 3 ambient field_model +
    // the existing 3-view digests) so this channel is visible across the bridge.
    if let Some(digs) = meta
        .get("knowledge_view_digests")
        .and_then(|v| v.as_array())
    {
        for d in digs {
            if let Some(view) = d.as_str() {
                sig.insert(format!("digest:{view}"));
            }
        }
    }
    sig
}

/// Whether the visible answer is a real answer (non-empty after think-strip).
fn answered(visible: &str) -> bool {
    !visible.trim().is_empty()
}

struct ParityArgs {
    bank: PathBuf,
    corpus: Option<String>,
    bridge_url: String,
    spec: String,
    limit: Option<usize>,
    warm_atlas: bool,
    out: Option<PathBuf>,
}

fn parse_parity_args(rest: &[String]) -> Result<ParityArgs, String> {
    let mut bank: Option<PathBuf> = None;
    let mut corpus = None;
    let mut bridge_url = DEFAULT_BRIDGE_URL.to_string();
    let mut spec = "bench:parity-compare".to_string();
    let mut limit = None;
    let mut warm_atlas = true;
    let mut out: Option<PathBuf> = None;

    let mut i = 0;
    macro_rules! val {
        ($l:expr) => {{
            i += 1;
            rest.get(i)
                .cloned()
                .ok_or_else(|| format!("{} requires a value", $l))?
        }};
    }
    while i < rest.len() {
        match rest[i].as_str() {
            "--bank" => bank = Some(PathBuf::from(val!("--bank"))),
            "--corpus" => corpus = Some(val!("--corpus")),
            "--bridge-url" => bridge_url = val!("--bridge-url"),
            "--spec" => spec = val!("--spec"),
            "--limit" => {
                limit = Some(
                    val!("--limit")
                        .parse()
                        .map_err(|_| "--limit must be a usize")?,
                )
            }
            "--no-warm-atlas" => warm_atlas = false,
            "--out" => out = Some(PathBuf::from(val!("--out"))),
            other => return Err(format!("unknown flag `{other}`")),
        }
        i += 1;
    }
    Ok(ParityArgs {
        bank: bank.ok_or("--bank is required")?,
        corpus,
        bridge_url,
        spec,
        limit,
        warm_atlas,
        out,
    })
}

pub async fn cmd_parity_compare(args: &[String]) -> i32 {
    if args
        .iter()
        .any(|a| a == "--help" || a == "-h" || a == "help")
    {
        help::print(&HELP);
        return 0;
    }
    // Globals first (base-url, dirs), then our flags from the remainder.
    let (mut globals, rest) = match parse_globals(args) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if globals.temperature.is_none() {
        globals.temperature = Some(0.0);
    }
    let pargs = match parse_parity_args(&rest) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            help::print(&HELP);
            return 2;
        }
    };

    let bank = match ChaosBank::load(&pargs.bank) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let corpus = match pargs
        .corpus
        .clone()
        .filter(|c| !c.is_empty())
        .or_else(|| Some(bank.meta.corpus.clone()).filter(|c| !c.is_empty()))
    {
        Some(c) => c,
        None => {
            eprintln!("error: no corpus — set --corpus or [meta].corpus in the bank");
            return 1;
        }
    };

    // Bench path: in-process Runtime (delegates inference to the daemon).
    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "error: could not build bench chat session (is the daemon up at {}?): {e}",
                globals.daemon_base
            );
            return 1;
        }
    };
    // Warm the sealed corpus's atlas into the in-process manager (cache-only
    // build_session would otherwise contribute 0 atlas contexts → the bench side
    // would silently measure base retrieval and the parity diff would be a lie).
    let atlas_warm_entries = if pargs.warm_atlas {
        let n = session.atlas_mgr.warm_one(&corpus).await;
        eprintln!(
            "[parity] atlas-warm: {n} context entr{} loaded for `{corpus}`",
            if n == 1 { "y" } else { "ies" }
        );
        if n == 0 {
            eprintln!(
                "[parity] WARN: atlas warm loaded 0 entries for `{corpus}` — if this corpus has an \
                 atlas, relax the filter (SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS=0 \
                 SOVEREIGN_ATLAS_INCLUDE_CLAIMS=1); a stale/atlas-less corpus makes the diff \
                 uninformative (Phase 4 readiness gate flags those)."
            );
        }
        n
    } else {
        0
    };

    // Freshness gate (Phase 4): a corpus whose recipe DECLARES an enrichment the
    // disk lacks (a stale local index) measures a DIFFERENT surface than the
    // recipe promises — a confounded comparison that would read as spurious
    // desktop deficiency. Surface it loudly; the run still proceeds (glassbox
    // over silent skip) but the report records the staleness so CI can gate on it.
    let corpus_stale_reason = session.corpus_engine.enrichment_drift(&corpus).await;
    if let Some(reason) = &corpus_stale_reason {
        eprintln!("[parity] WARN: corpus `{corpus}` looks STALE — {reason}");
        eprintln!(
            "[parity] WARN: the comparison below may be CONFOUNDED by missing enrichment; \
             re-sync/rebuild the corpus for a clean parity run."
        );
    }

    // Desktop path: the real command bridge.
    let client = BridgeClient::new(&pargs.bridge_url);
    if let Err(e) = client.healthz().await {
        eprintln!("error: {e}");
        return 1;
    }
    for ev in ["message-complete", "message-error"] {
        if let Err(e) = client.listen(ev).await {
            eprintln!("error: bridge listen {ev}: {e}");
            return 1;
        }
    }

    eprintln!(
        "[parity] bank={:?} corpus={corpus} questions={} transport: bench(in-process @ {}) vs desktop(bridge @ {})",
        pargs.bank,
        bank.questions.len(),
        globals.daemon_base,
        pargs.bridge_url,
    );

    let take = pargs.limit.unwrap_or(bank.questions.len());
    let mut rows = Vec::new();
    let mut deficient_questions = 0usize;
    let mut bridge_errors = 0usize;

    for (qi, q) in bank.questions.iter().take(take).enumerate() {
        let bench = run_live_pinned(&session, &corpus, &q.question, None).await;
        let bench_sig = extract_signals(&bench.metadata);

        let (desk_sig, desk_answered, desk_gate, bridge_error) =
            match run_bridge_live(&client, Some(&corpus), &q.question, &pargs.spec).await {
                Ok(turn) => {
                    let s = extract_signals(&turn.answer.metadata);
                    (
                        s,
                        answered(&turn.answer.visible),
                        turn.answer.gate_action.clone(),
                        None,
                    )
                }
                Err(e) => {
                    bridge_errors += 1;
                    (BTreeSet::new(), false, None, Some(e))
                }
            };

        let deficient: Vec<String> = bench_sig.difference(&desk_sig).cloned().collect();
        let surplus: Vec<String> = desk_sig.difference(&bench_sig).cloned().collect();
        let is_deficient = !deficient.is_empty() || bridge_error.is_some();
        if is_deficient {
            deficient_questions += 1;
        }

        // Glassbox per-question line. This block is the comparison report
        // itself — including the ERRORED line, which is a recorded per-probe
        // outcome (it counts toward `bridge_errors` and lands in the JSON
        // rows), not a program error — so the whole block goes to stdout.
        let fmt_set = |s: &BTreeSet<String>| -> String {
            if s.is_empty() {
                "{}".to_string()
            } else {
                format!("{{{}}}", s.iter().cloned().collect::<Vec<_>>().join(", "))
            }
        };
        println!("  [{:>2}/{}] {} ({})", qi + 1, take, q.id, q.qtype.label());
        if let Some(err) = &bridge_error {
            println!("         desktop turn ERRORED: {err}");
        }
        println!("         bench   signals: {}", fmt_set(&bench_sig));
        println!("         desktop signals: {}", fmt_set(&desk_sig));
        if is_deficient {
            println!(
                "         -> DESKTOP DEFICIENT: missing {{{}}}  [FAIL]",
                deficient.join(", ")
            );
        } else if !surplus.is_empty() {
            println!(
                "         -> PARITY OK (desktop surplus {{{}}})",
                surplus.join(", ")
            );
        } else {
            println!("         -> PARITY OK");
        }

        rows.push(serde_json::json!({
            "id": q.id,
            "qtype": q.qtype.label(),
            "question": q.question,
            "bench_signals": bench_sig.iter().cloned().collect::<Vec<_>>(),
            "desktop_signals": desk_sig.iter().cloned().collect::<Vec<_>>(),
            "deficient": deficient,
            "surplus": surplus,
            "bench_answered": answered(&bench.visible),
            "desktop_answered": desk_answered,
            "bench_gate": bench.gate_action,
            "desktop_gate": desk_gate,
            "bridge_error": bridge_error,
        }));
    }

    let result = if deficient_questions == 0 {
        "pass"
    } else {
        "fail"
    };
    // Rollup — the headline the operator ran the command for.
    println!(
        "[parity] SUMMARY: {} questions, {deficient_questions} desktop-deficient, {bridge_errors} bridge-error",
        rows.len(),
    );
    println!(
        "[parity] RESULT: {} ({} questions where desktop surfaced fewer enrichment legs than the bench)",
        result.to_uppercase(),
        deficient_questions,
    );

    let report = serde_json::json!({
        "bank": pargs.bank.to_string_lossy(),
        "corpus": corpus,
        "bridge_url": pargs.bridge_url,
        "base_url": globals.daemon_base,
        "atlas_warm_entries": atlas_warm_entries,
        "corpus_stale": corpus_stale_reason,
        "questions": rows,
        "summary": {
            "total": take.min(bank.questions.len()),
            "deficient_questions": deficient_questions,
            "bridge_errors": bridge_errors,
            "result": result,
        },
    });
    if let Some(out) = &pargs.out {
        if let Some(parent) = out.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&report) {
            Ok(s) => {
                if let Err(e) = std::fs::write(out, s) {
                    eprintln!("[parity] WARN: could not write report {out:?}: {e}");
                } else {
                    eprintln!("[parity] report → {out:?}");
                }
            }
            Err(e) => eprintln!("[parity] WARN: could not serialize report: {e}"),
        }
    }

    if deficient_questions == 0 {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_atom_enum_and_claim_from_chunk_source() {
        let meta = json!({
            "retrieved_chunks": [
                { "metadata": { "source": "atom-enum", "atom_type": "claim" } },
                { "metadata": { "source": "document" } },
            ]
        });
        let sig = extract_signals(&meta);
        assert!(sig.contains("atom_enum"));
        assert!(sig.contains("atom_claim"));
    }

    #[test]
    fn discriminates_all_four_source_legs_plus_digests() {
        let meta = json!({
            "retrieved_chunks": [
                { "metadata": { "source": "atom-enum" } },
                { "metadata": { "source": "code_intel_summary" } },
                { "metadata": { "source": "raptor" } },
                { "metadata": { "source": "bridge_boost" } },
                { "metadata": { "source": "document", "atlas:topic": "x" } },
            ],
            "knowledge_view_digests": ["personal-knowledge", "field:maple-house"],
        });
        let sig = extract_signals(&meta);
        for want in [
            "atom_enum",
            "code_intel",
            "raptor",
            "cross_corpus_bridge",
            "atlas_tag:topic",
            "digest:personal-knowledge",
            "digest:field:maple-house",
        ] {
            assert!(sig.contains(want), "missing signal {want} in {sig:?}");
        }
    }

    #[test]
    fn top_level_source_mirror_is_a_fallback() {
        // project_retrieved_chunks copies source to the top level; if the map is
        // absent we still catch it.
        let meta = json!({ "retrieved_chunks": [ { "source": "atom-enum" } ] });
        assert!(extract_signals(&meta).contains("atom_enum"));
    }

    #[test]
    fn empty_or_null_metadata_yields_no_signals() {
        assert!(extract_signals(&serde_json::Value::Null).is_empty());
        assert!(extract_signals(&json!({})).is_empty());
        assert!(extract_signals(&json!({ "retrieved_chunks": [] })).is_empty());
    }

    #[test]
    fn parity_gate_is_subset_not_equality() {
        // desktop ⊇ bench passes even with surplus; desktop ⊊ bench fails.
        let bench: BTreeSet<String> = ["atom_enum"].into_iter().map(String::from).collect();
        let desk_ok: BTreeSet<String> = ["atom_enum", "raptor"]
            .into_iter()
            .map(String::from)
            .collect();
        let desk_bad: BTreeSet<String> = ["raptor"].into_iter().map(String::from).collect();
        assert!(
            bench.difference(&desk_ok).next().is_none(),
            "superset should pass"
        );
        assert!(
            bench.difference(&desk_bad).next().is_some(),
            "deficient should fail"
        );
    }
}
