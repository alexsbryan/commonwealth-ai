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
    let strictly_contains =
        |c: (i32, i32), s: (i32, i32)| c.0 <= s.0 && s.1 <= c.1 && (c.1 - c.0) > (s.1 - s.0);
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
        let is_fn = pkg_and_desc(qual)
            .map(|(_, d)| is_function(d))
            .unwrap_or(false);
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
    let short = |q: &str| {
        by_qual
            .get(q)
            .map(|m| m.name.clone())
            .unwrap_or_else(|| q.to_string())
    };

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
                    adj.entry(caller.clone())
                        .or_default()
                        .insert(callee.clone());
                    in_fanin
                        .entry(callee.clone())
                        .or_default()
                        .insert(caller.clone());
                }
            } else if !file_all.contains(caller.as_str()) {
                ext_entered.insert(callee.clone()); // truly from outside the file
            }
            // else: caller is in-file but not top-level (test/nested) → ignore
        } else if caller_top_level {
            // function → in-file const/struct (data ref, drives placement)
            data_referrers
                .entry(callee.clone())
                .or_default()
                .insert(caller.clone());
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
        .filter(|q| {
            by_qual
                .get(*q)
                .map(|m| m.lines <= DISPATCHER_MAX_LINES)
                .unwrap_or(false)
        })
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
            let Some(callees) = adj.get(&cur) else {
                continue;
            };
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
            shared.push(SharedMember {
                member: m,
                owners: owner_names,
            });
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
            shared.push(SharedMember {
                member: m,
                owners: owner_names,
            });
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
        None => {
            out.push_str("No dispatcher detected (seeds = externally-called functions only).\n\n")
        }
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
    out.push_str(
        "Reached from ≥2 clusters; extracting them would create cross-module coupling.\n\n",
    );
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

/// Render the seam report as a paste-ready split goal for
/// `svrn solve <workdir> "goal" --verb split`. This is the bridge from
/// ADVISORY (`render_seam_report`, a human does the extraction) to
/// EXECUTABLE: the same facts, phrased as the instruction the solver
/// loop consumes. Shape proven by hand on grounding/mod.rs (2026-09-02,
/// 6,042 → 1,177 lines, three passes): name the façade rule, enumerate
/// concerns with their members, name what stays, name the end state.
/// Deterministic — same report, same goal text.
pub fn render_split_goal(r: &SeamReport, max_lines: usize) -> String {
    let extractable: Vec<&Cluster> = r.clusters.iter().filter(|c| c.members.len() > 1).collect();
    let singletons: Vec<&Cluster> = r.clusters.iter().filter(|c| c.members.len() == 1).collect();

    let mut out = String::new();
    out.push_str(&format!(
        "Split {} into concern submodules. Keep the file as a re-export façade: \
         every existing path module::Item must keep compiling via pub/pub(crate)/pub(super) \
         use re-exports — zero importer churn outside this module. Behavior-preserving \
         (ARCH §3.2, §10): no signature changes; move tests with the code they test into \
         each new module's #[cfg(test)] mod.\n\nConcern map, from the SCIP seam analysis",
        r.file
    ));
    if extractable.is_empty() {
        out.push_str(
            ": the call graph found no multi-member clusters — this file's seams are not \
             handler-shaped. A structural split is not indicated; do not force one.\n",
        );
        return out;
    }
    out.push_str(":\n");
    for (i, c) in extractable.iter().enumerate() {
        let members = c
            .members
            .iter()
            .map(|m| format!("{} ({}L)", m.name, m.lines))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "({}) {} — seed {} + {} members, ~{} lines: {}.\n",
            i + 1,
            c.name,
            c.seed,
            c.members.len(),
            c.total_lines,
            members
        ));
        if c.oversized {
            out.push_str(&format!(
                "    NOTE: {} is itself oversized — sub-split it along its own seams.\n",
                c.name
            ));
        }
    }
    if !r.shared.is_empty() {
        let shared = r
            .shared
            .iter()
            .map(|s| format!("{} ({}L)", s.member.name, s.member.lines))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "\nShared — stays in the façade (exclusive reach of: {}):\n  {}\n",
            r.shared
                .iter()
                .map(|s| s.owners.join("/"))
                .collect::<Vec<_>>()
                .join("; "),
            shared
        ));
    }
    if let Some(d) = &r.dispatcher {
        out.push_str(&format!("Dispatcher: {} — stays in the façade.\n", d));
    }
    if !singletons.is_empty() {
        out.push_str(&format!(
            "Singleton handlers (leave in the façade or fold; no separate module): {}\n",
            singletons
                .iter()
                .map(|c| format!("{} ({}L)", c.seed, c.total_lines))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for h in &r.merge_hints {
        out.push_str(&format!(
            "Merge candidate — {} co-own {}: one module at that coarser grain is acceptable.\n",
            h.seeds.join(" + "),
            h.shared_helpers.join(", ")
        ));
    }
    if !r.dead_code.is_empty() {
        out.push_str(&format!(
            "Dead code — do NOT move it; list it for deletion review: {}\n",
            r.dead_code.join(", ")
        ));
    }
    out.push_str(&format!(
        "\nEnd state: the façade holds only module declarations, re-exports and shared docs; \
         no file exceeds {} lines (ARCH §3.1). Cross-module visibility: moved items keep \
         their visibility; where a submodule needs a sibling's item, import it (pub(crate) \
         within the module tree is acceptable). The full crate test suite must stay green.\n",
        max_lines
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn fixture() -> SeamReport {
        SeamReport {
            file: "crates/app/src/cmd.rs".to_string(),
            total_symbols: 12,
            total_functions: 10,
            skipped_unqualified: 0,
            dispatcher: Some("run_cmd".to_string()),
            clusters: vec![
                Cluster {
                    name: "ingest".to_string(),
                    seed: "cmd_ingest".to_string(),
                    members: vec![
                        Member {
                            name: "cmd_ingest".to_string(),
                            qualified: "cmd_ingest".to_string(),
                            is_function: true,
                            line_start: 10,
                            line_end: 90,
                            lines: 80,
                        },
                        Member {
                            name: "validate_recipe".to_string(),
                            qualified: "validate_recipe".to_string(),
                            is_function: true,
                            line_start: 92,
                            line_end: 120,
                            lines: 28,
                        },
                    ],
                    total_lines: 108,
                    oversized: false,
                    dead: false,
                },
                Cluster {
                    name: "solo".to_string(),
                    seed: "cmd_solo".to_string(),
                    members: vec![],
                    total_lines: 0,
                    oversized: false,
                    dead: false,
                },
            ],
            shared: vec![SharedMember {
                member: Member {
                    name: "resolve_corpus".to_string(),
                    qualified: "resolve_corpus".to_string(),
                    is_function: true,
                    line_start: 200,
                    line_end: 220,
                    lines: 20,
                },
                owners: vec!["cmd_ingest".to_string(), "cmd_solo".to_string()],
            }],
            merge_hints: vec![],
            dead_code: vec!["old_helper".to_string()],
        }
    }

    /// Determinism is the point: the goal text is the contract the solver
    /// loop consumes, so the same report must render byte-identically.
    #[test]
    fn the_goal_render_is_deterministic_and_names_every_concern() {
        let r = fixture();
        let a = render_split_goal(&r, 1200);
        let b = render_split_goal(&r, 1200);
        assert_eq!(a, b);
        assert!(a.contains("crates/app/src/cmd.rs"));
        assert!(a.contains("(1) ingest — seed cmd_ingest + 2 members, ~108 lines"));
        assert!(a.contains("validate_recipe (28L)"));
        assert!(a.contains("Shared — stays in the façade"));
        assert!(a.contains("resolve_corpus (20L)"));
        assert!(a.contains("Dispatcher: run_cmd"));
        assert!(a.contains("no file exceeds 1200 lines"));
        assert!(a.contains("old_helper"));
        assert!(a.contains("do NOT move"));
    }

    /// A file with no handler-shaped seams must say so rather than emit
    /// an empty concern list a solver would have to improvise from.
    #[test]
    fn a_clusterless_report_declines_the_split() {
        let mut r = fixture();
        r.clusters = vec![];
        let goal = render_split_goal(&r, 1200);
        assert!(goal.contains("no multi-member clusters"));
        assert!(goal.contains("do not force one"));
    }
}

/// Locate the conventional tail `#[cfg(test)] mod tests { … }` block in
/// a file module: the LAST top-level `mod tests {` with its `#[cfg(test)]`
/// attribute directly above, running to end-of-file (nothing after the
/// closing brace but whitespace — the module-organization convention
/// this repo's splits follow). Returns `(attr_line, end_line)`, 1-based:
/// the span the split recipe cuts, i.e. `attr..end` is replaced by a
/// declaration and `attr+1+1..end-1` is the moved body. `None` when the
/// convention doesn't hold (tests not at the tail, no `mod tests`,
/// interleaved code after it) — the caller then plans without a tests
/// step rather than guessing spans.
pub fn find_tail_tests_span(path: &Path) -> Option<(usize, usize)> {
    let content = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let mut i = lines.len();
    while i > 0 {
        i -= 1;
        let trimmed = lines[i].trim_start();
        if trimmed == "}" && lines[i].starts_with('}') {
            break;
        }
        if !trimmed.is_empty() {
            // Meaningful content after the last `}` at this position —
            // the tail convention doesn't hold.
            return None;
        }
    }
    let end = i + 1; // 1-based line of the closing `}`
    let mut opener = None;
    for (idx, l) in lines.iter().enumerate().rev() {
        if l.starts_with("mod tests {") {
            opener = Some(idx);
            break;
        }
    }
    let opener = opener?;
    if opener + 2 > end {
        return None;
    }
    let attr = if opener > 0 && lines[opener - 1].trim() == "#[cfg(test)]" {
        opener - 1
    } else {
        return None;
    };
    Some((attr + 1, end))
}

/// Render the seam report as an executor-ready plan for
/// `cargo xtask refactor-apply [--land]` — the TOML counterpart of
/// [`render_split_goal`]. Emits: the tail tests move (when
/// `tests_span` is Some — see [`find_tail_tests_span`]), then every
/// cluster's contiguous member runs, ALL steps ordered by span start
/// DESCENDING so each cut leaves lower spans' numbers valid. The
/// author's remaining duties (module declarations, re-exports, the
/// verify command) are stated as comments in the emitted TOML — a plan
/// the executor refuses to guess is better than one it misapplies.
pub fn render_split_plan(
    r: &SeamReport,
    tests_span: Option<(usize, usize)>,
    max_lines: usize,
) -> String {
    let subject = &r.file;
    let (dir, stem) = match subject.rsplit_once('/') {
        Some((d, f)) => (d.to_string(), f.to_string()),
        None => (String::new(), subject.clone()),
    };
    let stem = stem.strip_suffix(".rs").unwrap_or(&stem).to_string();
    let module_dir = if dir.is_empty() {
        format!("{stem}/")
    } else {
        format!("{dir}/{stem}/")
    };

    let mut out = String::new();
    out.push_str(&format!(
        "# Split plan for `{subject}` — generated by `svrn code suggest-seams --plan`.\n\
         # Apply: cargo xtask refactor-apply <this-file> --land\n\
         # AUTHOR DUTIES before applying: (1) after the moves, add `mod <name>;` declarations\n\
         # and pub/pub(crate) re-exports in the subject file — a small [[patch]] step, or a\n\
         # follow-up edit; (2) check verify_cmd compiles the right crate + targets.\n"
    ));
    out.push_str("[plan]\n");
    out.push_str(&format!("subject = \"{subject}\"\n"));
    out.push_str("# verify_cmd = \"cargo test -p <crate> --lib --tests\"\n\n");

    let mut steps: Vec<(usize, usize, String, String)> = Vec::new(); // (start, end, dest, label)
    if let Some((attr, end)) = tests_span {
        // The body: everything between `mod tests {` (attr+1) and the
        // closing brace (end). The patch step then collapses
        // attr..end (attr + opener + stray close) to the declaration.
        steps.push((
            attr + 2,
            end.saturating_sub(1),
            format!("{module_dir}tests.rs"),
            "tests body".to_string(),
        ));
    }
    for c in r.clusters.iter().filter(|c| c.members.len() > 1) {
        let dest = format!("{module_dir}{}.rs", c.name);
        let mut spans: Vec<(usize, usize)> =
            c.members.iter().map(|m| (m.line_start as usize, m.line_end as usize)).collect();
        spans.sort();
        let merged: Vec<(usize, usize)> = spans.into_iter().fold(
            Vec::new(),
            |mut acc, (s, e)| {
                match acc.last_mut() {
                    Some(last) if s <= last.1 + 2 => last.1 = last.1.max(e),
                    _ => acc.push((s, e)),
                }
                acc
            },
        );
        for (s, e) in merged {
            steps.push((s, e, dest.clone(), c.name.clone()));
        }
    }
    steps.sort_by(|a, b| b.0.cmp(&a.0));
    for (s, e, dest, label) in &steps {
        out.push_str(&format!(
            "[[move]]\n# {label}\nstart = {s}\nend = {e}\ndest = \"{dest}\"\n\n"
        ));
    }
    if let Some((attr, end)) = tests_span {
        out.push_str(&format!(
            "[[patch]]\n# the tests opener + closing brace become a declaration\nstart = {attr}\nend = {end}\nbody = \"\"\"#[cfg(test)]\nmod tests;\"\"\"\n\n"
        ));
    }
    out.push_str(&format!(
        "# End state: no file over {max_lines} lines (ARCH §3.1). The executor runs\n\
         # verify_cmd after each step; git is the rollback.\n"
    ));
    out
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    use std::path::PathBuf;

    fn write_fixture(path: &Path) {
        std::fs::write(
            path,
            "use super::*;\n\npub fn thing() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn t() {\n        assert!(true);\n    }\n}\n",
        )
        .unwrap();
    }

    /// The tail convention finder: attr + opener + closing brace at EOF.
    #[test]
    fn find_tail_tests_span_finds_the_conventional_block() {
        let dir = tempfile_dir("span");
        let p: PathBuf = dir.join("f.rs");
        write_fixture(&p);
        // 8 lines total: 4 = #[cfg(test)], 5 = mod tests {, 9 = closing }? —
        // fixture: 1 use, 2 blank, 3 pub fn, 4 blank, 5 attr, 6 opener,
        // 7-9 body, 10 close. The span is (5, 10).
        let span = find_tail_tests_span(&p).expect("conventional tail block");
        assert_eq!(span, (5, 13));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tests_module_not_at_the_tail_is_declined() {
        let dir = tempfile_dir("span2");
        let p: PathBuf = dir.join("f.rs");
        std::fs::write(
            &p,
            "#[cfg(test)]\nmod tests {\n}\n\npub fn after() {}\n",
        )
        .unwrap();
        assert!(find_tail_tests_span(&p).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The rendered plan must be VALID TOML in exactly the shape
    /// `cargo xtask refactor-apply` parses: subject under [plan],
    /// moves ordered by span start DESCENDING, the tests patch last.
    #[test]
    fn the_plan_renders_as_executor_ready_toml() {
        let dir = tempfile_dir("plan");
        let p: PathBuf = dir.join("f.rs");
        write_fixture(&p);
        let span = find_tail_tests_span(&p);
        let mut r = crate::code::suggest_seams::tests::fixture();
        r.file = "crates/app/src/cmd.rs".to_string();
        let plan = render_split_plan(&r, span, 1200);
        eprintln!("=== RENDERED ===\n{plan}===");
        let v: serde_json::Value = toml::from_str(&plan).expect("valid TOML");
        eprintln!("=== PARSED moves === {:?}", v.get("move"));
        assert_eq!(
            v["plan"]["subject"].as_str(),
            Some("crates/app/src/cmd.rs")
        );
        let moves = v["move"].as_array().expect("moves array");
        // fixture cluster `ingest` has two members at 10-90 and 92-120
        // (merged into one span) + the tests move from the fixture file.
        let starts: Vec<u64> = moves
            .iter()
            .map(|m| m["start"].as_u64().unwrap())
            .collect();
        let mut sorted = starts.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(starts, sorted, "moves must run bottom-up");
        // [[patch]] is an array-of-tables even with a single entry.
        assert_eq!(
            v["patch"][0]["body"].as_str().unwrap().contains("mod tests;"),
            true
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tempfile_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("seams-plan-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
