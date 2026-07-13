// SPDX-License-Identifier: AGPL-3.0-or-later
//! Capability map — derive a clustered map of *what the codebase does* from the
//! SCIP call graph.
//!
//! ## The idea
//! A "capability" is a **cluster of entry points that share a reachable call
//! spine**. We root reachability at the places execution enters the system from
//! outside (CLI verbs, HTTP routes, tools, handlers), then cluster those entry
//! points by how much of the call graph they reach in common. Entry points that
//! flow into the same machinery belong to the same capability.
//!
//! ## Why this is language-agnostic
//! Every stage here except entry-point detection reads ONLY SCIP-spec structure —
//! the symbol grammar (`<scheme> <manager> <package> <version> <descriptor>`) and
//! the descriptor suffixes (`().` method · `#` type · `/` namespace). Those are
//! identical across `rust-analyzer`, `scip-typescript`, `scip-python`,
//! `scip-go`, `scip-java`, … so the substrate, reachability, clustering and
//! core/deps extraction generalize for free (see the parser self-test). The one
//! framework-specific concern — *where does an external caller enter?* — lives
//! behind [`EntryPointProvider`], with a universal [`FallbackProvider`] that needs
//! no per-stack knowledge.
//!
//! ## What it deliberately does NOT do
//! Resolve dynamic dispatch (trait method → impls). Linking every
//! `Provider::method()` call site to every impl over-connects the graph and
//! collapses the partition; trait boundaries are left intact.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::scip_graph::{ScipRefRecord, ScipSymbolRecord};

// ============================================================================
// SCIP symbol grammar — language-agnostic helpers (per the SCIP spec)
//   global: "<scheme> <manager> <package> <version> <descriptors>"
//   local:  "local <id>"
//   descriptor suffix: '/' namespace · '#' type · '.' term · '().' method
// ============================================================================

/// Split a SCIP global symbol into `(package, descriptor)`. Returns `None` for
/// `local …` symbols and anything that isn't a 5-field global symbol. Keys on
/// spec *position* (package = field 3, descriptor = field 5+), not on Rust.
pub fn pkg_and_desc(sym: &str) -> Option<(&str, &str)> {
    if sym.starts_with("local ") {
        return None;
    }
    // scheme, manager, package, version, <descriptor (may contain spaces)>
    let mut it = sym.splitn(5, ' ');
    let _scheme = it.next()?;
    let _manager = it.next()?;
    let pkg = it.next()?;
    let _version = it.next()?;
    let desc = it.next()?;
    if pkg.is_empty() || desc.is_empty() {
        return None;
    }
    Some((pkg, desc))
}

/// The final descriptor segment, e.g. `impl#[Runtime]handle_code_query().`.
fn leaf(desc: &str) -> &str {
    desc.rsplit('/').next().unwrap_or(desc)
}

/// A descriptor names a function/method iff it ends in a term (`.`) and its leaf
/// carries a parameter list (`(`). Fields end in `.` without parens; types end `#`.
pub fn is_function(desc: &str) -> bool {
    desc.ends_with('.') && leaf(desc).contains('(')
}

/// The bare method/function name, stripping the `(args).` suffix and any
/// `impl#[Concrete][Trait]` / `Type#` wrapper. `impl#[Runtime]run().` → `run`.
fn method_name(desc: &str) -> &str {
    let l = leaf(desc);
    let core = match l.find('(') {
        Some(i) => &l[..i],
        None => l.trim_end_matches('.'),
    };
    core.rsplit([']', '#']).next().unwrap_or(core)
}

/// The top descriptor segment (a coarse module label), or `(root)` for an
/// `impl#[…]`-rooted descriptor with no namespace prefix.
fn module_seg(desc: &str) -> &str {
    let head = desc.split('/').next().unwrap_or(desc);
    if head.is_empty() || head.starts_with("impl") {
        "(root)"
    } else {
        head
    }
}

/// A human-scannable rendering of a symbol: `package::leaf` (trailing `.` trimmed).
/// The map stores full qualified ids; callers use this only for display.
pub fn short(sym: &str) -> String {
    match pkg_and_desc(sym) {
        Some((pkg, desc)) => format!("{pkg}::{}", leaf(desc).trim_end_matches('.')),
        None => sym.to_string(),
    }
}

/// Test code, recognized either by descriptor (`tests/`, `harness/`, `test…`) or
/// by the defining file path (`/tests/`, `*_test.rs`).
fn is_test(sym: &str, qn2file: &HashMap<String, String>) -> bool {
    if let Some((_, desc)) = pkg_and_desc(sym) {
        if desc.contains("tests/") || desc.starts_with("harness/") || desc.starts_with("test") {
            return true;
        }
    }
    if let Some(f) = qn2file.get(sym) {
        if f.contains("/tests/") || f.ends_with("_test.rs") {
            return true;
        }
    }
    false
}

// ============================================================================
// Stage 1 — substrate: the first-party function-call graph
// ============================================================================

#[derive(Debug, Clone, Default, Serialize)]
pub struct SubstrateStats {
    pub first_party_packages: usize,
    pub kept_edges: usize,
    pub dropped_external: usize,
    pub dropped_nonfunction: usize,
    pub dropped_test: usize,
    pub nodes: usize,
}

/// The filtered call graph: first-party → first-party **function** call edges,
/// tests removed. Node identity is the full SCIP qualified string.
pub struct CallGraph {
    adj: HashMap<String, HashSet<String>>,
    rev: HashMap<String, HashSet<String>>,
    nodes: HashSet<String>,
    pub stats: SubstrateStats,
}

impl CallGraph {
    /// Build the substrate from a corpus's symbols + refs. "First-party" = any
    /// package that appears among the *defined* symbols (i.e. indexed in this
    /// repo); everything else (std, third-party crates) is external substrate.
    pub fn from_scip(symbols: &[ScipSymbolRecord], refs: &[ScipRefRecord]) -> Self {
        let mut first_party: HashSet<&str> = HashSet::new();
        let mut qn2file: HashMap<String, String> = HashMap::new();
        for s in symbols {
            if let Some((pkg, _)) = pkg_and_desc(&s.qualified_name) {
                first_party.insert(pkg);
            }
            if !s.qualified_name.is_empty() {
                qn2file.insert(s.qualified_name.clone(), s.file_path.clone());
            }
        }

        let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
        let mut rev: HashMap<String, HashSet<String>> = HashMap::new();
        let mut nodes: HashSet<String> = HashSet::new();
        let mut stats = SubstrateStats {
            first_party_packages: first_party.len(),
            ..Default::default()
        };

        for r in refs {
            let (a, b) = (&r.caller_qualified, &r.callee_qualified);
            if a.is_empty() || b.is_empty() || a == b {
                continue;
            }
            let pa = pkg_and_desc(a).map(|(p, _)| p);
            let bd = pkg_and_desc(b);
            let (pb, db) = match bd {
                Some((p, d)) => (Some(p), d),
                None => (None, ""),
            };
            match (pa, pb) {
                (Some(pa), Some(pb)) if first_party.contains(pa) && first_party.contains(pb) => {}
                _ => {
                    stats.dropped_external += 1;
                    continue;
                }
            }
            if !is_function(db) {
                stats.dropped_nonfunction += 1;
                continue;
            }
            if is_test(a, &qn2file) || is_test(b, &qn2file) {
                stats.dropped_test += 1;
                continue;
            }
            adj.entry(a.clone()).or_default().insert(b.clone());
            rev.entry(b.clone()).or_default().insert(a.clone());
            nodes.insert(a.clone());
            nodes.insert(b.clone());
            stats.kept_edges += 1;
        }
        stats.nodes = nodes.len();
        CallGraph {
            adj,
            rev,
            nodes,
            stats,
        }
    }

    /// All nodes that are functions (every node is a qualified id; entry-point
    /// providers filter to the callable ones).
    pub fn function_nodes(&self) -> impl Iterator<Item = &String> {
        self.nodes.iter().filter(|n| {
            pkg_and_desc(n)
                .map(|(_, d)| is_function(d))
                .unwrap_or(false)
        })
    }

    fn callees(&self, n: &str) -> Option<&HashSet<String>> {
        self.adj.get(n)
    }

    /// First-party in-degree of `n` restricted to function callers.
    fn in_degree(&self, n: &str) -> usize {
        self.rev.get(n).map(|s| s.len()).unwrap_or(0)
    }

    /// Downward reachable set (callees, transitively); excludes the root itself.
    fn reachable(&self, root: &str) -> HashSet<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut stack = vec![root.to_string()];
        while let Some(x) = stack.pop() {
            if let Some(cs) = self.callees(&x) {
                for y in cs {
                    if seen.insert(y.clone()) {
                        stack.push(y.clone());
                    }
                }
            }
        }
        seen.remove(root);
        seen
    }
}

// ============================================================================
// Stage 2 — entry-point detection (the pluggable, framework-specific seam)
// ============================================================================

/// Where does an external caller (user, framework, scheduler) enter first-party
/// code? Implementations recognize that boundary. Porting to a new stack means
/// adding an implementation here; nothing else in this module changes.
pub trait EntryPointProvider {
    fn roots(&self, g: &CallGraph) -> Vec<String>;
}

/// Sovereign/Rust-stack conventions. (Express: `app.VERB(path, h)` · FastAPI:
/// `@app.route` · Spring: `@RequestMapping` would each be a sibling provider.)
pub struct HeuristicProvider;

impl HeuristicProvider {
    /// Classify a descriptor as an entry point, or `None`. Public so a CLI can
    /// report the per-kind breakdown.
    pub fn classify(desc: &str) -> Option<&'static str> {
        if desc.contains("tests/") || desc.starts_with("harness/") || desc.starts_with("test") {
            return None;
        }
        let m = method_name(desc);
        if desc.contains("_cmd/") && (m == "run" || m.starts_with("run_")) {
            return Some("cli");
        }
        if desc.contains("Tool]") && m == "execute" {
            return Some("tool");
        }
        if desc.contains("Step]") && m == "run" {
            return Some("wfstep");
        }
        if (desc.starts_with("routes/")
            || desc.starts_with("routes_mcp/")
            || desc.starts_with("ws/"))
            && desc.ends_with(").")
        {
            return Some("http");
        }
        if m.starts_with("handle_") {
            return Some("handler");
        }
        None
    }
}

impl EntryPointProvider for HeuristicProvider {
    fn roots(&self, g: &CallGraph) -> Vec<String> {
        g.function_nodes()
            .filter(|n| {
                pkg_and_desc(n)
                    .and_then(|(_, d)| Self::classify(d))
                    .is_some()
            })
            .cloned()
            .collect()
    }
}

/// Universal, zero-config fallback: a root is a first-party function that no
/// first-party function calls (in-degree 0). High-fanout in-degree-0 nodes are
/// *dispatchers* — unwrapped to their callees so per-verb handlers surface
/// instead of a single `main`. Lower precision than a framework provider, but it
/// produces a map for any SCIP language with no rules at all.
pub struct FallbackProvider {
    pub dispatcher_fanout: usize,
}

impl Default for FallbackProvider {
    fn default() -> Self {
        Self {
            dispatcher_fanout: 12,
        }
    }
}

impl EntryPointProvider for FallbackProvider {
    fn roots(&self, g: &CallGraph) -> Vec<String> {
        let funcs: HashSet<&String> = g.function_nodes().collect();
        let mut out: HashSet<String> = HashSet::new();
        for f in &funcs {
            if g.in_degree(f) != 0 {
                continue;
            }
            let callees: Vec<&String> = g
                .callees(f)
                .map(|s| s.iter().filter(|c| funcs.contains(*c)).collect())
                .unwrap_or_default();
            if callees.len() >= self.dispatcher_fanout {
                for c in callees {
                    out.insert(c.clone());
                }
            } else {
                out.insert((*f).clone());
            }
        }
        out.into_iter().collect()
    }
}

// ============================================================================
// Stages 3-5 — reach → cluster → core/deps
// ============================================================================

/// One derived capability.
#[derive(Debug, Clone, Serialize)]
pub struct Capability {
    /// Deterministic `package/module` label over the core (fallback: entries).
    pub label: String,
    /// When this capability was split out of an oversized cluster (Stage 4b),
    /// the parent cluster's label; `None` for top-level capabilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub n_entries: usize,
    pub n_core: usize,
    /// Entry points (full qualified ids), sorted.
    pub entries: Vec<String>,
    /// The capability's own logic — functions reached (near-)exclusively by this
    /// cluster. Full qualified ids; this is the narration spine for Phase 4.
    pub core: Vec<String>,
    /// Shared services this capability leans on (high global reach_count).
    pub deps: Vec<String>,
    /// A few entry-point names, for labels/skimming.
    pub reps: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilityMap {
    pub capabilities: Vec<Capability>,
    pub stats: MapStats,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MapStats {
    pub substrate: SubstrateStats,
    pub roots: usize,
    pub roots_by_kind: HashMap<String, usize>,
    pub capabilities: usize,
    pub multi_entry: usize,
}

#[derive(Debug, Clone, Copy)]
pub enum ProviderKind {
    Heuristic,
    Fallback,
}

pub struct MapOptions {
    pub jaccard: f64,
    pub provider: ProviderKind,
    /// A node reached by ≥ this many entry points is treated as shared substrate
    /// when listing a capability's deps.
    pub shared_threshold: usize,
    /// Fraction of a node's reachers that must be inside the cluster for it to
    /// count as the cluster's own core.
    pub core_ownership: f64,
    /// A cluster whose core exceeds this is an oversized "megacluster" and is
    /// recursively decomposed into sub-capabilities (Stage 4b). Keys on core
    /// size, not entry count — a many-entry cluster with a small shared spine is
    /// coherent (e.g. the 9-verb watched-folder capability), not a megacluster.
    pub decompose_core_threshold: usize,
    /// Within a decomposition, a node reached by ≥ this fraction of the group's
    /// members is the group's shared sub-spine, subtracted before re-clustering.
    pub subcap_shared_tau: f64,
    /// Distinctive-reach Jaccard for grouping members into sub-capabilities.
    pub subcap_jaccard: f64,
    /// Max decomposition recursion depth.
    pub subcap_max_depth: usize,
}

impl Default for MapOptions {
    fn default() -> Self {
        Self {
            jaccard: 0.5,
            provider: ProviderKind::Heuristic,
            shared_threshold: 17,
            core_ownership: 0.6,
            decompose_core_threshold: 150,
            subcap_shared_tau: 0.5,
            subcap_jaccard: 0.3,
            subcap_max_depth: 4,
        }
    }
}

/// Build the capability map from a corpus's SCIP symbols + refs.
pub fn build(
    symbols: &[ScipSymbolRecord],
    refs: &[ScipRefRecord],
    opts: &MapOptions,
) -> CapabilityMap {
    let graph = CallGraph::from_scip(symbols, refs);
    let roots = match opts.provider {
        // Heuristic finds framework entry points (cli/tool/http keywords). A plain
        // library crate has none → fall through to the universal in-degree-0
        // detector so ANY repo yields a map ("just works" on an arbitrary codebase,
        // not only framework-shaped ones). A non-empty heuristic result is kept
        // as-is, so CLI/HTTP apps like this one are unchanged.
        ProviderKind::Heuristic => {
            let h = HeuristicProvider.roots(&graph);
            if h.is_empty() {
                FallbackProvider::default().roots(&graph)
            } else {
                h
            }
        }
        ProviderKind::Fallback => FallbackProvider::default().roots(&graph),
    };

    // Per-root reach + global reach_count.
    let reach: HashMap<String, HashSet<String>> = roots
        .iter()
        .map(|e| (e.clone(), graph.reachable(e)))
        .collect();
    let mut rc: HashMap<String, usize> = HashMap::new();
    for set in reach.values() {
        for n in set {
            *rc.entry(n.clone()).or_insert(0) += 1;
        }
    }

    let clusters = cluster_entries(&roots, &reach, opts.jaccard);

    let mut capabilities: Vec<Capability> = Vec::new();
    for members in &clusters {
        let cap = describe(members, &reach, &rc, opts);
        // Stage 4b: an oversized cluster (huge shared core, e.g. the conversational
        // runtime) is recursively decomposed into doc-sized sub-capabilities.
        if members.len() > 1 && cap.n_core > opts.decompose_core_threshold {
            let parent = cap.label.clone();
            for sub in decompose(members.clone(), &reach, opts, 0) {
                capabilities.push(subcap_capability(sub, &parent, &reach, &rc, opts));
            }
        } else {
            capabilities.push(cap);
        }
    }
    capabilities.sort_by(|a, b| (b.n_entries, b.n_core).cmp(&(a.n_entries, a.n_core)));

    let mut roots_by_kind: HashMap<String, usize> = HashMap::new();
    for r in &roots {
        if let Some((_, d)) = pkg_and_desc(r) {
            if let Some(k) = HeuristicProvider::classify(d) {
                *roots_by_kind.entry(k.to_string()).or_insert(0) += 1;
            }
        }
    }
    let multi_entry = capabilities.iter().filter(|c| c.n_entries > 1).count();
    let stats = MapStats {
        substrate: graph.stats.clone(),
        roots: roots.len(),
        roots_by_kind,
        capabilities: capabilities.len(),
        multi_entry,
    };
    CapabilityMap {
        capabilities,
        stats,
    }
}

/// Union-find on reach-set Jaccard. Entry points whose spines overlap by ≥
/// `threshold` merge into one capability. Roots reaching nothing are their own
/// (leaf) capability.
fn cluster_entries(
    roots: &[String],
    reach: &HashMap<String, HashSet<String>>,
    threshold: f64,
) -> Vec<Vec<String>> {
    let ents: Vec<&String> = roots
        .iter()
        .filter(|e| reach.get(*e).map(|s| !s.is_empty()).unwrap_or(false))
        .collect();
    let mut parent: Vec<usize> = (0..ents.len()).collect();
    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }
    for i in 0..ents.len() {
        let a = &reach[ents[i]];
        for j in (i + 1)..ents.len() {
            let b = &reach[ents[j]];
            let inter = a.intersection(b).count();
            if inter == 0 {
                continue;
            }
            let uni = a.len() + b.len() - inter;
            if inter as f64 / uni as f64 >= threshold {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                parent[ri] = rj;
            }
        }
    }
    let mut groups: HashMap<usize, Vec<String>> = HashMap::new();
    for i in 0..ents.len() {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(ents[i].clone());
    }
    let mut out: Vec<Vec<String>> = groups.into_values().collect();
    // Empty-reach roots: each a standalone (thin) capability.
    for e in roots {
        if reach.get(e).map(|s| s.is_empty()).unwrap_or(true) {
            out.push(vec![e.clone()]);
        }
    }
    out
}

/// Cluster-relative core/deps + a deterministic label.
fn describe(
    members: &[String],
    reach: &HashMap<String, HashSet<String>>,
    rc: &HashMap<String, usize>,
    opts: &MapOptions,
) -> Capability {
    let mut spine: HashSet<String> = HashSet::new();
    for m in members {
        if let Some(r) = reach.get(m) {
            spine.extend(r.iter().cloned());
        }
        spine.insert(m.clone());
    }
    let in_cluster = |n: &str| -> usize {
        members
            .iter()
            .filter(|m| m.as_str() == n || reach.get(*m).map(|r| r.contains(n)).unwrap_or(false))
            .count()
    };

    let mut core: Vec<String> = Vec::new();
    let mut deps: Vec<(usize, String)> = Vec::new();
    for n in &spine {
        let is_fn = pkg_and_desc(n)
            .map(|(_, d)| is_function(d))
            .unwrap_or(false);
        if !is_fn {
            continue;
        }
        let total = (*rc.get(n).unwrap_or(&0)).max(1);
        if in_cluster(n) as f64 / total as f64 >= opts.core_ownership {
            core.push(n.clone());
        } else if *rc.get(n).unwrap_or(&0) >= opts.shared_threshold {
            deps.push((*rc.get(n).unwrap_or(&0), n.clone()));
        }
    }
    core.sort();
    deps.sort_by(|a, b| b.0.cmp(&a.0));

    // Label: dominant (package, module) over the core; fall back to the entries
    // (a thin handler with no owned core still gets its own module label).
    let label = dominant_label(&core)
        .or_else(|| dominant_label(members))
        .unwrap_or_else(|| "?/?".to_string());

    let mut reps: Vec<String> = members
        .iter()
        .filter_map(|m| pkg_and_desc(m).map(|(_, d)| method_name(d).to_string()))
        .collect();
    reps.sort();
    reps.dedup();
    reps.truncate(6);

    let mut entries: Vec<String> = members.to_vec();
    entries.sort();

    Capability {
        label,
        parent: None,
        n_entries: entries.len(),
        n_core: core.len(),
        entries,
        core,
        deps: deps.into_iter().map(|(_, n)| n).collect(),
        reps,
    }
}

/// Render a capability map as a scannable markdown inventory: multi-entry
/// capabilities with their core + shared deps, then a standalone-by-module
/// histogram. Shared by the `code capability-map` CLI verb and the MCP
/// `capability_map` tool so the two surfaces never drift.
pub fn render_markdown(corpus_id: &str, map: &CapabilityMap) -> String {
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    let s = &map.stats;
    let mut out = String::new();
    let _ = writeln!(out, "# Capability map — {corpus_id}\n");
    let _ = writeln!(
        out,
        "_Derived from the SCIP call graph. {} capabilities from {} entry points \
         ({} multi-entry)._\n",
        s.capabilities, s.roots, s.multi_entry
    );
    let _ = writeln!(
        out,
        "Substrate: {} first-party call edges ({} external, {} type/module, {} test dropped) \
         over {} nodes.\n",
        s.substrate.kept_edges,
        s.substrate.dropped_external,
        s.substrate.dropped_nonfunction,
        s.substrate.dropped_test,
        s.substrate.nodes
    );

    let _ = writeln!(out, "## Multi-entry capabilities\n");
    for c in map.capabilities.iter().filter(|c| c.n_entries > 1) {
        match &c.parent {
            Some(p) => {
                let _ = writeln!(
                    out,
                    "### {} — {} entries, {} core fns  ·  part of {}",
                    c.label, c.n_entries, c.n_core, p
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "### {} — {} entries, {} core fns",
                    c.label, c.n_entries, c.n_core
                );
            }
        }
        write_capability_detail(&mut out, c);
    }

    // Single-entry capabilities split out of a megacluster (e.g. `run_bench`):
    // meaningful, so surfaced individually rather than histogrammed.
    let subcaps: Vec<&Capability> = map
        .capabilities
        .iter()
        .filter(|c| c.n_entries == 1 && c.parent.is_some())
        .collect();
    if !subcaps.is_empty() {
        let _ = writeln!(out, "## Decomposed sub-capabilities (single entry)\n");
        for c in subcaps {
            let p = c.parent.as_deref().unwrap_or("");
            let _ = writeln!(
                out,
                "### {} — {} core fns  ·  part of {}",
                c.label, c.n_core, p
            );
            write_capability_detail(&mut out, c);
        }
    }

    let _ = writeln!(out, "## Standalone capabilities\n");
    let mut by_label: BTreeMap<String, usize> = BTreeMap::new();
    for c in map
        .capabilities
        .iter()
        .filter(|c| c.n_entries == 1 && c.parent.is_none())
    {
        *by_label.entry(c.label.clone()).or_insert(0) += 1;
    }
    let mut rows: Vec<(String, usize)> = by_label.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    for (label, n) in rows {
        let _ = writeln!(out, "- {n}  {label}");
    }
    out
}

/// Render one capability's verbs / core / shared-deps lines.
fn write_capability_detail(out: &mut String, c: &Capability) {
    use std::fmt::Write as _;
    let _ = writeln!(out, "- verbs: {}", c.reps.join(", "));
    if !c.core.is_empty() {
        let core: Vec<String> = c.core.iter().take(8).map(|x| short(x)).collect();
        let _ = writeln!(out, "- core: {}", core.join(", "));
    }
    if !c.deps.is_empty() {
        let deps: Vec<String> = c.deps.iter().take(5).map(|x| short(x)).collect();
        let _ = writeln!(out, "- uses: {}", deps.join(", "));
    }
    let _ = writeln!(out);
}

fn dominant_label(syms: &[String]) -> Option<String> {
    let mut tally: HashMap<(String, String), usize> = HashMap::new();
    for s in syms {
        if let Some((pkg, desc)) = pkg_and_desc(s) {
            *tally
                .entry((pkg.to_string(), module_seg(desc).to_string()))
                .or_insert(0) += 1;
        }
    }
    tally
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|((pkg, module), _)| format!("{pkg}/{module}"))
}

// ── Stage 4b: recursive decomposition of an oversized cluster ───────────────
// A megacluster (e.g. the conversational runtime: many handlers over one huge
// shared spine) is cracked into doc-sized sub-capabilities by subtracting the
// group's OWN shared sub-spine and re-clustering members on what's left. The
// "own" is load-bearing — a single global subtraction fails on nested sharing
// (heavy CLI drivers share an infra spine the whole cluster doesn't), so this
// recurses, recomputing the shared spine at each level.

fn is_function_node(n: &str) -> bool {
    pkg_and_desc(n)
        .map(|(_, d)| is_function(d))
        .unwrap_or(false)
}

struct SubCap {
    members: Vec<String>,
    core: Vec<String>,
}

/// Each member's DISTINCTIVE reach: its reachable functions minus the group's
/// shared sub-spine (nodes reached by ≥ `tau` of the members).
fn local_distinct(
    group: &[String],
    reach: &HashMap<String, HashSet<String>>,
    tau: f64,
) -> HashMap<String, HashSet<String>> {
    let mut count: HashMap<&str, usize> = HashMap::new();
    for e in group {
        if let Some(r) = reach.get(e) {
            for n in r {
                *count.entry(n.as_str()).or_insert(0) += 1;
            }
        }
    }
    let n = group.len() as f64;
    let shared = |node: &str| count.get(node).copied().unwrap_or(0) as f64 / n >= tau;
    let mut dist: HashMap<String, HashSet<String>> = HashMap::new();
    for e in group {
        let d = reach
            .get(e)
            .map(|r| {
                r.iter()
                    .filter(|node| !shared(node) && is_function_node(node))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        dist.insert(e.clone(), d);
    }
    dist
}

/// Union-find members on distinctive-reach Jaccard ≥ `theta`. Members whose whole
/// spine is shared (empty distinctive) collapse into one "routing" leaf.
fn subcluster(
    group: &[String],
    dist: &HashMap<String, HashSet<String>>,
    theta: f64,
) -> Vec<Vec<String>> {
    let nz: Vec<&String> = group.iter().filter(|e| !dist[*e].is_empty()).collect();
    let mut parent: Vec<usize> = (0..nz.len()).collect();
    fn find(p: &mut [usize], mut x: usize) -> usize {
        while p[x] != x {
            p[x] = p[p[x]];
            x = p[x];
        }
        x
    }
    for i in 0..nz.len() {
        let a = &dist[nz[i]];
        for j in (i + 1)..nz.len() {
            let b = &dist[nz[j]];
            let inter = a.intersection(b).count();
            if inter > 0 && inter as f64 / (a.len() + b.len() - inter) as f64 >= theta {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                parent[ri] = rj;
            }
        }
    }
    let mut groups: HashMap<usize, Vec<String>> = HashMap::new();
    for i in 0..nz.len() {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(nz[i].clone());
    }
    let mut out: Vec<Vec<String>> = groups.into_values().collect();
    let routing: Vec<String> = group
        .iter()
        .filter(|e| dist[*e].is_empty())
        .cloned()
        .collect();
    if !routing.is_empty() {
        out.push(routing);
    }
    out
}

/// Recursively split `group` until each sub-group's distinctive core is doc-sized
/// (≤ `decompose_core_threshold`), or it's irreducible / a single entry / too deep.
fn decompose(
    group: Vec<String>,
    reach: &HashMap<String, HashSet<String>>,
    opts: &MapOptions,
    depth: usize,
) -> Vec<SubCap> {
    let dist = local_distinct(&group, reach, opts.subcap_shared_tau);
    let core: HashSet<String> = dist.values().flatten().cloned().collect();
    if core.len() <= opts.decompose_core_threshold || depth >= opts.subcap_max_depth {
        let mut c: Vec<String> = core.into_iter().collect();
        c.sort();
        return vec![SubCap {
            members: group,
            core: c,
        }];
    }
    let subs = subcluster(&group, &dist, opts.subcap_jaccard);
    if subs.len() <= 1 {
        let mut c: Vec<String> = core.into_iter().collect();
        c.sort();
        return vec![SubCap {
            members: group,
            core: c,
        }];
    }
    let mut out = Vec::new();
    for s in subs {
        if s.len() > 1 {
            out.extend(decompose(s, reach, opts, depth + 1));
        } else {
            let mut c: Vec<String> = dist
                .get(&s[0])
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect();
            c.sort();
            out.push(SubCap {
                members: s,
                core: c,
            });
        }
    }
    out
}

/// Build a `Capability` from a decomposed sub-group: its distinctive `core`, the
/// shared services it still calls (`deps`), a label, and its `parent` cluster.
fn subcap_capability(
    sub: SubCap,
    parent: &str,
    reach: &HashMap<String, HashSet<String>>,
    rc: &HashMap<String, usize>,
    opts: &MapOptions,
) -> Capability {
    let mut spine: HashSet<String> = HashSet::new();
    for m in &sub.members {
        if let Some(r) = reach.get(m) {
            spine.extend(r.iter().cloned());
        }
    }
    let core_set: HashSet<&String> = sub.core.iter().collect();
    let mut deps: Vec<(usize, String)> = spine
        .iter()
        .filter(|n| is_function_node(n) && !core_set.contains(*n))
        .filter_map(|n| {
            let c = *rc.get(n).unwrap_or(&0);
            (c >= opts.shared_threshold).then(|| (c, n.clone()))
        })
        .collect();
    deps.sort_by(|a, b| b.0.cmp(&a.0));

    let label = dominant_label(&sub.core)
        .or_else(|| dominant_label(&sub.members))
        .unwrap_or_else(|| "?/?".to_string());
    let mut reps: Vec<String> = sub
        .members
        .iter()
        .filter_map(|m| pkg_and_desc(m).map(|(_, d)| method_name(d).to_string()))
        .collect();
    reps.sort();
    reps.dedup();
    reps.truncate(6);
    let mut entries = sub.members;
    entries.sort();

    Capability {
        label,
        parent: Some(parent.to_string()),
        n_entries: entries.len(),
        n_core: sub.core.len(),
        entries,
        core: sub.core,
        deps: deps.into_iter().map(|(_, n)| n).collect(),
        reps,
    }
}

// ============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn sym(qn: &str) -> ScipSymbolRecord {
        ScipSymbolRecord {
            name: qn.rsplit('/').next().unwrap_or(qn).to_string(),
            qualified_name: qn.to_string(),
            kind: "function".into(),
            file_path: "src/x.rs".into(),
            line_start: 1,
            line_end: 2,
            language: "rust".into(),
        }
    }
    fn edge(a: &str, b: &str) -> ScipRefRecord {
        ScipRefRecord {
            caller_symbol: String::new(),
            callee_symbol: String::new(),
            caller_qualified: a.to_string(),
            callee_qualified: b.to_string(),
            file_path: "src/x.rs".into(),
            line: 1,
            ref_kind: "direct".into(),
        }
    }

    /// The parser keys on SCIP-spec position, not Rust — five indexers parse.
    #[test]
    fn scip_parser_is_language_generic() {
        let cases = [
            (
                "rust-analyzer cargo sov 0.1 runtime/impl#[Runtime]handle_code_query().",
                "sov",
                true,
                "handle_code_query",
            ),
            (
                "scip-typescript npm @sg/scip 1.0 src/index/readFile().",
                "@sg/scip",
                true,
                "readFile",
            ),
            (
                "scip-python python requests 2.31 requests/sessions/Session#get().",
                "requests",
                true,
                "get",
            ),
            (
                "scip-java maven com.google.guava 31.0 com/google/common/Foo#bar().",
                "com.google.guava",
                true,
                "bar",
            ),
            (
                "scip-go gomod github.com/x/y v1.2.3 pkg/Server#Serve().",
                "github.com/x/y",
                true,
                "Serve",
            ),
            ("rust-analyzer cargo std 1.0 vec/Vec#", "std", false, "Vec"),
        ];
        for (raw, pkg, isfn, mname) in cases {
            let (p, d) = pkg_and_desc(raw).expect("global symbol parses");
            assert_eq!(p, pkg, "package for {raw}");
            assert_eq!(is_function(d), isfn, "is_function for {raw}");
            if isfn {
                assert_eq!(method_name(d), mname, "method for {raw}");
            }
        }
        assert!(
            pkg_and_desc("local 42").is_none(),
            "locals are not global symbols"
        );
    }

    #[test]
    fn external_and_type_edges_are_filtered() {
        let symbols = vec![sym("rust-analyzer cargo app 0.1 a/run().")];
        let refs = vec![
            // first-party call edge — kept
            edge(
                "rust-analyzer cargo app 0.1 a/run().",
                "rust-analyzer cargo app 0.1 a/help().",
            ),
            // external callee — dropped
            edge(
                "rust-analyzer cargo app 0.1 a/run().",
                "rust-analyzer cargo std 1.0 vec/Vec#push().",
            ),
            // type ref (not a function) — dropped
            edge(
                "rust-analyzer cargo app 0.1 a/run().",
                "rust-analyzer cargo app 0.1 a/Config#",
            ),
        ];
        let g = CallGraph::from_scip(&symbols, &refs);
        assert_eq!(g.stats.kept_edges, 1);
        assert_eq!(g.stats.dropped_external, 1);
        assert_eq!(g.stats.dropped_nonfunction, 1);
    }

    #[test]
    fn shared_spine_clusters_disjoint_does_not() {
        // Two CLI entries share a helper -> one capability. A third, disjoint,
        // stays separate.
        let p = "rust-analyzer cargo app 0.1 ";
        let mk = |s: &str| format!("{p}{s}");
        let symbols: Vec<ScipSymbolRecord> = [
            "x_cmd/run_a().",
            "x_cmd/run_b().",
            "y_cmd/run_c().",
            "x/shared().",
            "x/deep().",
            "y/lonely().",
        ]
        .iter()
        .map(|s| sym(&mk(s)))
        .collect();
        let refs = vec![
            edge(&mk("x_cmd/run_a()."), &mk("x/shared().")),
            edge(&mk("x_cmd/run_b()."), &mk("x/shared().")),
            edge(&mk("x/shared()."), &mk("x/deep().")),
            edge(&mk("y_cmd/run_c()."), &mk("y/lonely().")),
        ];
        let map = build(&symbols, &refs, &MapOptions::default());
        // run_a + run_b coalesce; run_c is its own capability.
        let multi = map
            .capabilities
            .iter()
            .find(|c| c.n_entries == 2)
            .expect("a 2-entry capability");
        assert!(multi.reps.iter().any(|r| r == "run_a"));
        assert!(multi.reps.iter().any(|r| r == "run_b"));
        assert!(map
            .capabilities
            .iter()
            .any(|c| c.n_entries == 1 && c.reps.iter().any(|r| r == "run_c")));
    }

    #[test]
    fn oversized_cluster_decomposes_into_subcaps() {
        // Three entries share a big spine (s1..s3) so they cluster together, but
        // each also owns a unique deep fn (da/db/dc). With a low decompose
        // threshold the cluster is oversized and must split into per-entry
        // sub-capabilities — each tagged with the parent, none lost.
        let p = "rust-analyzer cargo app 0.1 ";
        let mk = |s: &str| format!("{p}{s}");
        let symbols: Vec<ScipSymbolRecord> = [
            "x_cmd/run_a().",
            "x_cmd/run_b().",
            "x_cmd/run_c().",
            "s/s1().",
            "s/s2().",
            "s/s3().",
            "d/da().",
            "d/db().",
            "d/dc().",
        ]
        .iter()
        .map(|s| sym(&mk(s)))
        .collect();
        let mut refs = Vec::new();
        for v in ["run_a", "run_b", "run_c"] {
            for s in ["s/s1().", "s/s2().", "s/s3()."] {
                refs.push(edge(&mk(&format!("x_cmd/{v}().")), &mk(s)));
            }
        }
        refs.push(edge(&mk("x_cmd/run_a()."), &mk("d/da().")));
        refs.push(edge(&mk("x_cmd/run_b()."), &mk("d/db().")));
        refs.push(edge(&mk("x_cmd/run_c()."), &mk("d/dc().")));

        let opts = MapOptions {
            decompose_core_threshold: 2,
            ..MapOptions::default()
        };
        let map = build(&symbols, &refs, &opts);

        let subcaps: Vec<&Capability> = map
            .capabilities
            .iter()
            .filter(|c| c.parent.is_some())
            .collect();
        assert!(
            subcaps.len() >= 2,
            "cluster decomposed into sub-capabilities"
        );
        let total: usize = map.capabilities.iter().map(|c| c.n_entries).sum();
        assert_eq!(total, 3, "every entry survives decomposition");
        let first = subcaps[0].parent.clone();
        assert!(
            subcaps.iter().all(|c| c.parent == first),
            "sub-caps share one parent"
        );
    }
}
