// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich capability-doc <corpus>` — narrate each derived capability
//! into a grounded, `file:line`-cited architecture document.
//!
//! Inputs (both already on disk):
//!   * the capability map  — `code capability-map` → `capabilities/<corpus>/capability_map.json`
//!   * the code-intel cache — `enrich code-intel`  → `indexes/<corpus>/code_intel_cache.json`
//!
//! Per capability: join its core spine to each function's plain-English summary,
//! prompt the daemon model to narrate WHAT it does / HOW it works / WHAT it leans
//! on — constrained to the spine symbols — then a deterministic grounding pass
//! flags any narrated code identifier that isn't in the spine (glassbox: flag,
//! don't retry). Output: `capability_doc.md` (+ `.json`), every section citing
//! `file:line`. Narration runs concurrently (ready for a multi-seq serving slot);
//! per-capability spine body-hashes gate incremental reuse across runs.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;

use corpus_engine::enrichment::code_intel::SymbolEnrichment;
use corpus_engine::enrichment::pipeline::{ChatCompletionFn, ChatPrompt};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use super::config::EnrichConfig;
use super::inference_client::{probe_daemon, DaemonInferenceClient};
use sovereign_cli_shared::help::{self, Help, HelpSection};

/// Concurrent narration calls in flight. Bounded so the daemon slot isn't
/// flooded; a multi-seq serving slot (n_seq_max>1) turns this into real parallelism.
const NARRATION_CONCURRENCY: usize = 8;
/// Cap on spine functions fed into one narration prompt (bounds prompt size).
const MAX_SPINE: usize = 40;

/// The validated narration prompt (scratch/narrate.py): accurate, spine-constrained.
const NARRATION_SYSTEM: &str = "You are writing ONE section of an architecture document for a software project. You are given a CAPABILITY — a thing the system does — defined by its entry points and the core functions behind it (each with a one-line summary), plus the shared services it relies on.

Write a clear, accurate prose section (2-4 short paragraphs):
1. One sentence on what this capability does for someone.
2. How it works — walk the core functions in a sensible order, in plain language.
3. What shared infrastructure it leans on.

Ground every statement in the functions and summaries provided. Do NOT invent functions, types, or behaviour that are not listed. Be accurate and specific, not flowery.";

const HELP: Help = Help {
    command: "svrn enrich capability-doc",
    summary: "Narrate every derived capability into a grounded, file:line-cited architecture document.",
    sections: &[
        HelpSection::Usage("svrn enrich capability-doc <corpus-id> [--filter=<label-substring>]"),
        HelpSection::Flags(&[
            (
                "<corpus-id>",
                "An installed code corpus with a capability map (run `svrn code capability-map \
                 <corpus>` first) and code-intel summaries (run `svrn enrich code-intel <corpus>`).",
            ),
            (
                "--filter=<s>",
                "Optional: narrate only capabilities whose label contains this substring (e.g. \
                 --filter=code). Empty = every capability with summaries.",
            ),
        ]),
        HelpSection::Notes(
            "Requires the daemon at localhost:9741. Capabilities whose core functions have no \
             code-intel summary yet are skipped (run `enrich code-intel` to cover them).",
        ),
    ],
};

// ── capability-map artifact (the JSON is the interface; deserialize locally) ──
#[derive(Deserialize)]
struct CapMapDoc {
    capabilities: Vec<Cap>,
}

#[derive(Deserialize, Clone)]
pub struct Cap {
    pub label: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub n_entries: usize,
    #[serde(default)]
    pub n_core: usize,
    #[serde(default)]
    pub entries: Vec<String>,
    #[serde(default)]
    pub core: Vec<String>,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub reps: Vec<String>,
}

// ── output artifact ──
#[derive(Serialize, Deserialize, Default)]
struct CapabilityDoc {
    corpus_id: String,
    capabilities: Vec<CapSection>,
}

#[derive(Serialize, Deserialize, Clone)]
struct CapSection {
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
    n_entries: usize,
    n_core: usize,
    /// Hash of the spine's body-hashes — unchanged spine ⇒ reuse prior narration.
    spine_hash: String,
    narration: String,
    spine: Vec<SpineFn>,
    /// Grounding: code identifiers the narration named that aren't in the spine.
    #[serde(default)]
    off_spine_mentions: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct SpineFn {
    name: String,
    file: String,
    line: u32,
    summary: String,
}

/// Owned narration input — built before the concurrent pass so tasks borrow nothing.
struct NarrationInput {
    cap: Cap,
    spine: Vec<SpineFn>,
    spine_hash: String,
}

// ── SCIP qualified-id helpers (mirror scratch/narrate.py) ──

/// `… <pkg> <ver> <descriptor>` → the bare method/field name (`find_callers`).
pub fn method_name(qualified: &str) -> String {
    let t: Vec<&str> = qualified.split(' ').collect();
    let desc = if t.len() >= 5 {
        t[4..].join(" ")
    } else {
        qualified.to_string()
    };
    let leaf = desc.rsplit('/').next().unwrap_or(&desc);
    let after = leaf.rsplit([']', '#']).next().unwrap_or(leaf);
    after
        .split('(')
        .next()
        .unwrap_or(after)
        .trim_end_matches('.')
        .to_string()
}

/// The concrete impl type from `impl#[TypeName]…`, if present.
pub fn impl_type(qualified: &str) -> Option<String> {
    let start = qualified.find("#[")?;
    let rest = &qualified[start + 2..];
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

/// Stable hash over a spine's (sorted) body-hashes, for incremental reuse.
fn spine_hash(body_hashes: &[String]) -> String {
    let mut sorted: Vec<&String> = body_hashes.iter().collect();
    sorted.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for b in sorted {
        b.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

/// Identifiers in `text` that look like code (snake_case or camelCase, ≥4 chars).
fn code_idents(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.into_iter().filter(|t| is_code_shaped(t)).collect()
}

fn is_code_shaped(t: &str) -> bool {
    if t.len() < 4 {
        return false;
    }
    if t.contains('_') {
        return true;
    }
    let b = t.as_bytes();
    (1..b.len()).any(|i| b[i - 1].is_ascii_lowercase() && b[i].is_ascii_uppercase())
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

pub fn load_caps(path: &Path) -> Result<Vec<Cap>, String> {
    let s = fs::read_to_string(path).map_err(|_| {
        format!(
            "no capability map at {} — run `svrn code capability-map` first",
            path.display()
        )
    })?;
    let doc: CapMapDoc =
        serde_json::from_str(&s).map_err(|e| format!("parsing {}: {e}", path.display()))?;
    Ok(doc.capabilities)
}

fn load_cache_by_qn(path: &Path) -> HashMap<String, SymbolEnrichment> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str::<Vec<SymbolEnrichment>>(&s)
            .map(|v| {
                v.into_iter()
                    .map(|e| (e.meta.qualified_name.clone(), e))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn load_prior(path: &Path) -> HashMap<String, CapSection> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str::<CapabilityDoc>(&s)
            .map(|d| {
                d.capabilities
                    .into_iter()
                    .map(|c| (c.label.clone(), c))
                    .collect()
            })
            .unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn build_input(cap: &Cap, cache: &HashMap<String, SymbolEnrichment>) -> NarrationInput {
    // Gather the SUMMARIZED core (skipping unenriched fns), then cap at MAX_SPINE.
    // `core` is sorted, so a raw `take(MAX_SPINE)` can land entirely on functions
    // from a crate that hasn't been enriched yet — filter on cache hits first.
    let mut spine = Vec::new();
    let mut body_hashes = Vec::new();
    for q in &cap.core {
        if let Some(e) = cache.get(q) {
            spine.push(SpineFn {
                name: method_name(q),
                file: e.meta.file_path.clone(),
                line: e.meta.line_start,
                summary: e.summary.clone(),
            });
            body_hashes.push(e.body_hash.clone());
            if spine.len() >= MAX_SPINE {
                break;
            }
        }
    }
    NarrationInput {
        cap: cap.clone(),
        spine,
        spine_hash: spine_hash(&body_hashes),
    }
}

fn build_prompt(inp: &NarrationInput) -> ChatPrompt {
    let core_lines: Vec<String> = inp
        .spine
        .iter()
        .map(|f| format!("- {}: {}", f.name, f.summary))
        .collect();
    let deps: Vec<String> = inp
        .cap
        .deps
        .iter()
        .take(8)
        .map(|d| method_name(d))
        .collect();
    let user = format!(
        "CAPABILITY: {}\nENTRY POINTS (verbs): {}\n\nCORE FUNCTIONS:\n{}\n\nSHARED SERVICES IT USES: {}\n\nWrite the section.",
        inp.cap.label,
        inp.cap.reps.join(", "),
        core_lines.join("\n"),
        deps.join(", "),
    );
    ChatPrompt::new(NARRATION_SYSTEM, user)
        .with_temperature(0.3)
        .with_max_output_tokens(650)
}

/// Deterministic grounding: code identifiers in the narration not in the spine.
fn off_spine_mentions(text: &str, inp: &NarrationInput) -> Vec<String> {
    let mut allowed: HashSet<String> = HashSet::new();
    for f in &inp.spine {
        allowed.insert(f.name.clone());
    }
    for q in inp.cap.deps.iter().chain(inp.cap.core.iter()) {
        allowed.insert(method_name(q));
        if let Some(ty) = impl_type(q) {
            allowed.insert(ty);
        }
    }
    for r in &inp.cap.reps {
        allowed.insert(r.clone());
    }
    let mut flagged = Vec::new();
    let mut seen = HashSet::new();
    for tok in code_idents(text) {
        if !allowed.contains(&tok) && seen.insert(tok.clone()) {
            flagged.push(tok);
        }
    }
    flagged
}

async fn narrate_one(chat: &ChatCompletionFn, inp: NarrationInput) -> CapSection {
    let prompt = build_prompt(&inp);
    let narration = match (chat)(&prompt).await {
        Ok(s) => s.trim().to_string(),
        Err(e) => format!("[narration unavailable: {e}]"),
    };
    let off_spine_mentions = off_spine_mentions(&narration, &inp);
    CapSection {
        label: inp.cap.label,
        parent: inp.cap.parent,
        n_entries: inp.cap.n_entries,
        n_core: inp.cap.n_core,
        spine_hash: inp.spine_hash,
        narration,
        spine: inp.spine,
        off_spine_mentions,
    }
}

fn render_markdown(doc: &CapabilityDoc) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "# {} — Capability Architecture (derived)\n\n",
        doc.corpus_id
    ));
    s.push_str(&format!(
        "_Derived from the SCIP call graph + code-intel summaries — {} capabilities. Every spine \
         function cites `file:line`. Regenerate with `svrn enrich capability-doc {}`._\n\n",
        doc.capabilities.len(),
        doc.corpus_id,
    ));
    for c in &doc.capabilities {
        let suffix = if c.parent.is_some() {
            "  ·  sub-capability"
        } else {
            ""
        };
        s.push_str(&format!("## {}{}\n", c.label, suffix));
        s.push_str(&format!(
            "_{} entr{} · {} core fn{}_\n\n",
            c.n_entries,
            if c.n_entries == 1 { "y" } else { "ies" },
            c.n_core,
            if c.n_core == 1 { "" } else { "s" },
        ));
        s.push_str(&c.narration);
        s.push_str("\n\n");
        if !c.spine.is_empty() {
            s.push_str("**Spine:**\n\n");
            for f in &c.spine {
                s.push_str(&format!(
                    "- `{}` — {} ({}:{})\n",
                    f.name, f.summary, f.file, f.line
                ));
            }
            s.push('\n');
        }
        if !c.off_spine_mentions.is_empty() {
            s.push_str(&format!(
                "> ⚠ grounding — narration named {} identifier(s) not in this capability's spine: {}\n\n",
                c.off_spine_mentions.len(),
                c.off_spine_mentions.join(", "),
            ));
        }
    }
    s
}

pub async fn cmd_capability_doc(args: &[String]) -> i32 {
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

    let caps = match load_caps(&caps_dir.join("capability_map.json")) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let cache = load_cache_by_qn(&corpus_dir.join("code_intel_cache.json"));
    if cache.is_empty() {
        eprintln!(
            "error: no code-intel summaries for '{corpus_id}' — run `svrn enrich code-intel {corpus_id}` first"
        );
        return 1;
    }

    let cfg = match EnrichConfig::require(&corpus_id) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if !probe_daemon(&cfg.base_url).await {
        eprintln!(
            "error: daemon is not responding at {} — start it first",
            cfg.base_url
        );
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

    let prior = load_prior(&caps_dir.join("capability_doc.json"));

    // Build owned inputs; skip capabilities with no summarized core (not enriched).
    let mut sections: Vec<CapSection> = Vec::new();
    let mut to_narrate: Vec<NarrationInput> = Vec::new();
    let mut skipped = 0usize;
    for cap in &caps {
        if let Some(f) = &filter {
            if !cap.label.contains(f.as_str()) {
                continue;
            }
        }
        let inp = build_input(cap, &cache);
        if inp.spine.is_empty() {
            skipped += 1;
            continue;
        }
        match prior.get(&inp.cap.label) {
            Some(p) if p.spine_hash == inp.spine_hash && !p.narration.is_empty() => {
                sections.push(p.clone());
            }
            _ => to_narrate.push(inp),
        }
    }

    let total = sections.len() + to_narrate.len();
    if total == 0 {
        eprintln!(
            "error: no capabilities to narrate (skipped {skipped} with no summarized core). \
             Run `enrich code-intel {corpus_id}` to cover them."
        );
        return 1;
    }
    println!(
        "capability-doc: {corpus_id} — {total} capabilities to document ({} reused, {} to narrate, {skipped} skipped: no summaries)",
        sections.len(),
        to_narrate.len(),
    );

    let narrated: Vec<CapSection> = stream::iter(to_narrate.into_iter().map(|inp| {
        let chat = chat.clone();
        async move { narrate_one(&chat, inp).await }
    }))
    .buffer_unordered(NARRATION_CONCURRENCY)
    .collect()
    .await;
    sections.extend(narrated);

    // Top-level capabilities first, then by breadth (entries) and depth (core).
    sections.sort_by(|a, b| {
        a.parent
            .is_some()
            .cmp(&b.parent.is_some())
            .then(b.n_entries.cmp(&a.n_entries))
            .then(b.n_core.cmp(&a.n_core))
            .then(a.label.cmp(&b.label))
    });

    let doc = CapabilityDoc {
        corpus_id: corpus_id.clone(),
        capabilities: sections,
    };
    if let Err(e) = fs::create_dir_all(&caps_dir) {
        eprintln!("error: creating {}: {e}", caps_dir.display());
        return 1;
    }
    let md_path = caps_dir.join("capability_doc.md");
    let json_path = caps_dir.join("capability_doc.json");
    if let Err(e) = fs::write(&md_path, render_markdown(&doc)) {
        eprintln!("error: writing {}: {e}", md_path.display());
        return 1;
    }
    match serde_json::to_string_pretty(&doc) {
        Ok(j) => {
            if let Err(e) = fs::write(&json_path, j) {
                eprintln!("error: writing {}: {e}", json_path.display());
                return 1;
            }
        }
        Err(e) => {
            eprintln!("error: serializing doc: {e}");
            return 1;
        }
    }

    let flagged = doc
        .capabilities
        .iter()
        .filter(|c| !c.off_spine_mentions.is_empty())
        .count();
    println!(
        "capability-doc: wrote {} sections to {} ({flagged} flagged for off-spine mentions to review)",
        doc.capabilities.len(),
        md_path.display(),
    );
    0
}
