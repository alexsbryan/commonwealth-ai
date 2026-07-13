// SPDX-License-Identifier: AGPL-3.0-or-later
//! Architecture metrics over the SCIP graph — the OBSERVED half of the
//! quality program's dependency-direction story.
//!
//! `xtask layer-gate` checks Cargo-DECLARED edges; this module computes what
//! the code actually does: which crates reference which (through re-exports,
//! which Cargo cannot see), which symbols carry each coupling edge, per-crate
//! fan-in/fan-out/instability, per-file fan-in, and intra-crate file-level
//! reference cycles. The DELTA between declared and observed is itself a
//! finding: a declared dep with zero observed references is a removal
//! candidate; observed references with no direct Cargo edge name the
//! coupling a re-export chain hides.
//!
//! Pure math over `&[ScipSymbolRecord]` / `&[ScipRefRecord]` (the same inputs
//! as `capability_map`) — no I/O, no new dependencies. Callers supply the
//! declared graph (from `cargo metadata`) if they have one.
//!
//! Honest limitations, stated once:
//! - SCIP misses macro-expanded references and some dynamic trait dispatch,
//!   so `DeclaredNeverObserved` is a CANDIDATE, never an auto-fail.
//!   Measured miss rate from the 2026-07-12 dead-edge cleanup: 3 of 13
//!   candidates were real edges the index missed — all function-scoped or
//!   single-handler `use` imports (`use dep::Item;` consumed in one spot).
//!   Grep before cutting.
//! - Cycles here are file-level *reference* cycles within a crate (Tarjan
//!   SCC over the intra-crate file graph) — `use`-only and type-only cycles
//!   that produce no resolved reference are invisible; `cargo modules
//!   dependencies --acyclic` in the weekly lane owns those.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

use crate::capability_map::{pkg_and_desc, short};
use crate::scip_graph::{ScipRefRecord, ScipSymbolRecord};

// ── Inputs ────────────────────────────────────────────────────────────────────

/// Cargo-declared workspace dependency edges, crate names normalized to
/// UNDERSCORES (SCIP package names are underscored; cargo names hyphenated —
/// use [`normalize_crate_name`] when building this).
#[derive(Debug, Default)]
pub struct DeclaredDeps {
    pub edges: BTreeMap<String, BTreeSet<String>>,
}

impl DeclaredDeps {
    /// Every crate name the declared graph knows (sources and targets).
    fn universe(&self) -> BTreeSet<&str> {
        self.edges
            .iter()
            .flat_map(|(k, vs)| std::iter::once(k.as_str()).chain(vs.iter().map(|v| v.as_str())))
            .collect()
    }
}

/// `-` → `_`, the join key between cargo and SCIP crate naming.
pub fn normalize_crate_name(name: &str) -> String {
    name.replace('-', "_")
}

pub struct ArchOptions {
    /// Carrier symbols listed per cross-crate edge.
    pub top_symbols_per_edge: usize,
    /// File-fan-in entries kept overall.
    pub top_files: usize,
    /// Cycles reported (largest first).
    pub max_cycles: usize,
}

impl Default for ArchOptions {
    fn default() -> Self {
        Self {
            top_symbols_per_edge: 10,
            top_files: 50,
            max_cycles: 20,
        }
    }
}

// ── Outputs ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct CrateMetrics {
    /// SCIP package name (underscored).
    pub name: String,
    /// Distinct crates whose code references this crate.
    pub fan_in: usize,
    /// Distinct crates this crate's code references.
    pub fan_out: usize,
    /// Total inbound / outbound cross-crate reference counts.
    pub in_refs: usize,
    pub out_refs: usize,
    /// Martin instability I = Ce/(Ca+Ce) over observed crate edges.
    /// 0 = maximally stable (depended-upon, depends on nothing);
    /// 1 = maximally unstable. `None` when isolated.
    pub instability: Option<f64>,
    /// Same shape over Cargo-declared edges, when a declared graph was given.
    pub declared_fan_in: Option<usize>,
    pub declared_fan_out: Option<usize>,
}

/// One observed crate→crate coupling edge, with the symbols that carry it —
/// the actionable input for interface extraction.
#[derive(Debug, Clone, Serialize)]
pub struct CrossCrateEdge {
    pub from_crate: String,
    pub to_crate: String,
    pub ref_count: usize,
    pub distinct_symbols: usize,
    /// (callee qualified name, reference count), heaviest first.
    pub top_symbols: Vec<(String, usize)>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum DepDelta {
    /// Cargo edge with zero observed symbol references — removal CANDIDATE
    /// (SCIP misses macro/dyn-dispatch refs; verify before cutting).
    DeclaredNeverObserved { from: String, to: String },
    /// Observed references with no direct Cargo edge — the reference reaches
    /// the true definer through a re-export chain. Names the implicit
    /// coupling glob re-exports hide.
    ObservedNotDeclared {
        from: String,
        to: String,
        ref_count: usize,
    },
}

/// Per-file fan-in within a crate (intra-crate references only) — the
/// seam-finding signal for god-file splits.
#[derive(Debug, Clone, Serialize)]
pub struct FileMetrics {
    pub crate_name: String,
    pub file: String,
    /// Distinct same-crate files referencing this file.
    pub fan_in: usize,
    pub fan_out: usize,
}

/// A strongly-connected component of ≥2 files in one crate's intra-crate
/// reference graph: these files can only be understood together.
#[derive(Debug, Clone, Serialize)]
pub struct FileCycle {
    pub crate_name: String,
    pub members: Vec<String>,
    /// Receipt edges (subset) proving the cycle.
    pub sample_edges: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ArchStats {
    pub crates: usize,
    pub refs_total: usize,
    pub refs_cross_crate: usize,
    pub refs_dropped_unattributed: usize,
    pub refs_dropped_test: usize,
    /// References into packages with no defined symbols in this corpus
    /// (std/core/alloc/third-party) — external substrate, not coupling.
    pub refs_dropped_external: usize,
}

#[derive(Debug, Serialize)]
pub struct ArchMetrics {
    /// Sorted by fan_in descending — the god-crate table.
    pub crates: Vec<CrateMetrics>,
    /// Sorted by ref_count descending.
    pub cross_edges: Vec<CrossCrateEdge>,
    pub deltas: Vec<DepDelta>,
    /// Top intra-crate file fan-in, descending.
    pub files: Vec<FileMetrics>,
    /// Largest first, capped at `max_cycles`.
    pub cycles: Vec<FileCycle>,
    pub stats: ArchStats,
}

// ── Compute ───────────────────────────────────────────────────────────────────

pub fn compute(
    symbols: &[ScipSymbolRecord],
    refs: &[ScipRefRecord],
    declared: Option<&DeclaredDeps>,
    opts: &ArchOptions,
) -> ArchMetrics {
    // qualified name → defining file, for test detection + file graphs.
    let mut qn2file: HashMap<&str, &str> = HashMap::new();
    let mut all_crates: BTreeSet<String> = BTreeSet::new();
    for s in symbols {
        if !s.qualified_name.is_empty() {
            qn2file.insert(&s.qualified_name, &s.file_path);
        }
        if let Some((pkg, _)) = pkg_and_desc(&s.qualified_name) {
            all_crates.insert(pkg.to_string());
        }
    }
    let is_test = |qn: &str| -> bool {
        if let Some((_, desc)) = pkg_and_desc(qn) {
            if desc.contains("tests/") || desc.starts_with("harness/") {
                return true;
            }
        }
        matches!(qn2file.get(qn), Some(f) if f.contains("/tests/") || f.ends_with("_test.rs"))
    };

    let mut stats = ArchStats {
        crates: all_crates.len(),
        ..Default::default()
    };

    // Crate-level aggregation. Unlike CallGraph::from_scip we KEEP
    // non-function references — type references carry coupling too.
    let mut edge_refs: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut edge_symbols: HashMap<(String, String), HashMap<String, usize>> = HashMap::new();
    // Intra-crate file graphs: crate → from_file → to_files.
    let mut file_adj: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();

    for r in refs {
        stats.refs_total += 1;
        let (Some((from_pkg, _)), Some((to_pkg, _))) = (
            pkg_and_desc(&r.caller_qualified),
            pkg_and_desc(&r.callee_qualified),
        ) else {
            stats.refs_dropped_unattributed += 1;
            continue;
        };
        // First-party only (the CallGraph discipline): a package with no
        // DEFINED symbols here is external substrate (std/core/serde/…) —
        // calling into it is not architecture coupling.
        if !all_crates.contains(from_pkg) || !all_crates.contains(to_pkg) {
            stats.refs_dropped_external += 1;
            continue;
        }
        if is_test(&r.caller_qualified) || is_test(&r.callee_qualified) {
            stats.refs_dropped_test += 1;
            continue;
        }
        if from_pkg != to_pkg {
            stats.refs_cross_crate += 1;
            let key = (from_pkg.to_string(), to_pkg.to_string());
            *edge_refs.entry(key.clone()).or_default() += 1;
            *edge_symbols
                .entry(key)
                .or_default()
                .entry(r.callee_qualified.clone())
                .or_default() += 1;
        } else if let (Some(&from_file), Some(&to_file)) = (
            qn2file.get(r.caller_qualified.as_str()),
            qn2file.get(r.callee_qualified.as_str()),
        ) {
            if from_file != to_file {
                file_adj
                    .entry(from_pkg.to_string())
                    .or_default()
                    .entry(from_file.to_string())
                    .or_default()
                    .insert(to_file.to_string());
            }
        }
    }

    // ── Crate metrics ────────────────────────────────────────────────────────
    let mut in_crates: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut out_crates: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut in_refs: BTreeMap<&str, usize> = BTreeMap::new();
    let mut out_refs: BTreeMap<&str, usize> = BTreeMap::new();
    for ((from, to), n) in &edge_refs {
        out_crates.entry(from).or_default().insert(to);
        in_crates.entry(to).or_default().insert(from);
        *out_refs.entry(from).or_default() += n;
        *in_refs.entry(to).or_default() += n;
    }
    let mut crates: Vec<CrateMetrics> = all_crates
        .iter()
        .map(|name| {
            let ca = in_crates.get(name.as_str()).map_or(0, BTreeSet::len);
            let ce = out_crates.get(name.as_str()).map_or(0, BTreeSet::len);
            let instability = if ca + ce > 0 {
                Some(ce as f64 / (ca + ce) as f64)
            } else {
                None
            };
            // SCIP package names may be hyphenated or underscored depending
            // on exporter vintage — every declared-side join normalizes.
            let norm = normalize_crate_name(name);
            let (dfi, dfo) = match declared {
                Some(d) => {
                    let dfo = d.edges.get(&norm).map(BTreeSet::len);
                    let dfi = Some(d.edges.values().filter(|vs| vs.contains(&norm)).count());
                    (dfi, dfo)
                }
                None => (None, None),
            };
            CrateMetrics {
                name: name.clone(),
                fan_in: ca,
                fan_out: ce,
                in_refs: in_refs.get(name.as_str()).copied().unwrap_or(0),
                out_refs: out_refs.get(name.as_str()).copied().unwrap_or(0),
                instability,
                declared_fan_in: dfi,
                declared_fan_out: dfo,
            }
        })
        .collect();
    crates.sort_by(|a, b| b.fan_in.cmp(&a.fan_in).then(a.name.cmp(&b.name)));

    // ── Cross-crate edges with carrier symbols ───────────────────────────────
    let mut cross_edges: Vec<CrossCrateEdge> = edge_refs
        .iter()
        .map(|((from, to), &n)| {
            let symbols = edge_symbols.get(&(from.clone(), to.clone()));
            let distinct = symbols.map_or(0, HashMap::len);
            let mut top: Vec<(String, usize)> = symbols
                .map(|m| m.iter().map(|(s, &c)| (s.clone(), c)).collect())
                .unwrap_or_default();
            top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            top.truncate(opts.top_symbols_per_edge);
            CrossCrateEdge {
                from_crate: from.clone(),
                to_crate: to.clone(),
                ref_count: n,
                distinct_symbols: distinct,
                top_symbols: top,
            }
        })
        .collect();
    cross_edges.sort_by(|a, b| b.ref_count.cmp(&a.ref_count));

    // ── Declared ↔ observed deltas ───────────────────────────────────────────
    let mut deltas: Vec<DepDelta> = Vec::new();
    if let Some(d) = declared {
        let universe = d.universe();
        // Normalized views of the observed side, for every declared join.
        let observed_pairs: BTreeSet<(String, String)> = edge_refs
            .keys()
            .map(|(f, t)| (normalize_crate_name(f), normalize_crate_name(t)))
            .collect();
        let seen_crates: BTreeSet<String> =
            all_crates.iter().map(|c| normalize_crate_name(c)).collect();
        for (from, tos) in &d.edges {
            // Only judge crates the SCIP index actually saw — a crate with
            // no indexed symbols proves nothing about its edges.
            if !seen_crates.contains(from) {
                continue;
            }
            for to in tos {
                if !seen_crates.contains(to) {
                    continue;
                }
                if !observed_pairs.contains(&(from.clone(), to.clone())) {
                    deltas.push(DepDelta::DeclaredNeverObserved {
                        from: from.clone(),
                        to: to.clone(),
                    });
                }
            }
        }
        for ((from, to), &n) in &edge_refs {
            let (nf, nt) = (normalize_crate_name(from), normalize_crate_name(to));
            if !universe.contains(nf.as_str()) || !universe.contains(nt.as_str()) {
                continue; // not a workspace pair (e.g. vendored code)
            }
            let declared_direct = d.edges.get(&nf).map(|s| s.contains(&nt)).unwrap_or(false);
            if !declared_direct {
                deltas.push(DepDelta::ObservedNotDeclared {
                    from: from.clone(),
                    to: to.clone(),
                    ref_count: n,
                });
            }
        }
    }

    // ── File fan-in + cycles ─────────────────────────────────────────────────
    let mut files: Vec<FileMetrics> = Vec::new();
    let mut cycles: Vec<FileCycle> = Vec::new();
    for (crate_name, adj) in &file_adj {
        let mut fan_in: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        for (from, tos) in adj {
            for to in tos {
                fan_in.entry(to).or_default().insert(from);
            }
        }
        let mut names: BTreeSet<&str> = adj.keys().map(String::as_str).collect();
        names.extend(fan_in.keys().copied());
        for name in &names {
            files.push(FileMetrics {
                crate_name: crate_name.clone(),
                file: (*name).to_string(),
                fan_in: fan_in.get(name).map_or(0, BTreeSet::len),
                fan_out: adj.get(*name).map_or(0, BTreeSet::len),
            });
        }
        for scc in tarjan_sccs(adj) {
            if scc.len() < 2 {
                continue;
            }
            let members: BTreeSet<&str> = scc.iter().map(String::as_str).collect();
            let mut sample_edges = Vec::new();
            'outer: for m in &scc {
                if let Some(tos) = adj.get(m.as_str()) {
                    for to in tos {
                        if members.contains(to.as_str()) {
                            sample_edges.push((m.clone(), to.clone()));
                            if sample_edges.len() >= 6 {
                                break 'outer;
                            }
                        }
                    }
                }
            }
            cycles.push(FileCycle {
                crate_name: crate_name.clone(),
                members: scc,
                sample_edges,
            });
        }
    }
    files.sort_by(|a, b| b.fan_in.cmp(&a.fan_in).then(a.file.cmp(&b.file)));
    files.truncate(opts.top_files);
    cycles.sort_by(|a, b| b.members.len().cmp(&a.members.len()));
    cycles.truncate(opts.max_cycles);

    ArchMetrics {
        crates,
        cross_edges,
        deltas,
        files,
        cycles,
        stats,
    }
}

/// Unordered file-pair set with ANY resolved reference between them (either
/// direction, any crate, tests INCLUDED — a shared test still relates two
/// files). The structural-edge oracle for the temporal-coupling join: "are
/// these files related at all, structurally?". Pairs are (min, max)-ordered.
pub fn file_edge_pairs(
    symbols: &[ScipSymbolRecord],
    refs: &[ScipRefRecord],
) -> std::collections::HashSet<(String, String)> {
    let mut qn2file: HashMap<&str, &str> = HashMap::new();
    for s in symbols {
        if !s.qualified_name.is_empty() {
            qn2file.insert(&s.qualified_name, &s.file_path);
        }
    }
    let mut out = std::collections::HashSet::new();
    for r in refs {
        if let (Some(&a), Some(&b)) = (
            qn2file.get(r.caller_qualified.as_str()),
            qn2file.get(r.callee_qualified.as_str()),
        ) {
            if a != b {
                let (x, y) = if a <= b { (a, b) } else { (b, a) };
                out.insert((x.to_string(), y.to_string()));
            }
        }
    }
    out
}

/// Iterative Tarjan SCC over a string-keyed adjacency map. Returns each SCC
/// as a sorted member list (singletons included; callers filter).
fn tarjan_sccs(adj: &BTreeMap<String, BTreeSet<String>>) -> Vec<Vec<String>> {
    // Index the node set (sources + targets), then build integer edges.
    let mut node_set: BTreeSet<&str> = BTreeSet::new();
    for (from, tos) in adj {
        node_set.insert(from.as_str());
        for to in tos {
            node_set.insert(to.as_str());
        }
    }
    let nodes: Vec<&str> = node_set.into_iter().collect();
    let index_of: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, &s)| (s, i)).collect();
    let n = nodes.len();
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (from, tos) in adj {
        let fi = index_of[from.as_str()];
        for to in tos {
            edges[fi].push(index_of[to.as_str()]);
        }
    }

    // Iterative Tarjan.
    const UNSET: usize = usize::MAX;
    let mut index = vec![UNSET; n];
    let mut lowlink = vec![UNSET; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index = 0usize;
    let mut sccs: Vec<Vec<String>> = Vec::new();

    for root in 0..n {
        if index[root] != UNSET {
            continue;
        }
        // (node, next child position)
        let mut call: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&mut (v, ref mut child)) = call.last_mut() {
            if *child == 0 {
                index[v] = next_index;
                lowlink[v] = next_index;
                next_index += 1;
                stack.push(v);
                on_stack[v] = true;
            }
            if *child < edges[v].len() {
                let w = edges[v][*child];
                *child += 1;
                if index[w] == UNSET {
                    call.push((w, 0));
                } else if on_stack[w] {
                    lowlink[v] = lowlink[v].min(index[w]);
                }
            } else {
                if lowlink[v] == index[v] {
                    let mut scc = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        scc.push(nodes[w].to_string());
                        if w == v {
                            break;
                        }
                    }
                    scc.sort();
                    sccs.push(scc);
                }
                let done = lowlink[v];
                call.pop();
                if let Some(&mut (parent, _)) = call.last_mut() {
                    lowlink[parent] = lowlink[parent].min(done);
                }
            }
        }
    }
    sccs
}

// ── Render ────────────────────────────────────────────────────────────────────

/// One rendering shared by the CLI verb and the MCP tool (the
/// `capability_map::render_markdown` discipline — surfaces can't drift).
pub fn render_markdown(corpus_id: &str, m: &ArchMetrics) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(out, "# Architecture report — {corpus_id}\n");
    let _ = writeln!(
        out,
        "_{} crates · {} refs ({} cross-crate; dropped: {} external, {} test, {} unattributed)_\n",
        m.stats.crates,
        m.stats.refs_total,
        m.stats.refs_cross_crate,
        m.stats.refs_dropped_external,
        m.stats.refs_dropped_test,
        m.stats.refs_dropped_unattributed,
    );

    let _ = writeln!(out, "## Crate coupling (observed)\n");
    let _ = writeln!(
        out,
        "| crate | fan-in | fan-out | instability | in-refs | out-refs | declared in/out |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|---|---|");
    for c in &m.crates {
        let inst = c
            .instability
            .map(|i| format!("{i:.2}"))
            .unwrap_or_else(|| "—".into());
        let decl = match (c.declared_fan_in, c.declared_fan_out) {
            (Some(i), Some(o)) => format!("{i}/{o}"),
            _ => "—".into(),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} |",
            c.name, c.fan_in, c.fan_out, inst, c.in_refs, c.out_refs, decl
        );
    }

    let _ = writeln!(out, "\n## Heaviest cross-crate edges\n");
    for e in m.cross_edges.iter().take(15) {
        let _ = writeln!(
            out,
            "### {} → {} — {} refs across {} symbols",
            e.from_crate, e.to_crate, e.ref_count, e.distinct_symbols
        );
        for (sym, n) in &e.top_symbols {
            let _ = writeln!(out, "- `{}` ({n})", short(sym));
        }
        let _ = writeln!(out);
    }

    if !m.deltas.is_empty() {
        let _ = writeln!(out, "## Declared ↔ observed deltas\n");
        for d in &m.deltas {
            match d {
                DepDelta::DeclaredNeverObserved { from, to } => {
                    let _ = writeln!(
                        out,
                        "- **declared-never-observed**: `{from}` → `{to}` — Cargo edge with \
                         zero observed references (removal candidate; verify macros/dyn \
                         dispatch before cutting)"
                    );
                }
                DepDelta::ObservedNotDeclared {
                    from,
                    to,
                    ref_count,
                } => {
                    let _ = writeln!(
                        out,
                        "- **observed-not-declared**: `{from}` → `{to}` ({ref_count} refs) — \
                         coupling reaches this crate through a re-export chain Cargo can't see"
                    );
                }
            }
        }
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "## File fan-in hotspots (intra-crate)\n");
    let _ = writeln!(out, "| file | crate | fan-in | fan-out |");
    let _ = writeln!(out, "|---|---|---|---|");
    for f in m.files.iter().take(15) {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |",
            f.file, f.crate_name, f.fan_in, f.fan_out
        );
    }

    if m.cycles.is_empty() {
        let _ = writeln!(out, "\n## Intra-crate file cycles\n\nNone detected.");
    } else {
        let _ = writeln!(out, "\n## Intra-crate file cycles\n");
        for c in &m.cycles {
            let shown = c.members.iter().take(8).cloned().collect::<Vec<_>>();
            let suffix = if c.members.len() > shown.len() {
                format!(
                    " … and {} more (full list in the JSON)",
                    c.members.len() - shown.len()
                )
            } else {
                String::new()
            };
            let _ = writeln!(
                out,
                "- **{}**: {} files entangled — {}{}",
                c.crate_name,
                c.members.len(),
                shown.join(" ⇄ "),
                suffix
            );
            for (a, b) in &c.sample_edges {
                let _ = writeln!(out, "  - {a} → {b}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(qn: &str, file: &str) -> ScipSymbolRecord {
        ScipSymbolRecord {
            name: qn.rsplit('/').next().unwrap_or(qn).to_string(),
            qualified_name: qn.to_string(),
            kind: "function".into(),
            file_path: file.to_string(),
            line_start: 1,
            line_end: 2,
            language: "rust".into(),
        }
    }

    fn r(caller: &str, callee: &str) -> ScipRefRecord {
        ScipRefRecord {
            caller_symbol: String::new(),
            callee_symbol: String::new(),
            caller_qualified: caller.to_string(),
            callee_qualified: callee.to_string(),
            file_path: String::new(),
            line: 1,
            ref_kind: "call".into(),
        }
    }

    // SCIP-shaped qualified names: "<scheme> <manager> <package> <version> <descriptor>"
    fn qn(pkg: &str, desc: &str) -> String {
        format!("rust-analyzer cargo {pkg} 0.1.0 {desc}")
    }

    #[test]
    fn crate_fan_in_out_and_instability() {
        let a1 = qn("crate_a", "src/lib.rs/f().");
        let b1 = qn("crate_b", "src/lib.rs/g().");
        let c1 = qn("crate_c", "src/lib.rs/h().");
        let symbols = vec![
            sym(&a1, "crate-a/src/lib.rs"),
            sym(&b1, "crate-b/src/lib.rs"),
            sym(&c1, "crate-c/src/lib.rs"),
        ];
        // a → b, c → b: b has fan-in 2, fan-out 0 → instability 0.
        let refs = vec![r(&a1, &b1), r(&c1, &b1), r(&a1, &b1)];
        let m = compute(&symbols, &refs, None, &ArchOptions::default());
        let b = m.crates.iter().find(|c| c.name == "crate_b").unwrap();
        assert_eq!(b.fan_in, 2);
        assert_eq!(b.fan_out, 0);
        assert_eq!(b.in_refs, 3);
        assert_eq!(b.instability, Some(0.0));
        let a = m.crates.iter().find(|c| c.name == "crate_a").unwrap();
        assert_eq!(a.instability, Some(1.0));
        // b is the top god-crate row.
        assert_eq!(m.crates[0].name, "crate_b");
        // The heaviest edge carries g() twice.
        assert_eq!(m.cross_edges[0].ref_count, 2);
        assert_eq!(m.cross_edges[0].top_symbols[0].1, 2);
    }

    #[test]
    fn deltas_both_directions() {
        let a1 = qn("crate_a", "src/lib.rs/f().");
        let b1 = qn("crate_b", "src/lib.rs/g().");
        let symbols = vec![
            sym(&a1, "crate-a/src/lib.rs"),
            sym(&b1, "crate-b/src/lib.rs"),
        ];
        let refs = vec![r(&a1, &b1)];
        let mut declared = DeclaredDeps::default();
        // Declared: a → c (never observed, but c has no symbols → skipped),
        // b → a (never observed, both indexed → delta). a → b is observed but
        // NOT declared → ObservedNotDeclared.
        declared
            .edges
            .entry("crate_b".into())
            .or_default()
            .insert("crate_a".into());
        declared
            .edges
            .entry("crate_a".into())
            .or_default()
            .insert("crate_c".into());
        let m = compute(&symbols, &refs, Some(&declared), &ArchOptions::default());
        assert!(m.deltas.iter().any(|d| matches!(
            d,
            DepDelta::DeclaredNeverObserved { from, to } if from == "crate_b" && to == "crate_a"
        )));
        assert!(m.deltas.iter().any(|d| matches!(
            d,
            DepDelta::ObservedNotDeclared { from, to, ref_count: 1 }
                if from == "crate_a" && to == "crate_b"
        )));
        // crate_c has no indexed symbols — no verdict about it.
        assert!(!m
            .deltas
            .iter()
            .any(|d| matches!(d, DepDelta::DeclaredNeverObserved { to, .. } if to == "crate_c")));
    }

    #[test]
    fn test_refs_are_dropped() {
        let a1 = qn("crate_a", "tests/it.rs/f().");
        let b1 = qn("crate_b", "src/lib.rs/g().");
        let symbols = vec![
            sym(&a1, "crate-a/tests/it.rs"),
            sym(&b1, "crate-b/src/lib.rs"),
        ];
        let refs = vec![r(&a1, &b1)];
        let m = compute(&symbols, &refs, None, &ArchOptions::default());
        assert_eq!(m.stats.refs_dropped_test, 1);
        assert!(m.cross_edges.is_empty());
    }

    #[test]
    fn file_fan_in_and_cycles() {
        let fa = qn("crate_a", "src/a.rs/fa().");
        let fb = qn("crate_a", "src/b.rs/fb().");
        let fc = qn("crate_a", "src/c.rs/fc().");
        let symbols = vec![
            sym(&fa, "crate-a/src/a.rs"),
            sym(&fb, "crate-a/src/b.rs"),
            sym(&fc, "crate-a/src/c.rs"),
        ];
        // a → b, b → a (cycle), c → a.
        let refs = vec![r(&fa, &fb), r(&fb, &fa), r(&fc, &fa)];
        let m = compute(&symbols, &refs, None, &ArchOptions::default());
        let a_metrics = m
            .files
            .iter()
            .find(|f| f.file == "crate-a/src/a.rs")
            .unwrap();
        assert_eq!(a_metrics.fan_in, 2); // b and c reference a
        assert_eq!(m.cycles.len(), 1);
        assert_eq!(m.cycles[0].members.len(), 2);
        assert!(!m.cycles[0].sample_edges.is_empty());
    }

    #[test]
    fn tarjan_finds_the_three_cycle_not_the_tail() {
        let mut adj: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (f, t) in [("a", "b"), ("b", "c"), ("c", "a"), ("c", "d")] {
            adj.entry(f.into()).or_default().insert(t.into());
        }
        let sccs = tarjan_sccs(&adj);
        let big: Vec<_> = sccs.iter().filter(|s| s.len() > 1).collect();
        assert_eq!(big.len(), 1);
        assert_eq!(big[0].len(), 3);
    }
}
