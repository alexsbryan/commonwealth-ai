// SPDX-License-Identifier: AGPL-3.0-or-later
//! Data derivations for the fieldglass page — pure functions over inputs the
//! caller loads (SCIP records, git history, the arch report, the layer map).
//!
//! House rule applied throughout (ARCH §10.6): nothing here re-implements a
//! decider that exists elsewhere. Layer verdicts come from
//! `arch_layers::evaluate`; co-change pairs from
//! `git_archaeology::compute_co_evolution`; workspace facts from
//! `arch_report::declared_deps_from_cargo`. The ONE new decider in this file
//! is the trait-membership descriptor grammar (`trait_method`) — no other
//! code in the workspace derives "which trait does this method belong to",
//! so it is created here, tested against real descriptor samples, and NOT
//! keyed on the `kind`/`ref_kind` DB columns (both are junk: rust-analyzer
//! barely populates SCIP `Kind`, and the exporter hardcodes
//! `ref_kind='direct'` — verified live 2026-08-06).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use corpus_engine_archaeology::git_archaeology::{compute_co_evolution, CommitRecord};
use corpus_engine_scip::arch_metrics::{normalize_crate_name, ArchMetrics, DepDelta};
use corpus_engine_scip::capability_map::{pkg_and_desc, short};
use corpus_engine_scip::scip_graph::{ScipRefRecord, ScipSymbolRecord};
use sovereign_tools::code::arch_report::DeclaredInfo;

use super::layout::{seriate, UnionFind};
use super::model::AgentStat;

// ── Serialized panel models (everything lands in __DATA__) ───────────────────

/// One crate node in the layer-flow panel.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CrateNode {
    pub name: String,
    /// Index into `layers` (bottom→top); -1 when unassigned/unmapped.
    pub layer: i32,
    pub in_refs: usize,
    pub out_refs: usize,
    pub instability: Option<f64>,
    pub fan_in: usize,
    pub fan_out: usize,
}

/// One observed cross-crate edge in the layer-flow panel.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FlowEdge {
    pub from: String,
    pub to: String,
    pub refs: usize,
    /// Display-shortened carrier symbols, heaviest first.
    pub top: Vec<String>,
    /// "ok" | "upward" | "forbidden" | "hidden" (re-export-hidden coupling —
    /// an ObservedNotDeclared delta).
    pub kind: &'static str,
}

/// One trait usage matrix (the ISP panel). Rows = caller crates, cols =
/// methods, both seriated so block structure is contiguous.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TraitMatrix {
    pub name: String,
    pub pkg: String,
    pub file: String,
    pub methods: Vec<String>,
    pub callers: Vec<String>,
    pub cells: Vec<Vec<u32>>,
    /// Per-cell caller files `(path, count)`, desc by count, capped — the
    /// P4 drill-through: a matrix cell answers "WHICH files bind this
    /// caller crate to this method". Indexed [caller][method], same
    /// seriated order as `cells`.
    pub cell_files: Vec<Vec<Vec<(String, u32)>>>,
    /// References to the trait TYPE itself (`dyn Trait` / bounds) — the
    /// "depends on the abstraction, not its methods" axis.
    pub dyn_refs: u32,
    pub total_refs: u32,
}

/// Cap on caller files listed per matrix cell — a drill-through list, not
/// an exhaustive index; the cap is stated in the cell tooltip when hit.
const CELL_FILES_CAP: usize = 6;

// ── ISP: trait usage matrices ────────────────────────────────────────────────

/// Parse a SCIP descriptor into `(trait_path_with_hash, method_name)` iff it
/// names a method DECLARED ON a nominal type (`…/Name#method().`). Inherent
/// and trait-impl methods live under `impl#[…]…` and are rejected, so in
/// rust-analyzer's grammar the survivors are trait-declared methods — the
/// call sites that bind to the abstraction rather than a concrete impl.
pub fn trait_method(desc: &str) -> Option<(&str, &str)> {
    if !desc.ends_with("().") {
        return None;
    }
    let leaf_start = desc.rfind('/').map_or(0, |i| i + 1);
    let leaf = &desc[leaf_start..];
    if leaf.starts_with("impl") {
        return None;
    }
    let hash_in_leaf = leaf.find('#')?;
    let hash = leaf_start + hash_in_leaf;
    let method = &desc[hash + 1..desc.find('(').unwrap_or(desc.len())];
    // A method segment containing '/' or '#' means the '#' we found was not
    // the final type boundary (nested descriptor) — reject rather than guess.
    if method.is_empty() || method.contains('/') || method.contains('#') {
        return None;
    }
    Some((&desc[..=hash], method))
}

fn is_test_context(qualified: &str, file: &str) -> bool {
    file.contains("/tests/")
        || file.ends_with("_test.rs")
        || pkg_and_desc(qualified)
            .map(|(_, d)| d.contains("tests/") || d.starts_with("test"))
            .unwrap_or(false)
}

/// Build seriated caller-crate × method matrices for the `top_n` most
/// referenced first-party traits. `members` is the UNDERSCORED
/// (normalize_crate_name) workspace-member set — external traits (serde,
/// tokio…) are out of scope: their segregation is not ours to fix.
pub fn trait_matrices(
    symbols: &[ScipSymbolRecord],
    refs: &[ScipRefRecord],
    members: &BTreeSet<String>,
    top_n: usize,
) -> Vec<TraitMatrix> {
    // (pkg, trait_path) → method → caller crate → caller file → count
    type Key = (String, String);
    #[allow(clippy::type_complexity)]
    let mut usage: BTreeMap<Key, BTreeMap<String, BTreeMap<String, BTreeMap<String, u32>>>> =
        BTreeMap::new();
    let mut dyn_refs: BTreeMap<Key, u32> = BTreeMap::new();

    for r in refs {
        let Some((pkg, desc)) = pkg_and_desc(&r.callee_qualified) else {
            continue;
        };
        if !members.contains(&normalize_crate_name(pkg)) {
            continue;
        }
        if let Some((trait_path, method)) = trait_method(desc) {
            if is_test_context(&r.caller_qualified, &r.file_path) {
                continue;
            }
            let Some((caller_pkg, _)) = pkg_and_desc(&r.caller_qualified) else {
                continue;
            };
            let key = (pkg.to_string(), trait_path.to_string());
            *usage
                .entry(key)
                .or_default()
                .entry(method.to_string())
                .or_default()
                .entry(normalize_crate_name(caller_pkg))
                .or_default()
                .entry(r.file_path.replace('\\', "/"))
                .or_insert(0) += 1;
        } else if desc.ends_with('#')
            && !desc[desc.rfind('/').map_or(0, |i| i + 1)..].starts_with("impl")
        {
            let key = (pkg.to_string(), desc.to_string());
            *dyn_refs.entry(key).or_insert(0) += 1;
        }
    }

    // Trait definition file, for the panel's click-through.
    let mut def_file: HashMap<(String, String), String> = HashMap::new();
    for s in symbols {
        if let Some((pkg, desc)) = pkg_and_desc(&s.qualified_name) {
            if desc.ends_with('#') {
                def_file.insert((pkg.to_string(), desc.to_string()), s.file_path.clone());
            }
        }
    }

    let mut out: Vec<TraitMatrix> = usage
        .into_iter()
        .filter_map(|((pkg, trait_path), methods)| {
            let callers: BTreeSet<&String> = methods.values().flat_map(|m| m.keys()).collect();
            // A matrix is only diagnostic with enough surface: ≥3 methods in
            // live use and ≥2 consuming crates. Below that there is no block
            // structure to see.
            if methods.len() < 3 || callers.len() < 2 {
                return None;
            }
            let method_names: Vec<String> = methods.keys().cloned().collect();
            let caller_names: Vec<String> = callers.into_iter().cloned().collect();
            let mut cells = vec![vec![0u32; method_names.len()]; caller_names.len()];
            let mut files = vec![vec![Vec::new(); method_names.len()]; caller_names.len()];
            let mut total = 0u32;
            for (mi, m) in method_names.iter().enumerate() {
                for (ci, c) in caller_names.iter().enumerate() {
                    let Some(per_file) = methods[m].get(c) else {
                        continue;
                    };
                    let v: u32 = per_file.values().sum();
                    cells[ci][mi] = v;
                    total += v;
                    let mut fs: Vec<(String, u32)> =
                        per_file.iter().map(|(f, n)| (f.clone(), *n)).collect();
                    fs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                    fs.truncate(CELL_FILES_CAP);
                    files[ci][mi] = fs;
                }
            }
            let (row_order, col_order) = seriate(&cells);
            let name = trait_path
                .trim_end_matches('#')
                .rsplit('/')
                .next()
                .unwrap_or(&trait_path)
                .to_string();
            Some(TraitMatrix {
                file: def_file
                    .get(&(pkg.clone(), trait_path.clone()))
                    .cloned()
                    .unwrap_or_default(),
                dyn_refs: dyn_refs
                    .get(&(pkg.clone(), trait_path))
                    .copied()
                    .unwrap_or(0),
                pkg,
                methods: col_order.iter().map(|&i| method_names[i].clone()).collect(),
                callers: row_order.iter().map(|&i| caller_names[i].clone()).collect(),
                cells: row_order
                    .iter()
                    .map(|&r| col_order.iter().map(|&c| cells[r][c]).collect())
                    .collect(),
                cell_files: row_order
                    .iter()
                    .map(|&r| {
                        col_order
                            .iter()
                            .map(|&c| std::mem::take(&mut files[r][c]))
                            .collect()
                    })
                    .collect(),
                total_refs: total,
                name,
            })
        })
        .collect();
    out.sort_by(|a, b| b.total_refs.cmp(&a.total_refs).then(a.name.cmp(&b.name)));
    out.truncate(top_n);
    out
}

// ── SRP: co-change communities + bridge files ────────────────────────────────

/// Window-filter the git harvest to recent `.rs` history, then union
/// co-evolution pairs (the SHIPPED decider, `compute_co_evolution`) into
/// communities. Returns (file → community id, community count). Ids are
/// stable across runs: communities are ordered by their smallest member.
pub fn srp_communities(
    history: &HashMap<PathBuf, Vec<CommitRecord>>,
    now_unix: i64,
    window_days: i64,
    correlation: f32,
    min_joint: u32,
) -> (BTreeMap<String, i32>, usize) {
    let cutoff = now_unix - window_days * 86_400;
    let filtered: HashMap<PathBuf, Vec<CommitRecord>> = history
        .iter()
        .filter(|(path, _)| path.extension().and_then(|e| e.to_str()) == Some("rs"))
        .map(|(path, commits)| {
            (
                path.clone(),
                commits
                    .iter()
                    .filter(|c| c.timestamp >= cutoff)
                    .cloned()
                    .collect(),
            )
        })
        .filter(|(_, commits): &(_, Vec<CommitRecord>)| !commits.is_empty())
        .collect();

    let pairs = compute_co_evolution(&filtered, correlation, min_joint);
    let mut uf = UnionFind::default();
    for p in &pairs {
        uf.union(
            &p.file_a.to_string_lossy().replace('\\', "/"),
            &p.file_b.to_string_lossy().replace('\\', "/"),
        );
    }
    let communities = uf.communities();
    let mut map = BTreeMap::new();
    for (id, members) in communities.iter().enumerate() {
        for m in members {
            map.insert(m.clone(), id as i32);
        }
    }
    (map, communities.len())
}

/// "Does this file change for more than one reason?" — the bridge score.
/// For each file, look at the co-change communities of the files that
/// structurally REFERENCE it. A file whose callers are split across
/// communities (each holding ≥ the minority floor) is serving two masters:
/// score = 1 − (largest community's share). 0.0 = monochrome or too little
/// evidence (`min_incoming`).
pub fn bridge_scores(
    symbols: &[ScipSymbolRecord],
    refs: &[ScipRefRecord],
    file_community: &BTreeMap<String, i32>,
    min_incoming: usize,
) -> BTreeMap<String, f32> {
    let def_file: HashMap<&str, &str> = symbols
        .iter()
        .map(|s| (s.qualified_name.as_str(), s.file_path.as_str()))
        .collect();

    let mut incoming: BTreeMap<&str, BTreeMap<i32, usize>> = BTreeMap::new();
    for r in refs {
        let Some(&target) = def_file.get(r.callee_qualified.as_str()) else {
            continue;
        };
        if target == r.file_path || is_test_context(&r.caller_qualified, &r.file_path) {
            continue;
        }
        let Some(&community) = file_community.get(r.file_path.as_str()) else {
            continue;
        };
        *incoming
            .entry(target)
            .or_default()
            .entry(community)
            .or_insert(0) += 1;
    }

    incoming
        .into_iter()
        .filter_map(|(file, by_community)| {
            let total: usize = by_community.values().sum();
            if total < min_incoming || by_community.len() < 2 {
                return None;
            }
            let max = by_community.values().copied().max().unwrap_or(0);
            let score = 1.0 - (max as f32 / total as f32);
            (score > 0.0).then(|| (file.to_string(), score))
        })
        .collect()
}

// ── OCP: churn recurrence / tollbooths ───────────────────────────────────────

/// Per-file commit counts inside the window, plus the window's distinct
/// commit total — the denominator that turns a count into "this file rides
/// N% of all commits" (the tollbooth signal: growth that re-edits the same
/// switchboards instead of adding beside them). Iterates the harvest map
/// directly; `compute_co_evolution` is O(n²) and must not be used for this.
///
/// The bound is SECONDS, not days: `--window 36h` is not a whole number of
/// days and rounding it to one would quietly measure a different window than
/// the page's label claims. One harvest serves every window — the caller
/// harvests once and varies only this argument (BUILD ONCE, EXTRACT SUBSETS).
pub fn churn_counts(
    history: &HashMap<PathBuf, Vec<CommitRecord>>,
    now_unix: i64,
    window_secs: i64,
) -> (BTreeMap<String, u32>, u32) {
    let cutoff = now_unix - window_secs;
    let mut per_file: BTreeMap<String, u32> = BTreeMap::new();
    let mut commits: std::collections::BTreeSet<&str> = Default::default();
    for (path, recs) in history {
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let recent = recs.iter().filter(|c| c.timestamp >= cutoff);
        let mut n = 0u32;
        for c in recent {
            n += 1;
            commits.insert(c.hash.as_str());
        }
        if n > 0 {
            per_file.insert(path.to_string_lossy().replace('\\', "/"), n);
        }
    }
    (per_file, commits.len() as u32)
}

// ── Increment mode: one harvest, any window ──────────────────────────────────
//
// `--window <dur>` re-tints the page's ACTIVITY measurements without moving
// the map: structure (treemap sizes, layout, layer flow, ISP matrices, SRP
// communities, duplication arcs, ghost edges) stays full-history, because a
// 48h window cannot support jaccard-over-joint-commits statistics and a
// layout that reshuffles daily is a layout the eye cannot learn.
//
// Both windowed inputs are decomposed ONCE upstream — the git harvest is a
// single `git log`, the transcript scan a single `cache-audit --by-file` —
// and every window is EXTRACTED from that one pass. Nothing here re-reads a
// source. This is also the substrate a Replay scrubber would animate: the
// day slices ARE the frames.

/// Parse a `--window` duration: an integer followed by `h` (hours) or `d`
/// (days). Returns seconds. `None` for anything else — the caller refuses
/// rather than guessing a unit, since a silently-misread window would label
/// the page with a period it did not measure (§18.3).
pub(super) fn parse_window_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    let (digits, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: i64 = digits.parse().ok()?;
    if n <= 0 {
        return None;
    }
    match unit {
        "h" => n.checked_mul(3_600),
        "d" => n.checked_mul(86_400),
        _ => None,
    }
}

/// Extract the window's subset of agent activity from the ALREADY-SCANNED
/// per-day slices — no re-scan, no second parse of the transcripts.
///
/// `cutoff_day` is the first UTC day INSIDE the window; earlier slices
/// contribute nothing. Granularity is deliberately the whole day: the
/// transcript slices are bucketed per UTC day, so the effective window is
/// the whole days CONTAINING the requested span and can reach up to 24h
/// further back than the label. The honesty footer says so — it is not
/// corrected for here, because silently trimming to a partial day would
/// drop real events to make a label look precise.
///
/// `sessions` is a distinct count unioned over the window's slices, never a
/// sum: one session working a file on three days is one session, not three.
pub(super) fn agent_window(
    table: &BTreeMap<String, AgentStat>,
    cutoff_day: i64,
) -> BTreeMap<String, AgentStat> {
    let mut out = BTreeMap::new();
    for (path, stat) in table {
        let mut acc = AgentStat::default();
        let mut sessions: BTreeSet<u32> = BTreeSet::new();
        for d in stat.days.iter().filter(|d| d.day >= cutoff_day) {
            acc.reads += d.reads;
            acc.read_tokens += d.read_tokens;
            acc.edits += d.edits;
            sessions.extend(d.sessions.iter().copied());
        }
        acc.sessions = sessions.len() as u64;
        if acc.reads > 0 || acc.edits > 0 {
            acc.days = stat
                .days
                .iter()
                .filter(|d| d.day >= cutoff_day)
                .cloned()
                .collect();
            out.insert(path.clone(), acc);
        }
    }
    out
}

// ── DIP: layer flow ──────────────────────────────────────────────────────────

/// Assign each cargo crate name to a layer index via the SAME wildcard
/// matcher the gate uses. Ambiguous/unassigned → -1 (and `evaluate` reports
/// those as violations on its own authority — this helper only places nodes).
fn layer_of(map: &arch_layers::LayerMap, name: &str) -> i32 {
    let matches: Vec<usize> = map
        .layers
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            l.crates
                .iter()
                .any(|p| arch_layers::wildcard_match(p, name))
        })
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [one] => *one as i32,
        _ => -1,
    }
}

/// Build the layer-flow panel: crate nodes placed in declared bands, observed
/// edges classified by `arch_layers::evaluate` (upward / forbidden), plus
/// re-export-hidden edges from the declared↔observed deltas.
pub fn build_flow(
    metrics: &ArchMetrics,
    info: &DeclaredInfo,
    map: &arch_layers::LayerMap,
) -> (Vec<CrateNode>, Vec<FlowEdge>, Vec<String>) {
    let cargo_name = |scip: &str| -> Option<String> {
        info.scip_to_cargo.get(&normalize_crate_name(scip)).cloned()
    };

    let crates_set: BTreeSet<String> = info.member_dirs.keys().cloned().collect();
    let edges: Vec<arch_layers::DepEdge> = metrics
        .cross_edges
        .iter()
        .filter_map(|e| {
            Some(arch_layers::DepEdge {
                from: cargo_name(&e.from_crate)?,
                to: cargo_name(&e.to_crate)?,
                kind: arch_layers::DepKind::Normal,
                // Observed reference — see arch_report.rs on why never true.
                optional: false,
            })
        })
        .collect();
    let violations = arch_layers::evaluate(map, &crates_set, &edges);
    let mut upward: BTreeSet<(String, String)> = BTreeSet::new();
    let mut forbidden: BTreeSet<(String, String)> = BTreeSet::new();
    for v in &violations {
        match v {
            arch_layers::Violation::UpwardEdge { from, to, .. } => {
                upward.insert((from.clone(), to.clone()));
            }
            arch_layers::Violation::ForbiddenEdge { from, to, .. } => {
                forbidden.insert((from.clone(), to.clone()));
            }
            _ => {}
        }
    }

    let nodes: Vec<CrateNode> = metrics
        .crates
        .iter()
        .map(|c| {
            let display = cargo_name(&c.name).unwrap_or_else(|| c.name.clone());
            CrateNode {
                layer: layer_of(map, &display),
                name: display,
                in_refs: c.in_refs,
                out_refs: c.out_refs,
                instability: c.instability,
                fan_in: c.fan_in,
                fan_out: c.fan_out,
            }
        })
        .collect();

    let mut flow: Vec<FlowEdge> = metrics
        .cross_edges
        .iter()
        .map(|e| {
            let from = cargo_name(&e.from_crate).unwrap_or_else(|| e.from_crate.clone());
            let to = cargo_name(&e.to_crate).unwrap_or_else(|| e.to_crate.clone());
            let pair = (from.clone(), to.clone());
            FlowEdge {
                kind: if forbidden.contains(&pair) {
                    "forbidden"
                } else if upward.contains(&pair) {
                    "upward"
                } else {
                    "ok"
                },
                top: e
                    .top_symbols
                    .iter()
                    .take(5)
                    .map(|(sym, n)| format!("{} ×{n}", short(sym)))
                    .collect(),
                refs: e.ref_count,
                from,
                to,
            }
        })
        .collect();
    for d in &metrics.deltas {
        if let DepDelta::ObservedNotDeclared {
            from,
            to,
            ref_count,
        } = d
        {
            flow.push(FlowEdge {
                from: cargo_name(from).unwrap_or_else(|| from.clone()),
                to: cargo_name(to).unwrap_or_else(|| to.clone()),
                refs: *ref_count,
                top: Vec::new(),
                kind: "hidden",
            });
        }
    }

    let layer_names = map.layers.iter().map(|l| l.name.clone()).collect();
    (nodes, flow, layer_names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_fieldglass::model::AgentDay;

    // Real descriptor shapes, verified against the live scip_graph.db on
    // 2026-08-06 — including the trap cases the junk `kind` column would
    // have mislabeled (a trait-impl method carries kind='trait' in the DB).
    #[test]
    fn trait_method_grammar_accepts_declarations_and_rejects_impls() {
        assert_eq!(
            trait_method("traits/Tool#execute()."),
            Some(("traits/Tool#", "execute"))
        );
        assert_eq!(
            trait_method("runtime/retrieval/Retriever#fetch()."),
            Some(("runtime/retrieval/Retriever#", "fetch"))
        );
        // Trait-impl method — belongs to the impl, not the abstraction.
        assert_eq!(
            trait_method("daemon_cmd/impl#[SolveCancelTool][Tool]execute()."),
            None
        );
        // Inherent impl method.
        assert_eq!(trait_method("registry/impl#[Registry]new()."), None);
        // Free function — no type boundary.
        assert_eq!(trait_method("scip_graph/normalize()."), None);
        // Type reference, not a method.
        assert_eq!(trait_method("traits/Tool#"), None);
        // Field (term without parens).
        assert_eq!(trait_method("types/Intent#label."), None);
    }

    fn sym(qualified: &str, file: &str) -> ScipSymbolRecord {
        ScipSymbolRecord {
            name: String::new(),
            qualified_name: qualified.to_string(),
            kind: "unknown".to_string(), // deliberately junk, like the live DB
            file_path: file.to_string(),
            line_start: 1,
            line_end: 1,
            language: "rust".to_string(),
        }
    }

    fn call(caller_q: &str, callee_q: &str, file: &str) -> ScipRefRecord {
        ScipRefRecord {
            caller_symbol: String::new(),
            callee_symbol: String::new(),
            caller_qualified: caller_q.to_string(),
            callee_qualified: callee_q.to_string(),
            file_path: file.to_string(),
            line: 1,
            start_col: -1,
            end_line: -1,
            end_col: -1,
            ref_kind: "direct".to_string(), // hardcoded by the exporter
        }
    }

    #[test]
    fn trait_matrices_build_and_seriate_a_planted_stapled_trait() {
        let q = |pkg: &str, desc: &str| format!("rust-analyzer cargo {pkg} 0.1.0 {desc}");
        let members: BTreeSet<String> = ["core", "store_a", "store_b", "net_a", "net_b"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let symbols = vec![sym(&q("core", "io/Blob#"), "core/src/io.rs")];
        // Blob# staples storage methods and network methods: store crates
        // call {get,put,del}, net crates call {send,recv,ping}.
        let mut refs = Vec::new();
        for (crate_name, methods) in [
            ("store_a", ["get", "put", "del"]),
            ("store_b", ["get", "put", "del"]),
            ("net_a", ["send", "recv", "ping"]),
            ("net_b", ["send", "recv", "ping"]),
        ] {
            for m in methods {
                for _ in 0..3 {
                    refs.push(call(
                        &q(crate_name, "lib/run()."),
                        &q("core", &format!("io/Blob#{m}().")),
                        &format!("{crate_name}/src/lib.rs"),
                    ));
                }
            }
        }
        let out = trait_matrices(&symbols, &refs, &members, 8);
        assert_eq!(out.len(), 1);
        let m = &out[0];
        assert_eq!(m.name, "Blob");
        assert_eq!(m.methods.len(), 6);
        assert_eq!(m.callers.len(), 4);
        // The negative control: after seriation the zero-cells form two
        // contiguous blocks — each caller row is a solid prefix or suffix.
        for row in &m.cells {
            let nonzero: Vec<bool> = row.iter().map(|&v| v > 0).collect();
            assert_eq!(
                nonzero.windows(2).filter(|w| w[0] != w[1]).count(),
                1,
                "each row is one contiguous block after seriation: {row:?}"
            );
        }
    }

    #[test]
    fn bridge_scores_flag_a_two_master_file() {
        let q = |pkg: &str, desc: &str| format!("rust-analyzer cargo {pkg} 0.1.0 {desc}");
        let symbols = vec![sym(&q("core", "util/helper()."), "core/src/util.rs")];
        let mut refs = Vec::new();
        for f in ["a1.rs", "a2.rs", "b1.rs", "b2.rs"] {
            for _ in 0..3 {
                refs.push(call(&q("app", "x/f()."), &q("core", "util/helper()."), f));
            }
        }
        let mut communities = BTreeMap::new();
        communities.insert("a1.rs".to_string(), 0);
        communities.insert("a2.rs".to_string(), 0);
        communities.insert("b1.rs".to_string(), 1);
        communities.insert("b2.rs".to_string(), 1);
        let scores = bridge_scores(&symbols, &refs, &communities, 5);
        let s = scores.get("core/src/util.rs").copied().unwrap_or(0.0);
        assert!(
            (s - 0.5).abs() < 1e-6,
            "even two-community split scores 0.5, got {s}"
        );
    }

    #[test]
    fn srp_windowing_drops_old_commits() {
        let rec = |ts: i64, files: &[&str]| CommitRecord {
            hash: format!("h{ts}"),
            timestamp: ts,
            author_email: "t@t".into(),
            subject: "s".into(),
            file_paths: files.iter().map(PathBuf::from).collect(),
        };
        let now = 1_000_000_000i64;
        let mut history: HashMap<PathBuf, Vec<CommitRecord>> = HashMap::new();
        // Five recent joint commits between a.rs and b.rs; ancient ones for c.
        let joint: Vec<CommitRecord> = (0..5)
            .map(|i| rec(now - i * 86_400, &["a.rs", "b.rs"]))
            .collect();
        history.insert(PathBuf::from("a.rs"), joint.clone());
        history.insert(PathBuf::from("b.rs"), joint);
        history.insert(
            PathBuf::from("c.rs"),
            (0..5)
                .map(|i| rec(now - (600 + i) * 86_400, &["c.rs", "a.rs"]))
                .collect(),
        );
        let (map, n) = srp_communities(&history, now, 548, 0.5, 5);
        assert_eq!(n, 1, "one live community");
        assert_eq!(map.get("a.rs"), Some(&0));
        assert_eq!(map.get("b.rs"), Some(&0));
        assert_eq!(
            map.get("c.rs"),
            None,
            "stale co-change is outside the window"
        );
    }

    // ── Increment mode: the window boundary ──────────────────────────────
    //
    // The instrument for `--window` is "does a row one second outside the
    // bound stay out". These tests are the validation of that instrument
    // (ARCH §18.4): each places the SAME event just inside and just outside
    // the window and asserts both directions, so a bound that silently
    // stopped applying — the failure that would render 90d heat labelled
    // 48h — cannot pass.

    fn commit_at(ts: i64, files: &[&str]) -> CommitRecord {
        CommitRecord {
            hash: format!("h{ts}"),
            timestamp: ts,
            author_email: "t@t".into(),
            subject: "s".into(),
            file_paths: files.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn churn_window_excludes_the_commit_one_second_past_the_bound() {
        let now = 1_000_000_000i64;
        let window = 48 * 3_600; // 48h
        let inside = now - window + 1;
        let outside = now - window - 1;
        let mut history: HashMap<PathBuf, Vec<CommitRecord>> = HashMap::new();
        history.insert(
            PathBuf::from("a.rs"),
            vec![commit_at(inside, &["a.rs"]), commit_at(outside, &["a.rs"])],
        );

        let (per_file, commits) = churn_counts(&history, now, window);
        assert_eq!(
            per_file.get("a.rs"),
            Some(&1),
            "only the in-window commit counts"
        );
        assert_eq!(commits, 1, "the denominator counts the same window");

        // The SAME commit, with the bound widened by two seconds, must count:
        // proves the exclusion above is the window and not some other filter.
        let (per_file, commits) = churn_counts(&history, now, window + 2);
        assert_eq!(per_file.get("a.rs"), Some(&2));
        assert_eq!(commits, 2);
    }

    #[test]
    fn churn_window_is_seconds_so_sub_day_windows_are_exact() {
        // 36h is not a whole number of days. A day-rounded bound would pull
        // in the 40h-old commit and mislabel the page.
        let now = 1_000_000_000i64;
        let mut history: HashMap<PathBuf, Vec<CommitRecord>> = HashMap::new();
        history.insert(
            PathBuf::from("a.rs"),
            vec![
                commit_at(now - 30 * 3_600, &["a.rs"]),
                commit_at(now - 40 * 3_600, &["a.rs"]),
            ],
        );
        let (per_file, _) = churn_counts(&history, now, 36 * 3_600);
        assert_eq!(
            per_file.get("a.rs"),
            Some(&1),
            "36h means 36h, not 1 day and not 2"
        );
    }

    #[test]
    fn churn_window_is_deterministic_for_the_same_harvest() {
        let now = 1_000_000_000i64;
        let mut history: HashMap<PathBuf, Vec<CommitRecord>> = HashMap::new();
        for (i, f) in ["a.rs", "b.rs", "c.rs"].iter().enumerate() {
            history.insert(
                PathBuf::from(*f),
                (0..4)
                    .map(|j| commit_at(now - (i as i64 * 3 + j) * 3_600, &[f]))
                    .collect(),
            );
        }
        let a = churn_counts(&history, now, 12 * 3_600);
        let b = churn_counts(&history, now, 12 * 3_600);
        assert_eq!(a, b, "same harvest + same window = identical output");
    }

    fn day(d: i64, reads: u64, tokens: u64, edits: u64, sessions: &[u32]) -> AgentDay {
        AgentDay {
            day: d,
            reads,
            read_tokens: tokens,
            edits,
            sessions: sessions.to_vec(),
        }
    }

    #[test]
    fn agent_window_excludes_the_bucket_one_day_past_the_bound() {
        // Same file, two day slices: 20672 (outside) and 20673 (inside).
        let mut table = BTreeMap::new();
        table.insert(
            "a.rs".to_string(),
            AgentStat {
                reads: 12,
                read_tokens: 9_000,
                edits: 3,
                sessions: 2,
                days: vec![
                    day(20672, 10, 8_000, 2, &[0]),
                    day(20673, 2, 1_000, 1, &[1]),
                ],
            },
        );

        let got = agent_window(&table, 20673);
        let a = got.get("a.rs").expect("in-window activity survives");
        assert_eq!(
            (a.reads, a.read_tokens, a.edits),
            (2, 1_000, 1),
            "only the day at/after the cutoff contributes"
        );
        assert_eq!(a.days.len(), 1, "the out-of-window slice is dropped too");

        // The SAME table one day earlier must include BOTH — otherwise the
        // assertion above would also pass for a function that returns nothing.
        let got = agent_window(&table, 20672);
        let a = got.get("a.rs").expect("both days in window");
        assert_eq!((a.reads, a.read_tokens, a.edits), (12, 9_000, 3));
        assert_eq!(
            (a.reads, a.read_tokens, a.edits),
            (
                table["a.rs"].reads,
                table["a.rs"].read_tokens,
                table["a.rs"].edits
            ),
            "a window covering every slice reproduces the totals exactly"
        );
    }

    #[test]
    fn agent_window_unions_sessions_rather_than_summing_them() {
        // One session (index 0) works the file on three days. That is ONE
        // session, not three — a sum would triple-count it.
        let mut table = BTreeMap::new();
        table.insert(
            "a.rs".to_string(),
            AgentStat {
                reads: 3,
                read_tokens: 300,
                edits: 0,
                sessions: 1,
                days: vec![
                    day(20671, 1, 100, 0, &[0]),
                    day(20672, 1, 100, 0, &[0]),
                    day(20673, 1, 100, 0, &[0, 4]),
                ],
            },
        );
        let got = agent_window(&table, 20671);
        assert_eq!(
            got["a.rs"].sessions, 2,
            "distinct sessions across the window, not a per-day sum"
        );
    }

    #[test]
    fn agent_window_drops_files_with_no_activity_inside_it() {
        let mut table = BTreeMap::new();
        table.insert(
            "cold.rs".to_string(),
            AgentStat {
                reads: 5,
                read_tokens: 500,
                edits: 1,
                sessions: 1,
                days: vec![day(20600, 5, 500, 1, &[0])],
            },
        );
        assert!(
            agent_window(&table, 20673).is_empty(),
            "a file untouched in the window is absent, not present with zeros"
        );
    }

    #[test]
    fn window_durations_parse_hours_and_days_and_refuse_the_rest() {
        assert_eq!(parse_window_secs("48h"), Some(172_800));
        assert_eq!(parse_window_secs("7d"), Some(604_800));
        assert_eq!(parse_window_secs(" 36h "), Some(129_600));
        // Refusals: a silently-misread unit would label the page with a
        // period it did not measure.
        for bad in ["48", "48hours", "h", "", "0d", "-3d", "1.5d", "48w", "d7"] {
            assert_eq!(parse_window_secs(bad), None, "{bad:?} must be refused");
        }
    }
}
