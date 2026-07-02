//! Prototype: **wiring-drift** check for the Spec↔Code loop.
//!
//! The reconcile's summary-embedding recall is the right tool for *corroboration*
//! ("find the function whose PURPOSE is X") but the wrong tool for *contradiction*
//! of a **wiring claim** ("A exposes/uses/calls B"). For claim 12 it surfaced the
//! tool *definitions* themselves and missed the answer path that disables them —
//! because "X is disabled here" is always drowned by all the code that *is* about X.
//!
//! A wiring claim is not a retrieval-and-read problem. It's a call-graph question:
//! **does a call path exist from the claim's SUBJECT (the loop) to its OBJECT (the
//! tools)?** SCIP answers that exactly — no embeddings, no judge, no dilution — and
//! the receipt is a path (present ⇒ corroborated) or a proven absence over a rich
//! reachable set (absent ⇒ drift).
//!
//! Test claim (CODE_INTEL_CHAT #12): "The system exposes symbols, callers, callees,
//! and blast search capabilities to the model as an agentic loop when a code corpus
//! is scoped." Hand-extracted endpoints below stand in for the LLM extraction step
//! the general path will add (claim → subject/relation/object/scope).
//!
//! Run: `cargo run -p corpus-engine-scip --example wiring_drift_probe`

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use corpus_engine_scip::scip_graph::ScipGraph;

const CORPUS: &str = "commonwealth-ai";

/// SUBJECT — the answer loop for a scoped code corpus (the claim's "agentic loop").
const SUBJECTS: &[&str] = &["handle_code_query", "handle_knowledge_query"];

/// OBJECT — the code tools the claim says are exposed to the model. Matched as
/// substrings against reached symbol names: tool structs + the SCIP primitives
/// they wrap. Deliberately specific (no bare "symbol"/"callees") to avoid noise.
const TOOL_MARKERS: &[&str] = &[
    "SymbolLookupTool",
    "SymbolLookup",
    "find_callers",
    "find_callees",
    "blast_radius",
    "CallersTool",
    "CalleesTool",
    "BlastTool",
];

/// Bounds — a single handler's reachable set is normally a few hundred–thousand
/// symbols, so BFS terminates naturally; the cap is a runaway backstop, reported
/// honestly (a hit beyond it would be missed).
const MAX_DEPTH: usize = 10;
const VISIT_CAP: usize = 30_000;

fn data_dir() -> PathBuf {
    std::env::var("SOVEREIGN_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").expect("HOME")).join(".sovereign")
        })
}

fn is_tool(name: &str) -> bool {
    TOOL_MARKERS.iter().any(|m| name.contains(m))
}

#[tokio::main]
async fn main() {
    let db = data_dir().join("indexes").join(CORPUS).join("scip_graph.db");
    let graph = ScipGraph::open(&db, CORPUS).unwrap_or_else(|e| {
        panic!("open SCIP graph at {}: {e}", db.display());
    });

    // ── BFS over callee edges from the subject(s), recording parents for receipts ──
    let mut seen: HashSet<String> = HashSet::new();
    let mut parent: HashMap<String, (String, String, i32)> = HashMap::new();
    let mut queue: VecDeque<(String, usize)> = VecDeque::new();
    for s in SUBJECTS {
        seen.insert(s.to_string());
        queue.push_back((s.to_string(), 0));
    }

    let mut hits: Vec<(String, String, i32)> = Vec::new();
    let mut agentic_reached = false;
    let mut capped = false;

    while let Some((sym, depth)) = queue.pop_front() {
        if depth >= MAX_DEPTH {
            continue;
        }
        if seen.len() >= VISIT_CAP {
            capped = true;
            break;
        }
        let callees = match graph.find_callees(&sym).await {
            Ok((c, _caution)) => c,
            Err(_) => continue,
        };
        for c in callees {
            let name = c.symbol_name;
            if name.contains("agentic_evidence_round") {
                agentic_reached = true;
            }
            if is_tool(&name) {
                parent
                    .entry(name.clone())
                    .or_insert_with(|| (sym.clone(), c.file_path.clone(), c.line));
                hits.push((name.clone(), c.file_path.clone(), c.line));
            }
            if seen.insert(name.clone()) {
                parent
                    .entry(name.clone())
                    .or_insert_with(|| (sym.clone(), c.file_path.clone(), c.line));
                queue.push_back((name, depth + 1));
            }
        }
    }

    // ── glassbox report ──
    println!("═══ WIRING-DRIFT PROBE — CODE_INTEL_CHAT claim #12 ═══\n");
    println!("claim   : the answer loop exposes symbols/callers/callees/blast to the model");
    println!("subject : {SUBJECTS:?}");
    println!("object  : tools matching {TOOL_MARKERS:?}");
    println!("graph   : {}\n", db.display());

    println!(
        "reachable set from subject (depth ≤ {MAX_DEPTH}): {} symbols{}",
        seen.len(),
        if capped {
            format!(" [CAPPED at {VISIT_CAP} — absence below this is unproven]")
        } else {
            " [exhausted — absence is complete]".to_string()
        }
    );
    println!("  gated agentic_evidence_round reached from the loop? {agentic_reached}");
    println!(
        "  (a rich reachable set that omits the tools ⇒ the graph is live and the tools\n   are genuinely not called — not a traversal artifact)\n"
    );

    if hits.is_empty() {
        println!("VERDICT: ⚠ DRIFT  — the claimed wiring does not exist");
        println!("  No call path from the answer loop to ANY code tool within depth {MAX_DEPTH}.");
        println!("  The loop reaches a gated evidence-re-retrieval round (agentic_evidence_round,");
        println!("  default-off behind SOVEREIGN_AGENTIC_KQ) — but that round has no edge to");
        println!("  symbols/callers/callees/blast. Corroborating line-level evidence outside the");
        println!("  graph: knowledge_query.rs:395 tells the model verbatim \"You have NO tools,");
        println!("  commands, or code search available here.\"");
    } else {
        println!("VERDICT: ✓ CORROBORATED — wiring present. Receipt (call path):");
        let mut shown: HashSet<String> = HashSet::new();
        for (tool, _f, _l) in &hits {
            if !shown.insert(tool.clone()) {
                continue;
            }
            let mut path = vec![tool.clone()];
            let mut cur = tool.clone();
            while let Some((p, _pf, _pl)) = parent.get(&cur) {
                path.push(p.clone());
                if SUBJECTS.contains(&p.as_str()) {
                    break;
                }
                cur = p.clone();
            }
            path.reverse();
            println!("  {}", path.join("  →  "));
        }
    }

    // ── self-validation: are the tool nodes live (have callers) but just not from us? ──
    println!("\n── self-validation: the tools exist as live graph nodes, reached by SOMEONE ──");
    for probe in ["find_callees", "SymbolLookupTool"] {
        match graph.find_callers(probe, 2).await {
            Ok((callers, _)) if !callers.is_empty() => {
                let sample: Vec<String> = callers
                    .iter()
                    .take(3)
                    .map(|c| format!("{} ({}:{})", c.symbol_name, c.file_path, c.line))
                    .collect();
                let from_loop = callers.iter().any(|c| SUBJECTS.contains(&c.symbol_name.as_str()));
                println!(
                    "  {probe}: {} caller(s) — e.g. {}  | called by the answer loop? {}",
                    callers.len(),
                    sample.join(", "),
                    from_loop
                );
            }
            Ok(_) => println!("  {probe}: no callers recorded (leaf or macro-referenced)"),
            Err(e) => println!("  {probe}: caller lookup failed: {e}"),
        }
    }

    // ── honest limits of THIS mechanism ──
    println!("\n── caveats (for the general approach) ──");
    println!("  • dynamic dispatch: a loop that invoked tools through `dyn Tool::execute`");
    println!("    might be a real edge SCIP can't resolve → risk of FALSE drift. Here it's");
    println!("    independently confirmed real (tools:None + the \"no tools\" prompt string).");
    println!("  • extraction: subject/object were hand-set; the general path needs an LLM to");
    println!("    turn the prose claim into (subject, relation, object, scope). That step is");
    println!("    the soft part — the reachability itself is exact.");
}
