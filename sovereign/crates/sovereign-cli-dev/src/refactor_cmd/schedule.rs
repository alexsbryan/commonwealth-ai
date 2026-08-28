// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn code refactor gate` — the entry gate over ALL FIVE work-table kinds,
//! and the ranked schedule it produces.
//!
//! The gate's output IS the schedule (rf-1 order): every kind's items get a
//! verdict, refusals appear AS refusals with reasons (never as absences,
//! ARCH §18.3), and the eligible items are ranked by sites-per-session-chunk.
//! Chunk divisors are DECLARED estimates, printed with their basis next to
//! every number they produce (ARCH §18.4) — the first real rung of each kind
//! replaces the estimate with a measurement.
//!
//! Instruments, one per kind, all read-only (ARCH §19 — reuse, not rebuild):
//!   field atoms   → the string-field census (this module; decl rule shared
//!                   with `scripts/factory-scale.py`)
//!   shapes        → `corpus_engine_scip::shape::shape_census`, at the FROZEN
//!                   detector settings (the campaign's guard: changing any of
//!                   them restarts the miss-rate series)
//!   names         → `corpus_engine_scip::converge::census` (reachable rows)
//!   behaviour     → `sovereign_tools::code::dry_report`
//!   arg loops     → the `scripts/hpr-cost.py` detection rule (this module)

use super::census;
use super::discover::WorkspaceMeta;
use super::gate::{self, TypeDecl};
use super::spec::RefactorSpec;
use corpus_engine_scip::converge::{
    census as name_census, cross_crate_reached, type_defs, SourceScope,
};
use corpus_engine_scip::shape::{field_signatures, shape_census, ShapeOptions};
use corpus_engine_scip::ScipGraph;
use std::fmt::Write as _;
use std::path::Path;

// ── Policy — the seat's standing rulings, encoded so the gate cannot forget
// them (ARCH §10 — structural, not remembered) ──────────────────────────────

/// Seat ruling 2026-08-23 (rf-1 order): these fields are `String` because
/// they ARE open text. Newtyping them is spec attack #3 — every minted type
/// is another thing a future symbol can duplicate. A gate that ranks by raw
/// count and schedules `name` has failed.
const OPEN_TEXT_ATOMS: &[&str] = &[
    "name",
    "content",
    "description",
    "text",
    "label",
    "title",
    "summary",
    "reason",
    "message",
    "question",
];

/// Closed sets are enums (ARCH §2) — an enum migration, not a newtype.
const ENUM_CANDIDATE_ATOMS: &[&str] = &["kind", "role"];

/// Atoms below this declaration count are tail, listed in aggregate only.
const ATOM_DECL_FLOOR: usize = 20;

// Chunk divisors — DECLARED estimates, each printed with its basis.
/// rf-3 (prepare) + rf-4 (apply sweep): the factory-scale bar's one-atom-one-
/// session standard, split across the two rungs that exist for it.
const NEWTYPE_CHUNKS: f64 = 2.0;
/// Operator standard (factory-deletion bar): one rung removes 10k+ net lines.
const DELETION_LINES_PER_CHUNK: f64 = 10_000.0;
/// Campaign math: a duplicate type plus impls/conversions is ~40 lines.
const LINES_PER_DUP_TYPE: f64 = 40.0;
/// H5: >=5x the measured hand control (hpr: 3 loops / 2 sessions = 1.5).
const ADOPT_API_LOOPS_PER_CHUNK: f64 = 7.5;

// ── Verdicts ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub struct ItemVerdict {
    pub item: String,
    pub kind: &'static str,
    pub verdict: &'static str,
    pub reason: String,
    /// Reach in the kind's own site unit (named in `site_unit`).
    pub sites: usize,
    pub site_unit: &'static str,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduleRow {
    pub rank: usize,
    pub item: String,
    pub kind: &'static str,
    pub sites: usize,
    pub site_unit: &'static str,
    pub est_chunks: f64,
    pub chunk_basis: &'static str,
    pub sites_per_chunk: f64,
}

pub async fn run_gate(args: &[String]) -> i32 {
    let mut corpus_id: Option<String> = None;
    let mut limit: usize = 10;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--corpus-id" => {
                i += 1;
                match args.get(i) {
                    Some(v) => corpus_id = Some(v.clone()),
                    None => {
                        eprintln!("error: --corpus-id requires a value");
                        return 1;
                    }
                }
            }
            "--limit" => {
                i += 1;
                match args.get(i).and_then(|s| s.parse::<usize>().ok()) {
                    Some(0) => limit = usize::MAX,
                    Some(n) => limit = n,
                    None => {
                        eprintln!("error: --limit requires an integer");
                        return 1;
                    }
                }
            }
            "--json" => json = true,
            "-h" | "--help" => {
                eprintln!("svrn code refactor gate [--corpus-id <id>] [--limit N] [--json]");
                return 0;
            }
            other => {
                eprintln!("error: unknown argument {other}");
                return 1;
            }
        }
        i += 1;
    }

    let root = match census::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let meta = match WorkspaceMeta::load(&root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let mut verdicts: Vec<ItemVerdict> = Vec::new();
    let mut schedule: Vec<ScheduleRow> = Vec::new();
    let mut instrument_notes: Vec<String> = Vec::new();

    // ── Kind 1: stringly-typed field atoms ──────────────────────────────
    let decl_files = census::walk_rs_files(&root, census::EXCLUDE_DIRS_DECL);
    let mention_files = census::walk_rs_files(&root, census::EXCLUDE_DIRS_MENTIONS);
    let atom_census = census::string_field_census(&decl_files);
    let mut atoms: Vec<(&String, &usize)> = atom_census
        .iter()
        .filter(|(_, c)| **c >= ATOM_DECL_FLOOR)
        .collect();
    atoms.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let tail = atom_census
        .values()
        .filter(|c| **c < ATOM_DECL_FLOOR)
        .count();

    let specs = load_specs(&root);
    for (atom, decls) in &atoms {
        let atom = atom.as_str();
        let decls = **decls;
        if OPEN_TEXT_ATOMS.contains(&atom) {
            verdicts.push(ItemVerdict {
                item: atom.to_string(),
                kind: "field-atom",
                verdict: "REFUSED",
                reason: "open text — it is String because it IS open text; newtyping it is \
                         spec attack #3 (every minted type is another thing a future symbol \
                         can duplicate). Seat ruling 2026-08-23."
                    .to_string(),
                sites: decls,
                site_unit: "String declarations",
            });
            continue;
        }
        if ENUM_CANDIDATE_ATOMS.contains(&atom) {
            verdicts.push(ItemVerdict {
                item: atom.to_string(),
                kind: "field-atom",
                verdict: "ENUM-CANDIDATE",
                reason: "closed set — an enum, never a newtype (ARCH §2). Needs its own \
                         retype-field spec with the variant inventory."
                    .to_string(),
                sites: decls,
                site_unit: "String declarations",
            });
            continue;
        }
        if atom == "id" {
            verdicts.push(ItemVerdict {
                item: atom.to_string(),
                kind: "field-atom",
                verdict: "AMBIGUOUS",
                reason: "`id` names no single concept — the right type differs per owning \
                         struct; per-type adjudication by the seat."
                    .to_string(),
                sites: decls,
                site_unit: "String declarations",
            });
            continue;
        }
        // A spec on disk is the seat's authoring — run the real entry gate.
        if let Some(spec) = specs.iter().find(|s| s.discover.seed.field == atom) {
            let report = gate::gate_spec(&root, &meta, spec, true);
            let mentions = census::mention_count(&mention_files, atom);
            if report.passed() {
                verdicts.push(ItemVerdict {
                    item: format!("{atom} -> {}", spec.target),
                    kind: "field-atom",
                    verdict: "SCHEDULED",
                    reason: format!(
                        "entry gate passed: representation {:?}, wire \"{}\", fixture {:?}, \
                         constructor {}",
                        report.representation,
                        report.wire_observed,
                        report.fixture,
                        report.constructor
                    ),
                    sites: mentions,
                    site_unit: "in-scope mentions",
                });
                let chunks = NEWTYPE_CHUNKS;
                schedule.push(ScheduleRow {
                    rank: 0,
                    item: format!("{} (spec {})", spec.target, spec.id),
                    kind: "newtype",
                    sites: mentions,
                    site_unit: "in-scope mentions",
                    est_chunks: chunks,
                    chunk_basis:
                        "rf-3 prepare + rf-4 apply (factory-scale bar: one atom per session)",
                    sites_per_chunk: mentions as f64 / chunks,
                });
            } else {
                verdicts.push(ItemVerdict {
                    item: format!("{atom} -> {}", spec.target),
                    kind: "field-atom",
                    verdict: "REFUSED",
                    reason: format!(
                        "entry gate failed: representation match {} ({:?}), wire consistent {} \
                         (declared {:?}, observed \"{}\"), fixture {:?} — filed, not scheduled",
                        report.representation_matches,
                        report.representation,
                        report.wire_consistent,
                        report.wire_declared.as_deref().unwrap_or("<undeclared>"),
                        report.wire_observed,
                        report.fixture
                    ),
                    sites: mentions,
                    site_unit: "in-scope mentions",
                });
            }
            continue;
        }
        // No spec. An id-shaped atom may already have a kernel-types target.
        if atom.ends_with("_id") {
            let type_name = pascal_case(atom);
            let kt_dir = meta.get("kernel-types").map(|p| p.dir.clone());
            let site = kt_dir
                .as_deref()
                .map(|d| gate::locate_type_def(d, &type_name));
            match site {
                Some(site) if site.decl == TypeDecl::DefineId => {
                    let mentions = census::mention_count(&mention_files, atom);
                    verdicts.push(ItemVerdict {
                        item: format!("{atom} -> kernel_types::{type_name}"),
                        kind: "field-atom",
                        verdict: "EXCLUDED",
                        reason: format!(
                            "wire MISMATCH: {} — `cargo check` would PASS the migration and \
                             every serialized consumer breaks (the node_id near-miss). Filed \
                             as a finding; blocked on the kernel-types three-encodings \
                             resolution.",
                            gate::observed_wire(&site, &type_name)
                        ),
                        sites: mentions,
                        site_unit: "in-scope mentions",
                    });
                }
                Some(site) if site.decl != TypeDecl::NotFound => {
                    verdicts.push(ItemVerdict {
                        item: format!("{atom} -> kernel_types::{type_name}"),
                        kind: "field-atom",
                        verdict: "NEEDS-SPEC",
                        reason: format!(
                            "target exists ({:?} at {}:{}) but no spec in quality/refactors/ \
                             declares its wire form + fixture — seat authors it",
                            site.decl,
                            site.file.display(),
                            site.line
                        ),
                        sites: decls,
                        site_unit: "String declarations",
                    });
                }
                _ => {
                    verdicts.push(ItemVerdict {
                        item: atom.to_string(),
                        kind: "field-atom",
                        verdict: "NEEDS-SPEC",
                        reason: "id-shaped, no kernel-types target yet — minting one is a \
                                 convergence decision (`svrn code converge noun` first)"
                            .to_string(),
                        sites: decls,
                        site_unit: "String declarations",
                    });
                }
            }
            continue;
        }
        verdicts.push(ItemVerdict {
            item: atom.to_string(),
            kind: "field-atom",
            verdict: "UNADJUDICATED",
            reason: "no spec and no standing ruling — candidate for the seat".to_string(),
            sites: decls,
            site_unit: "String declarations",
        });
    }

    // ── Kinds 2 + 3: shapes and names, off the SCIP graph ───────────────
    let indexes_dir = sovereign_cli_shared::dirs::sovereign_root().join("indexes");
    match crate::converge_cmd::resolve_corpus(corpus_id.clone(), &indexes_dir) {
        Ok(corpus) => {
            let db_path = indexes_dir.join(&corpus).join("scip_graph.db");
            match ScipGraph::open(&db_path, &corpus) {
                Ok(graph) => {
                    graph_kinds(
                        &graph,
                        &mut verdicts,
                        &mut schedule,
                        &mut instrument_notes,
                        limit,
                    )
                    .await;
                }
                Err(e) => instrument_notes.push(format!(
                    "shapes+names: COULD NOT JUDGE — opening {}: {e}",
                    db_path.display()
                )),
            }
            // ── Kind 4: duplicate behaviour ─────────────────────────────
            let index_path = indexes_dir.join(&corpus);
            if index_path.join("chunks.lance").exists() {
                behaviour_kind(&index_path, &corpus, &mut verdicts, &mut schedule, limit).await;
            } else {
                instrument_notes.push(format!(
                    "behaviour: COULD NOT JUDGE — no chunk index at {}",
                    index_path.join("chunks.lance").display()
                ));
            }
        }
        Err(_) => {
            instrument_notes.push(
                "shapes+names+behaviour: COULD NOT JUDGE — no code corpus resolved".to_string(),
            );
        }
    }

    // ── Kind 5: hand-rolled arg loops ───────────────────────────────────
    let scan = census::arg_loop_scan(&decl_files);
    let loops = scan.hand_rolled.len();
    let arms: usize = scan.hand_rolled.iter().map(|(_, a)| a).sum();
    if loops > 0 {
        verdicts.push(ItemVerdict {
            item: format!("{loops} hand-rolled flag surfaces ({arms} bare-flag match arms)"),
            kind: "adopt-api",
            verdict: "ELIGIBLE",
            reason: format!(
                "detection rule from scripts/hpr-cost.py; {} file(s) already derived; the \
                 adopt-api spec differs from corpus-id only in [discover]+[rules] (H4)",
                scan.derived
            ),
            sites: loops,
            site_unit: "files to convert",
        });
        let chunks = (loops as f64 / ADOPT_API_LOOPS_PER_CHUNK).max(1.0);
        schedule.push(ScheduleRow {
            rank: 0,
            item: "arg-loop adopt-api (clap derive)".to_string(),
            kind: "adopt-api",
            sites: loops,
            site_unit: "files to convert",
            est_chunks: chunks,
            chunk_basis: "H5: >=5x the hand control (hpr measured 1.5 loops/session by hand)",
            sites_per_chunk: loops as f64 / chunks,
        });
    }
    for path in &scan.mixed {
        verdicts.push(ItemVerdict {
            item: path
                .strip_prefix(&root)
                .unwrap_or(path)
                .display()
                .to_string(),
            kind: "adopt-api",
            verdict: "COULD-NOT-JUDGE",
            reason: "mixed surface (derive AND hand-rolled arms in one file) — which surface \
                     an added flag lands on is undetermined (hpr-cost.py rule; never scored)"
                .to_string(),
            sites: 0,
            site_unit: "files",
        });
    }

    // ── Rank ────────────────────────────────────────────────────────────
    schedule.sort_by(|a, b| {
        b.sites_per_chunk
            .partial_cmp(&a.sites_per_chunk)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (i, row) in schedule.iter_mut().enumerate() {
        row.rank = i + 1;
    }

    if json {
        let out = serde_json::json!({
            "atom_decl_floor": ATOM_DECL_FLOOR,
            "atom_tail_below_floor": tail,
            "verdicts": verdicts,
            "schedule": schedule,
            "instrument_notes": instrument_notes,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        return 0;
    }

    print!(
        "{}",
        render(&verdicts, &schedule, &instrument_notes, tail, limit)
    );
    0
}

async fn graph_kinds(
    graph: &ScipGraph,
    verdicts: &mut Vec<ItemVerdict>,
    schedule: &mut Vec<ScheduleRow>,
    notes: &mut Vec<String>,
    limit: usize,
) {
    let symbols = match graph.iter_all_symbols().await {
        Ok(s) => s,
        Err(e) => {
            notes.push(format!(
                "shapes+names: COULD NOT JUDGE — reading symbols: {e}"
            ));
            return;
        }
    };
    let refs = match graph.iter_all_refs().await {
        Ok(r) => r,
        Err(e) => {
            notes.push(format!("shapes+names: COULD NOT JUDGE — reading refs: {e}"));
            return;
        }
    };
    let scope = SourceScope::default();

    // Kind 2: duplicate SHAPES, at the frozen detector settings. The campaign
    // guard is explicit: changing any of these restarts the miss-rate series,
    // so `ShapeOptions::default()` (threshold 0.50, min_shared 3, rare_df 20,
    // min_fields 2) is the ONLY configuration this gate will ever run.
    let sigs = field_signatures(&symbols, &refs, &scope);
    let shapes = shape_census(&symbols, &sigs, &scope, &ShapeOptions::default());
    let exact: Vec<_> = shapes
        .groups
        .iter()
        .filter(|g| g.top_score >= 0.9999)
        .collect();
    let near = shapes.groups.len() - exact.len();
    let loser_types: usize = exact.iter().map(|g| g.members.len() - 1).sum();
    if !exact.is_empty() {
        let mut top: Vec<String> = exact
            .iter()
            .take(limit)
            .map(|g| {
                format!(
                    "{} ({} members)",
                    g.members.first().cloned().unwrap_or_default(),
                    g.members.len()
                )
            })
            .collect();
        if exact.len() > limit {
            top.push(format!("… +{} groups", exact.len() - limit));
        }
        verdicts.push(ItemVerdict {
            item: format!(
                "{} exact-shape groups, {} member types: {}",
                exact.len(),
                loser_types + exact.len(),
                top.join(", ")
            ),
            kind: "merge-shape",
            verdict: "ELIGIBLE-PENDING-FIXTURE",
            reason: "identical (field name, field type) sets across crates; wire fixture per \
                     group is authored at spec time and proven by `wire-check` (rf-2) before \
                     any apply"
                .to_string(),
            sites: loser_types,
            site_unit: "loser type definitions",
        });
        let est_lines = loser_types as f64 * LINES_PER_DUP_TYPE;
        let chunks = (est_lines / DELETION_LINES_PER_CHUNK).max(1.0);
        schedule.push(ScheduleRow {
            rank: 0,
            item: "merge-shape: exact duplicate shapes".to_string(),
            kind: "merge-shape",
            sites: loser_types,
            site_unit: "loser type definitions",
            est_chunks: chunks,
            chunk_basis: "factory-deletion bar: 10k net lines/session at ~40 lines/type",
            sites_per_chunk: loser_types as f64 / chunks,
        });
    }
    if near > 0 {
        verdicts.push(ItemVerdict {
            item: format!("{near} near-shape groups (score >= 0.50, < 1.0)"),
            kind: "merge-shape",
            verdict: "NEEDS-ADJUDICATION",
            reason: "same-concept-or-not is a semantic call — seat/residue ensemble, item by \
                     item; not schedulable in bulk"
                .to_string(),
            sites: near,
            site_unit: "groups",
        });
    }

    // Kind 3: duplicate NAMES that another crate can actually reach.
    let defs = type_defs(&symbols, &scope);
    let reached = cross_crate_reached(&defs, &refs, &scope);
    let names = name_census(&defs, &reached, &scope, false);
    let reachable: Vec<_> = names.rows.iter().filter(|r| r.is_reachable()).collect();
    if !reachable.is_empty() {
        let loser_defs: usize = reachable.iter().map(|r| r.defs.len() - 1).sum();
        let mut top: Vec<String> = reachable
            .iter()
            .take(limit)
            .map(|r| format!("{} (x{})", r.name, r.defs.len()))
            .collect();
        if reachable.len() > limit {
            top.push(format!("… +{}", reachable.len() - limit));
        }
        verdicts.push(ItemVerdict {
            item: format!(
                "{} reachable duplicate names: {}",
                reachable.len(),
                top.join(", ")
            ),
            kind: "delete-loser",
            verdict: "ELIGIBLE-PENDING-FIXTURE",
            reason: "each name is defined in >=2 crates and already referenced across a crate \
                     boundary — adoption can retire the losers. Winner choice per name is a \
                     `converge noun` dossier read, then a delete-loser spec."
                .to_string(),
            sites: loser_defs,
            site_unit: "loser definitions",
        });
        let est_lines = loser_defs as f64 * LINES_PER_DUP_TYPE;
        let chunks = (est_lines / DELETION_LINES_PER_CHUNK).max(1.0);
        schedule.push(ScheduleRow {
            rank: 0,
            item: "delete-loser: reachable duplicate names".to_string(),
            kind: "delete-loser",
            sites: loser_defs,
            site_unit: "loser definitions",
            est_chunks: chunks,
            chunk_basis: "factory-deletion bar: 10k net lines/session at ~40 lines/type",
            sites_per_chunk: loser_defs as f64 / chunks,
        });
    }
}

async fn behaviour_kind(
    index_path: &Path,
    corpus: &str,
    verdicts: &mut Vec<ItemVerdict>,
    schedule: &mut Vec<ScheduleRow>,
    _limit: usize,
) {
    use sovereign_tools::code::dry_report::{
        build_dry_report, DryInputs, DEFAULT_MIN_LINES, DEFAULT_NEAR_THRESHOLD,
    };
    let report = match build_dry_report(DryInputs {
        index_path,
        corpus_id: corpus,
        min_lines: DEFAULT_MIN_LINES,
        near_threshold: DEFAULT_NEAR_THRESHOLD,
        scope: None,
    })
    .await
    {
        Ok(r) => r,
        Err(e) => {
            verdicts.push(ItemVerdict {
                item: "dry-report".to_string(),
                kind: "behaviour",
                verdict: "COULD-NOT-JUDGE",
                reason: format!("dry-report failed: {e}"),
                sites: 0,
                site_unit: "",
            });
            return;
        }
    };
    let exact = report.exact_clones.len();
    let copies: usize = report
        .exact_clones
        .iter()
        .map(|c| c.members.len() - 1)
        .sum();
    let near = report.near_clusters.len();
    if exact > 0 {
        verdicts.push(ItemVerdict {
            item: format!(
                "{exact} exact-clone groups ({copies} redundant copies, ~{} redundant lines)",
                report.estimated_redundant_lines
            ),
            kind: "behaviour",
            verdict: "ELIGIBLE",
            reason: "byte-identical normalized bodies — mechanical dedupe; survivor \
                     visibility is the only per-group check"
                .to_string(),
            sites: copies,
            site_unit: "redundant copies",
        });
        let chunks = (report.estimated_redundant_lines as f64 / DELETION_LINES_PER_CHUNK).max(1.0);
        schedule.push(ScheduleRow {
            rank: 0,
            item: "behaviour: exact clone dedupe".to_string(),
            kind: "behaviour",
            sites: copies,
            site_unit: "redundant copies",
            est_chunks: chunks,
            chunk_basis: "factory-deletion bar over dry-report's own redundant-line estimate",
            sites_per_chunk: copies as f64 / chunks,
        });
    }
    if near > 0 {
        verdicts.push(ItemVerdict {
            item: format!("{near} near-clone clusters (cosine >= {DEFAULT_NEAR_THRESHOLD})"),
            kind: "behaviour",
            verdict: "NEEDS-ADJUDICATION",
            reason: "near is not identical — each cluster is a factor-out decision, not a \
                     mechanical dedupe"
                .to_string(),
            sites: near,
            site_unit: "clusters",
        });
    }
}

fn load_specs(root: &Path) -> Vec<RefactorSpec> {
    let dir = root.join("quality/refactors");
    let mut specs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) == Some("toml") {
                match RefactorSpec::load(&p) {
                    Ok(s) => specs.push(s),
                    Err(err) => eprintln!("warning: skipping {}: {err}", p.display()),
                }
            }
        }
    }
    specs.sort_by(|a, b| a.id.cmp(&b.id));
    specs
}

fn pascal_case(snake: &str) -> String {
    snake
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn render(
    verdicts: &[ItemVerdict],
    schedule: &[ScheduleRow],
    notes: &[String],
    atom_tail: usize,
    limit: usize,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "ENTRY GATE — five work-table kinds, verdict per item");
    let _ = writeln!(
        out,
        "(representation · wire+fixture · trait surface · fallibility; an item failing the \
         wire check is filed, never scheduled)"
    );
    let _ = writeln!(out);

    for kind in [
        "field-atom",
        "merge-shape",
        "delete-loser",
        "behaviour",
        "adopt-api",
    ] {
        let group: Vec<&ItemVerdict> = verdicts.iter().filter(|v| v.kind == kind).collect();
        if group.is_empty() {
            continue;
        }
        let _ = writeln!(out, "[{kind}]");
        for (n, v) in group.iter().enumerate() {
            if n >= limit && kind == "field-atom" {
                let _ = writeln!(
                    out,
                    "  … and {} more item(s) (--limit 0 for all)",
                    group.len() - n
                );
                break;
            }
            let _ = writeln!(
                out,
                "  {:<24} {:>6} {}  — {}",
                v.verdict,
                v.sites,
                if v.site_unit.is_empty() {
                    "items"
                } else {
                    v.site_unit
                },
                v.item
            );
            let _ = writeln!(out, "  {:<24} {}", "", v.reason);
        }
        let _ = writeln!(out);
    }
    if atom_tail > 0 {
        let _ = writeln!(
            out,
            "field-atom tail: {atom_tail} atom(s) below the {ATOM_DECL_FLOOR}-declaration floor — \
             not adjudicated this pass"
        );
        let _ = writeln!(out);
    }
    for n in notes {
        let _ = writeln!(out, "instrument: {n}");
    }
    if !notes.is_empty() {
        let _ = writeln!(out);
    }

    let _ = writeln!(
        out,
        "SCHEDULE — ranked by sites-per-session-chunk, largest first"
    );
    let _ = writeln!(
        out,
        "(chunk divisors are declared estimates until a rung of that kind lands; the basis \
         is printed with each row)"
    );
    for row in schedule {
        let _ = writeln!(
            out,
            "  {}. {:<44} {:>6} {} / {:.1} chunk(s) = {:>7.0} per chunk",
            row.rank, row.item, row.sites, row.site_unit, row.est_chunks, row.sites_per_chunk
        );
        let _ = writeln!(out, "     basis: {}", row.chunk_basis);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_text_and_enum_policies_do_not_overlap() {
        for a in ENUM_CANDIDATE_ATOMS {
            assert!(!OPEN_TEXT_ATOMS.contains(a), "{a} cannot be both");
        }
    }

    #[test]
    fn pascal_case_maps_id_atoms_to_type_names() {
        assert_eq!(pascal_case("corpus_id"), "CorpusId");
        assert_eq!(pascal_case("node_id"), "NodeId");
        assert_eq!(pascal_case("session_id"), "SessionId");
    }
}
