// SPDX-License-Identifier: AGPL-3.0-or-later
//! `arch_report` — the architectural posture of a codebase, derived from the
//! SCIP graph + Cargo metadata + (optionally) git history.
//!
//! The OBSERVED half of the quality program's dependency-direction story: the
//! xtask `layer-gate` enforces Cargo-DECLARED edges in CI; this tool reports
//! what the code actually references — god-crate fan-in, the symbols carrying
//! each cross-crate coupling edge, declared↔observed deltas, SCIP-observed
//! layer-map violations (through re-exports Cargo can't see), file fan-in
//! hotspots, intra-crate file cycles, file-size offenders, feature-axis
//! spread, and (with the `dev-tools` feature) temporal coupling: file pairs
//! that co-change in git without any structural edge — boundaries maintained
//! by hand.
//!
//! Deterministic, no model. The pure math lives in
//! `corpus_engine_scip::arch_metrics`; the layer-map semantics in the shared
//! `arch-layers` crate (same parser the xtask gate uses — the two halves
//! cannot drift). This tool resolves the corpus, assembles the sections, and
//! renders.
//!
//! Persistence: `sovereign code arch-report` (the CLI verb) writes
//! `~/.sovereign/arch/<corpus>/arch_report.{md,json}` + a fingerprint so the
//! cheap `arch_posture` reader can answer "what's our posture, is it stale?"
//! without recomputing. The MCP tool computes on demand and does not write
//! (its Effect::Read stays honest).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use sha2::{Digest, Sha256};

use sovereign_core::error::{Error, Result};
use sovereign_core::traits::Tool;
use sovereign_core::types::*;

use corpus_engine_scip::arch_metrics::{self, normalize_crate_name, ArchOptions, DeclaredDeps};
use corpus_engine_scip::ScipGraph;

/// Feature axes whose `cfg(feature = "…")` spread is worth watching — the
/// flags that reshape the dependency graph (see CLAUDE.md's build-thrash
/// notes).
const FEATURE_AXES: &[&str] = &["treesitter", "atos", "dev-tools"];

/// ARCH §3.1 file-size ceiling (mirrors the xtask arch-gate constant).
const FILE_SIZE_LIMIT: usize = 1200;

/// Co-change window: pre-carve-out history (the 2026-05 decompositions moved
/// whole modules without rename-following) poisons older pairs.
const TEMPORAL_WINDOW_DAYS: i64 = 548; // ~18 months

// ── Report data (persisted as arch_report.json) ──────────────────────────────

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TemporalPair {
    pub file_a: String,
    pub file_b: String,
    pub joint_commits: u32,
    pub correlation: f32,
    pub structural_edge: bool,
    pub cross_crate: bool,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TemporalSection {
    /// High co-change, NO structural edge — the boundary is fiction
    /// maintained by hand. Ranked by correlation × ln(joint).
    pub hidden_coupling: Vec<TemporalPair>,
    /// Co-change crossing crate boundaries — the crate boundary doesn't
    /// isolate change (the carve-out-quality metric).
    pub crate_boundary_fiction: Vec<TemporalPair>,
    /// Co-changing pairs WITH a structural edge in the same crate (healthy).
    pub healthy_pairs: usize,
    pub window_days: i64,
}

/// Everything the report knows, JSON-serializable for `arch_posture` + the
/// drift-report sidecar pattern.
#[derive(Debug, serde::Serialize)]
pub struct ArchReportData {
    pub corpus_id: String,
    pub metrics: arch_metrics::ArchMetrics,
    /// SCIP-observed layer-map violations (described), when
    /// quality/ARCH_LAYERS.toml was found.
    pub layer_violations: Option<Vec<String>>,
    /// (repo-relative path, line count) over the §3.1 ceiling.
    pub file_offenders: Option<Vec<(String, usize)>>,
    /// feature axis → cfg-site count.
    pub feature_axes: Option<BTreeMap<String, usize>>,
    pub temporal: Option<TemporalSection>,
    /// Input fingerprint — posture staleness key.
    pub fingerprint: String,
}

// ── Shared builder (CLI verb + MCP tool call the same code) ───────────────────

pub struct ArchReportInputs<'a> {
    pub db_path: &'a Path,
    pub corpus_id: &'a str,
    /// Workspace root. Without it the report is SCIP-only (no declared
    /// deps, no layer check, no filesystem or git sections).
    pub project_root: Option<&'a Path>,
    /// Compute the temporal-coupling section (needs `dev-tools` + git).
    pub include_git: bool,
}

pub async fn build_arch_report(inputs: ArchReportInputs<'_>) -> Result<ArchReportData> {
    let tool_err = |stage: &str, msg: String| Error::Tool {
        tool_id: "arch_report".to_string(),
        message: format!("{stage}: {msg}"),
    };

    let graph = ScipGraph::open(inputs.db_path, inputs.corpus_id)
        .map_err(|e| tool_err("open SCIP graph", e.to_string()))?;
    let symbols = graph
        .iter_all_symbols()
        .await
        .map_err(|e| tool_err("read symbols", e.to_string()))?;
    let refs = graph
        .iter_all_refs()
        .await
        .map_err(|e| tool_err("read refs", e.to_string()))?;

    // Declared graph + member map from cargo metadata (when rooted).
    let declared_info = inputs.project_root.and_then(declared_deps_from_cargo);
    let declared = declared_info.as_ref().map(|d| &d.deps);

    let metrics = arch_metrics::compute(&symbols, &refs, declared, &ArchOptions::default());

    // SCIP-observed layer check — same parser/evaluator as xtask layer-gate.
    let layer_violations = match (inputs.project_root, declared_info.as_ref()) {
        (Some(root), Some(info)) => {
            observed_layer_violations(root, &metrics, info).map_err(|e| tool_err("layer map", e))?
        }
        _ => None,
    };

    // Filesystem sections.
    let (file_offenders, feature_axes) = match inputs.project_root {
        Some(root) => {
            let (offenders, axes) = walk_filesystem_sections(root);
            (Some(offenders), Some(axes))
        }
        None => (None, None),
    };

    // Temporal coupling (feature-gated: archaeology is a dev-tools dep).
    let temporal = if inputs.include_git {
        match (inputs.project_root, declared_info.as_ref()) {
            (Some(root), Some(info)) => temporal_section(root, &symbols, &refs, &info.member_dirs),
            _ => None,
        }
    } else {
        None
    };

    let fingerprint = compute_fingerprint(inputs.db_path, inputs.project_root);

    Ok(ArchReportData {
        corpus_id: inputs.corpus_id.to_string(),
        metrics,
        layer_violations,
        file_offenders,
        feature_axes,
        temporal,
        fingerprint,
    })
}

/// One rendering for every surface.
pub fn render_report(data: &ArchReportData) -> String {
    use std::fmt::Write as _;
    let mut out = arch_metrics::render_markdown(&data.corpus_id, &data.metrics);

    match &data.layer_violations {
        Some(v) if v.is_empty() => {
            let _ = writeln!(
                out,
                "\n## Layer map (observed)\n\nNo SCIP-observed violations of \
                 quality/ARCH_LAYERS.toml — the re-export paths agree with the declared layers."
            );
        }
        Some(v) => {
            let _ = writeln!(out, "\n## Layer map (observed) — VIOLATIONS\n");
            for line in v {
                let _ = writeln!(out, "- {line}");
            }
        }
        None => {
            let _ = writeln!(
                out,
                "\n## Layer map (observed)\n\n_Skipped — no project root / \
                 quality/ARCH_LAYERS.toml available to this surface._"
            );
        }
    }

    if let Some(offenders) = &data.file_offenders {
        let _ = writeln!(
            out,
            "\n## File-size offenders (> {FILE_SIZE_LIMIT} lines; ARCH §3.1)\n"
        );
        for (path, lines) in offenders.iter().take(15) {
            let _ = writeln!(out, "- {path} — {lines}");
        }
        let _ = writeln!(
            out,
            "\n_{} total (arch-gate ratchets these)._",
            offenders.len()
        );
    }

    if let Some(axes) = &data.feature_axes {
        let _ = writeln!(out, "\n## Feature-axis spread (cfg sites)\n");
        for (axis, n) in axes {
            let _ = writeln!(out, "- `{axis}`: {n}");
        }
    }

    if let Some(t) = &data.temporal {
        let _ = writeln!(
            out,
            "\n## Temporal coupling (git, last {} days)\n",
            t.window_days
        );
        if t.hidden_coupling.is_empty() {
            let _ = writeln!(
                out,
                "No hidden coupling (high co-change without a structural edge)."
            );
        } else {
            let _ = writeln!(
                out,
                "### Hidden coupling — co-change with NO structural edge\n"
            );
            for p in t.hidden_coupling.iter().take(15) {
                let _ = writeln!(
                    out,
                    "- {} ⇄ {} — {} joint commits, r={:.2}{}",
                    p.file_a,
                    p.file_b,
                    p.joint_commits,
                    p.correlation,
                    if p.cross_crate { " (CROSS-CRATE)" } else { "" }
                );
            }
        }
        if !t.crate_boundary_fiction.is_empty() {
            let _ = writeln!(
                out,
                "\n### Crate-boundary fiction — co-change across crate lines\n"
            );
            for p in t.crate_boundary_fiction.iter().take(15) {
                let _ = writeln!(
                    out,
                    "- {} ⇄ {} — {} joint commits, r={:.2}{}",
                    p.file_a,
                    p.file_b,
                    p.joint_commits,
                    p.correlation,
                    if p.structural_edge {
                        ""
                    } else {
                        " (no structural edge)"
                    }
                );
            }
        }
        let _ = writeln!(
            out,
            "\n_{} co-changing pairs are structurally-linked same-crate (healthy)._",
            t.healthy_pairs
        );
    }
    out
}

/// Persist under `~/.sovereign/arch/<corpus>/` (the capability_findings
/// pattern). Returns the directory written.
pub fn persist_arch_report(data: &ArchReportData, markdown: &str) -> Result<PathBuf> {
    let dir = sovereign_contracts::rebrand::data_dir()
        .join("arch")
        .join(&data.corpus_id);
    let io_err = |e: std::io::Error| Error::Tool {
        tool_id: "arch_report".into(),
        message: format!("persist: {e}"),
    };
    std::fs::create_dir_all(&dir).map_err(io_err)?;
    std::fs::write(dir.join("arch_report.md"), markdown).map_err(io_err)?;
    let json = serde_json::to_string_pretty(data).map_err(|e| Error::Tool {
        tool_id: "arch_report".into(),
        message: format!("serialize: {e}"),
    })?;
    std::fs::write(dir.join("arch_report.json"), json).map_err(io_err)?;
    std::fs::write(dir.join("arch_report.fingerprint"), &data.fingerprint).map_err(io_err)?;
    Ok(dir)
}

/// Staleness key: the declared graph (Cargo.toml + Cargo.lock), the policy
/// (ARCH_LAYERS.toml), and the SCIP db identity (path + size + mtime — the
/// db is rewritten on re-export, so size+mtime is a faithful proxy).
pub fn compute_fingerprint(db_path: &Path, project_root: Option<&Path>) -> String {
    let mut h = Sha256::new();
    if let Some(root) = project_root {
        for rel in ["Cargo.toml", "Cargo.lock", "quality/ARCH_LAYERS.toml"] {
            if let Ok(bytes) = std::fs::read(root.join(rel)) {
                h.update(rel.as_bytes());
                h.update(&bytes);
            }
        }
    }
    h.update(db_path.to_string_lossy().as_bytes());
    if let Ok(meta) = std::fs::metadata(db_path) {
        h.update(meta.len().to_le_bytes());
        if let Ok(mtime) = meta.modified() {
            if let Ok(d) = mtime.duration_since(std::time::UNIX_EPOCH) {
                h.update(d.as_secs().to_le_bytes());
            }
        }
    }
    format!("{:x}", h.finalize())
}

// ── cargo metadata → DeclaredDeps ─────────────────────────────────────────────

/// Workspace facts derived from one `cargo metadata` run. Public so
/// downstream renderers (`svrn code fieldglass`) reuse THIS derivation
/// instead of re-parsing cargo metadata — one decider, one name (ARCH §10.6).
pub struct DeclaredInfo {
    /// Declared member→member dependency edges (dev-deps excluded).
    pub deps: DeclaredDeps,
    /// underscored SCIP name → hyphenated cargo name.
    pub scip_to_cargo: BTreeMap<String, String>,
    /// hyphenated cargo name → repo-relative crate dir.
    pub member_dirs: BTreeMap<String, String>,
}

pub fn declared_deps_from_cargo(root: &Path) -> Option<DeclaredInfo> {
    let out = std::process::Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let packages = v.get("packages")?.as_array()?;
    let member_names: BTreeSet<String> = packages
        .iter()
        .filter_map(|p| p.get("name")?.as_str().map(String::from))
        .collect();

    let mut deps = DeclaredDeps::default();
    let mut scip_to_cargo = BTreeMap::new();
    let mut member_dirs = BTreeMap::new();
    for p in packages {
        let name = p.get("name").and_then(|n| n.as_str())?.to_string();
        scip_to_cargo.insert(normalize_crate_name(&name), name.clone());
        if let Some(manifest) = p.get("manifest_path").and_then(|m| m.as_str()) {
            let dir = Path::new(manifest).parent().unwrap_or(Path::new(""));
            if let Ok(rel) = dir.strip_prefix(root) {
                member_dirs.insert(name.clone(), rel.to_string_lossy().replace('\\', "/"));
            }
        }
        let entry = deps.edges.entry(normalize_crate_name(&name)).or_default();
        if let Some(dep_list) = p.get("dependencies").and_then(|d| d.as_array()) {
            for d in dep_list {
                let dep_name = d.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let kind = d.get("kind").and_then(|k| k.as_str()); // None = normal
                if member_names.contains(dep_name) && kind != Some("dev") {
                    entry.insert(normalize_crate_name(dep_name));
                }
            }
        }
    }
    Some(DeclaredInfo {
        deps,
        scip_to_cargo,
        member_dirs,
    })
}

// ── Observed layer check ──────────────────────────────────────────────────────

/// Evaluate the OBSERVED crate edges against quality/ARCH_LAYERS.toml.
/// `Ok(None)` when the policy file doesn't exist. StaleException findings
/// are filtered: whether an exception is still needed is the DECLARED
/// gate's judgment, not this lens's.
fn observed_layer_violations(
    root: &Path,
    metrics: &arch_metrics::ArchMetrics,
    info: &DeclaredInfo,
) -> std::result::Result<Option<Vec<String>>, String> {
    let path = root.join("quality/ARCH_LAYERS.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let map = arch_layers::parse(&text)?;
    let crates: BTreeSet<String> = info.member_dirs.keys().cloned().collect();
    let edges: Vec<arch_layers::DepEdge> = metrics
        .cross_edges
        .iter()
        .filter_map(|e| {
            // Observed names may be hyphenated or underscored (exporter
            // vintage); the map is keyed on the normalized form.
            let from = info
                .scip_to_cargo
                .get(&normalize_crate_name(&e.from_crate))?;
            let to = info.scip_to_cargo.get(&normalize_crate_name(&e.to_crate))?;
            Some(arch_layers::DepEdge {
                from: from.clone(),
                to: to.clone(),
                kind: arch_layers::DepKind::Normal,
            })
        })
        .collect();
    let violations: Vec<String> = arch_layers::evaluate(&map, &crates, &edges)
        .into_iter()
        .filter(|v| !matches!(v, arch_layers::Violation::StaleException { .. }))
        .map(|v| v.describe())
        .collect();
    Ok(Some(violations))
}

// ── Filesystem sections ───────────────────────────────────────────────────────

fn walk_filesystem_sections(root: &Path) -> (Vec<(String, usize)>, BTreeMap<String, usize>) {
    let mut offenders: Vec<(String, usize)> = Vec::new();
    let mut axes: BTreeMap<String, usize> =
        FEATURE_AXES.iter().map(|a| (a.to_string(), 0)).collect();
    walk(root, root, &mut offenders, &mut axes);
    offenders.sort_by(|a, b| b.1.cmp(&a.1));
    return (offenders, axes);

    fn walk(
        dir: &Path,
        root: &Path,
        offenders: &mut Vec<(String, usize)>,
        axes: &mut BTreeMap<String, usize>,
    ) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if matches!(
                    name.as_ref(),
                    "target" | "vendor" | ".git" | "node_modules" | ".sovereign" | "dist"
                ) {
                    continue;
                }
                walk(&path, root, offenders, axes);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let lines = text.lines().count();
                if lines > FILE_SIZE_LIMIT {
                    let rel = path
                        .strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    offenders.push((rel, lines));
                }
                for (axis, count) in axes.iter_mut() {
                    let needle = format!("feature = \"{axis}\"");
                    *count += text.matches(&needle).count();
                }
            }
        }
    }
}

// ── Temporal coupling (dev-tools only: archaeology is an optional dep) ────────

#[cfg(feature = "dev-tools")]
fn temporal_section(
    root: &Path,
    symbols: &[corpus_engine_scip::ScipSymbolRecord],
    refs: &[corpus_engine_scip::ScipRefRecord],
    member_dirs: &BTreeMap<String, String>,
) -> Option<TemporalSection> {
    use corpus_engine_archaeology::git_archaeology::{
        batch_harvest_all_commits, compute_co_evolution,
    };

    let history = batch_harvest_all_commits(root).ok()?;
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64
        - TEMPORAL_WINDOW_DAYS * 86_400;

    // Window-filter + .rs only: pre-carve-out moves poison older pairs.
    let filtered: std::collections::HashMap<_, _> = history
        .into_iter()
        .filter(|(path, _)| path.extension().and_then(|e| e.to_str()) == Some("rs"))
        .filter_map(|(path, commits)| {
            let recent: Vec<_> = commits
                .into_iter()
                .filter(|c| c.timestamp >= cutoff)
                .collect();
            (!recent.is_empty()).then_some((path, recent))
        })
        .collect();

    // Stricter than the drift-report defaults (0.5/5): this feeds findings.
    let pairs = compute_co_evolution(&filtered, 0.6, 8);
    let structural = arch_metrics::file_edge_pairs(symbols, refs);

    let crate_of = |file: &str| -> Option<&str> {
        member_dirs
            .iter()
            .filter(|(_, dir)| !dir.is_empty() && file.starts_with(&format!("{dir}/")))
            .max_by_key(|(_, dir)| dir.len())
            .map(|(name, _)| name.as_str())
    };

    let mut hidden: Vec<TemporalPair> = Vec::new();
    let mut fiction: Vec<TemporalPair> = Vec::new();
    let mut healthy = 0usize;
    for p in pairs {
        let a = p.file_a.to_string_lossy().replace('\\', "/");
        let b = p.file_b.to_string_lossy().replace('\\', "/");
        let key = if a <= b {
            (a.clone(), b.clone())
        } else {
            (b.clone(), a.clone())
        };
        let structural_edge = structural.contains(&key);
        let cross_crate = match (crate_of(&a), crate_of(&b)) {
            (Some(ca), Some(cb)) => ca != cb,
            _ => false,
        };
        let tp = TemporalPair {
            file_a: a,
            file_b: b,
            joint_commits: p.joint_commits,
            correlation: p.correlation,
            structural_edge,
            cross_crate,
        };
        match (structural_edge, cross_crate) {
            (false, _) => hidden.push(tp),
            (true, true) => fiction.push(tp),
            (true, false) => healthy += 1,
        }
    }
    let rank = |p: &TemporalPair| -> f32 { p.correlation * (p.joint_commits as f32).ln_1p() };
    hidden.sort_by(|x, y| {
        rank(y)
            .partial_cmp(&rank(x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    fiction.sort_by(|x, y| {
        rank(y)
            .partial_cmp(&rank(x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Some(TemporalSection {
        hidden_coupling: hidden,
        crate_boundary_fiction: fiction,
        healthy_pairs: healthy,
        window_days: TEMPORAL_WINDOW_DAYS,
    })
}

#[cfg(not(feature = "dev-tools"))]
fn temporal_section(
    _root: &Path,
    _symbols: &[corpus_engine_scip::ScipSymbolRecord],
    _refs: &[corpus_engine_scip::ScipRefRecord],
    _member_dirs: &BTreeMap<String, String>,
) -> Option<TemporalSection> {
    None
}

// ── The MCP tool ──────────────────────────────────────────────────────────────

pub struct ArchReportTool {
    indexes_dir: PathBuf,
    project_root: Option<PathBuf>,
}

impl Default for ArchReportTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchReportTool {
    pub fn new() -> Self {
        Self {
            indexes_dir: sovereign_contracts::rebrand::data_dir().join("indexes"),
            project_root: None,
        }
    }

    pub fn with_indexes_dir(indexes_dir: PathBuf) -> Self {
        Self {
            indexes_dir,
            project_root: None,
        }
    }

    /// Workspace root — unlocks the declared-deps, layer-map, filesystem and
    /// git sections. Without it the report is SCIP-only.
    pub fn with_project_root(mut self, root: PathBuf) -> Self {
        self.project_root = Some(root);
        self
    }

    fn code_corpora(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.indexes_dir) {
            for e in entries.flatten() {
                if e.path().join("scip_graph.db").exists() {
                    if let Some(name) = e.file_name().to_str() {
                        out.push(name.to_string());
                    }
                }
            }
        }
        out.sort();
        out
    }
}

#[async_trait]
impl Tool for ArchReportTool {
    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            id: "arch_report".to_string(),
            name: "Architecture Report".to_string(),
            description: "The architectural posture of a codebase, from the SCIP graph: \
                god-crate fan-in/instability table, the heaviest cross-crate coupling edges \
                WITH the symbols that carry them (the input for interface extraction), \
                declared-vs-observed dependency deltas (removable Cargo edges; re-export-hidden \
                coupling), SCIP-observed layer-map violations, file fan-in hotspots, intra-crate \
                file cycles, file-size offenders, feature-axis spread, and (when git history is \
                enabled) temporal coupling — file pairs that change together without any \
                structural edge. Deterministic, no model. Use before refactors that move \
                boundaries, and to answer 'where is the coupling actually?'."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "corpus_id": {
                        "type": "string",
                        "description": "Code corpus id (an indexed repo). Optional when exactly one code corpus is indexed."
                    },
                    "include_git": {
                        "type": "boolean",
                        "description": "Compute the temporal-coupling section from git history (adds seconds).",
                        "default": false
                    }
                },
                "required": []
            }),
            examples: vec![ToolExample {
                situation: "You're planning to split a hub crate or move a boundary and need \
                    to know which symbols actually carry the coupling between crates — do NOT \
                    grep imports one file at a time; the reference graph already knows."
                    .into(),
                call: serde_json::json!({ "corpus_id": "commonwealth-ai", "include_git": true }),
            }],
            effect: Effect::Read,
            idempotency: Idempotency::Idempotent,
            latency: Latency::Slow,
            scope: Scope::Persistent,
            output_schema: Some(serde_json::json!({
                "type": "string",
                "description": "Markdown architecture report derived from the SCIP graph (+ cargo metadata + git when rooted)."
            })),
        }
    }

    fn required_permissions(&self) -> Vec<Permission> {
        vec![]
    }

    fn validate(&self, params: &serde_json::Value) -> Result<()> {
        if let Some(c) = params.get("corpus_id").and_then(|v| v.as_str()) {
            if c.is_empty()
                || !c
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            {
                return Err(Error::InvalidInput(format!(
                    "invalid corpus_id '{c}': alphanumeric plus '-' and '_' only"
                )));
            }
        }
        Ok(())
    }

    async fn execute(&self, params: &serde_json::Value, _ctx: &ToolContext) -> Result<StepOutput> {
        let corpus_id = match params.get("corpus_id").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => {
                let corpora = self.code_corpora();
                match corpora.len() {
                    1 => corpora[0].clone(),
                    0 => {
                        return Ok(StepOutput::Text(format!(
                            "No code corpus found under {}. Run `sovereign project init` in a \
                             repository first.",
                            self.indexes_dir.display()
                        )))
                    }
                    _ => {
                        return Ok(StepOutput::Text(format!(
                            "Multiple code corpora are indexed — pass `corpus_id`. Available: {}",
                            corpora.join(", ")
                        )))
                    }
                }
            }
        };
        let include_git = params
            .get("include_git")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let db_path = self.indexes_dir.join(&corpus_id).join("scip_graph.db");
        if !db_path.exists() {
            return Ok(StepOutput::Text(format!(
                "No SCIP graph at {} — `{corpus_id}` may not be indexed.",
                db_path.display()
            )));
        }

        let data = build_arch_report(ArchReportInputs {
            db_path: &db_path,
            corpus_id: &corpus_id,
            project_root: self.project_root.as_deref(),
            include_git,
        })
        .await?;
        Ok(StepOutput::Text(render_report(&data)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_id_matches_mcp_surface() {
        let tool = ArchReportTool::with_indexes_dir(PathBuf::from("/nonexistent"));
        assert_eq!(tool.descriptor().id, "arch_report");
        assert!(crate::mcp_surface::MCP_TOOLS_ALWAYS.contains(&tool.descriptor().id.as_str()));
    }

    #[test]
    fn corpus_id_validation_rejects_path_traversal() {
        let tool = ArchReportTool::with_indexes_dir(PathBuf::from("/nonexistent"));
        assert!(tool
            .validate(&serde_json::json!({"corpus_id": "../etc"}))
            .is_err());
        assert!(tool
            .validate(&serde_json::json!({"corpus_id": "commonwealth-ai"}))
            .is_ok());
    }

    #[test]
    fn fingerprint_changes_with_db_identity() {
        // Different db paths → different fingerprints even with no files.
        let a = compute_fingerprint(Path::new("/nonexistent/a.db"), None);
        let b = compute_fingerprint(Path::new("/nonexistent/b.db"), None);
        assert_ne!(a, b);
    }
}
