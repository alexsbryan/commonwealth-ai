// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn bench vault-report` — raw-folder → queryable-enriched-corpus
//! build-time benchmark.
//!
//! `bench book-report` already answers "how long from attaching ONE
//! document to a Ready asset". It cannot answer the question the folder
//! product actually poses: **how long from a folder of notes to a corpus
//! you can ask an enriched question of.** That path is a different
//! pipeline (`LocalCorpusManager::ingest` → `run_folder_tiered_enrichment`)
//! and, until this harness, was almost entirely untimed — the only
//! durable evidence a vault build ever left behind was per-note RAPTOR
//! checkpoint manifests, which cover exactly one of its seven phases.
//!
//! ## What this reports
//!
//! Two headlines, in the order a user experiences them:
//!
//! - **`time_to_rag_ready_ms`** — folder walk → chunks embedded and the
//!   Lance index queryable. After this the corpus answers retrieval
//!   questions; nothing is enriched yet.
//! - **`time_to_enriched_ms`** — everything above, plus per-chunk NER,
//!   per-note RAPTOR trees, motifs, and the vault-wide theme synthesis.
//!   This is the number the enrichment initiative is actually spending
//!   against.
//!
//! Underneath: a phase span table, a per-note table (the shape
//! `svrn enrich raptor` established), and the per-phase token/call
//! ledger from [`resource_meter`](super::resource_meter).
//!
//! ## How it observes without touching the pipeline
//!
//! `ENRICH_TURBO_HANDOFF.md` §2/§6.1 is explicit: use the existing
//! measurement system, and do not scatter `eprintln`s through the
//! enrichment path. This harness adds **zero production-code changes**.
//! Every phase boundary it reports comes from a seam the pipeline
//! already exposes as an injected `Arc<dyn Trait>`:
//!
//! | Phase | Seam |
//! |---|---|
//! | walk / stage / chunk / embed / index / IVF-PQ | `LocalCorpusProgress` callback on `LocalCorpusManager::ingest` |
//! | per-chunk NER | `MeteredEntityExtractor` over `ChunkEntityExtractor` |
//! | per-note RAPTOR + motifs | `MeteredTieredProvider` over `TieredEnrichmentProvider` |
//! | vault theme synthesis | the same decorator's `finalize_corpus` |
//! | typed extension | the same decorator's `post_finalize_corpus` |
//! | tokens / LLM calls / embeds | `MeteredInference` over `InferenceProvider` |
//!
//! Decorators, not instrumentation. The pipeline does not know it is
//! being measured, which is also why the numbers are trustworthy.
//!
//! ## Cold runs are the only valid runs
//!
//! Four independent checkpoint layers make a warm re-run report near-zero
//! work: the ingest resume cursor, the GLiNER delta (which marks
//! zero-entity chunks processed), the `note_already_current` skip, and
//! the per-note RAPTOR `input_hash` checkpoint. A build-time number from
//! a warm tree is not a build-time number.
//!
//! So `--cold` is not a convenience flag — it is the documented reset,
//! executed and reported: the corpus index dir is removed, and the SQLite
//! tiered state (`conv_raptor_nodes`, `conv_motifs`, `conv_skeletons`,
//! `chunk_entities`, `chunk_entity_progress`) plus `vault_themes` are
//! cleared. `LocalCorpusManager::reset_enrichment_state` does NOT do this
//! — it is status-only, and a run after it is warm.
//!
//! The harness refuses to run without an explicit `--cold` or `--warm`,
//! and every report carries `notes_skipped_already_current`. A run that
//! claims to be cold and skipped notes is self-evidently not, from the
//! report alone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use sovereign_contracts::probe::{
    ProbeEvidence, ProbeMode, ProbeRequest, VaultBuildEvidence, VaultBuildProbe, VaultSource,
};

use crate::chat_cmd::config::default_globals_for_voice_eval;
use sovereign_cli_base::help::{self, Help, HelpSection};
use sovereign_contracts::probe::ResourceReport;

const HELP: Help = Help {
    command: "svrn bench vault-report",
    summary: "Raw folder → queryable-enriched corpus, timed. Reports time_to_rag_ready and time_to_enriched with a per-phase and per-note breakdown.",
    sections: &[
        HelpSection::Usage(
            "svrn bench vault-report (--corpus-id <id> | --folder <path>) (--cold | --warm) \
             [--enrich-model <id>] [--no-gliner] [--allow-watcher] \
             [--output <path>] [--compare <timings.json>]",
        ),
        HelpSection::Flags(&[
            (
                "--corpus-id <id>",
                "Measure an already-registered folder corpus. This is the usual form — \
                 the corpus keeps its identity, so the number is comparable run to run.",
            ),
            (
                "--folder <path>",
                "Register the folder as a new document-folder corpus first, then measure it. \
                 Use for a fixture vault that is not installed yet.",
            ),
            (
                "--cold",
                "Delete this corpus's index dir AND its SQLite tiered state (raptor nodes, \
                 motifs, skeletons, chunk entities, entity progress, vault themes) before \
                 measuring. THIS IS THE ONLY MODE THAT PRODUCES A BASELINE — see the module \
                 docs for the four checkpoint layers a warm run short-circuits on.",
            ),
            (
                "--warm",
                "Measure without resetting. Legitimate for 'how long does a no-op re-sweep \
                 take', and for nothing else. The report will show most notes skipped.",
            ),
            (
                "--enrich-model <id>",
                "Serve the enrichment pipeline from a specific model instead of the daemon's \
                 primary. The report records both.",
            ),
            (
                "--no-gliner",
                "Skip the local NER pass entirely, leaving RAPTOR-only entities. The A/B \
                 partner for measuring what the NER phase actually costs.",
            ),
            (
                "--allow-watcher",
                "Proceed even though the target is a watched folder and the daemon is live. \
                 Without this the harness refuses: the daemon's sweeper and its boot-time \
                 resume_interrupted_enrichment will both write to the corpus you are timing.",
            ),
            (
                "--output <path>",
                "Extra copy of timings.json. Default: \
                 ~/.svrnmesh/bench-runs/vault-report/<ts>/timings.json.",
            ),
            (
                "--compare <timings.json>",
                "Print a phase-by-phase delta table against a prior run.",
            ),
        ]),
        HelpSection::Notes(
            "svrn's probe (`svrn __probe`, vault-build mode) runs the real folder pipeline — LocalCorpusManager::ingest \
             for tier 1, run_folder_tiered_enrichment for tiers 2/3 — with the inference \
             provider, entity extractor and tiered provider each wrapped in a metering \
             decorator. No production code is instrumented; every number comes from a seam \
             the pipeline already exposes.",
        ),
        HelpSection::Notes(
            "Reading the report: notes_skipped_already_current is the cold-run truth-teller. \
             A --cold run should report 0. Anything else means state survived the reset and \
             the totals understate the real build.",
        ),
    ],
};

// ─────────────────────────────────────────────────────────────────────
// Report shape
// ─────────────────────────────────────────────────────────────────────

pub use sovereign_contracts::probe::{ColdReset, IngestTransition, NoteRecord, PhaseSpan};
/// Rollup over [`NoteRecord`]s. Mirrors the counters
/// `svrn enrich raptor` prints, which is the accounting shape the
/// operator already reads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NoteSummary {
    pub total: usize,
    pub built: usize,
    /// The cold-run truth-teller. A `--cold` run reporting non-zero here
    /// did not actually start cold.
    pub skipped_already_current: usize,
    pub failed: usize,
    /// Summed per-note cost. Exceeds the RAPTOR phase span when notes
    /// run concurrently; equals it when they do not.
    pub sum_ms: u64,
    pub median_ms: u64,
    pub mean_ms: u64,
    pub p90_ms: u64,
    pub max_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slowest_doc_id: Option<String>,
}

fn motif_path_built() -> String {
    "built".to_string()
}

/// The persisted run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultReportRun {
    pub schema: String,
    pub bench_id: String,
    pub started_at_unix: u64,
    pub corpus_id: String,
    pub root_path: String,
    /// `cold` or `warm`. A comparison across modes is meaningless;
    /// `--compare` refuses it.
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cold_reset: Option<ColdReset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrich_model: Option<String>,
    pub embed_model: String,
    /// `gliner` · `disabled` · `unavailable`. The routing truth-teller
    /// for the NER phase — trust it over the flag you passed.
    pub entity_path: String,
    /// `built` · `skipped` · `removed`. The T3 motif index's routing
    /// truth-teller, and now a historical marker: `built` = a run from
    /// before 2026-08-02, `skipped` = one of the `--no-motifs` ablation
    /// arms that settled it, `removed` = the pass no longer exists on
    /// the folder path. Defaults to `built` so runs recorded before the
    /// field existed — the 2026-08-02 cold baseline among them — still
    /// deserialize under `--compare`.
    #[serde(default = "motif_path_built")]
    pub motif_path: String,

    pub files_indexed: usize,
    pub chunks_written: u64,
    pub documents_enriched: usize,
    pub entity_mentions: usize,

    /// Folder → Lance index queryable. The retrieval-ready headline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_rag_ready_ms: Option<u64>,
    /// Folder → NER + RAPTOR + themes all persisted. The enrichment
    /// headline, and the number this initiative spends against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_enriched_ms: Option<u64>,
    pub total_ms: u64,

    pub phases: Vec<PhaseSpan>,
    pub ingest_transitions: Vec<IngestTransition>,
    pub notes: Vec<NoteRecord>,
    pub note_summary: NoteSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceReport>,
    pub terminated_at_phase: String,
}

// ─────────────────────────────────────────────────────────────────────
// Args
// ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Cold,
    Warm,
}

impl Mode {
    fn label(&self) -> &'static str {
        match self {
            Mode::Cold => "cold",
            Mode::Warm => "warm",
        }
    }
}

/// The flag surface, as a struct. THE FIELD LIST IS THE FLAG LIST — `clap`
/// derives `--corpus-id` from `corpus_id`, the value coercion from the field
/// type (`Option<PathBuf>` parses a path, `bool` is a presence flag), and the
/// two exclusivity rules from the `group(...)` attributes below. Adding a flag
/// here is adding a field; there is no second table to keep in sync and no
/// parse loop to extend.
///
/// `clap` is free in this crate — it is already in the build graph via
/// `sovereign-eval` (`cargo tree -p sovereign-cli-llm -i clap` → v4.6.1), so
/// this costs no new dependency.
///
/// `disable_help_flag` because `--help` is served by [`HELP`] through
/// `sovereign_cli_base::help::print`, which is unchanged.
#[derive(clap::Parser, Debug, Default)]
#[command(
    // The `Usage:` line inside a parse error says what the user TYPED. Taken
    // from `HELP.command` rather than spelled again, so the two cannot drift;
    // without it clap names the binary (`sovereign-cli-llm`), which is not a
    // command anyone runs.
    name = HELP.command,
    no_binary_name = true,
    disable_help_flag = true,
    group(clap::ArgGroup::new("source").required(true).args(["corpus_id", "folder"])),
    group(clap::ArgGroup::new("run_mode").args(["cold", "warm"])),
)]
struct Opts {
    #[arg(long)]
    corpus_id: Option<String>,
    #[arg(long)]
    folder: Option<PathBuf>,
    #[arg(long)]
    cold: bool,
    #[arg(long)]
    warm: bool,
    #[arg(long)]
    enrich_model: Option<String>,
    #[arg(long)]
    no_gliner: bool,
    #[arg(long)]
    allow_watcher: bool,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    compare: Option<PathBuf>,
}

impl Opts {
    /// Total, not `Option<Mode>`: [`parse_args`] rejects the neither case and
    /// the `run_mode` group rejects the both case, so by the time an `Opts`
    /// exists exactly one is set. This retires a
    /// `.expect("parse_args guarantees a mode")` — the guarantee is now carried
    /// by the type instead of by a comment (ARCH §7).
    fn mode(&self) -> Mode {
        if self.cold {
            Mode::Cold
        } else {
            Mode::Warm
        }
    }
}

fn parse_args(args: &[String]) -> std::result::Result<Opts, String> {
    let o = sovereign_cli_base::flag_surface::parse::<Opts>(args)?;
    // The ONE rule kept by hand, deliberately. `run_mode` is declared
    // non-required so that this message survives: clap's own "required
    // arguments were not provided" is correct and says nothing about WHY the
    // choice exists, and that reasoning is the point. The two rules it does
    // not carry — source required, source exclusive — are mechanical, and
    // clap's rendering of them is strictly better than the strings it replaced.
    if !o.cold && !o.warm {
        return Err(
            "need --cold or --warm. A build-time number is only meaningful from a cold tree \
             (four checkpoint layers short-circuit a warm one); requiring the choice keeps a \
             warm run from being baselined by accident. See --help."
                .to_string(),
        );
    }
    Ok(o)
}

// ─────────────────────────────────────────────────────────────────────
// Entry point
// ─────────────────────────────────────────────────────────────────────

pub async fn cmd_vault_report(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        help::print(&HELP);
        return 0;
    }
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let compare = opts.compare.clone();
    let output = opts.output.clone();
    match run(opts).await {
        Ok(report) => {
            print_summary(&report);
            if let Err(e) = persist_report(&report, output.as_deref()) {
                eprintln!("warning: persist report: {e}");
            }
            if let Some(baseline) = compare {
                if let Err(e) = print_delta(&report, &baseline) {
                    eprintln!("compare: {e}");
                    return 1;
                }
            }
            if report.note_summary.failed > 0 {
                return 1;
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

async fn run(opts: Opts) -> std::result::Result<VaultReportRun, String> {
    let mode = opts.mode();
    let spec = VaultBuildProbe {
        cold: mode == Mode::Cold,
        source: match (opts.corpus_id, opts.folder) {
            (Some(corpus_id), _) => VaultSource::Corpus { corpus_id },
            (None, Some(path)) => VaultSource::Folder { path },
            (None, None) => unreachable!("parse_args guarantees one of the two"),
        },
        enrich_model: opts.enrich_model,
        no_gliner: opts.no_gliner,
        allow_watcher: opts.allow_watcher,
    };
    let request = ProbeRequest {
        mode: ProbeMode::VaultBuild,
        questions: Vec::new(),
        corpus: String::new(),
        limit: 0,
        isolate: false,
        atlas: None,
        attached: None,
        vault: Some(spec),
    };
    let globals = default_globals_for_voice_eval();
    match crate::eval_cmd::run_probe(&globals, &request)? {
        ProbeEvidence::VaultBuild(build) => Ok(report(*build, &mode)),
        other => Err(format!(
            "`svrn __probe` answered a vault build with {} evidence",
            other.mode().as_str()
        )),
    }
}

/// The run's report: the build as observed, plus bench's rollups over its
/// notes and the run's identity.
fn report(b: VaultBuildEvidence, mode: &Mode) -> VaultReportRun {
    let mut phases = b.phases;
    if let Some(span) = raptor_span(&b.notes) {
        phases.push(span);
    }
    phases.sort_by_key(|p| p.start_ms);
    let note_summary = summarise_notes(&b.notes);
    VaultReportRun {
        schema: "vault-report/v1".to_string(),
        bench_id: run_id(),
        started_at_unix: sovereign_time::unix_now_u64(),
        corpus_id: b.corpus_id,
        root_path: b.root_path,
        mode: mode.label().to_string(),
        cold_reset: b.cold_reset,
        enrich_model: b.enrich_model,
        embed_model: b.embed_model,
        entity_path: b.entity_path,
        motif_path: b.motif_path,
        files_indexed: b.files_indexed,
        chunks_written: b.chunks_written,
        documents_enriched: b.documents_enriched,
        entity_mentions: b.entity_mentions,
        time_to_rag_ready_ms: b.time_to_rag_ready_ms,
        time_to_enriched_ms: b.time_to_enriched_ms,
        total_ms: b.total_ms,
        phases,
        ingest_transitions: b.ingest_transitions,
        notes: b.notes,
        note_summary,
        resources: Some(b.resources),
        terminated_at_phase: "complete".to_string(),
    }
}

/// Wall-clock span of the per-note RAPTOR phase: first note start →
/// last note end. Deliberately not the sum — notes may overlap, and a
/// sum of concurrent work is not a duration. `NoteSummary::sum_ms`
/// carries the summed cost separately.
fn raptor_span(notes: &[NoteRecord]) -> Option<PhaseSpan> {
    let built: Vec<&NoteRecord> = notes
        .iter()
        .filter(|n| n.outcome != "skipped_already_current")
        .collect();
    if built.is_empty() {
        return None;
    }
    let start = built.iter().map(|n| n.start_ms).min()?;
    let end = built.iter().map(|n| n.start_ms + n.ms).max()?;
    Some(PhaseSpan {
        phase: "raptor_per_note".to_string(),
        start_ms: start,
        end_ms: end,
        ms: end.saturating_sub(start),
        detail: serde_json::json!({ "notes": built.len() }),
    })
}

fn summarise_notes(notes: &[NoteRecord]) -> NoteSummary {
    let mut s = NoteSummary {
        total: notes.len(),
        ..Default::default()
    };
    let mut built_ms: Vec<u64> = Vec::new();
    let mut slowest: Option<(&str, u64)> = None;
    for n in notes {
        match n.outcome.as_str() {
            "built" => {
                s.built += 1;
                built_ms.push(n.ms);
                s.sum_ms += n.ms;
                if slowest.map(|(_, m)| n.ms > m).unwrap_or(true) {
                    slowest = Some((n.doc_id.as_str(), n.ms));
                }
            }
            "skipped_already_current" => s.skipped_already_current += 1,
            _ => s.failed += 1,
        }
    }
    if !built_ms.is_empty() {
        built_ms.sort_unstable();
        s.median_ms = built_ms[built_ms.len() / 2];
        s.mean_ms = s.sum_ms / built_ms.len() as u64;
        let p90_idx = ((built_ms.len() as f64 * 0.9).ceil() as usize).saturating_sub(1);
        s.p90_ms = built_ms[p90_idx.min(built_ms.len() - 1)];
        s.max_ms = *built_ms.last().unwrap_or(&0);
        s.slowest_doc_id = slowest.map(|(d, _)| d.to_string());
    }
    s
}

// ─────────────────────────────────────────────────────────────────────
// Rendering + persistence
// ─────────────────────────────────────────────────────────────────────

fn secs(ms: u64) -> String {
    if ms >= 60_000 {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    } else {
        format!("{:.1}s", ms as f64 / 1000.0)
    }
}

fn print_summary(r: &VaultReportRun) {
    println!();
    println!("vault build report — {} ({})", r.corpus_id, r.mode);
    println!("  folder:  {}", r.root_path);
    println!(
        "  scope:   {} file(s) · {} chunk(s) · {} document(s) · {} entity mention(s)",
        r.files_indexed, r.chunks_written, r.documents_enriched, r.entity_mentions
    );
    println!(
        "  models:  enrich={} embed={}",
        r.enrich_model.as_deref().unwrap_or("-"),
        r.embed_model
    );
    println!("  ner:     {}", r.entity_path);
    println!("  motifs:  {}", r.motif_path);
    println!();
    println!(
        "  TIME TO RAG READY   {}",
        r.time_to_rag_ready_ms
            .map(secs)
            .unwrap_or_else(|| "-".into())
    );
    println!(
        "  TIME TO ENRICHED    {}",
        r.time_to_enriched_ms
            .map(secs)
            .unwrap_or_else(|| "-".into())
    );
    println!();

    println!("  phase                     start      elapsed     share");
    println!("  ─────────────────────────────────────────────────────────");
    let total = r.time_to_enriched_ms.unwrap_or(r.total_ms).max(1);
    for p in &r.phases {
        println!(
            "  {:<22}  {:>8}  {:>10}  {:>6.1}%",
            p.phase,
            secs(p.start_ms),
            secs(p.ms),
            p.ms as f64 * 100.0 / total as f64
        );
    }
    println!();

    let n = &r.note_summary;
    println!(
        "  notes: {} total · {} built · {} skipped(already current) · {} failed",
        n.total, n.built, n.skipped_already_current, n.failed
    );
    if n.built > 0 {
        println!(
            "         per-note median {} · mean {} · p90 {} · max {}{}",
            secs(n.median_ms),
            secs(n.mean_ms),
            secs(n.p90_ms),
            secs(n.max_ms),
            n.slowest_doc_id
                .as_deref()
                .map(|d| format!(" ({d})"))
                .unwrap_or_default()
        );
        println!("         summed per-note cost {}", secs(n.sum_ms));
    }
    if r.mode == "cold" && n.skipped_already_current > 0 {
        println!();
        println!(
            "  WARNING: a --cold run skipped {} note(s) as already-current. State survived the \
             reset; these totals UNDERSTATE the real build and must not be baselined.",
            n.skipped_already_current
        );
    }
    if let Some(cr) = &r.cold_reset {
        if !cr.warnings.is_empty() {
            println!();
            println!("  WARNING: cold reset was incomplete:");
            for w in &cr.warnings {
                println!("    - {w}");
            }
        }
    }
    if let Some(res) = &r.resources {
        println!();
        println!("  resources (per-phase LLM/embed ledger)");
        for p in &res.phases {
            println!(
                "    {:<16} calls {:>4} · prompt {:>8} tok · completion {:>7} tok · embeds {:>5} ({} texts)",
                p.phase,
                p.bucket.llm_calls,
                p.bucket.prompt_tokens,
                p.bucket.completion_tokens,
                p.bucket.embed_calls,
                p.bucket.embed_texts
            );
        }
    }
    println!();
}

fn print_delta(r: &VaultReportRun, baseline: &Path) -> std::result::Result<(), String> {
    let raw = std::fs::read_to_string(baseline)
        .map_err(|e| format!("read {}: {e}", baseline.display()))?;
    let base: VaultReportRun =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", baseline.display()))?;
    if base.mode != r.mode {
        return Err(format!(
            "baseline is a '{}' run and this is a '{}' run — the comparison would be \
             meaningless (a warm run short-circuits most of the work)",
            base.mode, r.mode
        ));
    }
    println!("  delta vs {} ({})", baseline.display(), base.corpus_id);
    println!("  phase                     baseline       this        Δ");
    println!("  ─────────────────────────────────────────────────────────");
    let mut base_by_phase: BTreeMap<&str, u64> = BTreeMap::new();
    for p in &base.phases {
        *base_by_phase.entry(p.phase.as_str()).or_insert(0) += p.ms;
    }
    for p in &r.phases {
        let b = base_by_phase.get(p.phase.as_str()).copied();
        match b {
            Some(b) => println!(
                "  {:<22}  {:>10}  {:>9}  {:>+8.1}s",
                p.phase,
                secs(b),
                secs(p.ms),
                (p.ms as f64 - b as f64) / 1000.0
            ),
            None => println!(
                "  {:<22}  {:>10}  {:>9}  {:>9}",
                p.phase,
                "-",
                secs(p.ms),
                "new"
            ),
        }
    }
    let hb = base.time_to_enriched_ms.unwrap_or(base.total_ms);
    let hn = r.time_to_enriched_ms.unwrap_or(r.total_ms);
    println!(
        "  {:<22}  {:>10}  {:>9}  {:>+8.1}s",
        "TIME TO ENRICHED",
        secs(hb),
        secs(hn),
        (hn as f64 - hb as f64) / 1000.0
    );
    Ok(())
}

fn persist_report(
    r: &VaultReportRun,
    explicit_output: Option<&Path>,
) -> std::result::Result<(), String> {
    let dir = sovereign_contracts::rebrand::svrnmesh_root()
        .join("bench-runs/vault-report")
        .join(&r.bench_id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let json_path = dir.join("timings.json");
    let json = serde_json::to_string_pretty(r).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&json_path, &json).map_err(|e| format!("write {}: {e}", json_path.display()))?;
    eprintln!("      timings: {}", json_path.display());
    if let Some(extra) = explicit_output {
        if extra != json_path {
            std::fs::write(extra, &json).map_err(|e| format!("write {}: {e}", extra.display()))?;
            eprintln!("      timings: {}", extra.display());
        }
    }
    Ok(())
}

fn run_id() -> String {
    format!("{}", sovereign_time::unix_now_u64())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(doc: &str, ms: u64, start: u64, outcome: &str) -> NoteRecord {
        NoteRecord {
            doc_id: doc.to_string(),
            chunks: 10,
            bucket: "small".to_string(),
            ms,
            start_ms: start,
            outcome: outcome.to_string(),
            error: None,
        }
    }

    #[test]
    fn mode_is_required_so_a_warm_run_is_never_baselined_by_accident() {
        let err = parse_args(&["--corpus-id".into(), "x".into()]).unwrap_err();
        assert!(err.contains("--cold or --warm"), "got: {err}");
    }

    #[test]
    fn corpus_and_folder_are_mutually_exclusive() {
        let err = parse_args(&[
            "--corpus-id".into(),
            "x".into(),
            "--folder".into(),
            "/tmp".into(),
            "--cold".into(),
        ])
        .unwrap_err();
        // Asserts the RULE, not the wording. The phrasing is clap's since the
        // flag surface became `#[derive(clap::Parser)]` — it renders "cannot be
        // used with" and names both flags, where the hand-rolled loop said
        // "mutually exclusive". A user-facing text change, declared, not silent.
        assert!(
            err.contains("--corpus-id") && err.contains("--folder"),
            "got: {err}"
        );
    }

    #[test]
    fn summary_separates_built_from_skipped() {
        let notes = vec![
            note("a", 1000, 0, "built"),
            note("b", 3000, 10, "built"),
            note("c", 5, 20, "skipped_already_current"),
            note("d", 100, 30, "failed"),
        ];
        let s = summarise_notes(&notes);
        assert_eq!(s.total, 4);
        assert_eq!(s.built, 2);
        assert_eq!(s.skipped_already_current, 1);
        assert_eq!(s.failed, 1);
        // Skipped and failed notes must not enter the cost stats — the
        // whole point of the split is that a skip is not a build.
        assert_eq!(s.sum_ms, 4000);
        assert_eq!(s.mean_ms, 2000);
        assert_eq!(s.max_ms, 3000);
        assert_eq!(s.slowest_doc_id.as_deref(), Some("b"));
    }

    #[test]
    fn raptor_span_is_a_duration_not_a_sum() {
        // Two fully-overlapping 3s notes span 3s of wall-clock, not 6s.
        let notes = vec![note("a", 3000, 0, "built"), note("b", 3000, 0, "built")];
        let span = raptor_span(&notes).expect("built notes yield a span");
        assert_eq!(span.ms, 3000);
        assert_eq!(summarise_notes(&notes).sum_ms, 6000);
    }

    #[test]
    fn raptor_span_is_none_when_everything_was_skipped() {
        let notes = vec![note("a", 5, 0, "skipped_already_current")];
        assert!(raptor_span(&notes).is_none());
    }

    // ── flag surface ────────────────────────────────────────────────────
    // These pin the BEHAVIOUR the hand-rolled loop had, so the conversion to
    // `#[derive(clap::Parser)]` is a refactor and not a silent change of what
    // the command accepts. They also show the next author how to test a flag
    // surface without booting the command.

    fn parse(argv: &[&str]) -> std::result::Result<Opts, String> {
        super::parse_args(&argv.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn field_name_becomes_the_long_flag_and_the_type_does_the_coercion() {
        let o = parse(&["--corpus-id", "vault", "--cold", "--output", "/tmp/r.json"]).unwrap();
        assert_eq!(o.corpus_id.as_deref(), Some("vault"));
        assert_eq!(o.output, Some(std::path::PathBuf::from("/tmp/r.json")));
        assert!(o.cold && !o.warm);
    }

    #[test]
    fn key_equals_value_is_accepted() {
        // The form four hand-rolled CLIs silently dropped (nc-22b). clap takes
        // it for free; the loop this replaced had to be taught it.
        let o = parse(&["--corpus-id=vault", "--warm"]).unwrap();
        assert_eq!(o.corpus_id.as_deref(), Some("vault"));
    }

    #[test]
    fn mode_is_total_once_an_opts_exists() {
        assert_eq!(
            parse(&["--folder", "/tmp", "--cold"]).unwrap().mode(),
            Mode::Cold
        );
        assert_eq!(
            parse(&["--folder", "/tmp", "--warm"]).unwrap().mode(),
            Mode::Warm
        );
    }

    #[test]
    fn a_source_is_required() {
        // Exclusivity is already covered by
        // `corpus_and_folder_are_mutually_exclusive` above; only the
        // no-source-at-all case is new.
        assert!(parse(&["--cold"]).is_err());
    }

    #[test]
    fn both_modes_is_rejected_by_the_group() {
        assert!(parse(&["--corpus-id", "v", "--cold", "--warm"]).is_err());
    }

    #[test]
    fn an_unknown_flag_is_still_rejected() {
        assert!(parse(&["--corpus-id", "v", "--cold", "--nope"]).is_err());
    }

    #[test]
    fn a_parse_error_carries_one_prefix_and_names_the_typed_command() {
        // Composed exactly as `cmd_vault_report` composes it. `clap`'s own
        // rendering also opens `error: `, so before `flag_surface::parse`
        // owned the stripping this read `error: error: …`; and the `Usage:`
        // line named `sovereign-cli-llm`, a binary no user invokes.
        let rendered = format!("error: {}", parse(&["--nope"]).unwrap_err());
        assert!(!rendered.starts_with("error: error:"), "got: {rendered}");
        assert!(
            rendered.contains("Usage: svrn bench vault-report"),
            "got: {rendered}"
        );
    }
}
