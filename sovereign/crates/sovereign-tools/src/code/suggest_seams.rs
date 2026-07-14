// SPDX-License-Identifier: AGPL-3.0-or-later
//! `suggest_seams` — advisory god-file split proposals from the SCIP graph.
//!
//! Answers "if this oversized file were split into submodules, where are the
//! seams and which helpers must stay behind?" — the analysis a human does by
//! hand before a decomposition (and which we did by hand for `project_cmd.rs`).
//!
//! Pure structure, no model. The insight from doing it by hand: command-family
//! seams are a CALL-GRAPH property, not a semantic one. Each handler is a root
//! (dispatched, or externally called); its private helpers are exactly the
//! functions reachable ONLY through it. So:
//!
//!   1. Nodes  = TOP-LEVEL symbols DEFINED in the file (`symbols_in_file`,
//!      restricted by line-range containment — rust-analyzer also records nested
//!      locals, closures, and `#[cfg(test)]` fns as symbols; those are dropped).
//!   2. Edges  = intra-file function→function call edges (`all_qualified_edges`,
//!      filtered to both endpoints in-file and the callee being a function per
//!      `capability_map::is_function` — the same filter the capability map uses,
//!      NOT the unreliable `kind`/`ref_kind` columns).
//!   3. Seeds  = functions called from OUTSIDE the file, plus the direct callees
//!      of the dispatcher, minus the dispatcher itself. The dispatcher is the
//!      in-file function whose callees are themselves externally-entered handlers
//!      (a thin router that delegates) — NOT the one with the largest raw
//!      fan-out, which is typically a fat handler calling many private helpers.
//!   4. A non-seed helper belongs to the seed that EXCLUSIVELY reaches it
//!      (barrier-BFS: stop at other seeds). Reached from ≥2 seeds → SHARED, it
//!      stays in `mod.rs`. This is a dominator relation over the call graph.
//!   5. Consts / structs (data, not called) are placed by which cluster's
//!      functions reference them (SCIP data-refs); referenced across clusters →
//!      shared.
//!
//! Advisory only: it prints proposed modules + the shared-stays set + merge
//! hints + dead-code + oversized flags. A human (or agent) does the extraction.
//! No embeddings needed for the structural split; a later pass can name clusters
//! and gauge cohesion from the code embeddings.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

use sovereign_core::error::{Error, Result};

use corpus_engine_scip::capability_map::{is_function, pkg_and_desc};
use corpus_engine_scip::ScipGraph;

/// ARCH §3.1 file-size ceiling (mirrors the xtask arch-gate constant).
const FILE_SIZE_LIMIT: usize = 1200;
/// A function needs at least this many externally-entered callees to count as a
/// dispatcher (below it, there's no hub to seed handlers from).
const DISPATCHER_MIN_FANOUT: usize = 3;
/// A dispatcher is a THIN router whose body just delegates. Past this many lines
/// it is a fat orchestrator, not a router — do not treat it as a dispatcher (it
/// would wrongly "stay in mod.rs"). Flat-API files with no thin router then
/// correctly report no dispatcher, and every public method becomes its own seed.
const DISPATCHER_MAX_LINES: usize = 150;

pub struct SeamInputs<'a> {
    pub db_path: &'a Path,
    pub corpus_id: &'a str,
    /// File path exactly as SCIP stores it (repo-relative).
    pub file_path: &'a str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Member {
    pub name: String,
    pub qualified: String,
    pub is_function: bool,
    pub line_start: i32,
    pub line_end: i32,
    pub lines: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Cluster {
    /// Proposed submodule name (handler name with a `cmd_` prefix stripped).
    pub name: String,
    /// The seed function that anchors the cluster.
    pub seed: String,
    pub members: Vec<Member>,
    pub total_lines: usize,
    pub oversized: bool,
    /// Seed has no caller at all (external or in-file) — dead-code candidate.
    pub dead: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SharedMember {
    pub member: Member,
    /// Seed names whose exclusive-reach includes this helper (why it stays).
    pub owners: Vec<String>,
}

/// Two-or-three seeds that exclusively co-own a helper — a natural module
/// boundary at a coarser grain than one-cluster-per-handler.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MergeHint {
    pub seeds: Vec<String>,
    pub shared_helpers: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct SeamReport {
    pub file: String,
    pub total_symbols: usize,
    pub total_functions: usize,
    pub skipped_unqualified: usize,
    pub dispatcher: Option<String>,
    pub clusters: Vec<Cluster>,
    pub shared: Vec<SharedMember>,
    pub merge_hints: Vec<MergeHint>,
    pub dead_code: Vec<String>,
}

fn module_name(fn_name: &str) -> String {
    fn_name.strip_prefix("cmd_").unwrap_or(fn_name).to_string()
}

/// Build the advisory seam report for a single file from its SCIP graph.
pub async fn build_seam_report(inputs: SeamInputs<'_>) -> Result<SeamReport> {
    let err = |stage: &str, msg: String| Error::Tool {
        tool_id: "suggest_seams".to_string(),
        message: format!("{stage}: {msg}"),
    };

    let graph = ScipGraph::open(inputs.db_path, inputs.corpus_id)
        .map_err(|e| err("open SCIP graph", e.to_string()))?;

    let syms = graph
        .symbols_in_file(inputs.file_path)
        .await
        .map_err(|e| err("symbols_in_file", e.to_string()))?;
    if syms.is_empty() {
        return Err(err(
            "no symbols",
            format!(
                "no symbols defined in '{}' — is the file indexed, and is the path repo-relative \
                 exactly as SCIP stores it (e.g. sovereign/crates/…/foo.rs)?",
                inputs.file_path
            ),
        ));
    }

    // ── Nodes: dedupe, then keep only TOP-LEVEL symbols ─────────────────────
    // rust-analyzer records nested locals, closures, and test-module fns as
    // symbols too; splitting is about top-level items only. A symbol is
    // top-level when the file-module is its ONLY strict container (line-range
    // containment — the `kind` column is unreliable). This also drops the
    // `#[cfg(test)] mod tests` bodies (contained by the test module → depth ≥2).
    let mut skipped_unqualified = 0usize;
    let mut uniq: HashMap<String, (i32, i32, String)> = HashMap::new();
    for s in &syms {
        if s.qualified_name.is_empty() {
            skipped_unqualified += 1;
            continue;
        }
        let span = s.line_end - s.line_start;
        uniq.entry(s.qualified_name.clone())
            .and_modify(|e| {
                if span > (e.1 - e.0) {
                    *e = (s.line_start, s.line_end, s.name.clone());
                }
            })
            .or_insert((s.line_start, s.line_end, s.name.clone()));
    }
    let items: Vec<(String, i32, i32, String)> = uniq
        .into_iter()
        .map(|(q, (a, b, n))| (q, a, b, n))
        .collect();
    // The file-module: the widest span, if it covers most of the file.
    let file_lines = {
        let lo = items.iter().map(|(_, a, _, _)| *a).min().unwrap_or(0);
        let hi = items.iter().map(|(_, _, b, _)| *b).max().unwrap_or(0);
        (hi - lo + 1).max(1)
    };
    let root_span = items
        .iter()
        .map(|(_, a, b, _)| (*a, *b))
        .max_by_key(|(a, b)| b - a)
        .filter(|(a, b)| (b - a + 1) * 10 >= file_lines * 6); // ≥60% coverage
    let strictly_contains = |c: (i32, i32), s: (i32, i32)| {
        c.0 <= s.0 && s.1 <= c.1 && (c.1 - c.0) > (s.1 - s.0)
    };
    let container_count = |s: (i32, i32)| {
        items
            .iter()
            .filter(|(_, a, b, _)| strictly_contains((*a, *b), s))
            .count()
    };
    // Depth of a genuine top-level item: 1 if a file-module root exists, else 0.
    let target_depth = if root_span.is_some() { 1 } else { 0 };

    let mut by_qual: HashMap<String, Member> = HashMap::new();
    let mut file_quals: HashSet<String> = HashSet::new();
    let mut fn_quals: HashSet<String> = HashSet::new();
    for (qual, start, end, name) in &items {
        // Skip synthetic symbols with no real span (derive-generated `default`,
        // etc. — SCIP records them at line 0).
        if *start <= 0 {
            continue;
        }
        // Skip the file-module root itself and anything nested below top level.
        if root_span == Some((*start, *end)) {
            continue;
        }
        if container_count((*start, *end)) != target_depth {
            continue;
        }
        let is_fn = pkg_and_desc(qual).map(|(_, d)| is_function(d)).unwrap_or(false);
        let lines = (end - start + 1).max(0) as usize;
        file_quals.insert(qual.clone());
        if is_fn {
            fn_quals.insert(qual.clone());
        }
        by_qual.insert(
            qual.clone(),
            Member {
                name: name.clone(),
                qualified: qual.clone(),
                is_function: is_fn,
                line_start: *start,
                line_end: *end,
                lines,
            },
        );
    }
    let short = |q: &str| by_qual.get(q).map(|m| m.name.clone()).unwrap_or_else(|| q.to_string());

    // ── Edges: intra-file function call graph + fan-in + external entry + data refs ──
    let edges = graph
        .all_qualified_edges()
        .await
        .map_err(|e| err("all_qualified_edges", e.to_string()))?;

    let mut adj: HashMap<String, BTreeSet<String>> = HashMap::new(); // fn -> in-file fn callees
    let mut in_fanin: HashMap<String, BTreeSet<String>> = HashMap::new(); // fn -> in-file fn callers
    let mut ext_entered: HashSet<String> = HashSet::new(); // fn called from outside the file
    let mut data_referrers: HashMap<String, BTreeSet<String>> = HashMap::new(); // non-fn sym -> in-file fn callers

    // "External entry" must be judged against EVERY symbol the file defines, not
    // just the top-level nodes: a call from a `#[cfg(test)]` fn or a nested
    // closure (both dropped by the containment filter) is still an INTERNAL call,
    // and must not promote its callee to a seed. `file_all` is that full set.
    let file_all: HashSet<&str> = syms
        .iter()
        .filter(|s| !s.qualified_name.is_empty())
        .map(|s| s.qualified_name.as_str())
        .collect();

    for (caller, callee) in &edges {
        if !file_quals.contains(callee) {
            continue; // callee must be a top-level file node (something we place)
        }
        let caller_top_level = fn_quals.contains(caller); // caller is a top-level fn node
        if fn_quals.contains(callee) {
            if caller_top_level {
                if caller != callee {
                    adj.entry(caller.clone()).or_default().insert(callee.clone());
                    in_fanin.entry(callee.clone()).or_default().insert(caller.clone());
                }
            } else if !file_all.contains(caller.as_str()) {
                ext_entered.insert(callee.clone()); // truly from outside the file
            }
            // else: caller is in-file but not top-level (test/nested) → ignore
        } else if caller_top_level {
            // function → in-file const/struct (data ref, drives placement)
            data_referrers.entry(callee.clone()).or_default().insert(caller.clone());
        }
    }

    let out_deg = |q: &str| adj.get(q).map(|s| s.len()).unwrap_or(0);
    // The dispatcher is the function whose callees are themselves handlers —
    // externally-entered functions. Raw fan-out misdetects a FAT handler (one
    // that calls many private helpers) as the router; a thin router's fan-out is
    // to OTHER externally-entered functions. Score on that; break ties on raw
    // fan-out. Candidates are all in-file functions (a router need not itself be
    // externally entered), so a purely-internal dispatcher is still found.
    let ext_fanout = |q: &str| {
        adj.get(q)
            .map(|s| s.iter().filter(|c| ext_entered.contains(*c)).count())
            .unwrap_or(0)
    };

    // ── Seeds: dispatcher's callees ∪ externally-entered, minus the dispatcher ──
    let dispatcher: Option<String> = fn_quals
        .iter()
        .max_by_key(|q| (ext_fanout(q), out_deg(q)))
        .filter(|q| ext_fanout(q) >= DISPATCHER_MIN_FANOUT)
        .filter(|q| by_qual.get(*q).map(|m| m.lines <= DISPATCHER_MAX_LINES).unwrap_or(false))
        .cloned();

    let mut seeds: BTreeSet<String> = ext_entered.iter().cloned().collect();
    if let Some(d) = &dispatcher {
        if let Some(callees) = adj.get(d) {
            seeds.extend(callees.iter().cloned());
        }
        seeds.remove(d);
    }
    // Uncalled functions (no caller anywhere) are dead-code roots — seed them so
    // they surface, unless they're the dispatcher itself.
    for q in &fn_quals {
        let has_in = in_fanin.get(q).map(|s| !s.is_empty()).unwrap_or(false);
        let has_ext = ext_entered.contains(q);
        if !has_in && !has_ext && Some(q) != dispatcher.as_ref() {
            seeds.insert(q.clone());
        }
    }
    seeds.retain(|q| fn_quals.contains(q));

    // ── Ownership: barrier-BFS from each seed, stopping at other seeds ───────
    let mut owners: HashMap<String, BTreeSet<String>> = HashMap::new(); // non-seed fn -> reaching seeds
    for seed in &seeds {
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        visited.insert(seed.clone());
        queue.push_back(seed.clone());
        while let Some(cur) = queue.pop_front() {
            let Some(callees) = adj.get(&cur) else { continue };
            for c in callees {
                if visited.contains(c) {
                    continue;
                }
                if seeds.contains(c) {
                    continue; // barrier: another seed owns its own subtree
                }
                visited.insert(c.clone());
                owners.entry(c.clone()).or_default().insert(seed.clone());
                queue.push_back(c.clone());
            }
        }
    }

    // Unique-owner fns join a cluster; multi-owner fns are shared.
    let mut cluster_of: HashMap<String, String> = HashMap::new();
    let mut shared_fns: BTreeSet<String> = BTreeSet::new();
    for (f, os) in &owners {
        if os.len() == 1 {
            cluster_of.insert(f.clone(), os.iter().next().unwrap().clone());
        } else {
            shared_fns.insert(f.clone());
        }
    }
    for s in &seeds {
        cluster_of.insert(s.clone(), s.clone());
    }

    // ── Place non-function symbols (consts/structs) by their referrers ───────
    let mut cluster_data: HashMap<String, BTreeSet<String>> = HashMap::new();
    let mut shared_data: BTreeSet<String> = BTreeSet::new();
    for q in file_quals.iter().filter(|q| !fn_quals.contains(*q)) {
        let Some(refs) = data_referrers.get(q) else {
            continue; // unreferenced (or referenced only externally) — leave in mod.rs
        };
        let ref_clusters: BTreeSet<String> = refs
            .iter()
            .filter_map(|r| cluster_of.get(r).cloned())
            .collect();
        match ref_clusters.len() {
            1 => {
                cluster_data
                    .entry(ref_clusters.into_iter().next().unwrap())
                    .or_default()
                    .insert(q.clone());
            }
            0 => {}
            _ => {
                shared_data.insert(q.clone());
            }
        }
    }

    // ── Assemble clusters ────────────────────────────────────────────────────
    let member_of = |q: &str| by_qual.get(q).cloned();
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut dead_code: Vec<String> = Vec::new();
    for seed in &seeds {
        let mut members: Vec<Member> = Vec::new();
        if let Some(m) = member_of(seed) {
            members.push(m);
        }
        for (f, owner) in &cluster_of {
            if owner == seed && f != seed {
                if let Some(m) = member_of(f) {
                    members.push(m);
                }
            }
        }
        if let Some(ds) = cluster_data.get(seed) {
            for d in ds {
                if let Some(m) = member_of(d) {
                    members.push(m);
                }
            }
        }
        members.sort_by_key(|m| m.line_start);
        let total_lines: usize = members.iter().map(|m| m.lines).sum();
        let has_in = in_fanin.get(seed).map(|s| !s.is_empty()).unwrap_or(false);
        let dead = !has_in && !ext_entered.contains(seed);
        if dead {
            dead_code.push(short(seed));
        }
        clusters.push(Cluster {
            name: module_name(&short(seed)),
            seed: short(seed),
            members,
            total_lines,
            oversized: total_lines > FILE_SIZE_LIMIT,
            dead,
        });
    }
    // Biggest proposed modules first — those are the ones worth extracting.
    clusters.sort_by(|a, b| b.total_lines.cmp(&a.total_lines).then(a.name.cmp(&b.name)));

    // ── Shared-stays list (mod.rs) + merge hints ─────────────────────────────
    let mut shared: Vec<SharedMember> = Vec::new();
    let mut merge_map: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();
    for f in &shared_fns {
        let owner_names: Vec<String> = owners[f].iter().map(|s| short(s)).collect();
        if owners[f].len() <= 3 {
            let key: Vec<String> = owner_names.clone();
            merge_map.entry(key).or_default().insert(short(f));
        }
        if let Some(m) = member_of(f) {
            shared.push(SharedMember { member: m, owners: owner_names });
        }
    }
    for d in &shared_data {
        if let Some(m) = member_of(d) {
            let owner_names: Vec<String> = data_referrers
                .get(d)
                .map(|refs| {
                    refs.iter()
                        .filter_map(|r| cluster_of.get(r).map(|c| short(c)))
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect()
                })
                .unwrap_or_default();
            shared.push(SharedMember { member: m, owners: owner_names });
        }
    }
    shared.sort_by(|a, b| b.member.lines.cmp(&a.member.lines));

    let merge_hints: Vec<MergeHint> = merge_map
        .into_iter()
        .filter(|(seeds, _)| seeds.len() >= 2)
        .map(|(seeds, helpers)| MergeHint {
            seeds,
            shared_helpers: helpers.into_iter().collect(),
        })
        .collect();

    dead_code.sort();

    Ok(SeamReport {
        file: inputs.file_path.to_string(),
        total_symbols: by_qual.len(),
        total_functions: fn_quals.len(),
        skipped_unqualified,
        dispatcher: dispatcher.map(|d| short(&d)),
        clusters,
        shared,
        merge_hints,
        dead_code,
    })
}

/// Render the report as human-readable markdown.
pub fn render_seam_report(r: &SeamReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Seam suggestions — `{}`\n\n", r.file));
    out.push_str(&format!(
        "{} symbols ({} functions){}. ",
        r.total_symbols,
        r.total_functions,
        if r.skipped_unqualified > 0 {
            format!(", {} unqualified rows skipped", r.skipped_unqualified)
        } else {
            String::new()
        }
    ));
    match &r.dispatcher {
        Some(d) => out.push_str(&format!("Dispatcher: `{d}` (stays in mod.rs).\n\n")),
        None => out.push_str("No dispatcher detected (seeds = externally-called functions only).\n\n"),
    }

    let extractable: Vec<&Cluster> = r.clusters.iter().filter(|c| c.members.len() > 1).collect();
    let singletons: Vec<&Cluster> = r.clusters.iter().filter(|c| c.members.len() == 1).collect();

    out.push_str(&format!("## Proposed modules ({})\n\n", extractable.len()));
    out.push_str("| module | seed | members | lines | flags |\n|---|---|--:|--:|---|\n");
    for c in &extractable {
        let mut flags = Vec::new();
        if c.oversized {
            flags.push("**OVERSIZED — sub-split**".to_string());
        }
        if c.dead {
            flags.push("dead (uncalled)".to_string());
        }
        out.push_str(&format!(
            "| `{}` | `{}` | {} | {} | {} |\n",
            c.name,
            c.seed,
            c.members.len(),
            c.total_lines,
            flags.join(", ")
        ));
    }
    out.push('\n');

    for c in &extractable {
        out.push_str(&format!(
            "<details><summary><code>{}</code> — {} members, {} lines</summary>\n\n",
            c.name,
            c.members.len(),
            c.total_lines
        ));
        for m in &c.members {
            out.push_str(&format!(
                "- `{}` {}:{}–{} ({} lines){}\n",
                m.name,
                r.file,
                m.line_start,
                m.line_end,
                m.lines,
                if m.is_function { "" } else { " [data]" }
            ));
        }
        out.push_str("\n</details>\n\n");
    }

    if !r.merge_hints.is_empty() {
        out.push_str("## Merge candidates (seeds that exclusively share helpers)\n\n");
        for h in &r.merge_hints {
            out.push_str(&format!(
                "- {} → share {}\n",
                h.seeds
                    .iter()
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(" + "),
                h.shared_helpers
                    .iter()
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "## Shared — stay in mod.rs ({})\n\n",
        r.shared.len()
    ));
    out.push_str("Reached from ≥2 clusters; extracting them would create cross-module coupling.\n\n");
    for s in &r.shared {
        out.push_str(&format!(
            "- `{}` ({} lines) — used by {}\n",
            s.member.name,
            s.member.lines,
            s.owners
                .iter()
                .map(|o| format!("`{o}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out.push('\n');

    if !singletons.is_empty() {
        out.push_str(&format!(
            "## Singleton handlers — leave in mod.rs or fold ({}): {}\n\n",
            singletons.len(),
            singletons
                .iter()
                .map(|c| format!("`{}` ({}L)", c.seed, c.total_lines))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    if !r.dead_code.is_empty() {
        out.push_str(&format!(
            "## Dead code — no callers ({}): {}\n\n",
            r.dead_code.len(),
            r.dead_code
                .iter()
                .map(|d| format!("`{d}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    out
}
