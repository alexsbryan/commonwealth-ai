//! `sovereign archaeology-eval <atlas-corpus> [--inquiry <toml>...] [--baseline <path>]`
//!
//! Witness-checks-and-baseline-diff eval for the
//! [`git_archaeology`] sidecar. Run-measure-iterate-improve loop:
//!
//! 1. Run archaeology (today: `sovereign git-archaeology …`).
//! 2. Run this — get a witness rate, fabrication count, inquiry
//!    verdicts, and a diff vs. last saved baseline.
//! 3. Save the current run as the new baseline (`--save-baseline`)
//!    when you're confident it's an improvement.
//! 4. Iterate the archaeology prompt / threshold / model and re-run
//!    eval. Watch the CSV trend climb.
//!
//! Inquiries (`--inquiry path/to/foo.toml`) are how you teach the
//! eval what "good" looks like for cases you've manually verified.
//! Each inquiry becomes a permanent regression case.

use std::path::{Path, PathBuf};

use corpus_engine_archaeology::archaeology_eval::{
    diff_against_baseline, parse_inquiry_toml, run_eval, BaselineDiff, EvalReport, Inquiry,
    Verdict, WitnessKind,
};
use corpus_engine_archaeology::git_archaeology::GitArchaeologyReport;

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

    println!("=== sovereign archaeology-eval ===");
    println!("  atlas        = {}", parsed.atlas_corpus_id);
    println!("  inquiries    = {}", parsed.inquiry_paths.len());
    if let Some(b) = &parsed.baseline {
        println!("  baseline     = {}", b.display());
    } else {
        println!("  baseline     = <auto: ~/.sovereign/eval/baselines/{}.eval.json>", parsed.atlas_corpus_id);
    }
    println!();

    // ── Load archaeology sidecar ──────────────────────────────
    let archaeology_path = home_dir()
        .join(".sovereign/indexes")
        .join(&parsed.atlas_corpus_id)
        .join("atlas")
        .join("git_archaeology.json");
    let archaeology = match load_archaeology(&archaeology_path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("✗ {e}");
            eprintln!(
                "  hint: run `sovereign git-archaeology {}` first.",
                parsed.atlas_corpus_id
            );
            return 1;
        }
    };
    println!(
        "  · loaded {} provenance entries from {}",
        archaeology.provenance.len(),
        archaeology_path.display(),
    );

    // ── Load inquiries ────────────────────────────────────────
    let mut inquiries = match load_inquiries(&parsed.inquiry_paths) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("✗ {e}");
            return 1;
        }
    };
    if let Some(dir) = &parsed.inquiries_dir {
        match corpus_engine_archaeology::archaeology_eval::load_inquiries_from_dir(dir) {
            Ok(mut from_dir) => {
                println!(
                    "  · loaded {} inquiries from {}",
                    from_dir.len(),
                    dir.display()
                );
                inquiries.append(&mut from_dir);
            }
            Err(e) => {
                eprintln!("✗ {}: {e}", dir.display());
                return 1;
            }
        }
    }
    if !inquiries.is_empty() {
        println!("  · {} inquiries total", inquiries.len());
    }

    // ── Run eval ──────────────────────────────────────────────
    let report = match run_eval(
        &parsed.atlas_corpus_id,
        &archaeology.repo_root,
        &archaeology.provenance,
        &inquiries,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("✗ run_eval: {e}");
            return 1;
        }
    };
    println!(
        "  · {} atoms · {:.0}% witness rate · {} fabricated",
        report.atom_count,
        report.witness_rate * 100.0,
        report.fabricated_atoms,
    );

    // ── Diff against baseline ─────────────────────────────────
    let baseline_path = parsed
        .baseline
        .clone()
        .unwrap_or_else(|| default_baseline_path(&parsed.atlas_corpus_id));
    let diff = match load_baseline(&baseline_path) {
        Some(prev) => {
            let d = diff_against_baseline(&report, &prev);
            println!(
                "  · vs baseline: +{} added, -{} removed, {} score-changed, Δrate {:+.2}%",
                d.added.len(),
                d.removed.len(),
                d.score_changes.len(),
                d.witness_rate_delta * 100.0,
            );
            Some(d)
        }
        None => {
            println!(
                "  · no baseline found at {} — run with --save-baseline to seed one",
                baseline_path.display()
            );
            None
        }
    };

    // ── Render report ─────────────────────────────────────────
    let md = render_markdown(&report, diff.as_ref());

    let output_md = parsed
        .output
        .clone()
        .unwrap_or_else(|| home_dir().join(".sovereign/eval").join(format!("{}.eval.md", parsed.atlas_corpus_id)));
    if let Some(parent) = output_md.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&output_md, &md) {
        eprintln!("✗ write {}: {e}", output_md.display());
        return 1;
    }
    println!("  ✓ wrote {}", output_md.display());

    // ── Append CSV history ────────────────────────────────────
    let history_path = home_dir().join(".sovereign/eval/history.csv");
    if let Err(e) = append_history_row(&history_path, &report, diff.as_ref()) {
        eprintln!("⚠ history append failed (non-fatal): {e}");
    } else {
        println!("  ✓ appended {}", history_path.display());
    }

    // ── Save baseline ────────────────────────────────────────
    if parsed.save_baseline {
        if let Some(parent) = baseline_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&report) {
            Ok(body) => match std::fs::write(&baseline_path, body) {
                Ok(()) => println!("  ✓ saved baseline → {}", baseline_path.display()),
                Err(e) => eprintln!("⚠ save baseline {}: {e}", baseline_path.display()),
            },
            Err(e) => eprintln!("⚠ serialise baseline: {e}"),
        }
    }

    // ── Exit code ────────────────────────────────────────────
    // Two modes:
    //   - Default (no flag): non-zero on absolute failure (any
    //     inquiry failing OR any fabricated atom). Suitable when
    //     you want a hard floor.
    //   - `--gate-on-baseline`: non-zero only when this run is
    //     *worse* than the baseline (fewer passing inquiries OR
    //     more fabricated atoms). Pre-existing failures are
    //     tolerated. Used by the pre-push ratchet so commits that
    //     don't introduce new regressions can still land.
    let curr_passing = report.inquiry_verdicts.iter().filter(|v| v.passing).count();
    let curr_fab = report.fabricated_atoms;
    let any_inquiry_failed = curr_passing < report.inquiry_verdicts.len();
    let baseline_loaded = load_baseline(&baseline_path);

    if parsed.gate_on_baseline {
        let Some(b) = &baseline_loaded else {
            // No baseline yet — treat as a fresh seed; pass.
            return 0;
        };
        let prev_passing = b.inquiry_verdicts.iter().filter(|v| v.passing).count();
        let prev_fab = b.fabricated_atoms;
        let mut regressed = false;
        if curr_passing < prev_passing {
            eprintln!(
                "\n✗ regression: {} → {} inquiries passing (Δ {})",
                prev_passing,
                curr_passing,
                curr_passing as i64 - prev_passing as i64,
            );
            for v in &report.inquiry_verdicts {
                if !v.passing
                    && b.inquiry_verdicts
                        .iter()
                        .any(|pv| pv.inquiry_id == v.inquiry_id && pv.passing)
                {
                    eprintln!("    newly failing: `{}`", v.inquiry_id);
                }
            }
            regressed = true;
        }
        if curr_fab > prev_fab {
            eprintln!(
                "\n✗ regression: {} → {} fabricated atoms (Δ +{})",
                prev_fab,
                curr_fab,
                curr_fab - prev_fab,
            );
            regressed = true;
        }
        return if regressed { 1 } else { 0 };
    }

    if curr_fab > 0 || any_inquiry_failed {
        eprintln!();
        if curr_fab > 0 {
            eprintln!("✗ {curr_fab} fabricated atoms detected");
        }
        for v in &report.inquiry_verdicts {
            if !v.passing {
                eprintln!("✗ inquiry `{}` failed", v.inquiry_id);
            }
        }
        return 1;
    }
    0
}

// ── Args ─────────────────────────────────────────────────────

#[derive(Default)]
struct Args {
    atlas_corpus_id: String,
    inquiry_paths: Vec<PathBuf>,
    /// When set, every `*.toml` under the directory is loaded as an
    /// inquiry alongside any `--inquiry` paths. Lets the pre-push
    /// ratchet hook be a one-liner.
    inquiries_dir: Option<PathBuf>,
    baseline: Option<PathBuf>,
    output: Option<PathBuf>,
    save_baseline: bool,
    /// Pre-push-style ratchet: exit 0 even when some inquiries fail,
    /// so long as the count of *passing* inquiries hasn't decreased
    /// vs baseline AND fabricated_atoms hasn't increased. Without
    /// this flag the command exits non-zero on any absolute failure.
    gate_on_baseline: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--inquiry" => {
                let v = args.get(i + 1).ok_or("--inquiry requires a value")?;
                out.inquiry_paths.push(PathBuf::from(v));
                i += 2;
            }
            "--inquiries-dir" => {
                let v = args.get(i + 1).ok_or("--inquiries-dir requires a value")?;
                out.inquiries_dir = Some(PathBuf::from(v));
                i += 2;
            }
            "--gate-on-baseline" => {
                out.gate_on_baseline = true;
                i += 1;
            }
            "--baseline" => {
                let v = args.get(i + 1).ok_or("--baseline requires a value")?;
                out.baseline = Some(PathBuf::from(v));
                i += 2;
            }
            "--output" => {
                let v = args.get(i + 1).ok_or("--output requires a value")?;
                out.output = Some(PathBuf::from(v));
                i += 2;
            }
            "--save-baseline" => {
                out.save_baseline = true;
                i += 1;
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
            "missing positional <atlas-corpus-id>. usage: sovereign archaeology-eval \
             <atlas-corpus-id> [--inquiry <toml>...] [--baseline <path>] \
             [--output <md>] [--save-baseline]"
                .into(),
        );
    }
    Ok(out)
}

// ── IO helpers ───────────────────────────────────────────────

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

fn default_baseline_path(atlas: &str) -> PathBuf {
    home_dir()
        .join(".sovereign/eval/baselines")
        .join(format!("{atlas}.eval.json"))
}

fn load_archaeology(path: &Path) -> Result<GitArchaeologyReport, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))
}

fn load_baseline(path: &Path) -> Option<EvalReport> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn load_inquiries(paths: &[PathBuf]) -> Result<Vec<Inquiry>, String> {
    let mut out = Vec::new();
    for p in paths {
        let raw = std::fs::read_to_string(p)
            .map_err(|e| format!("read inquiry {}: {e}", p.display()))?;
        let inq = parse_inquiry_toml(&raw)
            .map_err(|e| format!("parse inquiry {}: {e}", p.display()))?;
        out.push(inq);
    }
    Ok(out)
}

fn append_history_row(
    path: &Path,
    report: &EvalReport,
    diff: Option<&BaselineDiff>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let need_header = !path.exists();
    let inquiries_passing = report
        .inquiry_verdicts
        .iter()
        .filter(|v| v.passing)
        .count();
    let inquiries_total = report.inquiry_verdicts.len();
    let delta = diff.map(|d| d.witness_rate_delta).unwrap_or(0.0);
    let row = format!(
        "{},{},{},{:.4},{},{},{}/{},{:+.4}\n",
        iso_timestamp(report.generated_at),
        report.atlas_corpus_id,
        report.atom_count,
        report.witness_rate,
        report.fabricated_atoms,
        diff.map(|d| d.score_changes.len()).unwrap_or(0),
        inquiries_passing,
        inquiries_total,
        delta,
    );
    let mut existing = if need_header {
        String::from(
            "timestamp,atlas,atoms,witness_rate,fabricated,baseline_score_changes,inquiries_passing,witness_rate_delta\n",
        )
    } else {
        std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?
    };
    existing.push_str(&row);
    std::fs::write(path, existing)
        .map_err(|e| format!("write {}: {e}", path.display()))
}

fn iso_timestamp(ts: i64) -> String {
    use chrono::TimeZone;
    chrono::Utc
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| format!("unix:{ts}"))
}

// ── Markdown rendering ───────────────────────────────────────

const TOP_N: usize = 10;

fn render_markdown(report: &EvalReport, diff: Option<&BaselineDiff>) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Archaeology eval — `{}`\n\n",
        report.atlas_corpus_id
    ));
    out.push_str(&format!(
        "*{} atoms · {:.0}% witness rate · {} fabricated · {} inquiries ({} passing)*\n\n",
        report.atom_count,
        report.witness_rate * 100.0,
        report.fabricated_atoms,
        report.inquiry_verdicts.len(),
        report
            .inquiry_verdicts
            .iter()
            .filter(|v| v.passing)
            .count(),
    ));
    out.push_str(&format!("Repo: `{}`\n\n", report.repo_root.display()));

    // ── Headline counters by witness kind ─────────────────────
    out.push_str("## Witness rollup\n\n");
    let mut by_kind: std::collections::BTreeMap<&'static str, (u32, u32, u32)> =
        std::collections::BTreeMap::new();
    for c in &report.witness_checks {
        let entry = by_kind.entry(witness_label(c.kind)).or_insert((0, 0, 0));
        match c.verdict {
            Verdict::Pass => entry.0 += 1,
            Verdict::Fail => entry.1 += 1,
            Verdict::Stale => entry.2 += 1,
        }
    }
    out.push_str("| Check | Pass | Fail | Stale |\n|---|---:|---:|---:|\n");
    for (label, (p, f, s)) in &by_kind {
        out.push_str(&format!("| {label} | {p} | {f} | {s} |\n"));
    }
    out.push('\n');

    // ── Inquiry verdicts ─────────────────────────────────────
    if !report.inquiry_verdicts.is_empty() {
        out.push_str("## Inquiries\n\n");
        for v in &report.inquiry_verdicts {
            let badge = if v.passing { "✓" } else { "✗" };
            out.push_str(&format!(
                "- {badge} **{}** ({}) — {} matched atom(s) · {:.0}% aggregate score\n",
                v.title,
                v.inquiry_id,
                v.matched_atoms.len(),
                v.aggregate_score * 100.0,
            ));
            for note in v.notes.iter().take(3) {
                out.push_str(&format!("  - _{note}_\n"));
            }
            if v.notes.len() > 3 {
                out.push_str(&format!("  - _…and {} more_\n", v.notes.len() - 3));
            }
        }
        out.push('\n');
    }

    // ── Lowest-score atoms ───────────────────────────────────
    let mut by_score: Vec<&_> = report.atom_witnesses.iter().collect();
    by_score.sort_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.failed.cmp(&a.failed))
    });
    let lowest: Vec<_> = by_score
        .iter()
        .filter(|w| w.failed > 0)
        .take(TOP_N)
        .collect();
    if !lowest.is_empty() {
        out.push_str("## Lowest-witness atoms\n\n");
        for w in &lowest {
            out.push_str(&format!(
                "- `{}` ({}) · score {:.2} ({} pass, {} fail, {} stale)\n",
                w.file_path.display(),
                w.atom_id,
                w.score,
                w.passed,
                w.failed,
                w.stale,
            ));
        }
        out.push('\n');
    }

    // ── Baseline diff ────────────────────────────────────────
    if let Some(d) = diff {
        out.push_str(&format!(
            "## Baseline diff (Δrate {:+.2}%)\n\n",
            d.witness_rate_delta * 100.0
        ));
        out.push_str(&format!(
            "- Added: {} · Removed: {} · Score-changed: {} · Path-changed: {}\n\n",
            d.added.len(),
            d.removed.len(),
            d.score_changes.len(),
            d.path_changes.len(),
        ));
        if !d.score_changes.is_empty() {
            out.push_str("**Score changes (top 10)**\n\n");
            for sc in d.score_changes.iter().take(TOP_N) {
                let arrow = if sc.curr_score >= sc.prev_score {
                    "↑"
                } else {
                    "↓"
                };
                out.push_str(&format!(
                    "- {} {} · {:.2} → {:.2}\n",
                    arrow, sc.atom_id, sc.prev_score, sc.curr_score
                ));
            }
            out.push('\n');
        }
        if !d.path_changes.is_empty() {
            out.push_str("**Path changes**\n\n");
            for pc in d.path_changes.iter().take(TOP_N) {
                out.push_str(&format!(
                    "- {} · `{}` → `{}`\n",
                    pc.atom_id,
                    pc.prev_path.display(),
                    pc.curr_path.display(),
                ));
            }
            out.push('\n');
        }
    }

    out
}

fn witness_label(k: WitnessKind) -> &'static str {
    match k {
        WitnessKind::FirstSeenCommitExists => "first_seen exists",
        WitnessKind::LastModifiedCommitExists => "last_modified exists",
        WitnessKind::FirstSeenTouchesFile => "first_seen touches file",
        WitnessKind::FileExistsAtHead => "file at HEAD",
        WitnessKind::KeywordPresent => "keyword present",
        WitnessKind::AuthorPresent => "author present",
        WitnessKind::DateInRange => "date in range",
    }
}

const HELP: crate::util::help::Help = crate::util::help::Help {
    command: "sovereign archaeology-eval",
    summary: "Eval the git-archaeology sidecar — witness checks, baseline diff, inquiry verdicts.",
    sections: &[
        crate::util::help::HelpSection::Usage(
            "sovereign archaeology-eval <atlas-corpus-id> [--inquiry <toml>...] \
             [--baseline <path>] [--output <md>] [--save-baseline]",
        ),
        crate::util::help::HelpSection::Flags(&[
            (
                "--inquiry <toml>",
                "Path to a curated regression case (TOML). Repeat for multiple. \
                 Each inquiry's `file_globs` selects which atoms it targets, and its \
                 `keywords` / `authors` / `date_range` add inquiry-specific witness checks.",
            ),
            (
                "--baseline <path>",
                "Path to a previous run's eval report (JSON). Default: \
                 ~/.sovereign/eval/baselines/<atlas>.eval.json. The diff section \
                 surfaces atoms added/removed/score-changed since the baseline.",
            ),
            (
                "--output <md>",
                "Where to write the markdown report. Default: \
                 ~/.sovereign/eval/<atlas>.eval.md.",
            ),
            (
                "--save-baseline",
                "After running, save the current report as the new baseline. Use \
                 once per intentional improvement so future runs diff against it.",
            ),
        ]),
        crate::util::help::HelpSection::Notes(
            "Reads `~/.sovereign/indexes/<atlas>/atlas/git_archaeology.json` produced by \
             `sovereign git-archaeology`. Appends one CSV row per run to \
             `~/.sovereign/eval/history.csv` so trends are visible across iterations. \
             Exit code is non-zero when any inquiry fails or any fabrication is detected — \
             CI-friendly.",
        ),
    ],
};
