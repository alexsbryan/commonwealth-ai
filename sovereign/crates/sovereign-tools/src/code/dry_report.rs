// SPDX-License-Identifier: AGPL-3.0-or-later
//! `dry_report` — semantic-duplication ("DRY") report from the code embeddings.
//!
//! Answers "where is this codebase repeating itself?" — copy-paste and
//! near-duplicate functions a human should consider factoring into one place.
//!
//! Two independent tiers, on purpose — each has a blind spot the other covers:
//!
//!   1. EXACT clones — read every function definition from the SCIP graph
//!      (`iter_all_symbols`), slice its body from source, normalize (trim +
//!      drop blank lines, so formatting-only differences still match), and group
//!      by hash. Byte/near-byte identical bodies (Type-1/light-2); zero
//!      embedding math.
//!
//!      CRITICAL: this tier reads SOURCE, not the LanceDB chunk index. The chunk
//!      index is `chunks_deduped: true` — byte-identical chunks are collapsed to
//!      ONE row before any reader sees them, so an index-based exact tier is
//!      structurally blind to exact duplication (it once reported 1 where there
//!      were 15 identical `now()` helpers). The SCIP graph records every
//!      definition un-deduped, so it is the correct source of truth here. This
//!      tier also uses its OWN low line floor (`EXACT_MIN_LINES`), independent of
//!      the near tier's `min_lines` — a 5-line helper copied 15× is a real DRY
//!      problem even though it is far below the semantic-noise floor.
//!
//!   2. NEAR clones — cosine similarity ≥ threshold over the per-symbol
//!      embeddings in the chunk index. Catches reworded / semantically-similar
//!      copies (Type-3/4) that no hash can. A size-ratio prefilter prunes
//!      implausible pairs and the pairwise pass is parallelised with
//!      `std::thread::scope` (no new dep). Symbols already reported as exact
//!      clones are excluded so they are not double-counted. The `min_lines`
//!      floor suppresses boilerplate noise HERE (where fuzzy matching is prone
//!      to it) — not on the exact tier.
//!
//! Near-clone edges are unioned into clusters (a connected component = one group
//! of things that are all mutually-ish similar). Advisory only: it prints the
//! groups with `file:line` for each member; a human decides what to factor out.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use corpus_engine::index::CorpusIndex;
use corpus_engine_scip::capability_map::{is_function, pkg_and_desc};
use corpus_engine_scip::ScipGraph;
use sovereign_core::error::{Error, Result};

/// Skip symbols shorter than this — trivial getters/one-liners are "duplicated"
/// everywhere and are noise, not a DRY problem. Applies to the NEAR (embedding)
/// tier only.
pub const DEFAULT_MIN_LINES: usize = 8;
/// The EXACT tier's own floor — deliberately low. A 5-line helper copied N times
/// is real duplication (this is the length band `DEFAULT_MIN_LINES` hid), but
/// 1–2 line bodies are too trivial to be worth reporting.
const EXACT_MIN_LINES: usize = 3;
/// Cosine ≥ this counts two symbols as near-duplicates. High bar: Rust is full
/// of structurally-similar boilerplate, and we want copy-paste, not "both are
/// match statements".
pub const DEFAULT_NEAR_THRESHOLD: f32 = 0.95;
/// Two symbols whose lengths differ by more than this ratio are not compared —
/// clones keep roughly the same size (observed: real cross-crate clones cluster
/// within 1.0–1.3×), and this bounds the O(n²) pairwise work.
const SIZE_RATIO: f32 = 1.5;

pub struct DryInputs<'a> {
    /// Corpus index directory (`~/.sovereign/indexes/<corpus_id>`).
    pub index_path: &'a Path,
    pub corpus_id: &'a str,
    /// Only symbols at least this many lines are considered.
    pub min_lines: usize,
    /// Cosine threshold for the near-clone tier.
    pub near_threshold: f32,
    /// Optional `file_path` prefix filter (e.g. a crate dir) — restricts the
    /// report to one subtree.
    pub scope: Option<&'a str>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SymbolRef {
    pub symbol: String,
    pub kind: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub lines: usize,
    pub is_public: bool,
}

/// One normalized-body signature shared by ≥2 distinct source locations.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExactClone {
    /// blake3 hex of the normalized (trimmed, blank-line-dropped) body.
    pub signature: String,
    pub lines: usize,
    pub members: Vec<SymbolRef>,
}

/// A connected component of near-duplicate symbols.
#[derive(Debug, Clone, serde::Serialize)]
pub struct NearCluster {
    pub members: Vec<SymbolRef>,
    pub min_sim: f32,
    pub max_sim: f32,
    /// Representative (longest) member's line count — the rough "size" of the
    /// duplicated unit.
    pub unit_lines: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DryReport {
    pub corpus_id: String,
    pub min_lines: usize,
    pub near_threshold: f32,
    pub scope: Option<String>,
    /// Total code symbols (function/method) seen before the min-lines filter.
    pub total_symbols: usize,
    /// Distinct source locations considered after filtering + dedup.
    pub considered: usize,
    /// Symbols dropped for lacking an embedding (defensive; T1 indexes have none).
    pub skipped_no_embedding: usize,
    pub exact_clones: Vec<ExactClone>,
    pub near_clusters: Vec<NearCluster>,
    /// Rough lower bound on removable lines: for each group, (copies − 1) × lines.
    pub estimated_redundant_lines: usize,
}

/// Internal working record: a distinct source location + its normalized vector.
struct Candidate {
    r: SymbolRef,
    embedding: Vec<f32>,
}

fn err(id: &str, message: String) -> Error {
    Error::Tool {
        tool_id: format!("dry_report:{id}"),
        message,
    }
}

pub async fn build_dry_report(inputs: DryInputs<'_>) -> Result<DryReport> {
    let t0 = std::time::Instant::now();
    let index = CorpusIndex::open(inputs.index_path)
        .await
        .map_err(|e| err("open", e.to_string()))?;

    // ONE full scan of the chunk table — every chunk + embedding. (A per-doc
    // fan-out would be one full scan PER source file on an unindexed column.)
    let rows = index
        .all_chunks_with_embeddings()
        .await
        .map_err(|e| err("all_chunks_with_embeddings", e.to_string()))?;

    let mut total_symbols = 0usize;
    let mut skipped_no_embedding = 0usize;
    // Dedup key: one distinct source location. Oversize symbols are split into
    // several chunks sharing a (file, line_start, line_end); keep the widest.
    let mut by_loc: HashMap<(String, u32, u32), Candidate> = HashMap::new();

    for (row, embedding) in rows {
        let Some(meta) = row.metadata_raw.as_deref() else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(meta) else {
            continue;
        };
        // Only code symbols carry these fields; knowledge chunks won't.
        let (Some(symbol), Some(kind), Some(file)) = (
            v.get("symbol_name").and_then(|x| x.as_str()),
            v.get("symbol_kind").and_then(|x| x.as_str()),
            v.get("file_path").and_then(|x| x.as_str()),
        ) else {
            continue;
        };
        if kind != "function" && kind != "method" {
            continue;
        }
        let line_start = v.get("line_start").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        let line_end = v.get("line_end").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        let lines = (line_end.saturating_sub(line_start) as usize) + 1;
        total_symbols += 1;

        if let Some(prefix) = inputs.scope {
            if !file.starts_with(prefix) {
                continue;
            }
        }
        if lines < inputs.min_lines {
            continue;
        }
        if embedding.is_empty() {
            skipped_no_embedding += 1;
            continue;
        }

        let cand = Candidate {
            r: SymbolRef {
                symbol: symbol.to_string(),
                kind: kind.to_string(),
                file: file.to_string(),
                line_start,
                line_end,
                lines,
                is_public: v
                    .get("is_public")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false),
            },
            embedding: normalize(&embedding),
        };
        // Keep the widest chunk per source location (split-chunk collapse).
        by_loc
            .entry((file.to_string(), line_start, line_end))
            .and_modify(|e| {
                if cand.r.lines > e.r.lines {
                    *e = Candidate {
                        r: cand.r.clone(),
                        embedding: cand.embedding.clone(),
                    };
                }
            })
            .or_insert(cand);
    }

    let mut candidates: Vec<Candidate> = by_loc.into_values().collect();
    eprintln!(
        "dry_report: loaded {} distinct code symbols (of {total_symbols}) in {:.1}s",
        candidates.len(),
        t0.elapsed().as_secs_f32()
    );
    // ── Tier 1: EXACT clones — from SOURCE via SCIP, NOT the embedding index ──
    // The chunk index is content-hash-deduped, so byte-identical bodies are
    // collapsed to one row before we see them (this once hid 15 copies of
    // `now()`). Read every function definition from the SCIP graph (un-deduped)
    // and hash its normalized body. Independent of the near tier and its floor.
    // Also returns the `use … as` re-export aliases it found — the near tier
    // must drop those (the embedding index tags them as functions too).
    let db_path = inputs.index_path.join("scip_graph.db");
    let source_root = corpus_source_root(inputs.index_path);
    let (exact_clones, alias_syms) =
        exact_clones_from_source(&db_path, inputs.corpus_id, &source_root, inputs.scope)
            .await
            .unwrap_or_else(|e| {
                eprintln!("dry_report: exact-clone tier unavailable ({e}); near clones only");
                (Vec::new(), HashSet::new())
            });
    eprintln!(
        "dry_report: exact tier found {} clone group(s) from source (SCIP, un-deduped)",
        exact_clones.len()
    );

    // Drop re-export aliases from the near candidates — same false positive the
    // exact tier's `is_use_alias` guard filters, but the embedding index carries
    // these as functions too, so two same-named re-exports would score ≈1.0.
    if !alias_syms.is_empty() {
        let before = candidates.len();
        candidates.retain(|c| !alias_syms.contains(&(c.r.file.clone(), c.r.symbol.clone())));
        let dropped = before - candidates.len();
        if dropped > 0 {
            eprintln!("dry_report: dropped {dropped} use-alias symbol(s) from near tier");
        }
    }

    // Exclude exact-clone locations from the near tier so they are not
    // double-reported. Keyed on (file, line_start) — a distinct source site.
    // Distinct source locations the near tier actually weighs (post alias-drop).
    let considered = candidates.len();
    let exact_locs: HashSet<(&str, u32)> = exact_clones
        .iter()
        .flat_map(|g| g.members.iter().map(|m| (m.file.as_str(), m.line_start)))
        .collect();
    let exact_dupes: Vec<bool> = candidates
        .iter()
        .map(|c| exact_locs.contains(&(c.r.file.as_str(), c.r.line_start)))
        .collect();

    // ── Tier 2: near clones by cosine, over non-redundant reps ───────────────
    let t_pairwise = std::time::Instant::now();
    // Stable index list into `candidates`, sorted ascending by length so the
    // size-ratio prefilter can `break` the inner loop.
    let mut reps: Vec<usize> = (0..candidates.len()).filter(|&i| !exact_dupes[i]).collect();
    reps.sort_by_key(|&i| candidates[i].r.lines);

    let threshold = inputs.near_threshold;
    let n = reps.len();
    let num_threads = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
        .clamp(1, 16);

    // Heartbeat: this pass runs MINUTES with no output, and silence here is
    // indistinguishable from a hang (reported live 2026-08-06 — an operator
    // watched 8 quiet minutes and assumed a deadlock; under CPU contention
    // from a resident model the pass runs ~2x longer). §9.1: a long-running
    // branch with no event is not finished.
    eprintln!(
        "dry_report: near pass starting — O(n²) over {n} reps on {num_threads} threads; \
         minutes-scale, longer under CPU load"
    );
    let progress = std::sync::atomic::AtomicUsize::new(0);
    let tick = (n / 10).max(1);
    let progress_ref = &progress;
    let candidates_ref = &candidates;
    let reps_ref = &reps;
    let edges: Vec<(usize, usize, f32)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..num_threads)
            .map(|t| {
                s.spawn(move || {
                    let mut local: Vec<(usize, usize, f32)> = Vec::new();
                    let mut a = t;
                    while a < n {
                        let done = progress_ref.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if done % tick == 0 {
                            eprintln!(
                                "dry_report: near pass {done}/{n} rows ({:.0}s elapsed)",
                                t_pairwise.elapsed().as_secs_f32()
                            );
                        }
                        let ci = reps_ref[a];
                        let la = candidates_ref[ci].r.lines as f32;
                        let emb_a = &candidates_ref[ci].embedding;
                        for b in (a + 1)..n {
                            let cj = reps_ref[b];
                            let lb = candidates_ref[cj].r.lines as f32;
                            if lb > la * SIZE_RATIO {
                                break; // reps sorted ascending → no further match possible
                            }
                            let sim = dot(emb_a, &candidates_ref[cj].embedding);
                            if sim >= threshold {
                                local.push((ci, cj, sim));
                            }
                        }
                        a += num_threads;
                    }
                    local
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect()
    });

    eprintln!(
        "dry_report: near pass over {n} reps → {} edges in {:.1}s ({num_threads} threads)",
        edges.len(),
        t_pairwise.elapsed().as_secs_f32()
    );

    // Union-find over near edges → clusters.
    let mut uf = UnionFind::new(candidates.len());
    let mut sim_of: HashMap<(usize, usize), f32> = HashMap::new();
    for (i, j, sim) in &edges {
        uf.union(*i, *j);
        sim_of.insert((*i.min(j), *i.max(j)), *sim);
    }
    let mut comp_members: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, j, _) in &edges {
        comp_members.entry(uf.find(*i)).or_default();
        comp_members.entry(uf.find(*j)).or_default();
    }
    // Collect distinct members per component.
    let mut seen_in_comp: HashMap<usize, Vec<usize>> = HashMap::new();
    {
        let mut member_set: HashMap<usize, std::collections::BTreeSet<usize>> = HashMap::new();
        for (i, j, _) in &edges {
            let root = uf.find(*i);
            let e = member_set.entry(root).or_default();
            e.insert(*i);
            e.insert(*j);
        }
        for (root, set) in member_set {
            seen_in_comp.insert(root, set.into_iter().collect());
        }
    }

    let mut near_clusters: Vec<NearCluster> = Vec::new();
    for (_root, members) in seen_in_comp {
        if members.len() < 2 {
            continue;
        }
        // sim range across the edges internal to this component
        let mut min_sim = 1.0f32;
        let mut max_sim = 0.0f32;
        for a in 0..members.len() {
            for b in (a + 1)..members.len() {
                let key = (members[a].min(members[b]), members[a].max(members[b]));
                if let Some(s) = sim_of.get(&key) {
                    min_sim = min_sim.min(*s);
                    max_sim = max_sim.max(*s);
                }
            }
        }
        let unit_lines = members
            .iter()
            .map(|&i| candidates[i].r.lines)
            .max()
            .unwrap_or(0);
        let mut refs: Vec<SymbolRef> = members.iter().map(|&i| candidates[i].r.clone()).collect();
        refs.sort_by(|a, b| a.file.cmp(&b.file).then(a.line_start.cmp(&b.line_start)));
        near_clusters.push(NearCluster {
            members: refs,
            min_sim: if max_sim == 0.0 { 0.0 } else { min_sim },
            max_sim,
            unit_lines,
        });
    }
    near_clusters.sort_by(|a, b| {
        (b.members.len() * b.unit_lines)
            .cmp(&(a.members.len() * a.unit_lines))
            .then(
                b.max_sim
                    .partial_cmp(&a.max_sim)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    let estimated_redundant_lines = exact_clones
        .iter()
        .map(|c| (c.members.len().saturating_sub(1)) * c.lines)
        .chain(
            near_clusters
                .iter()
                .map(|c| (c.members.len().saturating_sub(1)) * c.unit_lines),
        )
        .sum();

    Ok(DryReport {
        corpus_id: inputs.corpus_id.to_string(),
        min_lines: inputs.min_lines,
        near_threshold: inputs.near_threshold,
        scope: inputs.scope.map(String::from),
        total_symbols,
        considered,
        skipped_no_embedding,
        exact_clones,
        near_clusters,
        estimated_redundant_lines,
    })
}

/// Repo root the corpus was indexed from — where SCIP's repo-relative paths
/// resolve. Read from the corpus meta; falls back to the current directory.
fn corpus_source_root(index_path: &Path) -> PathBuf {
    let meta = index_path.join("_corpus_meta.json");
    if let Ok(txt) = std::fs::read_to_string(&meta) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            if let Some(sp) = v.get("source_path").and_then(|x| x.as_str()) {
                if !sp.is_empty() {
                    return PathBuf::from(sp);
                }
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Normalize a symbol body for exact-clone comparison: trim each line, drop
/// blank lines, collapse internal whitespace runs. Catches Type-1 (identical)
/// plus formatting-only Type-2 variants (indentation, blank lines, spacing).
fn normalize_body(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect()
}

/// True if the first meaningful line of a body is a `use` re-export alias, not a
/// function signature. rust-analyzer emits `use path::real_fn as alias;` with a
/// function-shaped SCIP descriptor (`mod/alias().`), so `is_function` accepts it
/// and its "body" is whatever source lines the range happens to span — pure
/// noise that groups into false clones (every one of our own consolidation
/// re-exports would false-positive). A genuine function body's first normalized
/// line is its signature or an attribute; it is never a bare `use`. Reject those.
fn is_use_alias(body: &[String]) -> bool {
    let Some(first) = body.first() else {
        return false;
    };
    let mut s = first.as_str();
    // Strip a leading visibility modifier: `pub` optionally followed by `(...)`.
    if let Some(rest) = s.strip_prefix("pub") {
        s = rest.trim_start();
        if let Some(after_paren) = s.strip_prefix('(').and_then(|r| r.split_once(')')) {
            s = after_paren.1.trim_start();
        }
    }
    s.starts_with("use ")
}

/// EXACT-clone tier: every function definition from the SCIP graph (un-deduped),
/// bodies read from source and grouped by normalized-body hash. See the module
/// docs for why this reads source rather than the content-hash-deduped index.
///
/// Returns `(clone groups, alias identities)`. The second value is the set of
/// `(file, symbol_name)` for every symbol that turned out to be a `use … as`
/// re-export alias rather than a real function (see [`is_use_alias`]). The near
/// tier needs it: the embedding index tags these aliases as functions too, so
/// without the exclusion two same-named re-exports score cosine ≈ 1.0 and
/// masquerade as a near-clone. We identify them here — where we already have the
/// source open — and hand the set back rather than re-reading source per tier.
/// Keyed on `(file, name)` deliberately, not line: the chunk index and SCIP
/// disagree on a symbol's line range, so a line-based key would miss the match.
async fn exact_clones_from_source(
    db_path: &Path,
    corpus_id: &str,
    source_root: &Path,
    scope: Option<&str>,
) -> Result<(Vec<ExactClone>, HashSet<(String, String)>)> {
    let graph = ScipGraph::open(db_path, corpus_id).map_err(|e| err("scip_open", e.to_string()))?;
    let syms = graph
        .iter_all_symbols()
        .await
        .map_err(|e| err("iter_all_symbols", e.to_string()))?;

    // Function symbols, grouped by file so each file is read exactly once.
    let mut by_file: BTreeMap<String, Vec<(String, i32, i32)>> = BTreeMap::new();
    for rec in syms {
        let is_fn = pkg_and_desc(&rec.qualified_name)
            .map(|(_, d)| is_function(d))
            .unwrap_or(false)
            || rec.kind == "function"
            || rec.kind == "method";
        if !is_fn || rec.line_start <= 0 {
            continue;
        }
        let span = (rec.line_end - rec.line_start + 1).max(0) as usize;
        if span < EXACT_MIN_LINES {
            continue;
        }
        if let Some(prefix) = scope {
            if !rec.file_path.starts_with(prefix) {
                continue;
            }
        }
        by_file
            .entry(rec.file_path)
            .or_default()
            .push((rec.name, rec.line_start, rec.line_end));
    }

    // signature (blake3 hex of normalized body) -> distinct member locations
    let mut groups: HashMap<String, Vec<SymbolRef>> = HashMap::new();
    let mut seen_loc: HashSet<(String, i32, i32)> = HashSet::new();
    // (file, symbol_name) of every `use … as` alias masquerading as a function.
    let mut alias_syms: HashSet<(String, String)> = HashSet::new();
    for (file, mut defs) in by_file {
        let Ok(content) = std::fs::read_to_string(source_root.join(&file)) else {
            continue; // file moved/deleted since indexing — skip, don't fail
        };
        let flines: Vec<&str> = content.lines().collect();
        defs.sort();
        defs.dedup();
        for (name, start, end) in defs {
            if !seen_loc.insert((file.clone(), start, end)) {
                continue; // same source location recorded twice
            }
            let s = (start.max(1) - 1) as usize;
            let e = (end.max(1) as usize).min(flines.len());
            if s >= e {
                continue;
            }
            let body = normalize_body(&flines[s..e]);
            if body.len() < EXACT_MIN_LINES {
                continue; // mostly blank/comment lines once normalized
            }
            if is_use_alias(&body) {
                // Re-export alias with a function-shaped descriptor, not a body.
                // Record it so the near tier drops it too, then skip.
                alias_syms.insert((file.clone(), name));
                continue;
            }
            let sig = blake3::hash(body.join("\n").as_bytes())
                .to_hex()
                .to_string();
            groups.entry(sig).or_default().push(SymbolRef {
                symbol: name,
                kind: "function".to_string(),
                file: file.clone(),
                line_start: start as u32,
                line_end: end as u32,
                lines: (end - start + 1).max(0) as usize,
                is_public: false, // not carried by the SCIP record
            });
        }
    }

    let mut out: Vec<ExactClone> = groups
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .map(|(signature, mut members)| {
            members.sort_by(|a, b| a.file.cmp(&b.file).then(a.line_start.cmp(&b.line_start)));
            let lines = members.iter().map(|m| m.lines).max().unwrap_or(0);
            ExactClone {
                signature,
                lines,
                members,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then(b.lines.cmp(&a.lines))
    });
    Ok((out, alias_syms))
}

fn normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    // Vectors are length-normalized, so the dot product IS the cosine similarity.
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }
    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        // path compression
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        if self.rank[ra] < self.rank[rb] {
            self.parent[ra] = rb;
        } else if self.rank[ra] > self.rank[rb] {
            self.parent[rb] = ra;
        } else {
            self.parent[rb] = ra;
            self.rank[ra] += 1;
        }
    }
}

fn short_hash(h: &str) -> &str {
    &h[..h.len().min(12)]
}

pub fn render_dry_report(r: &DryReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("# DRY report — `{}`\n\n", r.corpus_id));
    if let Some(scope) = &r.scope {
        out.push_str(&format!("Scope: `{scope}`\n\n"));
    }
    out.push_str(&format!(
        "{} function/method symbols · {} distinct locations considered \
         (≥{} lines) · near-clone threshold cosine ≥ {:.2}\n\n",
        r.total_symbols, r.considered, r.min_lines, r.near_threshold
    ));
    out.push_str(&format!(
        "**{}** exact-clone groups · **{}** near-clone clusters · \
         ~**{}** redundant lines (lower bound).\n\n",
        r.exact_clones.len(),
        r.near_clusters.len(),
        r.estimated_redundant_lines
    ));

    if !r.exact_clones.is_empty() {
        out.push_str("## Exact clones (identical bodies, modulo formatting)\n\n");
        for c in &r.exact_clones {
            out.push_str(&format!(
                "- **{} copies × {} lines** (`{}`)\n",
                c.members.len(),
                c.lines,
                short_hash(&c.signature)
            ));
            for m in &c.members {
                out.push_str(&format!(
                    "  - `{}` — {}:{}\n",
                    m.symbol, m.file, m.line_start
                ));
            }
        }
        out.push('\n');
    }

    if !r.near_clusters.is_empty() {
        out.push_str("## Near clones (similar, not identical)\n\n");
        for c in &r.near_clusters {
            out.push_str(&format!(
                "- **{} symbols** · ~{} lines · cosine {:.3}–{:.3}\n",
                c.members.len(),
                c.unit_lines,
                c.min_sim,
                c.max_sim
            ));
            for m in &c.members {
                let vis = if m.is_public { "pub " } else { "" };
                out.push_str(&format!(
                    "  - {}`{}` — {}:{}–{} ({} lines)\n",
                    vis, m.symbol, m.file, m.line_start, m.line_end, m.lines
                ));
            }
        }
        out.push('\n');
    }

    if r.exact_clones.is_empty() && r.near_clusters.is_empty() {
        out.push_str("_No duplication found above the configured thresholds._\n");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(lines: &[&str]) -> Vec<String> {
        normalize_body(lines)
    }

    #[test]
    fn use_aliases_are_rejected() {
        // All forms of re-export alias rust-analyzer tags with a `fn().` descriptor.
        assert!(is_use_alias(&body(&[
            "use commonwealth_core::clock::unix_now_secs as now_secs;"
        ])));
        assert!(is_use_alias(&body(&[
            "pub use sovereign_core::time::unix_now as unix_now;"
        ])));
        assert!(is_use_alias(&body(&["pub(crate) use foo::bar as bar;"])));
        assert!(is_use_alias(&body(&["pub(super) use foo::bar as bar;"])));
    }

    #[test]
    fn real_function_bodies_are_kept() {
        // A genuine body never opens with `use` — signatures and attributes do.
        assert!(!is_use_alias(&body(&[
            "fn now_secs() -> u64 {",
            "    SystemTime::now()",
            "}"
        ])));
        assert!(!is_use_alias(&body(&[
            "pub fn ctx() -> Ctx {",
            "    Ctx::default()",
            "}"
        ])));
        assert!(!is_use_alias(&body(&[
            "#[tokio::test]",
            "async fn run() {",
            "    let x = use_it();", // `use` mid-body must not trip the guard
            "}"
        ])));
        assert!(!is_use_alias(&[])); // empty body: not an alias
    }
}
