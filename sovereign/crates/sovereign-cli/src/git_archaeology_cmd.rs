//! `sovereign git-archaeology <code-corpus> [--source-path <path>] [--output <md>]`
//!
//! Walks the code corpus' git history once and produces a temporal
//! enrichment sidecar: per-atom provenance (first-seen / last-modified
//! / stability / authors / staleness) plus co-evolution edges between
//! files that always change together. JSON sidecar carries the full
//! per-atom + per-pair detail; markdown digest is the human surface
//! and the input the drift-report renderer folds in.
//!
//! Standalone command + the workhorse for `sovereign drift detect`'s
//! Step 3.5. Mirrors `rough_edges_cmd.rs` line-for-line in shape.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use corpus_engine::enrichment::atlas::{read_atlas_atoms, AtomEnvelope};
use corpus_engine_archaeology::git_archaeology::{
    batch_harvest_all_commits, compute_co_evolution, discover_repo_root, enrich_atom,
    source_to_repo_relative, AtomProvenance, GitArchaeologyReport, Staleness, StalenessSummary,
};

const DEFAULT_THRESHOLD: f32 = 0.5;
const DEFAULT_MIN_JOINT: u32 = 5;
/// How many of each list to render in the markdown digest. The full
/// list always lives in the JSON sidecar.
const DIGEST_TOP_N: usize = 10;

pub async fn run(args: &[String]) -> i32 {
    if crate::util::help::wants_help(args) {
        crate::util::help::print(&HELP);
        return 0;
    }
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };

    let source_corpus_id = parsed.resolved_source_corpus_id();
    let source_path = match resolve_source_path(&parsed, &source_corpus_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    println!("=== sovereign git-archaeology ===");
    println!("  atlas        = {}", parsed.atlas_corpus_id);
    println!("  source corpus= {}", source_corpus_id);
    println!("  source       = {}", source_path.display());
    if let Some(o) = &parsed.output {
        println!("  output       = {}", o.display());
    } else {
        println!("  output       = <stdout>");
    }
    println!("  threshold    = {:.2}", parsed.threshold);
    println!("  min-joint    = {}", parsed.min_joint_commits);
    println!();

    // ── Step 1: walk git ───────────────────────────────────────
    let repo_root = match discover_repo_root(&source_path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("✗ git: {e}");
            eprintln!(
                "  hint: source_path must live inside a git repository. \
                 If your corpus was indexed from a non-versioned tree, \
                 archaeology has nothing to anchor on — skip this step."
            );
            return 1;
        }
    };
    println!("  repo root    = {}", repo_root.display());

    let history = match batch_harvest_all_commits(&repo_root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("✗ git log: {e}");
            return 1;
        }
    };
    println!("  · harvested {} files of git history", history.len());

    // ── Step 2: chunk_id → file_path map ───────────────────────
    let chunk_path_map = match build_chunk_path_map(&source_corpus_id).await {
        Ok(m) => m,
        Err(e) => {
            eprintln!("✗ chunk index: {e}");
            return 1;
        }
    };
    println!(
        "  · resolved {} code chunks → file paths",
        chunk_path_map.len()
    );

    // ── Step 3: load atoms + atlas mtime ───────────────────────
    let atlas_dir = atlas_dir_for(&parsed.atlas_corpus_id);
    let atoms_file = match read_atlas_atoms(&atlas_dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "✗ read {}: {e}",
                atlas_dir.join("atoms.json").display()
            );
            eprintln!(
                "  hint: build the structural atlas first via `sovereign enrich ingest \
                 {} --source-corpus {}`.",
                parsed.atlas_corpus_id, source_corpus_id
            );
            return 1;
        }
    };
    let atlas_built_at = atlas_built_timestamp(&atlas_dir).unwrap_or(0);
    println!(
        "  · loaded {} atoms (atlas built {})",
        atoms_file.atoms.len(),
        format_iso_date(atlas_built_at)
    );

    // ── Step 4: per-atom enrichment ────────────────────────────
    let mut provenance: Vec<AtomProvenance> = Vec::new();
    let mut atoms_without_path: usize = 0;
    let mut atoms_without_history: usize = 0;
    for atom in &atoms_file.atoms {
        let Some(chunk_id) = anchor_chunk_id(atom) else {
            atoms_without_path += 1;
            continue;
        };
        let Some(file_path) = chunk_path_map.get(chunk_id) else {
            // Wikipedia atoms or chunks whose metadata didn't carry a
            // file_path. Counted but silently skipped — archaeology is
            // a code-corpus enrichment.
            atoms_without_path += 1;
            continue;
        };
        let lifted = source_to_repo_relative(&source_path, &repo_root, file_path);
        match enrich_atom(atom.id().as_str(), &lifted, &history, atlas_built_at) {
            Some(p) => provenance.push(p),
            None => atoms_without_history += 1,
        }
    }
    println!(
        "  · enriched {} atoms ({} skipped: no path, {} skipped: no history)",
        provenance.len(),
        atoms_without_path,
        atoms_without_history
    );

    // ── Step 5: co-evolution edges ─────────────────────────────
    let co_evolution =
        compute_co_evolution(&history, parsed.threshold, parsed.min_joint_commits);
    println!(
        "  · {} co-evolution pairs (threshold={:.2}, min_joint={})",
        co_evolution.len(),
        parsed.threshold,
        parsed.min_joint_commits
    );

    // ── Step 6: assemble report + write outputs ───────────────
    let staleness_summary = summarise_staleness(&provenance);
    let report = GitArchaeologyReport {
        corpus_id: parsed.atlas_corpus_id.clone(),
        repo_root: repo_root.clone(),
        atlas_built_at,
        atom_count: atoms_file.atoms.len(),
        atoms_with_history: provenance.len(),
        follows_renames: false,
        provenance,
        co_evolution,
        staleness_summary,
    };

    let md = render_markdown(&report);

    let json_sidecar_default = atlas_dir.join("git_archaeology.json");
    let (json_path, md_target) = if let Some(out) = &parsed.output {
        let json = sidecar_json_path(out);
        (json, Some(out.clone()))
    } else {
        (json_sidecar_default.clone(), None)
    };

    if let Some(parent) = json_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json_body = match serde_json::to_string_pretty(&report) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("✗ serialise sidecar: {e}");
            return 1;
        }
    };
    if let Err(e) = std::fs::write(&json_path, json_body) {
        eprintln!("✗ write {}: {e}", json_path.display());
        return 1;
    }
    println!("  ✓ wrote {}", json_path.display());

    match md_target {
        Some(path) => {
            if let Err(e) = std::fs::write(&path, &md) {
                eprintln!("✗ write {}: {e}", path.display());
                return 1;
            }
            println!("  ✓ wrote {}", path.display());
        }
        None => {
            print!("{md}");
        }
    }

    0
}

// ── Argument parsing ─────────────────────────────────────────

#[derive(Default)]
struct Args {
    /// Where atoms.json lives. Typically `<id>-self-atlas`.
    atlas_corpus_id: String,
    /// Where chunks live (`chunks.lance`) and where source_path is
    /// stamped in `_corpus_meta.json`. Defaults to
    /// `atlas_corpus_id` minus a trailing `-self-atlas` suffix.
    source_corpus_id: Option<String>,
    source_path: Option<PathBuf>,
    output: Option<PathBuf>,
    threshold: f32,
    min_joint_commits: u32,
}

impl Args {
    /// Resolve the source-corpus id from explicit `--source-corpus`
    /// or by stripping `-self-atlas` from the atlas id.
    fn resolved_source_corpus_id(&self) -> String {
        if let Some(s) = &self.source_corpus_id {
            return s.clone();
        }
        self.atlas_corpus_id
            .strip_suffix("-self-atlas")
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.atlas_corpus_id.clone())
    }
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        threshold: DEFAULT_THRESHOLD,
        min_joint_commits: DEFAULT_MIN_JOINT,
        ..Args::default()
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source-corpus" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--source-corpus requires a value")?;
                out.source_corpus_id = Some(v.clone());
                i += 2;
            }
            "--source-path" => {
                let v = args
                    .get(i + 1)
                    .ok_or("--source-path requires a value")?;
                out.source_path = Some(PathBuf::from(v));
                i += 2;
            }
            "--output" => {
                let v = args.get(i + 1).ok_or("--output requires a value")?;
                out.output = Some(PathBuf::from(v));
                i += 2;
            }
            "--threshold" => {
                let v = args.get(i + 1).ok_or("--threshold requires a value")?;
                out.threshold = v
                    .parse::<f32>()
                    .map_err(|e| format!("--threshold `{v}`: {e}"))?;
                if !(0.0..=1.0).contains(&out.threshold) {
                    return Err("--threshold must be in [0.0, 1.0]".into());
                }
                i += 2;
            }
            "--min-joint" => {
                let v = args.get(i + 1).ok_or("--min-joint requires a value")?;
                out.min_joint_commits = v
                    .parse::<u32>()
                    .map_err(|e| format!("--min-joint `{v}`: {e}"))?;
                i += 2;
            }
            s if !s.starts_with("--") && out.atlas_corpus_id.is_empty() => {
                out.atlas_corpus_id = s.to_string();
                i += 1;
            }
            other => return Err(format!("unrecognised argument: {other}")),
        }
    }
    if out.atlas_corpus_id.is_empty() {
        return Err(
            "missing positional <atlas-corpus-id>. usage: sovereign git-archaeology \
             <atlas-corpus-id> [--source-corpus <id>] [--source-path <path>] \
             [--output <md>] [--threshold N] [--min-joint N]"
                .into(),
        );
    }
    Ok(out)
}

// ── Resolution helpers ───────────────────────────────────────

/// Source path comes from one of:
/// 1. Explicit `--source-path <path>` (highest priority).
/// 2. The corpus's `_corpus_meta.json` `source_path` field (set by
///    `sovereign code index` for code corpora).
/// 3. Fall back to error if neither is present.
///
/// Mirrors [`crate::rough_edges_cmd::resolve_source_path`] — the two
/// commands need the same lookup but each owns its own struct shape,
/// and lifting this into a shared util is one refactor too many for
/// v1.
fn resolve_source_path(args: &Args, source_corpus_id: &str) -> Result<PathBuf, String> {
    if let Some(p) = &args.source_path {
        if !p.exists() {
            return Err(format!(
                "--source-path {} does not exist",
                p.display()
            ));
        }
        return Ok(p.clone());
    }
    let canonical_meta = home_dir()
        .join(".sovereign/indexes")
        .join(source_corpus_id)
        .join("_corpus_meta.json");
    let partition_meta = home_dir()
        .join(".sovereign/indexes")
        .join(format!("{source_corpus_id}-partition-local"))
        .join("_corpus_meta.json");
    let meta_path = if canonical_meta.exists() {
        canonical_meta
    } else if partition_meta.exists() {
        partition_meta
    } else {
        return Err(format!(
            "corpus '{source_corpus_id}' not found at {} (or {}) — run `sovereign code index` \
             first or pass --source-path",
            canonical_meta.display(),
            partition_meta.display()
        ));
    };
    let raw = std::fs::read_to_string(&meta_path)
        .map_err(|e| format!("read {}: {e}", meta_path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse {}: {e}", meta_path.display()))?;
    let s = v
        .get("source_path")
        .and_then(|x| x.as_str())
        .ok_or_else(|| {
            format!(
                "corpus '{source_corpus_id}' has no source_path stamped — pass --source-path \
                 explicitly. (Only code-corpus installs from `sovereign code \
                 index` stamp a source_path.)"
            )
        })?;
    let p = PathBuf::from(s);
    if !p.exists() {
        return Err(format!(
            "stamped source_path {} no longer exists — pass --source-path",
            p.display()
        ));
    }
    Ok(p)
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

fn atlas_dir_for(corpus_id: &str) -> PathBuf {
    home_dir()
        .join(".sovereign/indexes")
        .join(corpus_id)
        .join("atlas")
}

fn sidecar_json_path(md: &Path) -> PathBuf {
    let mut p = md.to_path_buf();
    p.set_extension("json");
    p
}

/// Atom anchor: pick the `chunk_id` that ties this atom to a specific
/// chunk row (and therefore a file path).
///
/// - Entity → `first_appearance.chunk_id`
/// - Anything else → first chunk in `evidence`, or `None` if empty
fn anchor_chunk_id(atom: &AtomEnvelope) -> Option<&str> {
    use AtomEnvelope::*;
    match atom {
        Entity(e) => Some(e.first_appearance.chunk_id.as_str()),
        Event(a) => a.evidence.first().map(|c| c.chunk_id.as_str()),
        State(a) => a.evidence.first().map(|c| c.chunk_id.as_str()),
        Relation(a) => a.evidence.first().map(|c| c.chunk_id.as_str()),
        Claim(a) => a.evidence.first().map(|c| c.chunk_id.as_str()),
        Question(a) => a.raised_at.first().map(|c| c.chunk_id.as_str()),
        Configuration(a) => a.evidence.first().map(|c| c.chunk_id.as_str()),
        ArgumentReconstruction(a) => a.evidence.first().map(|c| c.chunk_id.as_str()),
        Position(p) => Some(p.first_appearance.chunk_id.as_str()),
        Opposition(o) => Some(o.first_appearance.chunk_id.as_str()),
    }
}

/// Open the corpus index and walk every chunk, building a map from
/// the **stringified chunk id** (which is what `ChunkRef.chunk_id`
/// stores for code atoms — see `code_walk.rs:650`) to the chunk's
/// `file_path` taken from `metadata_raw`.
async fn build_chunk_path_map(corpus_id: &str) -> Result<HashMap<String, PathBuf>, String> {
    let canonical = home_dir().join(".sovereign/indexes").join(corpus_id);
    let partition = home_dir()
        .join(".sovereign/indexes")
        .join(format!("{corpus_id}-partition-local"));
    // Use `_corpus_meta.json` as the existence marker — the bare `<id>`
    // directory is sometimes pre-created for SCIP graph DB even when
    // the corpus chunks live under `-partition-local`.
    let index_dir = if canonical.join("_corpus_meta.json").exists() {
        canonical
    } else if partition.join("_corpus_meta.json").exists() {
        partition
    } else {
        return Err(format!(
            "no chunk index for corpus '{corpus_id}' at {} (or partition-local sibling) — \
             run `sovereign code index <path> --corpus-id {corpus_id}` first",
            home_dir().join(".sovereign/indexes").display()
        ));
    };
    let index = corpus_engine::CorpusIndex::open(&index_dir)
        .await
        .map_err(|e| format!("open {}: {e}", index_dir.display()))?;
    let chunks = index
        .all_chunks_full()
        .await
        .map_err(|e| format!("read chunks: {e}"))?;

    let mut map = HashMap::with_capacity(chunks.len());
    for row in chunks {
        let Some(meta_raw) = row.metadata_raw.as_deref() else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(meta_raw) else {
            continue;
        };
        let Some(file_path) = meta.get("file_path").and_then(|v| v.as_str()) else {
            // Wikipedia chunks land here — no file_path key.
            continue;
        };
        map.insert(row.id.to_string(), PathBuf::from(file_path));
    }
    Ok(map)
}

fn atlas_built_timestamp(atlas_dir: &Path) -> Option<i64> {
    let path = atlas_dir.join("atoms.json");
    let meta = std::fs::metadata(&path).ok()?;
    let mtime = meta.modified().ok()?;
    let dur = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dur.as_secs() as i64)
}

fn summarise_staleness(provenance: &[AtomProvenance]) -> StalenessSummary {
    let mut s = StalenessSummary::default();
    for p in provenance {
        match p.staleness {
            Staleness::Fresh => s.fresh += 1,
            Staleness::Moved => s.moved += 1,
        }
    }
    s
}

fn format_iso_date(ts: i64) -> String {
    if ts <= 0 {
        return "<unknown>".into();
    }
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| format!("unix:{ts}"))
}

// ── Markdown rendering ───────────────────────────────────────

fn render_markdown(report: &GitArchaeologyReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Git Archaeology — `{}`\n\n",
        report.corpus_id
    ));
    out.push_str(&format!(
        "*{} of {} atoms enriched · {} fresh / {} moved · {} co-evolution pairs*\n\n",
        report.atoms_with_history,
        report.atom_count,
        report.staleness_summary.fresh,
        report.staleness_summary.moved,
        report.co_evolution.len(),
    ));
    out.push_str(&format!("Repo: `{}`\n", report.repo_root.display()));
    out.push_str(&format!(
        "Atlas built: {}\n",
        format_iso_date(report.atlas_built_at)
    ));
    if !report.follows_renames {
        out.push_str(
            "_Renames not followed in v1 — files moved across history surface as \
             two distinct atoms._\n",
        );
    }
    out.push('\n');

    // ── Stability highlights ───────────────────────────────────
    out.push_str("## Stability highlights\n\n");
    out.push_str(
        "_Oldest unchanged code in the atlas — load-bearing architectural commitments._\n\n",
    );
    let mut by_stability: Vec<&AtomProvenance> = report
        .provenance
        .iter()
        .filter(|p| matches!(p.staleness, Staleness::Fresh))
        .collect();
    by_stability.sort_by(|a, b| b.stability_days.cmp(&a.stability_days));
    if by_stability.is_empty() {
        out.push_str("_(none)_\n\n");
    } else {
        for p in by_stability.iter().take(DIGEST_TOP_N) {
            out.push_str(&format!(
                "- `{}` ({}, {} days, {} commits, primary: {})\n",
                p.file_path.display(),
                p.atom_id,
                p.stability_days,
                p.modification_count,
                p.primary_authors.join(", "),
            ));
        }
        out.push('\n');
    }

    // ── Recent volatility ──────────────────────────────────────
    out.push_str("## Recent volatility\n\n");
    out.push_str(
        "_Most-recently-modified atoms — currently active surfaces in the codebase._\n\n",
    );
    let mut by_recency: Vec<&AtomProvenance> = report.provenance.iter().collect();
    by_recency.sort_by(|a, b| b.last_modified.date_iso.cmp(&a.last_modified.date_iso));
    if by_recency.is_empty() {
        out.push_str("_(none)_\n\n");
    } else {
        for p in by_recency.iter().take(DIGEST_TOP_N) {
            out.push_str(&format!(
                "- `{}` ({}, last touched {} by {} — \"{}\")\n",
                p.file_path.display(),
                p.atom_id,
                p.last_modified.date_iso,
                p.last_modified.author_email,
                truncate(&p.last_modified.subject, 60),
            ));
        }
        out.push('\n');
    }

    // ── Co-evolution clusters ──────────────────────────────────
    out.push_str("## Co-evolution clusters\n\n");
    out.push_str(
        "_Files that change together — implicit coupling the code's syntactic \
         structure doesn't reveal._\n\n",
    );
    if report.co_evolution.is_empty() {
        out.push_str("_(none above threshold)_\n\n");
    } else {
        for pair in report.co_evolution.iter().take(DIGEST_TOP_N) {
            out.push_str(&format!(
                "- `{}` ↔ `{}` ({:.0}% — {} joint of {} touching either)\n",
                pair.file_a.display(),
                pair.file_b.display(),
                pair.correlation * 100.0,
                pair.joint_commits,
                pair.joint_commits + pair.a_only + pair.b_only,
            ));
        }
        out.push('\n');
    }

    // ── Staleness queue ────────────────────────────────────────
    if report.staleness_summary.moved > 0 {
        out.push_str("## Staleness queue\n\n");
        out.push_str(&format!(
            "_{} atoms anchored to code that has been modified since the atlas \
             was built. These are the candidates for re-extraction or LLM \
             re-validation._\n\n",
            report.staleness_summary.moved
        ));
        let moved: Vec<&AtomProvenance> = report
            .provenance
            .iter()
            .filter(|p| matches!(p.staleness, Staleness::Moved))
            .collect();
        for p in moved.iter().take(DIGEST_TOP_N) {
            out.push_str(&format!(
                "- `{}` ({}, last touched {})\n",
                p.file_path.display(),
                p.atom_id,
                p.last_modified.date_iso,
            ));
        }
        if moved.len() > DIGEST_TOP_N {
            out.push_str(&format!(
                "- *…and {} more (see JSON sidecar)*\n",
                moved.len() - DIGEST_TOP_N
            ));
        }
        out.push('\n');
    }

    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign git-archaeology",
    summary: "Walk a code corpus' git history and emit per-atom provenance + co-evolution edges.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign git-archaeology <corpus-id> [--source-path <dir>] [--output <md>] \
             [--threshold N] [--min-joint N]",
        ),
        crate::util::help::HelpSection::Flags(&[
            (
                "--source-path <dir>",
                "Override the source path stamped in the corpus's _corpus_meta.json. \
                 Must live inside a git repository.",
            ),
            (
                "--output <md>",
                "Write the markdown digest to this path; the JSON sidecar lands at <output>.json. \
                 Default: print markdown to stdout, write sidecar to \
                 ~/.sovereign/indexes/<corpus>/atlas/git_archaeology.json.",
            ),
            (
                "--threshold N",
                "Co-evolution jaccard threshold in [0.0, 1.0]. Default 0.5 — half of all commits \
                 touching either file must touch both for the pair to register.",
            ),
            (
                "--min-joint N",
                "Minimum joint-commit count for a co-evolution pair. Default 5. Drops the \
                 scaffolding-era false positives where two files were edited together once \
                 in the initial commit and never again.",
            ),
        ]),
        crate::util::help::HelpSection::Notes(
            "Reads the structural atlas from ~/.sovereign/indexes/<corpus>/atlas/atoms.json. \
             Build it first via `sovereign enrich ingest <id> --source-corpus <id>` if you \
             haven't. Standalone surface; also called from `sovereign drift detect` to fold \
             provenance into the unified drift digest.",
        ),
    ],
};
