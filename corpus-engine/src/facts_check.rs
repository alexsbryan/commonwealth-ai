//! Deterministic dispatch over the fact base — the "check" half of spec↔code drift.
//!
//! Given a claim's structured [`Tag`] plus the [`Facts`] and the SCIP call graph, produce a
//! cited [`Verdict`]. This is the Rust port of the Python `fact_pipeline` dispatch, preserving
//! the hard-won **safety invariant**: a DRIFT verdict requires a *cited contradicting fact*
//! (a config field set to the opposite of what the claim asserts); the absence of a fact is
//! always `Unverifiable`, never drift. EXISTS/LITERAL are pure fact lookups (exact — no
//! prose-resolution looseness). CONFIG/CALLS scope to the flow via entry-restricted resolution
//! + the qualified call graph (collision-free file-stem scoping).
//!
//! Only enabled with `treesitter` (which brings `corpus-engine-scip`). See
//! `docs/internal/FACT_BASE_SCALE_OUT.md`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::facts::Facts;
use crate::types::EmbedFn;
use corpus_engine_scip::scip_graph::ScipGraph;

/// A claim's structured tag — the dispatch input (produced by the tagger).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Tag {
    pub relation: String, // EXISTS | LITERAL | CONFIG | CALLS | (other → deferred)
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub field: String,
    #[serde(default)]
    pub literal: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub expected: String, // "YES" (default) | "NO"
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum VerdictKind {
    Drift,
    Corroborated,
    Unverifiable,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub kind: VerdictKind,
    pub receipt: String,
}

impl Verdict {
    fn corrob(r: impl Into<String>) -> Self {
        Verdict {
            kind: VerdictKind::Corroborated,
            receipt: r.into(),
        }
    }
    fn drift(r: impl Into<String>) -> Self {
        Verdict {
            kind: VerdictKind::Drift,
            receipt: r.into(),
        }
    }
    fn unver(r: impl Into<String>) -> Self {
        Verdict {
            kind: VerdictKind::Unverifiable,
            receipt: r.into(),
        }
    }
}

fn expected_present(t: &Tag) -> bool {
    !t.expected.eq_ignore_ascii_case("NO")
}

/// file basename without extension: `.../runtime/knowledge_query.rs` → `knowledge_query`.
fn fstem(path: &str) -> &str {
    let base = path.rsplit('/').next().unwrap_or(path);
    base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base)
}

/// source-file stem from a SCIP qualified id's module path (collision-free, language-agnostic):
/// `...runtime/handlers/knowledge_query/impl#[R]f().` → `knowledge_query`.
fn qstem(q: &str) -> String {
    let path = q.split('#').next().unwrap_or(q).trim();
    path.split('/')
        .rfind(|s| !s.is_empty() && *s != "impl" && *s != "mod")
        .unwrap_or("")
        .to_string()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

// ── EXISTS ─ pure fact lookup, exact only (safe: no prose→garbage resolution) ──
pub fn check_exists(facts: &Facts, target: &str, present: bool) -> Verdict {
    if let Some(d) = facts.fn_defs.iter().find(|d| d.name == target) {
        if present {
            Verdict::corrob(format!("{target} defined at {}:{}", d.file, d.line))
        } else {
            Verdict::drift(format!(
                "{target} exists at {}:{}, claim asserts absence",
                d.file, d.line
            ))
        }
    } else {
        Verdict::unver(format!("{target} not defined"))
    }
}

// ── LITERAL ─ pure fact lookup ──
pub fn check_literal(facts: &Facts, literal: &str, present: bool) -> Verdict {
    if literal.len() < 4 {
        return Verdict::unver("literal too short to match");
    }
    if let Some(s) = facts.str_lits.iter().find(|s| s.content.contains(literal)) {
        if present {
            Verdict::corrob(format!("literal \"{literal}\" at {}:{}", s.file, s.line))
        } else {
            Verdict::drift(format!(
                "literal \"{literal}\" present at {}:{}, claim asserts absence",
                s.file, s.line
            ))
        }
    } else {
        Verdict::unver(format!("literal \"{literal}\" not found"))
    }
}

/// In-memory call adjacency: `caller_qualified → [callee_qualified]`. Built once per corpus so
/// every BFS is HashMap lookups, not one async SQL round-trip per node.
pub type Adjacency = std::collections::HashMap<String, Vec<String>>;

/// Load all qualified call edges once and index them for fast in-memory traversal.
pub async fn build_adjacency(graph: &ScipGraph) -> Adjacency {
    let mut adj: Adjacency = std::collections::HashMap::new();
    if let Ok(edges) = graph.all_qualified_edges().await {
        for (caller, callee) in edges {
            adj.entry(caller).or_default().push(callee);
        }
    }
    adj
}

/// Seed callers whose qualified id contains an entry name and looks like a fn call.
fn seed_from(adj: &Adjacency, entries: &[String]) -> Vec<String> {
    adj.keys()
        .filter(|k| k.trim_end().ends_with("().") && entries.iter().any(|e| k.contains(e.as_str())))
        .cloned()
        .collect()
}

/// The set of source-file stems the flow touches — in-memory qualified BFS (collision-free).
pub fn neighborhood_stems(adj: &Adjacency, entries: &[String], depth: usize) -> HashSet<String> {
    let mut frontier = seed_from(adj, entries);
    let mut seen: HashSet<String> = frontier.iter().cloned().collect();
    for _ in 0..depth {
        let mut next = Vec::new();
        for caller in &frontier {
            if let Some(callees) = adj.get(caller) {
                for c in callees {
                    if !c.is_empty() && seen.insert(c.clone()) && c.trim_end().ends_with("().") {
                        next.push(c.clone());
                    }
                }
            }
        }
        frontier = next;
    }
    seen.iter()
        .filter(|s| s.trim_end().ends_with("()."))
        .map(|s| qstem(s))
        .collect()
}

// ── CONFIG ─ the data-flow drift check; all/none/mixed with the safety invariant ──
pub fn check_config(
    facts: &Facts,
    scope_stems: &HashSet<String>,
    field: &str,
    present: bool,
) -> Verdict {
    let hits: Vec<_> = facts
        .ctor_fields
        .iter()
        .filter(|c| c.field == field && scope_stems.contains(fstem(&c.file)))
        .collect();
    if hits.is_empty() {
        return Verdict::unver(format!("no `{field}` config fact in the resolved scope"));
    }
    // "absent value" is language-specific (Rust: `None`) — the per-language seam alongside the
    // tree-sitter query pack in `facts.rs`. A field set to it = the capability is off here.
    let (mut has_none, mut has_some) = (false, false);
    for c in &hits {
        if c.value.starts_with("None") {
            has_none = true;
        } else {
            has_some = true;
        }
    }
    let c = hits[0];
    if present {
        if has_some && !has_none {
            Verdict::corrob(format!("{field} set present at {}:{}", c.file, c.line))
        } else if has_none && !has_some {
            // cited contradiction: claim asserts present, all scoped sites set it absent
            Verdict::drift(format!(
                "{field}=None at all {} scoped site(s), e.g. {}:{}",
                hits.len(),
                c.file,
                c.line
            ))
        } else {
            Verdict::unver(format!("{field} mixed in scope — impure, abstain"))
        }
    } else {
        Verdict::unver(format!("{field} present, claim asserts absent"))
    }
}

// ── CALLS ─ reachability; absence != drift (safety) — in-memory BFS ──
pub fn reaches(adj: &Adjacency, entries: &[String], target: &str, depth: usize) -> bool {
    let mut frontier = seed_from(adj, entries);
    let mut seen: HashSet<String> = frontier.iter().cloned().collect();
    for _ in 0..depth {
        let mut next = Vec::new();
        for caller in &frontier {
            if let Some(callees) = adj.get(caller) {
                for c in callees {
                    if c.contains(target) {
                        return true;
                    }
                    if seen.insert(c.clone()) {
                        next.push(c.clone());
                    }
                }
            }
        }
        frontier = next;
    }
    false
}

/// Restrict subject resolution to capability ENTRIES (name, vector) — the front-doors, not all
/// 31k — so the claim's predicate can't drift to leaf tool definitions. Returns the top entry.
pub async fn resolve_entry(
    entries: &[(String, Vec<f32>)],
    embed: &EmbedFn,
    text: &str,
) -> Option<String> {
    let q = embed(text).await.ok()?;
    entries
        .iter()
        .max_by(|x, y| {
            cosine(&q, &x.1)
                .partial_cmp(&cosine(&q, &y.1))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(n, _)| n.clone())
}

/// Full dispatch. `graph`/`entries`/`embed` are only needed for CONFIG/CALLS; EXISTS/LITERAL
/// work with `facts` alone. Anything outside the schema → `Unverifiable` (defer to the fuzzy path).
pub async fn check_claim(
    facts: &Facts,
    adj: Option<&Adjacency>,
    entries: &[(String, Vec<f32>)],
    embed: Option<&EmbedFn>,
    claim: &str,
    tag: &Tag,
) -> Verdict {
    let present = expected_present(tag);
    match tag.relation.as_str() {
        "EXISTS" => check_exists(facts, &tag.target, present),
        "LITERAL" => check_literal(facts, &tag.literal, present),
        "CONFIG" => {
            let (adj, embed) = match (adj, embed) {
                (Some(a), Some(e)) => (a, e),
                _ => return Verdict::unver("CONFIG needs the call graph + embedder"),
            };
            let entry = match resolve_entry(entries, embed, claim).await {
                Some(e) => e,
                None => return Verdict::unver("subject did not resolve to an entry"),
            };
            let stems = neighborhood_stems(adj, &[entry], 2);
            check_config(facts, &stems, &tag.field, present)
        }
        "CALLS" => {
            let (adj, embed) = match (adj, embed) {
                (Some(a), Some(e)) => (a, e),
                _ => return Verdict::unver("CALLS needs the call graph + embedder"),
            };
            let entry = resolve_entry(entries, embed, claim)
                .await
                .unwrap_or_default();
            if !entry.is_empty() && reaches(adj, &[entry], &tag.target, 2) {
                Verdict::corrob(format!("scope reaches {}", tag.target))
            } else {
                Verdict::unver(format!("no path to {} (absence != drift)", tag.target))
            }
        }
        other => Verdict::unver(format!("{other} — deferred to the fuzzy path")),
    }
}
