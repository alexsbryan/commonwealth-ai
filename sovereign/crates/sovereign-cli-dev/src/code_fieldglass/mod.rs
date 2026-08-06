// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code fieldglass` — the architecture rendered so an experienced eye
//! can judge health at a glance. Evidence, never verdicts: each panel is a
//! projection in which a specific pathology has an unmistakable SHAPE
//! (upward arrows against the layer grain, a block-diagonal trait matrix, a
//! cat's cradle of duplication arcs) — no scores, no gates, nothing to
//! Goodhart. The full product rationale lives in `docs/FIELDGLASS.md`.
//!
//! Own module per ARCH §3.1 (`code_cmd.rs` is on the oversized baseline).
//! The page template is DATA per §6.2 — a sibling `fieldglass.html` pulled
//! in with `include_str!`, self-contained: no CDN, no network, layout
//! computed HERE deterministically (see `layout.rs` for why).
//!
//! Freshness: unlike the persisted-report readers, this verb COMPUTES its
//! inputs in-process through the same `build_arch_report` path the
//! `arch-report` verb uses, so a stale-render state cannot exist. That costs
//! one extra full SCIP load (symbols + refs are also needed raw for the
//! ISP/SRP derivations, and `build_arch_report` does not expose its copy) —
//! measured acceptable for an on-demand render, and preferred over mirror
//! deserialization structs that could drift (§10.6).

mod assemble;
mod derive;
mod layout;
mod model;

use assemble::*;
use model::*;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use corpus_engine_archaeology::git_archaeology::batch_harvest_all_commits;
use corpus_engine_scip::ScipGraph;
use sovereign_tools::code::arch_report::{
    build_arch_report, declared_deps_from_cargo, ArchReportInputs,
};
use sovereign_tools::code::dry_report::{build_dry_report, DryInputs};

const TEMPLATE: &str = include_str!("fieldglass.html");

/// Canvas the treemap panel is laid out on. Fixed: stable geometry is the
/// point (the page scales it via viewBox).
const CANVAS_W: f64 = 1560.0;
const CANVAS_H: f64 = 960.0;
/// Vertical pixels reserved inside each crate rect for its label.
const CRATE_LABEL_PAD: f64 = 16.0;

/// SRP co-change window/thresholds. Window matches the arch temporal section
/// (18 months — pre-carve-out history poisons pairs, same rationale); the
/// pair thresholds are the drift-report defaults, looser than the arch
/// hidden-coupling ones because community DETECTION wants more edges than
/// hidden-coupling REPORTING does.
const SRP_WINDOW_DAYS: i64 = 548;
const SRP_CORRELATION: f32 = 0.5;
const SRP_MIN_JOINT: u32 = 5;
/// A bridge verdict needs evidence: fewer than this many attributable
/// incoming references and the file renders unscored, not "clean".
const BRIDGE_MIN_INCOMING: usize = 8;
/// Trait matrices rendered (top by total refs).
const ISP_TOP_N: usize = 12;
/// Churn/tollbooth window. Shorter than the SRP window on purpose: "which
/// switchboards does every feature re-edit" is a question about NOW.
const CHURN_WINDOW_DAYS: i64 = 90;
/// Comprehension-tax entries need this many reads before they rank —
/// below it, "re-read a lot" is one curious session, not a signal.
const TAX_MIN_READS: u64 = 5;
/// Duplication arcs kept per near-clone cluster and in total — beyond this
/// the panel stops being readable; the honesty footer reports what was cut.
const DUP_ARCS_PER_CLUSTER: usize = 10;
const DUP_ARCS_TOTAL: usize = 400;

// ── The page's data model (serialized into __DATA__) ─────────────────────────

// ── CLI entry ────────────────────────────────────────────────────────────────

pub(crate) async fn run(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut json_path: Option<PathBuf> = None;
    let mut root_override: Option<PathBuf> = None;
    let mut open_flag = false;
    let mut include_git = true;
    let mut include_dup = true;
    let mut include_agent = true;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" | "help" => {
                print_help();
                return 0;
            }
            "--out" => {
                i += 1;
                out_path = args.get(i).map(PathBuf::from);
            }
            "--json" => {
                i += 1;
                json_path = args.get(i).map(PathBuf::from);
            }
            "--root" => {
                i += 1;
                root_override = args.get(i).map(PathBuf::from);
            }
            "--open" => open_flag = true,
            "--no-git" => include_git = false,
            "--no-dup" => include_dup = false,
            "--no-agent" => include_agent = false,
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag {flag}");
                print_help();
                return 1;
            }
            positional => {
                if corpus_id.is_none() {
                    corpus_id = Some(positional.to_string());
                }
            }
        }
        i += 1;
    }

    // Corpus + root resolution — same shape as `arch-report`.
    let indexes_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".sovereign")
        .join("indexes");
    let corpus_id = match corpus_id {
        Some(c) => c,
        None => {
            let mut corpora: Vec<String> = std::fs::read_dir(&indexes_dir)
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().join("scip_graph.db").exists())
                        .filter_map(|e| e.file_name().to_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            corpora.sort();
            match corpora.len() {
                1 => corpora.remove(0),
                0 => {
                    eprintln!(
                        "error: no code corpus under {} — run `svrn project init` first",
                        indexes_dir.display()
                    );
                    return 1;
                }
                _ => {
                    eprintln!("error: multiple code corpora — pass one of: {}", corpora.join(", "));
                    return 1;
                }
            }
        }
    };
    let db_path = indexes_dir.join(&corpus_id).join("scip_graph.db");
    if !db_path.exists() {
        eprintln!("error: no SCIP graph at {}", db_path.display());
        eprintln!("       build one: `svrn project init` (new corpus) or `svrn project refresh` (existing)");
        return 1;
    }
    let Some(root) = root_override.or_else(|| {
        std::env::current_dir().ok().filter(|d| d.join("Cargo.toml").exists())
    }) else {
        // Unlike arch-report, fieldglass does not degrade to SCIP-only: the
        // treemap, layer flow, and SRP panels all need the workspace. Refuse
        // loudly rather than render a mostly-empty page (§18.3: never
        // silently substitute).
        eprintln!("error: no workspace root — run from the repo or pass --root <path>");
        return 1;
    };

    let mut notes: Vec<String> = Vec::new();
    let stage = |name: &str, t: std::time::Instant| {
        tracing::debug!(stage = name, ms = t.elapsed().as_millis() as u64, "fieldglass:stage");
        eprintln!("  {name}: {:.1}s", t.elapsed().as_secs_f64());
    };

    // 1 — the arch report, computed fresh through the shared builder.
    let t = std::time::Instant::now();
    let report = match build_arch_report(ArchReportInputs {
        db_path: &db_path,
        corpus_id: &corpus_id,
        project_root: Some(&root),
        include_git,
    })
    .await
    {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: arch report: {e}");
            return 1;
        }
    };
    stage("arch report", t);

    // 2 — workspace facts + layer map (the same deciders the gates use).
    let Some(info) = declared_deps_from_cargo(&root) else {
        eprintln!("error: `cargo metadata` failed under {}", root.display());
        return 1;
    };
    let layer_map = match std::fs::read_to_string(root.join("quality/ARCH_LAYERS.toml")) {
        Ok(text) => match arch_layers::parse(&text) {
            Ok(m) => Some(m),
            Err(e) => {
                eprintln!("error: ARCH_LAYERS.toml: {e}");
                return 1;
            }
        },
        Err(_) => {
            notes.push("no quality/ARCH_LAYERS.toml — layer bands unavailable".to_string());
            None
        }
    };

    // 3 — raw SCIP records for the ISP + bridge derivations.
    let t = std::time::Instant::now();
    let graph = match ScipGraph::open(&db_path, &corpus_id) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: open scip graph: {e}");
            return 1;
        }
    };
    let (symbols, refs) = match (graph.iter_all_symbols().await, graph.iter_all_refs().await) {
        (Ok(s), Ok(r)) => (s, r),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("error: scip load: {e}");
            return 1;
        }
    };
    stage("scip load", t);

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Input freshness — the picture must state which commit it describes
    // (§18.4: validate the instrument). The structural panels are only as
    // current as the SCIP index; the duplication near tier only as current
    // as the chunk-embedding index. Both lag silently during normal
    // operation unless surfaced HERE.
    let scip_head = graph.last_indexed_head().await.unwrap_or_default();
    let scip_commits_behind = commits_behind(&root, &scip_head);
    let chunk_index_age_days = chunk_index_age_days(&indexes_dir.join(&corpus_id), now_unix);
    if let Some(n) = scip_commits_behind.filter(|&n| n > 0) {
        notes.push(format!(
            "structure panels describe commit {} — {n} commit(s) behind HEAD \
             (refresh: the daemon reindexer, or `svrn project refresh`)",
            &scip_head[..scip_head.len().min(8)]
        ));
        tracing::warn!(behind = n, "fieldglass:scip_index_behind_head");
    }
    if let Some(age) = chunk_index_age_days.filter(|&a| a > 7.0) {
        notes.push(format!(
            "duplication NEAR tier reads a {age:.0}-day-old embedding index \
             (refresh: `svrn code index . --corpus-id={corpus_id}`); the exact tier is fresh"
        ));
        tracing::warn!(age_days = age, "fieldglass:chunk_index_stale");
    }

    // 4 — every source .rs file, for an honest mass map. The repo's git
    // index decides what "source" means (tracked + untracked-not-ignored);
    // the walk fallback is for gitless checkouts and says so.
    let t = std::time::Instant::now();
    let source_set = git_source_set(&root);
    if source_set.is_none() {
        notes.push(
            "git unavailable — file universe from a filesystem walk (.git/target* skipped); \
             ignore rules not honored, agent-heat noise filter off"
                .to_string(),
        );
        tracing::warn!("fieldglass:git_source_set_unavailable");
    }
    let (walked, outside) = walk_rs_files(&root, &info.member_dirs, source_set.as_ref());
    stage("file walk", t);

    // 5 — ISP matrices.
    let t = std::time::Instant::now();
    let members: BTreeSet<String> = info
        .member_dirs
        .keys()
        .map(|n| corpus_engine_scip::arch_metrics::normalize_crate_name(n))
        .collect();
    let isp = derive::trait_matrices(&symbols, &refs, &members, ISP_TOP_N);
    stage("isp matrices", t);

    // 6 — git-derived layers: SRP communities + bridge scores + churn.
    // One harvest (one `git log` subprocess), consumed by both.
    let t = std::time::Instant::now();
    let history = if include_git {
        match batch_harvest_all_commits(&root) {
            Ok(h) => Some(h),
            Err(e) => {
                notes.push(format!("git harvest failed ({e}) — SRP and churn panels dark"));
                None
            }
        }
    } else {
        notes.push("--no-git: SRP communities, churn and ghost edges skipped".to_string());
        None
    };
    let (file_community, n_communities) = history
        .as_ref()
        .map(|h| {
            derive::srp_communities(h, now_unix, SRP_WINDOW_DAYS, SRP_CORRELATION, SRP_MIN_JOINT)
        })
        .unwrap_or_default();
    let (churn, churn_commits) = history
        .as_ref()
        .map(|h| derive::churn_counts(h, now_unix, CHURN_WINDOW_DAYS))
        .unwrap_or_default();
    let bridges = derive::bridge_scores(&symbols, &refs, &file_community, BRIDGE_MIN_INCOMING);
    stage("srp + churn", t);

    // 6b — agent activity from session transcripts, via the OWNING parser
    // (`cache-audit --by-file` in the sovereign-cli sibling — §10.6: one
    // transcript decider, shelled not reimplemented).
    let t = std::time::Instant::now();
    let (agent, agent_sessions, agent_first, agent_last, agent_non_source) = if include_agent {
        match agent_activity(&root, source_set.as_ref()) {
            Ok(x) => x,
            Err(e) => {
                notes.push(format!("agent-heat pass unavailable ({e}) — panel dark"));
                (BTreeMap::new(), 0, 0, 0, 0)
            }
        }
    } else {
        notes.push("--no-agent: agent heat skipped".to_string());
        (BTreeMap::new(), 0, 0, 0, 0)
    };
    if agent_non_source > 0 {
        notes.push(format!(
            "agent heat: {agent_non_source} path(s) outside the git source set \
             (ignored/generated) excluded — real spend, architecture noise"
        ));
    }
    stage("agent activity", t);

    // 7 — duplication arcs from the SHIPPED clone detector.
    let t = std::time::Instant::now();
    if include_dup {
        eprintln!(
            "  duplication: near-clone pass is minutes-scale (O(n²) over the symbol \
             embeddings; longer under CPU load) — progress lines follow; skip with --no-dup"
        );
    }
    let (dup_arcs, dup_dropped, dup_clusters) = if include_dup {
        match build_dry_report(DryInputs {
            index_path: &indexes_dir.join(&corpus_id),
            corpus_id: &corpus_id,
            min_lines: sovereign_tools::code::dry_report::DEFAULT_MIN_LINES,
            near_threshold: sovereign_tools::code::dry_report::DEFAULT_NEAR_THRESHOLD,
            scope: None,
        })
        .await
        {
            Ok(r) => dup_arcs_from(&r, &root),
            Err(e) => {
                notes.push(format!("dry-report failed ({e}) — duplication panel dark"));
                (Vec::new(), 0, Vec::new())
            }
        }
    } else {
        notes.push("--no-dup: duplication panel skipped".to_string());
        (Vec::new(), 0, Vec::new())
    };
    stage("duplication", t);

    // 8 — layer flow.
    let (crates, flow_edges, layers) = match &layer_map {
        Some(map) => derive::build_flow(&report.metrics, &info, map),
        None => (Vec::new(), Vec::new(), Vec::new()),
    };

    // 9 — treemap layout. Crates in (layer, name) order — the fixed order is
    // what keeps the map's neighborhoods stable day over day.
    let file_fan_in: BTreeMap<&str, usize> = report
        .metrics
        .files
        .iter()
        .map(|f| (f.file.as_str(), f.fan_in))
        .collect();
    let crate_layer: BTreeMap<&str, i32> =
        crates.iter().map(|c| (c.name.as_str(), c.layer)).collect();
    let (crate_rects, files) = layout_treemap(
        &walked,
        &crate_layer,
        &file_fan_in,
        &file_community,
        &bridges,
        &agent,
        &churn,
    );

    // 10 — ghost edges (temporal), mapped onto treemap paths.
    let ghosts: Vec<GhostEdge> = report
        .temporal
        .as_ref()
        .map(|t| {
            t.hidden_coupling
                .iter()
                .map(|p| (p, false))
                .chain(t.crate_boundary_fiction.iter().map(|p| (p, true)))
                .map(|(p, fiction)| GhostEdge {
                    a: p.file_a.clone(),
                    b: p.file_b.clone(),
                    joint: p.joint_commits,
                    corr: p.correlation,
                    fiction,
                })
                .collect()
        })
        .unwrap_or_default();

    // Resolve outputs BEFORE building data: the delta layer reads the
    // previous render's sidecar from the same path it is about to replace.
    let out_path = out_path
        .unwrap_or_else(|| sovereign_root().join("arch").join(&corpus_id).join("fieldglass.html"));
    let json_path = json_path.unwrap_or_else(|| out_path.with_extension("json"));
    let delta = compute_delta(&json_path, &files);

    let head = git_head(&root);
    let stats = &report.metrics.stats;
    let attention = Attention {
        comprehension_tax: {
            let mut tax: Vec<TaxEntry> = agent
                .iter()
                .filter(|(_, a)| a.reads >= TAX_MIN_READS)
                .map(|(p, a)| TaxEntry {
                    path: p.clone(),
                    reads: a.reads,
                    read_tokens: a.read_tokens,
                    edits: a.edits,
                    sessions: a.sessions,
                })
                .collect();
            // Rank by tokens-per-edit — read-hot AND edit-cold, not merely big.
            tax.sort_by(|x, y| {
                let rx = x.read_tokens / (x.edits + 1);
                let ry = y.read_tokens / (y.edits + 1);
                ry.cmp(&rx).then(x.path.cmp(&y.path))
            });
            tax.truncate(12);
            tax
        },
        tollbooths: {
            let mut t: Vec<(String, u32, f32)> = churn
                .iter()
                .map(|(p, n)| (p.clone(), *n, *n as f32 / churn_commits.max(1) as f32))
                .collect();
            t.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
            t.truncate(12);
            t
        },
        dup_clusters,
        bridges: {
            let mut b: Vec<(String, f32)> =
                bridges.iter().map(|(p, s)| (p.clone(), *s)).collect();
            b.sort_by(|x, y| {
                y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal).then(x.0.cmp(&y.0))
            });
            b.truncate(12);
            b
        },
        offenders: {
            let mut o: Vec<(String, usize)> = walked
                .iter()
                .filter(|(_, _, lines)| *lines > 1200)
                .map(|(path, _, lines)| (path.clone(), *lines))
                .collect();
            o.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
            o.truncate(12);
            o
        },
    };
    let data = FieldglassData {
        corpus: corpus_id.clone(),
        head,
        generated_unix: now_unix,
        canvas_w: CANVAS_W,
        canvas_h: CANVAS_H,
        layers,
        crates,
        flow_edges,
        crate_rects,
        honesty: Honesty {
            scip_head,
            scip_commits_behind,
            chunk_index_age_days,
            refs_total: stats.refs_total,
            refs_cross_crate: stats.refs_cross_crate,
            refs_dropped_unattributed: stats.refs_dropped_unattributed,
            refs_dropped_test: stats.refs_dropped_test,
            refs_dropped_external: stats.refs_dropped_external,
            temporal_window_days: SRP_WINDOW_DAYS,
            srp_correlation: SRP_CORRELATION,
            srp_min_joint: SRP_MIN_JOINT,
            dry_threshold: sovereign_tools::code::dry_report::DEFAULT_NEAR_THRESHOLD,
            dry_min_lines: sovereign_tools::code::dry_report::DEFAULT_MIN_LINES,
            files_walked: walked.len(),
            files_outside_crates: outside,
            communities: n_communities,
            dup_arcs_dropped: dup_dropped,
            agent_sessions,
            agent_first_mtime: agent_first,
            agent_last_mtime: agent_last,
            churn_window_days: CHURN_WINDOW_DAYS,
            churn_commits,
            notes,
        },
        files,
        ghosts,
        dup_arcs,
        isp,
        attention,
        delta,
    };

    let html = render_html(&data);
    if let Some(parent) = out_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("error: cannot create {}: {e}", parent.display());
            return 1;
        }
    }
    if let Err(e) = std::fs::write(&out_path, &html) {
        eprintln!("error: writing {}: {e}", out_path.display());
        return 1;
    }
    // JSON sidecar — same md+json house pattern as fleet-report; the next
    // render's delta layer diffs against it. A DEGRADED render (any layer
    // skipped) must NOT replace it: a `--no-dup` quick pass overwriting the
    // baseline would make tomorrow's glance diff against a partial snapshot
    // — silently, which is exactly the §18.2 failure. Full renders own the
    // baseline; degraded ones say they left it alone.
    if include_git && include_dup && include_agent {
        match serde_json::to_string_pretty(&data) {
            Ok(j) => {
                if let Err(e) = std::fs::write(&json_path, j) {
                    eprintln!("warning: sidecar {}: {e}", json_path.display());
                }
            }
            Err(e) => eprintln!("warning: sidecar serialize: {e}"),
        }
    } else {
        println!(
            "  degraded render (layer(s) skipped) — delta baseline at {} preserved",
            json_path.display()
        );
    }

    let abs = std::fs::canonicalize(&out_path).unwrap_or(out_path);
    println!(
        "wrote {} — {} files, {} crates, {} flow edges ({} violations), {} trait matrices, \
         {} communities, {} dup arcs, {} ghost edges",
        abs.display(),
        data.files.len(),
        data.crate_rects.len(),
        data.flow_edges.len(),
        data.flow_edges.iter().filter(|e| e.kind == "upward" || e.kind == "forbidden").count(),
        data.isp.len(),
        data.honesty.communities,
        data.dup_arcs.len(),
        data.ghosts.len(),
    );
    tracing::info!(
        corpus = %data.corpus,
        files = data.files.len(),
        matrices = data.isp.len(),
        "fieldglass:rendered"
    );
    if open_flag {
        open_in_browser(&abs);
    }
    0
}

fn print_help() {
    eprintln!(
        "Usage: svrn code fieldglass [corpus-id] [--out <file.html>] [--json <file.json>]\n\
         \x20                        [--root <path>] [--open] [--no-git] [--no-dup] [--no-agent]\n\n\
         Render the architecture-health page (evidence, not verdicts):\n\
         treemap + layer flow + trait (ISP) matrices + co-change (SRP)\n\
         communities + duplication arcs + temporal ghost edges + agent\n\
         read/write heat (from session transcripts) + churn tollbooths +\n\
         a since-last-render delta.\n\
         Default output: ~/.sovereign/arch/<corpus>/fieldglass.html (+ .json sidecar).\n\
         How to read each panel: docs/FIELDGLASS.md."
    );
}

/// Render the self-contained page. Pure function of `data` — the golden
/// determinism test hashes its output.
fn render_html(data: &FieldglassData) -> String {
    let json = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());
    // Never let a `</` inside a JSON string close the inline <script>.
    let json = json.replace("</", "<\\/");
    TEMPLATE
        .replace("__TITLE__", &format!("fieldglass — {}", data.corpus))
        .replace("__DATA__", &json)
}

fn sovereign_root() -> PathBuf {
    sovereign_cli_shared::dirs::sovereign_root()
}

fn open_in_browser(path: &Path) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    match std::process::Command::new(opener).arg(path).spawn() {
        Ok(_) => println!("  opening in your browser…"),
        Err(e) => eprintln!("  note: couldn't launch {opener} ({e}); open the path above manually"),
    }
}

#[cfg(test)]
mod tests {
    use super::derive::{CrateNode, FlowEdge, TraitMatrix};
    use super::*;

    fn fixture_data() -> FieldglassData {
        FieldglassData {
            corpus: "fixture".into(),
            head: "abc1234".into(),
            generated_unix: 1_754_000_000,
            canvas_w: CANVAS_W,
            canvas_h: CANVAS_H,
            layers: vec!["contract".into(), "runtime".into(), "hosts".into()],
            crates: vec![
                CrateNode {
                    name: "low".into(),
                    layer: 0,
                    in_refs: 100,
                    out_refs: 1,
                    instability: Some(0.01),
                    fan_in: 5,
                    fan_out: 1,
                },
                CrateNode {
                    name: "high".into(),
                    layer: 2,
                    in_refs: 3,
                    out_refs: 90,
                    instability: Some(0.97),
                    fan_in: 1,
                    fan_out: 5,
                },
            ],
            // Negative control #1: a planted UPWARD edge must render as one.
            flow_edges: vec![FlowEdge {
                from: "low".into(),
                to: "high".into(),
                refs: 12,
                top: vec!["high::Thing ×12".into()],
                kind: "upward",
            }],
            crate_rects: vec![CrateRect {
                name: "low".into(),
                layer: 0,
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            }],
            files: vec![FileLeaf {
                path: "low/src/lib.rs".into(),
                crate_name: "low".into(),
                x: 1.0,
                y: 16.0,
                w: 98.0,
                h: 84.0,
                lines: 1400,
                fan_in: 9,
                community: 0,
                bridge: 0.45,
                offender: true,
                reads: 40,
                read_tokens: 90_000,
                edits: 1,
                agent_sessions: 12,
                commits_90d: 33,
            }],
            ghosts: vec![GhostEdge {
                a: "low/src/lib.rs".into(),
                b: "high/src/main.rs".into(),
                joint: 9,
                corr: 0.8,
                fiction: false,
            }],
            // Negative control #2: a planted clone pair must render.
            dup_arcs: vec![DupArc {
                a: "low/src/lib.rs".into(),
                a_line: 10,
                b: "high/src/main.rs".into(),
                b_line: 20,
                sim: 0.97,
                exact: false,
                lines: 30,
            }],
            // Negative control #3: a planted block-diagonal trait matrix.
            isp: vec![TraitMatrix {
                name: "Stapled".into(),
                pkg: "low".into(),
                file: "low/src/traits.rs".into(),
                methods: vec!["get".into(), "put".into(), "send".into(), "recv".into()],
                callers: vec!["store".into(), "net".into()],
                cells: vec![vec![5, 4, 0, 0], vec![0, 0, 6, 7]],
                cell_files: vec![
                    vec![vec![("store/src/db.rs".into(), 5)], vec![("store/src/db.rs".into(), 4)], vec![], vec![]],
                    vec![vec![], vec![], vec![("net/src/io.rs".into(), 6)], vec![("net/src/io.rs".into(), 7)]],
                ],
                dyn_refs: 3,
                total_refs: 22,
            }],
            // Negative control #5: a planted read-hot/edit-cold file and a
            // planted tollbooth must reach the page.
            delta: Some(Delta {
                prev_unix: 1_753_000_000,
                grown: vec![("low/src/lib.rs".into(), 250)],
                new_offenders: vec!["low/src/lib.rs".into()],
                new_files: 1,
                removed_files: 0,
            }),
            attention: Attention {
                comprehension_tax: vec![TaxEntry {
                    path: "low/src/lib.rs".into(),
                    reads: 40,
                    read_tokens: 90_000,
                    edits: 1,
                    sessions: 12,
                }],
                tollbooths: vec![("low/src/lib.rs".into(), 33, 0.61)],
                dup_clusters: vec![DupClusterSummary {
                    label: "clone_family".into(),
                    files: vec!["low/src/lib.rs".into(), "high/src/main.rs".into()],
                    members: 2,
                    lines: 30,
                    redundant: 30,
                    exact: false,
                }],
                bridges: vec![("low/src/lib.rs".into(), 0.45)],
                offenders: vec![("low/src/lib.rs".into(), 1400)],
            },
            honesty: Honesty {
                scip_head: "92602386".into(),
                // Negative control #4: the fixture plants stale inputs — the
                // page must SAY so rather than render as current.
                scip_commits_behind: Some(4),
                chunk_index_age_days: Some(12.8),
                refs_total: 1000,
                refs_cross_crate: 100,
                refs_dropped_unattributed: 200,
                refs_dropped_test: 50,
                refs_dropped_external: 300,
                temporal_window_days: SRP_WINDOW_DAYS,
                srp_correlation: SRP_CORRELATION,
                srp_min_joint: SRP_MIN_JOINT,
                dry_threshold: 0.95,
                dry_min_lines: 8,
                files_walked: 1,
                files_outside_crates: 2,
                communities: 1,
                dup_arcs_dropped: 0,
                agent_sessions: 12,
                agent_first_mtime: 1_750_000_000,
                agent_last_mtime: 1_754_000_000,
                churn_window_days: CHURN_WINDOW_DAYS,
                churn_commits: 54,
                notes: vec!["fixture".into()],
            },
        }
    }

    #[test]
    fn render_is_byte_deterministic() {
        use sha2::{Digest, Sha256};
        let data = fixture_data();
        let h1 = Sha256::digest(render_html(&data).as_bytes());
        let h2 = Sha256::digest(render_html(&data).as_bytes());
        assert_eq!(h1, h2, "same data must render byte-identical HTML");
    }

    #[test]
    fn planted_pathologies_reach_the_page() {
        // §18.1 — watch the gate fail: each panel must be ABLE to show its
        // disease. The fixture plants one instance of each; the emitted page
        // must carry the marker the renderer keys on.
        let html = render_html(&fixture_data());
        assert!(html.contains("\"kind\":\"upward\""), "planted upward layer edge");
        assert!(html.contains("\"dup_arcs\":[{"), "planted clone pair");
        assert!(html.contains("\"Stapled\""), "planted block-diagonal trait");
        assert!(html.contains("\"offender\":true"), "planted 1200-line offender");
        assert!(html.contains("\"notes\":[\"fixture\"]"), "honesty notes surface");
        assert!(
            html.contains("\"scip_commits_behind\":4") && html.contains("\"chunk_index_age_days\":12.8"),
            "planted stale inputs reach the page (negative control #4)"
        );
        assert!(
            html.contains("\"comprehension_tax\":[{") && html.contains("\"tollbooths\":[["),
            "planted read-hot/edit-cold file and tollbooth reach the page (negative control #5)"
        );
        assert!(
            html.contains("\"prev_unix\":1753000000"),
            "planted since-last-render delta reaches the page"
        );
        // P4 — the one-canvas contract: flow/ISP are drill-downs (present
        // but closed), the field carries the DIP arrows + trait marks, and
        // a matrix cell carries its call-site files for the drill-through.
        assert!(
            html.contains(r#"<section id="flow" class="drill">"#)
                && html.contains(r#"<section id="isp" class="drill">"#),
            "flow and ISP render as closed drill-downs, not stacked panels (P4)"
        );
        assert!(
            html.contains("map-arr-up") && html.contains("openTrait"),
            "DIP arrows and trait marks are wired on the field (P4)"
        );
        assert!(
            html.contains("\"cell_files\":[[") && html.contains("store/src/db.rs"),
            "planted matrix cell carries its call-site files (P4 drill-through)"
        );
        // Self-containment: no external fetch vectors. (`http://www.w3.org`
        // appears legitimately as the SVG namespace CONSTANT — not a fetch.)
        assert!(
            !html.contains("<script src=")
                && !html.contains("<link ")
                && !html.contains("@import")
                && !html.contains("unpkg.com"),
            "page must be self-contained — no CDN fetches"
        );
    }

    #[test]
    fn script_escape_guard_holds() {
        let mut data = fixture_data();
        data.honesty.notes = vec!["</script><script>alert(1)</script>".into()];
        let html = render_html(&data);
        assert!(
            !html.contains("</script><script>alert"),
            "a `</` inside data must not close the inline script"
        );
    }
}
