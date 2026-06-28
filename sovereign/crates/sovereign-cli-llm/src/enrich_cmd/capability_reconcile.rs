// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign enrich capability-reconcile <corpus>` — reconcile the DERIVED
//! capabilities against the project's architecture docs. "Drift reports taken to
//! the next level": where the existing drift system reconciles *names* (does this
//! symbol exist?), this reconciles *capabilities* (does the code do what the docs
//! claim, and do the docs describe everything the code does?).
//!
//! Three finding kinds:
//!   * CORROBORATED — a doc references / describes this capability.
//!   * UNDOCUMENTED — no doc describes it. Two-layer: a cheap deterministic pass
//!     (capability spine identifiers ↔ doc backtick-refs) generates candidates,
//!     then an LLM verifies semantically ("does any doc passage describe this
//!     capability's job?") — killing false-undocumenteds that are prose-described
//!     without naming functions.
//!   * DRIFTED — a corroborated capability whose documented claim CONTRADICTS what
//!     the code actually does (judged from the capability's narration).
//!
//! Output: `capability_findings.{md,json}` + a `.fingerprint` over the narrative
//! docs (so `capability_posture` can report freshness, mirroring `drift_*`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use corpus_engine::enrichment::pipeline::{ChatCompletionFn, ChatPrompt};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use super::capability_doc::{impl_type, load_caps, method_name, Cap};
use super::config::EnrichConfig;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use sovereign_cli_shared::help::{self, Help, HelpSection};
use sovereign_tools::code::drift_posture::write_fingerprint;

const LLM_CONCURRENCY: usize = 8;
const RETRIEVE_K: usize = 6;

/// Generic identifiers that must not count as distinctive capability evidence —
/// they leak through `impl#[Type][Trait]` spines and doc prose alike (mirror
/// scratch/recon.py STOP). Anything shorter than 4 chars is also dropped.
const STOP: &[&str] = &[
    "new", "run", "execute", "build", "self", "impl", "from", "into", "next", "drop",
    "Clone", "Default", "Self", "Debug", "Eq", "PartialEq", "Hash", "Copy", "Ord",
    "PartialOrd", "Send", "Sync", "Serialize", "Deserialize", "Display", "Error",
    "Iterator", "IntoIterator", "Result", "Option", "Vec", "String", "Box", "Arc",
    "From", "Into", "TryFrom", "Ordering", "HashMap", "HashSet", "BTreeMap",
];

const VERIFY_SYSTEM: &str = "You decide whether a CAPABILITY is documented. Architecture docs describe what the system DOES, in prose — they almost NEVER name internal function names, and you must NOT require them to. Judge purely by MEANING: does any excerpt explain the same job / purpose / behaviour this capability performs? If an excerpt conveys what this capability is for (even without naming any of its functions), that is DOCUMENTED. Answer UNDOCUMENTED only if no excerpt addresses what this capability actually does. Answer on one line: 'DOCUMENTED: <doc> — <why, <=12 words>' or 'UNDOCUMENTED: <why, <=12 words>'.";

const DRIFT_SYSTEM: &str = "You check whether documentation has DRIFTED from the code. You are given (A) what a capability ACTUALLY does, derived from its code, and (B) excerpts from the architecture docs that describe it. Decide whether any excerpt CONTRADICTS the code behaviour — claims something the code does not do, or describes it working differently (e.g. 'synchronous' when the code is async, 'loops over tools' when the code is single-shot). Report drift ONLY for a real, specific contradiction — NOT mere incompleteness, different wording, or the code computing more detail than the doc summarizes. 'Read-only', 'safe', and 'pure' describe SIDE EFFECTS (no writes or mutation): a tool that reads data and computes, analyzes, or calculates a result is still read-only — that is NOT drift. Answer on one line: 'DRIFT: <the specific contradiction, <=20 words>' or 'OK'.";

const HELP: Help = Help {
    command: "sovereign enrich capability-reconcile",
    summary: "Reconcile derived capabilities against the architecture docs: corroborated / undocumented / drifted.",
    sections: &[
        HelpSection::Usage("sovereign enrich capability-reconcile <corpus-id> [--filter=<label-substring>] [--no-drift] [--render-only]"),
        HelpSection::Flags(&[
            ("<corpus-id>", "An installed code corpus with a capability map (run `sovereign code capability-map` first)."),
            ("--filter=<s>", "Optional: only reconcile capabilities whose label contains this substring."),
            ("--no-drift", "Skip the 5b behavioural-drift pass (faster; corroborated/undocumented only)."),
            ("--render-only", "Re-render capability_findings.md from the existing JSON — no LLM, no daemon."),
        ]),
        HelpSection::Notes(
            "Requires the daemon at localhost:9741. Behavioural drift needs narrations — run \
             `sovereign enrich capability-doc <corpus>` first so corroborated capabilities can be \
             checked claim-vs-code.",
        ),
    ],
};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum FindingKind {
    Drifted,
    Undocumented,
    Corroborated,
}

#[derive(Serialize, Deserialize, Clone)]
struct CapabilityFinding {
    kind: FindingKind,
    label: String,
    n_entries: usize,
    n_core: usize,
    evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    docs: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct FindingSet {
    corpus_id: String,
    corroborated: usize,
    undocumented: usize,
    drifted: usize,
    findings: Vec<CapabilityFinding>,
}

fn is_stop(s: &str) -> bool {
    s.len() < 4 || STOP.contains(&s)
}

/// Distinctive identifiers a capability is "made of": concrete impl types +
/// method names from its core/entries, plus its entry-point reps.
fn cap_idents(cap: &Cap) -> HashSet<String> {
    let mut out = HashSet::new();
    for q in cap.core.iter().chain(cap.entries.iter()) {
        let m = method_name(q);
        if !is_stop(&m) {
            out.insert(m);
        }
        if let Some(t) = impl_type(q) {
            if !is_stop(&t) {
                out.insert(t);
            }
        }
    }
    for r in &cap.reps {
        if !is_stop(r) {
            out.insert(r.clone());
        }
    }
    out
}

/// Identifier tokens in a string (alphanumeric + `_` runs).
fn idents_in(s: &str) -> impl Iterator<Item = &str> {
    s.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty())
}

/// Doc side: every identifier the docs mention inside `backtick` spans → the docs
/// that mention it. Backticks are the docs' deliberate code references.
fn backtick_refs(docs: &[(String, String)]) -> HashMap<String, HashSet<String>> {
    let mut refs: HashMap<String, HashSet<String>> = HashMap::new();
    for (name, text) in docs {
        let bytes = text.as_bytes();
        let mut i = 0;
        while let Some(start) = text[i..].find('`') {
            let s = i + start + 1;
            if let Some(end_rel) = text[s..].find('`') {
                let span = &text[s..s + end_rel];
                for tok in idents_in(span) {
                    if !is_stop(tok) {
                        refs.entry(tok.to_string()).or_default().insert(name.clone());
                    }
                }
                i = s + end_rel + 1;
            } else {
                break;
            }
        }
        let _ = bytes;
    }
    refs
}

/// Split docs into non-trivial paragraphs for keyword retrieval.
fn doc_paragraphs(docs: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, text) in docs {
        for para in text.split("\n\n") {
            let p = para.trim();
            if p.len() > 60 {
                out.push((name.clone(), p.to_string()));
            }
        }
    }
    out
}

/// Top-k paragraphs by keyword hit count (cheap BM25-lite).
fn retrieve<'a>(paras: &'a [(String, String)], keywords: &[String], k: usize) -> Vec<&'a (String, String)> {
    let kws: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();
    let mut scored: Vec<(usize, &(String, String))> = paras
        .iter()
        .map(|p| {
            let low = p.1.to_lowercase();
            let score = kws.iter().map(|k| low.matches(k.as_str()).count()).sum::<usize>();
            (score, p)
        })
        .filter(|(s, _)| *s > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(k).map(|(_, p)| p).collect()
}

/// Keywords identifying a capability for retrieval: label segments + rep names.
fn cap_keywords(cap: &Cap) -> Vec<String> {
    let mut kws: Vec<String> = cap.label.split(['/', '_']).map(|s| s.to_string()).collect();
    kws.extend(cap.reps.iter().flat_map(|r| r.split('_').map(|s| s.to_string())));
    kws.into_iter().filter(|k| k.len() >= 4).collect()
}

fn discover_docs(source: &Path) -> Vec<PathBuf> {
    let mut docs = Vec::new();
    for rel in [
        "sovereign/SYSTEM_OVERVIEW.md",
        "sovereign/ARCH_PRINCIPLES.md",
        "SYSTEM_OVERVIEW.md",
        "ARCH_PRINCIPLES.md",
        "ARCHITECTURE.md",
        "README.md",
    ] {
        let p = source.join(rel);
        if p.is_file() {
            docs.push(p);
        }
    }
    for dir in ["sovereign/docs", "docs"] {
        if let Ok(rd) = fs::read_dir(source.join(dir)) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) == Some("md") {
                    docs.push(p);
                }
            }
        }
    }
    docs.sort();
    docs.dedup();
    docs
}

fn read_source_path(corpus_dir: &Path) -> Option<PathBuf> {
    let raw = fs::read_to_string(corpus_dir.join("_corpus_meta.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("source_path").and_then(|s| s.as_str()).map(PathBuf::from)
}

/// label → first ~2 paragraphs of its narration (the best "what it actually does").
fn load_narrations(caps_dir: &Path) -> HashMap<String, String> {
    #[derive(Deserialize)]
    struct Doc {
        capabilities: Vec<Sec>,
    }
    #[derive(Deserialize)]
    struct Sec {
        label: String,
        #[serde(default)]
        narration: String,
    }
    match fs::read_to_string(caps_dir.join("capability_doc.json")) {
        Ok(s) => serde_json::from_str::<Doc>(&s)
            .map(|d| d.capabilities.into_iter().map(|c| (c.label, c.narration)).collect())
            .unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn parse_flag(args: &[String], key: &str) -> Option<String> {
    let eq = format!("{key}=");
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix(&eq) {
            return Some(v.to_string());
        }
        if a == key {
            return args.get(i + 1).cloned();
        }
    }
    None
}

async fn ask(chat: &ChatCompletionFn, system: &str, user: String) -> String {
    let prompt = ChatPrompt::new(system, user).with_temperature(0.1).with_max_output_tokens(120);
    match (chat)(&prompt).await {
        Ok(s) => s.trim().to_string(),
        Err(e) => format!("[verify unavailable: {e}]"),
    }
}

fn render_markdown(set: &FindingSet) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {} — Capability Reconciliation (derived vs docs)\n\n", set.corpus_id));
    s.push_str(&format!(
        "_Derived capabilities reconciled against the architecture docs — {} corroborated · {} undocumented · {} drifted. \
         Regenerate with `sovereign enrich capability-reconcile {}`._\n\n",
        set.corroborated, set.undocumented, set.drifted, set.corpus_id,
    ));

    let by = |k: FindingKind| set.findings.iter().filter(move |f| f.kind == k);

    if set.drifted > 0 {
        s.push_str(&format!("## ⚠ Drift — docs contradict the code ({})\n\n", set.drifted));
        for f in by(FindingKind::Drifted) {
            s.push_str(&format!("- **{}** — {}", f.label, f.evidence));
            if let Some(d) = &f.docs {
                s.push_str(&format!("  _[{}]_", d));
            }
            s.push('\n');
        }
        s.push('\n');
    }

    // Undocumented: collapse repeated dominant-labels (sub-capabilities under one
    // capability area share a label) into one crisp line per area, so the human/demo
    // view reads cleanly. The JSON mirror below keeps every finding for the tools.
    let mut undoc: std::collections::BTreeMap<&str, (usize, usize, &str)> = std::collections::BTreeMap::new();
    for f in by(FindingKind::Undocumented) {
        let e = undoc.entry(f.label.as_str()).or_insert((0, 0, ""));
        e.0 += 1;
        e.1 += f.n_entries;
        if e.2.is_empty() {
            e.2 = f.evidence.as_str();
        }
    }
    s.push_str(&format!(
        "## ⚠ Undocumented capabilities ({} across {} areas)\n\n",
        set.undocumented,
        undoc.len()
    ));
    s.push_str("_Things the system does that no architecture doc describes (LLM-verified)._\n\n");
    for (label, (n, entries, note)) in &undoc {
        if *n > 1 {
            s.push_str(&format!("- **{}** — {} sub-capabilities ({}e) — {}\n", label, n, entries, note));
        } else {
            s.push_str(&format!("- **{}** ({}e) — {}\n", label, entries, note));
        }
    }
    s.push('\n');

    s.push_str(&format!("## ✓ Corroborated ({})\n\n", set.corroborated));
    for f in by(FindingKind::Corroborated) {
        s.push_str(&format!("- {} — {}", f.label, f.evidence));
        if let Some(d) = &f.docs {
            s.push_str(&format!("  _[{}]_", d));
        }
        s.push('\n');
    }
    s.push('\n');
    s
}

pub async fn cmd_capability_reconcile(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&HELP);
        return 0;
    }
    let Some(query) = args.iter().find(|a| !a.starts_with('-')) else {
        eprintln!("error: missing <corpus-id>");
        eprintln!();
        help::print(&HELP);
        return 2;
    };
    let filter = parse_flag(args, "--filter");
    let do_drift = !args.iter().any(|a| a == "--no-drift");

    let data_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".sovereign"));
    let indexes_dir = data_dir.join("indexes");
    let corpus_id = match crate::corpus_resolve::resolve_corpus_id(&indexes_dir, query) {
        Ok(id) => id,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 1;
        }
    };
    let corpus_dir = indexes_dir.join(&corpus_id);
    let caps_dir = data_dir.join("capabilities").join(&corpus_id);

    // --render-only: regenerate the markdown view from the existing FindingSet
    // JSON (e.g. after a render/dedup tweak) without re-running the LLM stages
    // or touching the daemon. The JSON is the source of truth; the .md is a view.
    if args.iter().any(|a| a == "--render-only") {
        let json_path = caps_dir.join("capability_findings.json");
        let raw = match fs::read_to_string(&json_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: reading {} (run the full reconcile first): {e}", json_path.display());
                return 1;
            }
        };
        let set: FindingSet = match serde_json::from_str(&raw) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: parsing {}: {e}", json_path.display());
                return 1;
            }
        };
        let md_path = caps_dir.join("capability_findings.md");
        if let Err(e) = fs::write(&md_path, render_markdown(&set)) {
            eprintln!("error: writing {}: {e}", md_path.display());
            return 1;
        }
        println!(
            "capability-reconcile: re-rendered {} corroborated · {} undocumented · {} drifted → {}",
            set.corroborated, set.undocumented, set.drifted, md_path.display()
        );
        return 0;
    }

    let mut caps = match load_caps(&caps_dir.join("capability_map.json")) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if let Some(f) = &filter {
        caps.retain(|c| c.label.contains(f.as_str()));
    }

    // Doc side: discover + read the architecture narratives.
    let Some(source) = read_source_path(&corpus_dir) else {
        eprintln!("error: no source_path in {}/_corpus_meta.json", corpus_dir.display());
        return 1;
    };
    let doc_paths = discover_docs(&source);
    if doc_paths.is_empty() {
        eprintln!("error: no architecture docs found under {}", source.display());
        return 1;
    }
    let docs: Vec<(String, String)> = doc_paths
        .iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().to_string();
            let text = fs::read_to_string(p).ok()?;
            Some((name, text))
        })
        .collect();
    let refs = backtick_refs(&docs);
    let paras = doc_paragraphs(&docs);
    println!(
        "capability-reconcile: {corpus_id} — {} capabilities vs {} docs ({} backtick refs)",
        caps.len(),
        docs.len(),
        refs.len(),
    );

    // ── 5a deterministic: corroborated vs undocumented-candidate ──
    let mut corroborated: Vec<(Cap, String, String)> = Vec::new(); // (cap, matched idents, docs)
    let mut undoc_candidates: Vec<Cap> = Vec::new();
    for cap in &caps {
        let idents = cap_idents(cap);
        let mut matched: Vec<&String> = idents.iter().filter(|i| refs.contains_key(*i)).collect();
        if matched.is_empty() {
            undoc_candidates.push(cap.clone());
        } else {
            matched.sort();
            let mut docset: HashSet<String> = HashSet::new();
            for m in &matched {
                if let Some(d) = refs.get(*m) {
                    docset.extend(d.iter().cloned());
                }
            }
            let mut dv: Vec<String> = docset.into_iter().collect();
            dv.sort();
            let ev = matched.iter().take(4).map(|s| s.as_str()).collect::<Vec<_>>().join(", ");
            corroborated.push((cap.clone(), ev, dv.into_iter().take(3).collect::<Vec<_>>().join(", ")));
        }
    }

    // Build the LLM provider only if there's semantic work to do.
    let cfg = match EnrichConfig::require(&corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if !probe_daemon(&cfg.base_url).await {
        eprintln!("error: daemon is not responding at {} — start it first", cfg.base_url);
        return 2;
    }
    let client = match DaemonInferenceClient::from_enrich_config(&cfg) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: building daemon client: {e}");
            return 1;
        }
    };
    let (_embed, chat) = client.into_closures();
    let narrations = load_narrations(&caps_dir);

    // ── 5b verify: which undocumented-candidates are TRULY undocumented? ──
    println!(
        "capability-reconcile: 5a → {} corroborated, {} undocumented-candidates; verifying semantically…",
        corroborated.len(),
        undoc_candidates.len(),
    );
    let verified: Vec<(Cap, bool, String)> = stream::iter(undoc_candidates.into_iter().map(|cap| {
        let chat = chat.clone();
        let paras = &paras;
        let narrations = &narrations;
        async move {
            let kws = cap_keywords(&cap);
            let hits = retrieve(paras, &kws, RETRIEVE_K);
            let excerpts = hits
                .iter()
                .map(|(n, p)| format!("[{n}] {}", p.chars().take(450).collect::<String>()))
                .collect::<Vec<_>>()
                .join("\n\n");
            let identity = narrations
                .get(&cap.label)
                .map(|n| format!("{} — {}", cap.label, n.chars().take(400).collect::<String>()))
                .unwrap_or_else(|| {
                    let methods: Vec<String> =
                        cap.core.iter().chain(cap.entries.iter()).map(|q| method_name(q)).filter(|m| !is_stop(m)).take(10).collect();
                    format!("{} — functions: {}", cap.label, methods.join(", "))
                });
            let excerpts = if excerpts.is_empty() { "(no relevant excerpts found)".to_string() } else { excerpts };
            let verdict = ask(
                &chat,
                VERIFY_SYSTEM,
                format!("CAPABILITY:\n{identity}\n\nDOC EXCERPTS:\n{excerpts}\n\nVerdict:"),
            )
            .await;
            let documented = verdict.to_ascii_uppercase().starts_with("DOCUMENTED");
            (cap, documented, verdict)
        }
    }))
    .buffer_unordered(LLM_CONCURRENCY)
    .collect()
    .await;

    // ── 5b drift: corroborated capabilities whose docs contradict the code ──
    let mut drift_findings: Vec<(String, String, String)> = Vec::new(); // (label, contradiction, docs)
    if do_drift {
        let drift_targets: Vec<&(Cap, String, String)> =
            corroborated.iter().filter(|(c, _, _)| narrations.contains_key(&c.label)).collect();
        println!("capability-reconcile: 5b drift → checking {} corroborated capabilities with narrations…", drift_targets.len());
        let drift_results: Vec<Option<(String, String, String)>> = stream::iter(drift_targets.into_iter().map(|(cap, _ev, docs)| {
            let chat = chat.clone();
            let paras = &paras;
            let narrations = &narrations;
            async move {
                let narration = narrations.get(&cap.label)?;
                let kws = cap_keywords(cap);
                let hits = retrieve(paras, &kws, RETRIEVE_K);
                if hits.is_empty() {
                    return None;
                }
                let excerpts = hits
                    .iter()
                    .map(|(n, p)| format!("[{n}] {}", p.chars().take(450).collect::<String>()))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let verdict = ask(
                    &chat,
                    DRIFT_SYSTEM,
                    format!(
                        "(A) WHAT THE CAPABILITY ACTUALLY DOES:\n{}\n\n(B) DOC EXCERPTS:\n{}\n\nVerdict:",
                        narration.chars().take(700).collect::<String>(),
                        excerpts,
                    ),
                )
                .await;
                if verdict.to_ascii_uppercase().starts_with("DRIFT") {
                    let contradiction = verdict.splitn(2, ':').nth(1).unwrap_or(&verdict).trim().to_string();
                    Some((cap.label.clone(), contradiction, docs.clone()))
                } else {
                    None
                }
            }
        }))
        .buffer_unordered(LLM_CONCURRENCY)
        .collect()
        .await;
        drift_findings = drift_results.into_iter().flatten().collect();
    }

    // ── assemble findings ──
    let mut findings: Vec<CapabilityFinding> = Vec::new();
    let drifted_labels: HashSet<&String> = drift_findings.iter().map(|(l, _, _)| l).collect();

    for (label, contradiction, docs) in &drift_findings {
        let cap = caps.iter().find(|c| &c.label == label);
        findings.push(CapabilityFinding {
            kind: FindingKind::Drifted,
            label: label.clone(),
            n_entries: cap.map(|c| c.n_entries).unwrap_or(0),
            n_core: cap.map(|c| c.n_core).unwrap_or(0),
            evidence: contradiction.clone(),
            docs: Some(docs.clone()),
        });
    }
    for (cap, documented, verdict) in &verified {
        if *documented {
            // prose-documented after all → corroborated by meaning
            findings.push(CapabilityFinding {
                kind: FindingKind::Corroborated,
                label: cap.label.clone(),
                n_entries: cap.n_entries,
                n_core: cap.n_core,
                evidence: format!("described in prose — {}", verdict.splitn(2, ':').nth(1).unwrap_or("").trim()),
                docs: None,
            });
        } else {
            findings.push(CapabilityFinding {
                kind: FindingKind::Undocumented,
                label: cap.label.clone(),
                n_entries: cap.n_entries,
                n_core: cap.n_core,
                evidence: verdict.splitn(2, ':').nth(1).unwrap_or(&verdict).trim().to_string(),
                docs: None,
            });
        }
    }
    for (cap, ev, docs) in &corroborated {
        if drifted_labels.contains(&cap.label) {
            continue; // already surfaced as a (more severe) drift finding
        }
        findings.push(CapabilityFinding {
            kind: FindingKind::Corroborated,
            label: cap.label.clone(),
            n_entries: cap.n_entries,
            n_core: cap.n_core,
            evidence: format!("docs reference {}", ev),
            docs: if docs.is_empty() { None } else { Some(docs.clone()) },
        });
    }

    let set = FindingSet {
        corpus_id: corpus_id.clone(),
        corroborated: findings.iter().filter(|f| f.kind == FindingKind::Corroborated).count(),
        undocumented: findings.iter().filter(|f| f.kind == FindingKind::Undocumented).count(),
        drifted: findings.iter().filter(|f| f.kind == FindingKind::Drifted).count(),
        findings,
    };

    if let Err(e) = fs::create_dir_all(&caps_dir) {
        eprintln!("error: creating {}: {e}", caps_dir.display());
        return 1;
    }
    let md_path = caps_dir.join("capability_findings.md");
    let json_path = caps_dir.join("capability_findings.json");
    if let Err(e) = fs::write(&md_path, render_markdown(&set)) {
        eprintln!("error: writing {}: {e}", md_path.display());
        return 1;
    }
    match serde_json::to_string_pretty(&set) {
        Ok(j) => {
            if let Err(e) = fs::write(&json_path, j) {
                eprintln!("error: writing {}: {e}", json_path.display());
                return 1;
            }
        }
        Err(e) => {
            eprintln!("error: serializing findings: {e}");
            return 1;
        }
    }
    // Freshness fingerprint over the narrative docs (mirrors drift; powers capability_posture).
    if let Err(e) = write_fingerprint(&caps_dir, &doc_paths, &md_path) {
        eprintln!("warning: could not write fingerprint: {e}");
    }

    println!(
        "capability-reconcile: {} corroborated · {} undocumented · {} drifted → {}",
        set.corroborated,
        set.undocumented,
        set.drifted,
        md_path.display(),
    );
    0
}
