//! Tauri command surface for atlas enrichment.
//!
//! Mirrors the layout of `local_corpus_commands.rs`:
//!
//!   - one job_id-scoped event channel per `enrich_build` invocation
//!     (`enrich://progress/{job_id}`), carrying typed
//!     `EnrichProgress` events the UI consumes via
//!     `listen<EnrichProgress>(channel, ...)`.
//!   - `enrich_errors` + `enrich_sep_ingest` commands are
//!     synchronous — they shell out to the CLI once and return
//!     the parsed response.
//!
//! The heavy lifting (subprocess + stdout parsing) lives in
//! `sovereign_tools::enrich`; this module is a translation layer
//! between Tauri IPC and that library.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use corpus_engine::enrichment::atlas::{read_atlas_atoms, AtomEnvelope};
use serde::Serialize;
use sovereign_tools::enrich::{
    fire_cancellation, new_cancellation_flag, resolve_sovereign_cli, run_enrich_build,
    CancellationFlag, EnrichBuildConfig, EnrichProgressFn,
};
use tauri::{AppHandle, Emitter};

/// Per-module registry for in-flight enrichment jobs. Tracks:
///
/// - `active_corpora` — corpus ids with a running build. A second
///   `enrich_build_async` against the same corpus is rejected so
///   two concurrent runs can't race on `cache/*.json`.
/// - `cancel_flags` — per-job cancellation flags the
///   `enrich_cancel_build` command flips.
///
/// Module-local via `OnceLock` rather than on `AppState` because
/// the state is feature-scoped and keeping enrichment's
/// concurrency primitives next to its commands is easier to reason
/// about than spreading them across `state.rs` + `enrich_commands.rs`.
#[derive(Default)]
struct EnrichJobRegistry {
    active_corpora: HashSet<String>,
    cancel_flags: HashMap<String, CancellationFlag>,
    /// Reverse lookup so the UI can attach to an in-flight job for a
    /// corpus it didn't start itself (e.g. the first-corpus onboarding
    /// flow finds a build that Settings→Enrichment kicked off first).
    job_id_by_corpus: HashMap<String, String>,
}

static REGISTRY: OnceLock<Mutex<EnrichJobRegistry>> = OnceLock::new();

fn registry() -> &'static Mutex<EnrichJobRegistry> {
    REGISTRY.get_or_init(|| Mutex::new(EnrichJobRegistry::default()))
}

/// Attempt to reserve a corpus for a build. Returns the
/// cancellation flag for the new job on success, `Err` if the
/// corpus already has a build in flight.
fn reserve_corpus(corpus_id: &str, job_id: &str) -> Result<CancellationFlag, String> {
    let mut reg = registry().lock().expect("enrich job registry lock");
    if reg.active_corpora.contains(corpus_id) {
        return Err(format!(
            "A build is already running for `{corpus_id}`. Cancel it first or wait for it to finish."
        ));
    }
    let flag = new_cancellation_flag();
    reg.active_corpora.insert(corpus_id.to_string());
    reg.cancel_flags.insert(job_id.to_string(), flag.clone());
    reg.job_id_by_corpus
        .insert(corpus_id.to_string(), job_id.to_string());
    Ok(flag)
}

/// Drop a job's reservation + cancel flag. Safe to call multiple
/// times (the second call is a no-op); the subprocess-spawned
/// task calls it on every exit path.
fn release_corpus(corpus_id: &str, job_id: &str) {
    let mut reg = registry().lock().expect("enrich job registry lock");
    reg.active_corpora.remove(corpus_id);
    reg.cancel_flags.remove(job_id);
    // Only clear the corpus→job mapping if it still points at *this*
    // job_id. A stale call on an already-replaced job shouldn't wipe
    // the new entry.
    if reg
        .job_id_by_corpus
        .get(corpus_id)
        .map(|s| s.as_str() == job_id)
        .unwrap_or(false)
    {
        reg.job_id_by_corpus.remove(corpus_id);
    }
}

fn progress_channel(job_id: &str) -> String {
    format!("enrich://progress/{job_id}")
}

fn new_job_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Build a progress callback that emits every event on the
/// job-scoped Tauri channel. Failed emits are swallowed (UI
/// window closed, e.g.) because they should not abort a
/// running build — the caller can re-subscribe and read the
/// terminal state from the run file.
fn make_emitter(app: AppHandle, job_id: String) -> EnrichProgressFn {
    let channel = progress_channel(&job_id);
    Arc::new(move |evt| {
        let _ = app.emit(&channel, &evt);
    })
}

// ─── Command: enrich_build_async ─────────────────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct EnrichBuildHandle {
    pub job_id: String,
    pub corpus_id: String,
    pub channel: String,
}

/// Spawn an `enrich build <corpus_id>` subprocess and stream
/// progress on `enrich://progress/{job_id}`. Returns immediately
/// with the handle the UI needs to subscribe.
///
/// `chapters: None` runs `--full`; `Some(vec![...])` runs the
/// given subset via `--chapters`.
/// `skip_steps` forwards `--skip <step>` flags so the UI can
/// pre-exclude (for example) the Phase 8 configuration step on a
/// long-running corpus.
///
/// Rejects with an error if another build is already running for
/// the same corpus — two concurrent runs would race on
/// `cache/*.json` and corrupt the phase inputs for each other.
/// The error message includes "already running" so the UI can
/// decide whether to offer a Cancel button on the existing row.
#[tauri::command]
pub async fn enrich_build_async(
    app: AppHandle,
    corpus_id: String,
    chapters: Option<Vec<String>>,
    skip_steps: Option<Vec<String>>,
) -> Result<EnrichBuildHandle, String> {
    let job_id = new_job_id();
    let channel = progress_channel(&job_id);
    let progress = make_emitter(app.clone(), job_id.clone());
    let corpus_id_for_task = corpus_id.clone();

    // Reserve the corpus before spawning. If this fails, no
    // subprocess is started and no progress events are emitted —
    // the caller gets the rejection synchronously.
    let cancel_flag = reserve_corpus(&corpus_id, &job_id)?;

    // Construct extra_args from the typed inputs. Keeps the
    // subprocess spawn deterministic: one path, no shell
    // interpolation.
    let mut extra_args: Vec<String> = Vec::new();
    match chapters {
        Some(ids) if ids.is_empty() => {
            release_corpus(&corpus_id, &job_id);
            return Err("chapter list cannot be empty — omit the field for --full".into());
        }
        Some(ids) => {
            extra_args.push("--chapters".into());
            extra_args.push(ids.join(","));
        }
        None => extra_args.push("--full".into()),
    }
    if let Some(skips) = skip_steps {
        for s in skips {
            extra_args.push("--skip".into());
            extra_args.push(s);
        }
    }

    let job_id_for_task = job_id.clone();

    // Spawn so the Tauri command returns quickly. The UI drives
    // the progress panel off the emitter; the terminal event is
    // `Complete`, `Aborted`, or `SpawnFailed`. The registry
    // cleanup lands in every branch including panic — see the
    // wrapped `tokio::task::spawn` with an explicit `drop` guard
    // via a defer pattern if panics become a concern; for now
    // the three explicit cleanup paths cover the observed cases.
    tokio::spawn(async move {
        let config = EnrichBuildConfig {
            cli_path: None, // PATH lookup — installed sovereign-cli
            extra_args,
            cancel: Some(cancel_flag),
        };
        let result = run_enrich_build(&corpus_id_for_task, config, Some(progress.clone())).await;

        // Always release the corpus + drop the cancel flag once
        // the subprocess finishes, regardless of how it exited.
        release_corpus(&corpus_id_for_task, &job_id_for_task);

        match result {
            Ok(outcome) => {
                tracing::info!(
                    corpus_id = %outcome.corpus_id,
                    exit_code = outcome.exit_code,
                    cancelled = outcome.cancelled,
                    unrecognised_lines = outcome.unrecognised_lines.len(),
                    "enrich build subprocess finished"
                );
            }
            Err(e) => {
                // Subprocess failed to spawn at all (CLI not in
                // PATH, permission denied, …). Emit `SpawnFailed`
                // rather than `Aborted` so the UI doesn't
                // mis-attribute the failure to the first step —
                // no step actually ran.
                use corpus_engine::enrichment::pipeline::EnrichProgress;
                progress(EnrichProgress::SpawnFailed {
                    corpus_id: corpus_id_for_task.clone(),
                    message: format!("could not spawn sovereign-cli: {e}"),
                });
                tracing::error!(
                    corpus_id = %corpus_id_for_task,
                    error = %e,
                    "could not spawn sovereign-cli enrich build"
                );
            }
        }
    });

    Ok(EnrichBuildHandle {
        job_id,
        corpus_id,
        channel,
    })
}

/// Request cancellation of an in-flight build. Idempotent —
/// calling twice is fine. Returns `true` when the job was known
/// and flagged, `false` when the job_id wasn't tracked (either
/// already finished and pruned, or never started).
///
/// The actual subprocess kill happens in the library layer on
/// the next stdout-read poll; typical latency is sub-second (the
/// CLI emits at least one line per chapter). A terminal
/// `Cancelled` event (distinct from `Aborted` and `SpawnFailed`)
/// follows, carrying the step that was running at kill time.
#[tauri::command]
pub async fn enrich_cancel_build(job_id: String) -> Result<bool, String> {
    let reg = registry()
        .lock()
        .map_err(|e| format!("registry lock: {e}"))?;
    match reg.cancel_flags.get(&job_id) {
        Some(flag) => {
            fire_cancellation(flag);
            tracing::info!(job_id = %job_id, "enrich build cancellation requested");
            Ok(true)
        }
        None => Ok(false),
    }
}

// ─── Command: enrich_errors ──────────────────────────────────────────

/// Read the structured-failure aggregate for one corpus. Shells
/// out to `sovereign-cli enrich errors <corpus> --json` and
/// returns the parsed JSON.
///
/// Returns a JSON array of `PhaseFailureView` records (each one
/// is a `PhaseFailure` plus the `remediation` hint for its kind —
/// see the CLI's `--json` path). The UI renders these grouped
/// by kind.
///
/// Subject to a 10-second timeout: if the CLI hangs (corrupt run
/// file, I/O stall, etc.) we surface a clear error rather than
/// blocking the UI panel forever.
#[tauri::command]
pub async fn enrich_errors(corpus_id: String) -> Result<serde_json::Value, String> {
    use tokio::process::Command;
    use tokio::time::{timeout, Duration};

    /// Hard cap on the subprocess call. The aggregator is
    /// expected to finish in well under a second for any real
    /// corpus (reads JSON from disk, groups in-memory); 10s is
    /// comfortably over the P99 while still bounding "something
    /// is wrong" scenarios like a disk stall or a filesystem
    /// hang.
    const ERRORS_TIMEOUT: Duration = Duration::from_secs(10);

    let bin = resolve_sovereign_cli().ok_or_else(|| {
        "sovereign-cli not found on PATH or alongside the desktop binary. \
         Build via `cargo build --release -p sovereign-cli` and put it on \
         $PATH, or set SOVEREIGN_CLI=/abs/path/to/sovereign-cli."
            .to_string()
    })?;
    let fut = async {
        Command::new(&bin)
            .arg("enrich")
            .arg("errors")
            .arg(&corpus_id)
            .arg("--json")
            .output()
            .await
            .map_err(|e| format!("spawning sovereign-cli: {e}"))
    };
    let output = timeout(ERRORS_TIMEOUT, fut).await.map_err(|_| {
        format!(
            "sovereign-cli enrich errors timed out after {}s — check for a hung \
             filesystem or a corrupt run file",
            ERRORS_TIMEOUT.as_secs()
        )
    })??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "sovereign-cli enrich errors exited with code {}: {stderr}",
            output.status.code().unwrap_or(-1)
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).map_err(|e| {
        format!(
            "parsing enrich errors JSON: {e} (first 200 chars: {})",
            stdout.chars().take(200).collect::<String>()
        )
    })
}

// ─── Command: enrich_sep_ingest ──────────────────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct SepIngestResult {
    pub corpus_id: String,
    pub slug: String,
    /// Text of the CLI's stdout for the ingest — the UI shows
    /// this inside a collapsible panel so an operator can audit
    /// exactly what was scaffolded.
    pub log: String,
}

/// Scaffold a per-article SEP enrichment corpus from the cached
/// parquet. Equivalent to
/// `sovereign-cli enrich sep-ingest <slug> [--paragraphs-per-section N]`.
///
/// Validates the slug shape server-side (`[a-z0-9-]+`) before
/// shelling. tokio::Command escapes args correctly, but a
/// malformed slug would still end up as part of the output
/// filename at `~/.sovereign/corpora/sep/articles/<slug>.md`;
/// the defensive clamp avoids weird paths for zero real cost.
///
/// Subject to a 60-second timeout — the 1 GB parquet can take a
/// handful of seconds to scan on a cold disk; 60s is well over
/// the P99 while still bounding hangs.
#[tauri::command]
pub async fn enrich_sep_ingest(
    slug: String,
    paragraphs_per_section: Option<usize>,
) -> Result<SepIngestResult, String> {
    use tokio::process::Command;
    use tokio::time::{timeout, Duration};

    if !is_valid_sep_slug(&slug) {
        return Err(format!(
            "invalid SEP slug `{slug}`: slugs must match `[A-Za-z0-9-]+` \
             (ASCII letters, digits, or hyphens only — the parquet's \
             `category` column is mostly lowercase but a handful of \
             entries like `18thGerman-preKant` are mixed-case)"
        ));
    }

    const SEP_INGEST_TIMEOUT: Duration = Duration::from_secs(60);

    let bin = resolve_sovereign_cli().ok_or_else(|| {
        "sovereign-cli not found on PATH or alongside the desktop binary. \
         Build via `cargo build --release -p sovereign-cli` and put it on \
         $PATH, or set SOVEREIGN_CLI=/abs/path/to/sovereign-cli."
            .to_string()
    })?;
    let mut cmd = Command::new(&bin);
    cmd.arg("enrich").arg("sep-ingest").arg(&slug);
    if let Some(n) = paragraphs_per_section {
        cmd.arg("--paragraphs-per-section").arg(n.to_string());
    }
    let fut = async move {
        cmd.output()
            .await
            .map_err(|e| format!("spawning sovereign-cli: {e}"))
    };
    let output = timeout(SEP_INGEST_TIMEOUT, fut).await.map_err(|_| {
        format!(
            "sovereign-cli enrich sep-ingest timed out after {}s — is the SEP \
             parquet at ~/.sovereign/indexes/_downloads/sep.parquet readable?",
            SEP_INGEST_TIMEOUT.as_secs()
        )
    })??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "sovereign-cli enrich sep-ingest exited with code {}: {stderr}",
            output.status.code().unwrap_or(-1)
        ));
    }
    Ok(SepIngestResult {
        corpus_id: format!("sep-{slug}"),
        slug,
        log: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

/// SEP slug validator: lowercase ASCII, digits, and hyphens
/// only, non-empty, 64-char cap. Matches the category slugs
/// that appear in the parquet (`compatibilism`,
/// `recursive-functions`, `18thGerman-preKant`, …). Reject
/// everything else — defensive clamp before a CLI shell-out
/// that writes a file named after the slug. Accepts both cases
/// because the parquet has 5 mixed-case categories
/// (`18thGerman-preKant`, `emotion-Christian-tradition`,
/// `equivME`, `physics-Rpcc`, `statphys-Boltzmann`).
fn is_valid_sep_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > 64 {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_alphabetic() || c.is_ascii_digit() || c == '-')
}

// ─── Command: enrich_list_corpora ────────────────────────────────────

/// Inventory of enrichment corpora on disk. Reads
/// `~/.sovereign/enrichment/*/config.json` directly — no CLI
/// call needed for this (faster + no PATH dep).
#[derive(Debug, Serialize, Clone)]
pub struct EnrichedCorpusSummary {
    pub corpus_id: String,
    pub pipeline_id: String,
    pub source_path: String,
    pub created_at: String,
}

#[tauri::command]
pub async fn enrich_list_corpora() -> Result<Vec<EnrichedCorpusSummary>, String> {
    let root = enrichment_root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let entries =
        std::fs::read_dir(&root).map_err(|e| format!("reading {}: {e}", root.display()))?;
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let corpus_id = match entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let config_path = entry.path().join("config.json");
        if !config_path.exists() {
            continue;
        }
        let raw = match std::fs::read_to_string(&config_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let v: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        out.push(EnrichedCorpusSummary {
            corpus_id,
            pipeline_id: v
                .get("pipeline_id")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            source_path: v
                .get("source_path")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
            created_at: v
                .get("created_at")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

fn enrichment_root() -> PathBuf {
    sovereign_root().join("enrichment")
}

fn sovereign_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".sovereign")
}

fn index_root_for(corpus_id: &str) -> PathBuf {
    sovereign_root().join("indexes").join(corpus_id)
}

fn staging_jsonl_path_for(corpus_id: &str) -> PathBuf {
    sovereign_root()
        .join("local-corpus-staging")
        .join(format!("{corpus_id}.jsonl"))
}

// ─── Command: enrich_init_for_local_corpus ───────────────────────────

/// Shape: the atlas pipelines the UI is allowed to pin. `enrich init`
/// itself accepts more variants (including the legacy `literary`
/// questions-only flow), but the onboarding + Settings surfaces only
/// drive atlas-producing pipelines — matches the validation
/// `build.rs` already does at phase time (`pipeline_id.ends_with("_atlas")`).
const ALLOWED_ATLAS_PIPELINES: &[&str] = &["literary_atlas", "philosophy_atlas"];

/// Summary of the sampled documents, so the UI can say "ready to ask
/// about X, Y, Z" after the sample atlas build finishes. Populated by
/// `enrich_init_for_local_corpus` only when `sample_size` is Some.
#[derive(Debug, Serialize, Clone, Default)]
pub struct SampledDocuments {
    /// Document titles (up to `sample_size`) in the order they were
    /// included. Drawn from the JSONL `title` field.
    pub titles: Vec<String>,
    /// Total JSONL records available on disk at init time. If
    /// `titles.len() < total`, the atlas covers only a sample and a
    /// follow-up "Extend atlas" surface can offer the rest.
    pub total: usize,
}

/// Idempotent bridge from a local-corpus ingest (staged JSONL) to the
/// atlas enrichment pipeline.
///
/// Responsibilities:
///   1. Validate the pipeline id is an atlas variant.
///   2. If `config.json` already exists, matches the requested pipeline
///      AND is consistent with the requested `sample_size` (same
///      source-file line count), return Ok — no-op so the UI can call
///      this unconditionally. Pipeline OR sample-size change forces a
///      rebuild via `--force`.
///   3. Otherwise synthesise a plaintext source from the staged JSONL
///      (one `===== <title> =====` delimiter per document, limited to
///      `sample_size` records when Some), then shell to `sovereign-cli
///      enrich init` with `--force`.
///
/// Sample-first time-to-first-value: picking the first 5 docs as the
/// sample typically lands the atlas in 2–3 min on an M2 Max, vs 15–30
/// for a full folder. The user gets starter questions and can begin
/// asking immediately; the remaining docs stay searchable via the
/// ingest index (no atlas dependency) while they wait.
#[tauri::command]
pub async fn enrich_init_for_local_corpus(
    corpus_id: String,
    pipeline_id: String,
    sample_size: Option<usize>,
) -> Result<SampledDocuments, String> {
    use tokio::process::Command;
    use tokio::time::{timeout, Duration};

    if !ALLOWED_ATLAS_PIPELINES.contains(&pipeline_id.as_str()) {
        return Err(format!(
            "invalid pipeline `{pipeline_id}`: must be one of {ALLOWED_ATLAS_PIPELINES:?}"
        ));
    }

    // Verify the staged JSONL exists first — everything else needs it.
    let staging_path = staging_jsonl_path_for(&corpus_id);
    if !staging_path.exists() {
        return Err(format!(
            "no staged JSONL for `{corpus_id}` at {} — run `lc_ingest` first",
            staging_path.display()
        ));
    }

    // Idempotency guard. The onboarding flow may retry this call
    // after a transient error; treat "already pinned to this pipeline
    // with the same sample size" as success.
    let enrich_root = enrichment_root().join(&corpus_id);
    let config_path = enrich_root.join("config.json");
    let synthetic_source = enrich_root.join("source.txt");
    if config_path.exists() && synthetic_source.exists() {
        if let Ok(raw) = std::fs::read_to_string(&config_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                let pipeline_matches =
                    v.get("pipeline_id").and_then(|p| p.as_str()) == Some(pipeline_id.as_str());
                if pipeline_matches
                    && existing_source_matches_sample(&synthetic_source, sample_size)
                {
                    tracing::info!(
                        corpus_id = %corpus_id,
                        pipeline_id = %pipeline_id,
                        ?sample_size,
                        "enrich_init_for_local_corpus: already initialized consistently, no-op"
                    );
                    return read_sampled_documents(&staging_path, sample_size);
                }
            }
        }
    }

    // Build the synthetic plaintext. The enrichment root is where the
    // CLI will create `cache/`, `exemplars/`, `runs/`; we pre-create
    // it so `source.txt` has a home.
    std::fs::create_dir_all(&enrich_root)
        .map_err(|e| format!("mkdir {}: {e}", enrich_root.display()))?;
    let sampled = synthesize_plaintext_from_jsonl(&staging_path, &synthetic_source, sample_size)
        .map_err(|e| format!("synthesising plaintext source: {e}"))?;

    // Shell to `sovereign-cli enrich init --force`. `--force` is
    // correct here because we're re-scaffolding from scratch when the
    // pipeline id differs from what's on disk (the idempotency guard
    // above already handled the matching-pipeline case).
    const INIT_TIMEOUT: Duration = Duration::from_secs(30);
    let bin = resolve_sovereign_cli().ok_or_else(|| {
        "sovereign-cli not found on PATH or alongside the desktop binary. \
         Build via `cargo build --release -p sovereign-cli` and put it on \
         $PATH, or set SOVEREIGN_CLI=/abs/path/to/sovereign-cli."
            .to_string()
    })?;
    let fut = async {
        Command::new(&bin)
            .arg("enrich")
            .arg("init")
            .arg(&corpus_id)
            .arg("--source")
            .arg(&synthetic_source)
            .arg("--pipeline")
            .arg(&pipeline_id)
            .arg("--chapter-regex")
            .arg(r"(?m)^=====\s+.+\s+=====\s*$")
            .arg("--min-section-body-words")
            .arg("20")
            .arg("--force")
            .output()
            .await
            .map_err(|e| format!("spawning sovereign-cli: {e}"))
    };
    let output = timeout(INIT_TIMEOUT, fut).await.map_err(|_| {
        format!(
            "sovereign-cli enrich init timed out after {}s",
            INIT_TIMEOUT.as_secs()
        )
    })??;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "sovereign-cli enrich init exited with code {}: {stderr}",
            output.status.code().unwrap_or(-1)
        ));
    }
    tracing::info!(
        corpus_id = %corpus_id,
        pipeline_id = %pipeline_id,
        synthetic_source = %synthetic_source.display(),
        sample_size = sampled.titles.len(),
        total_docs = sampled.total,
        "enrich_init_for_local_corpus scaffolded"
    );
    Ok(sampled)
}

/// True when the existing synthetic `source.txt` already covers the
/// requested sample size. Uses section-header count as the cheap
/// proxy — rewriting the file is cheap relative to rerunning atlas
/// init, so a false negative here is fine.
fn existing_source_matches_sample(source_path: &Path, sample_size: Option<usize>) -> bool {
    use std::fs;
    use std::io::{BufRead, BufReader};
    let Ok(f) = fs::File::open(source_path) else {
        return false;
    };
    let header_count = BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter(|line| {
            let t = line.trim();
            t.starts_with("===== ") && t.ends_with(" =====")
        })
        .count();
    match sample_size {
        Some(n) => header_count == n,
        None => header_count > 0,
    }
}

/// Post-hoc read of the sampled-document titles for the "no-op when
/// already initialized" branch. Reads at most `sample_size` titles.
fn read_sampled_documents(
    jsonl_path: &Path,
    sample_size: Option<usize>,
) -> Result<SampledDocuments, String> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};
    let f = File::open(jsonl_path).map_err(|e| format!("reading {}: {e}", jsonl_path.display()))?;
    let reader = BufReader::new(f);
    let mut titles: Vec<String> = Vec::new();
    let mut total = 0usize;
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v): Result<serde_json::Value, _> = serde_json::from_str(&line) else {
            continue;
        };
        let content_nonempty = v
            .get("content")
            .and_then(|s| s.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if !content_nonempty {
            continue;
        }
        total += 1;
        if let Some(n) = sample_size {
            if titles.len() >= n {
                continue;
            }
        }
        let title = v
            .get("title")
            .and_then(|s| s.as_str())
            .unwrap_or("Untitled")
            .to_string();
        titles.push(title);
    }
    Ok(SampledDocuments { titles, total })
}

/// Read a local-corpus staged JSONL (one `{id,title,content,source_path}`
/// record per line) and write a single plaintext file with each
/// document bracketed by `===== <title> =====` headers.
///
/// When `sample_size` is `Some(n)`, only the first `n` records with
/// non-empty content are written to `out_path` — this is the
/// sample-first TTFV path. The returned `SampledDocuments.total`
/// always reflects every usable record in the JSONL, so the UI can
/// tell the user "atlas covers 5 of your 47 documents".
///
/// The regex `enrich_init_for_local_corpus` pins at init time
/// (`(?m)^=====\s+.+\s+=====\s*$`) matches those headers exactly. An
/// accidental `=====` inside a title is mangled to `ooooo` so the
/// section detector doesn't see it twice.
fn synthesize_plaintext_from_jsonl(
    jsonl_path: &Path,
    out_path: &Path,
    sample_size: Option<usize>,
) -> std::io::Result<SampledDocuments> {
    use std::fs::File;
    use std::io::{BufRead, BufReader, BufWriter, Write};

    let f = File::open(jsonl_path)?;
    let reader = BufReader::new(f);
    let mut writer = BufWriter::new(File::create(out_path)?);
    let mut titles: Vec<String> = Vec::new();
    let mut total = 0usize;
    let mut wrote_any = false;
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "synthesize_plaintext_from_jsonl: skipping bad line");
                continue;
            }
        };
        let title = v
            .get("title")
            .and_then(|s| s.as_str())
            .unwrap_or("Untitled");
        let content = v.get("content").and_then(|s| s.as_str()).unwrap_or("");
        if content.trim().is_empty() {
            continue;
        }
        total += 1;
        // When sampling, stop writing after `n` docs — but keep
        // counting so the caller knows how many total usable
        // records exist.
        if let Some(n) = sample_size {
            if titles.len() >= n {
                continue;
            }
        }
        let safe_title = title.replace("=====", "ooooo");
        writeln!(writer, "===== {safe_title} =====")?;
        writeln!(writer)?;
        writeln!(writer, "{}", content.trim_end())?;
        writeln!(writer)?;
        wrote_any = true;
        titles.push(title.to_string());
    }
    writer.flush()?;
    if !wrote_any {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "no usable records in {} — every line was empty or missing a non-empty `content`",
                jsonl_path.display()
            ),
        ));
    }
    Ok(SampledDocuments { titles, total })
}

// ─── Command: enrich_estimate ────────────────────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct EnrichEstimate {
    pub sections: usize,
    pub total_words: u64,
    pub est_tokens: u64,
    pub minutes_low: u32,
    pub minutes_high: u32,
}

/// Return a pre-run estimate of an atlas enrichment build.
///
/// Heuristic: atlas pipelines run 7–8 phases per section. Observed
/// wall-clock on M2 Max against `brothers_karamazov` ranges roughly
/// 1.5–3 min per section end-to-end (Phase 1 dominates; clustering +
/// resolution are O(total atoms), not per-section). This is
/// deliberately presented as a range, not a point estimate — the
/// onboarding UI uses it to set expectation, not to gate a decision.
///
/// Requires `enrich_init_for_local_corpus` to have run (chapters.json
/// is written at `enrich init` time).
#[tauri::command]
pub async fn enrich_estimate(corpus_id: String) -> Result<EnrichEstimate, String> {
    let chapters_path = index_root_for(&corpus_id).join("chapters.json");
    if !chapters_path.exists() {
        return Err(format!(
            "no chapter manifest for `{corpus_id}` at {} — run \
             `enrich_init_for_local_corpus` first",
            chapters_path.display()
        ));
    }
    let raw = std::fs::read_to_string(&chapters_path)
        .map_err(|e| format!("reading {}: {e}", chapters_path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("parsing chapters.json: {e}"))?;
    let chapters = v
        .get("chapters")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    let sections = chapters.len();
    let total_words: u64 = chapters
        .iter()
        .filter_map(|c| c.get("word_count").and_then(|w| w.as_u64()))
        .sum();
    // 4-char-per-token rule used by the pipeline's token counter
    // (`runner.rs:text.len()/4`) — keep the UI estimate consistent.
    // Avg word ~= 5 chars for English prose, so tokens ≈ words × 1.3.
    let est_tokens = (total_words as f64 * 1.3).round() as u64;
    // Range floor of 1 min so a single-section corpus doesn't display
    // "0 minutes".
    let minutes_low = ((sections as f32 * 1.5).ceil() as u32).max(1);
    let minutes_high = ((sections as f32 * 3.0).ceil() as u32).max(minutes_low + 1);
    Ok(EnrichEstimate {
        sections,
        total_words,
        est_tokens,
        minutes_low,
        minutes_high,
    })
}

// ─── Command: enrich_get_active_job ──────────────────────────────────

/// If an enrichment build is currently in flight for this corpus,
/// return the job_id + channel the UI can subscribe to. Enables the
/// "attach to existing build" path when a user navigates to a corpus
/// whose build was kicked off from another surface (e.g., onboarding
/// flow started it, user opens Settings mid-build).
#[derive(Debug, Serialize, Clone)]
pub struct ActiveEnrichJob {
    pub job_id: String,
    pub channel: String,
}

#[tauri::command]
pub async fn enrich_get_active_job(corpus_id: String) -> Result<Option<ActiveEnrichJob>, String> {
    let reg = registry()
        .lock()
        .map_err(|e| format!("registry lock: {e}"))?;
    match reg.job_id_by_corpus.get(&corpus_id) {
        Some(job_id) => Ok(Some(ActiveEnrichJob {
            channel: progress_channel(job_id),
            job_id: job_id.clone(),
        })),
        None => Ok(None),
    }
}

// ─── Command: enrich_get_starter_questions ───────────────────────────

#[derive(Debug, Serialize, Clone)]
pub struct StarterQuestion {
    pub text: String,
    pub atom_id: String,
    pub source_section: Option<String>,
    pub question_type: String,
}

/// Return up to `limit` starter questions mined from the atlas.
///
/// Heuristic (shipped question atoms lack a salience or `addressed_by`
/// field — verified against three live corpora). Ranking:
///
///   1. Length window 25..=220 chars (drops too-terse fragments and
///      run-on multi-clause questions).
///   2. Question-type preference, in order: Thematic, Interpretive,
///      Open, Factual, Rhetorical, Other.
///   3. Diversify by first `raised_at.chunk_id`: at most one question
///      per section in the returned set, as far as `limit` and corpus
///      size permit.
///
/// Returns an empty vec (NOT an error) when atoms.json is absent — the
/// UI branches on vec length to decide whether to fall back to
/// excerpt-based starters.
#[tauri::command]
pub async fn enrich_get_starter_questions(
    corpus_id: String,
    limit: usize,
) -> Result<Vec<StarterQuestion>, String> {
    let atlas_dir = index_root_for(&corpus_id).join("atlas");
    if !atlas_dir.exists() {
        return Ok(Vec::new());
    }
    let atoms_file = read_atlas_atoms(&atlas_dir)
        .map_err(|e| format!("reading atoms.json under {}: {e}", atlas_dir.display()))?;

    let starters = rank_starter_questions(&atoms_file.atoms, limit);
    tracing::debug!(
        corpus_id = %corpus_id,
        total_atoms = atoms_file.atoms.len(),
        question_atoms = atoms_file.atoms.iter().filter(|a| matches!(a, AtomEnvelope::Question(_))).count(),
        returned = starters.len(),
        "enrich_get_starter_questions"
    );
    Ok(starters)
}

/// Core ranker. Separated from the Tauri command so unit tests can
/// feed it synthetic atom slices without touching the filesystem.
fn rank_starter_questions(atoms: &[AtomEnvelope], limit: usize) -> Vec<StarterQuestion> {
    if limit == 0 {
        return Vec::new();
    }
    // Tier score — lower is better.
    fn tier(q_type: &str) -> u8 {
        match q_type {
            "thematic" => 0,
            "interpretive" => 1,
            "open" => 2,
            "factual" => 3,
            "rhetorical" => 4,
            _ => 5,
        }
    }
    // Collect candidates that pass the length + shape filters.
    let mut candidates: Vec<StarterQuestion> = atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Question(q) => {
                let text = q.content.trim();
                let char_count = text.chars().count();
                if !(25..=220).contains(&char_count) {
                    return None;
                }
                // Normalise trailing punctuation to a question mark.
                let cleaned = if text.ends_with('?') {
                    text.to_string()
                } else {
                    let stripped = text.trim_end_matches(['.', '!', ',', ';', ':']);
                    format!("{stripped}?")
                };
                let source_section = q
                    .raised_at
                    .first()
                    .map(|r| r.chunk_id.clone())
                    .filter(|s| !s.is_empty());
                Some(StarterQuestion {
                    text: cleaned,
                    atom_id: q.id.as_str().to_string(),
                    source_section,
                    question_type: q.question_type.as_str_repr().to_string(),
                })
            }
            _ => None,
        })
        .collect();
    // Stable sort by (tier, then atom_id) so ties resolve deterministically.
    candidates.sort_by(|a, b| {
        tier(&a.question_type)
            .cmp(&tier(&b.question_type))
            .then_with(|| a.atom_id.cmp(&b.atom_id))
    });
    // Round-robin diversify by source_section. First pass: pick one
    // per section in tier order. Second pass: fill remaining slots
    // from the leftover pool.
    let mut picked: Vec<StarterQuestion> = Vec::with_capacity(limit);
    let mut used_sections: HashSet<String> = HashSet::new();
    let mut leftovers: Vec<StarterQuestion> = Vec::new();
    for q in candidates {
        if picked.len() >= limit {
            leftovers.push(q);
            continue;
        }
        match &q.source_section {
            Some(section) if !used_sections.contains(section) => {
                used_sections.insert(section.clone());
                picked.push(q);
            }
            _ => leftovers.push(q),
        }
    }
    for q in leftovers {
        if picked.len() >= limit {
            break;
        }
        picked.push(q);
    }
    picked
}

// ─── Command: mark_first_run_complete / is_first_run ─────────────────

/// Marker file under `~/.sovereign/first_run_complete`. Absence
/// signals "user has not finished the onboarding corpus flow yet".
/// Content is an ISO-8601 timestamp so a future version can reason
/// about when onboarding completed (e.g. re-onboarding after a major
/// schema change).
fn first_run_marker_path() -> PathBuf {
    sovereign_root().join("first_run_complete")
}

#[tauri::command]
pub async fn is_first_run() -> Result<bool, String> {
    Ok(!first_run_marker_path().exists())
}

#[tauri::command]
pub async fn mark_first_run_complete() -> Result<(), String> {
    let path = first_run_marker_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let ts = chrono::Utc::now().to_rfc3339();
    std::fs::write(&path, &ts).map_err(|e| format!("writing {}: {e}", path.display()))?;
    tracing::info!(path = %path.display(), "first_run_complete marker written");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_channel_format_is_stable() {
        // The UI's `listen` call hardcodes this shape; a rename
        // would silently break the subscription.
        assert_eq!(progress_channel("abc-123"), "enrich://progress/abc-123");
    }

    #[test]
    fn enrichment_root_nests_under_dot_sovereign() {
        let root = enrichment_root();
        assert!(
            root.ends_with("enrichment"),
            "enrichment root should end with `enrichment`, got {}",
            root.display()
        );
        assert!(
            root.to_string_lossy().contains(".sovereign"),
            "enrichment root should be under ~/.sovereign, got {}",
            root.display()
        );
    }

    #[test]
    fn reserve_corpus_rejects_concurrent_build_on_same_corpus() {
        // The registry is module-scoped; scope this test to a
        // unique corpus id so it doesn't race with other tests
        // (the OnceLock means all test fns share one registry
        // instance).
        let corpus = "concurrency_guard_test_a";
        let _flag1 = reserve_corpus(corpus, "job-1").expect("first reserve");
        let err = reserve_corpus(corpus, "job-2").expect_err("second reserve should fail");
        assert!(
            err.contains("already running"),
            "error message should say `already running`, got: {err}"
        );
        // Release so the test doesn't leak the corpus into the
        // registry for subsequent tests in the same process.
        release_corpus(corpus, "job-1");
    }

    #[test]
    fn reserve_corpus_is_reusable_after_release() {
        // The guard must not leak — once `release_corpus` is
        // called, a second reserve should succeed. Otherwise a
        // failed build would lock the corpus permanently.
        let corpus = "concurrency_guard_test_b";
        let _f1 = reserve_corpus(corpus, "job-1").unwrap();
        release_corpus(corpus, "job-1");
        let _f2 = reserve_corpus(corpus, "job-2").expect("reserve after release");
        release_corpus(corpus, "job-2");
    }

    #[test]
    fn different_corpora_can_build_concurrently() {
        // The guard is per-corpus, NOT a global build mutex.
        // Landing 3 wants the operator to be able to run SEP
        // article builds in parallel (one-article-per-corpus
        // means each has its own cache directory).
        let a = "concurrency_guard_test_c_alpha";
        let b = "concurrency_guard_test_c_beta";
        let _f1 = reserve_corpus(a, "job-a").unwrap();
        let _f2 = reserve_corpus(b, "job-b").expect("different corpus should pass");
        release_corpus(a, "job-a");
        release_corpus(b, "job-b");
    }

    #[test]
    fn sep_slug_validator_accepts_real_plato_slugs() {
        assert!(is_valid_sep_slug("compatibilism"));
        assert!(is_valid_sep_slug("recursive-functions"));
        assert!(is_valid_sep_slug("18thGerman-preKant"));
        assert!(is_valid_sep_slug("emotion-Christian-tradition"));
    }

    #[test]
    fn sep_slug_validator_rejects_path_traversal_and_weird_chars() {
        assert!(!is_valid_sep_slug("../etc/passwd"));
        assert!(!is_valid_sep_slug("slug with spaces"));
        assert!(!is_valid_sep_slug("slug/with/slashes"));
        assert!(!is_valid_sep_slug("unicode-ümlaut"));
        assert!(!is_valid_sep_slug(""));
        assert!(!is_valid_sep_slug(&"a".repeat(65)));
    }

    #[test]
    fn cancel_flag_is_shared_between_register_and_fire() {
        // The store attaches a listener using the flag returned
        // by `reserve_corpus`; the cancel command flips the
        // same flag. Two clones of the `Arc<AtomicBool>` must
        // observe each other's writes.
        let corpus = "concurrency_guard_test_d";
        let flag_handle = reserve_corpus(corpus, "job-cancel").expect("reserve");
        // Fire via the registry path (what the Tauri command
        // does). Observe through the flag handle returned by
        // reserve.
        {
            let reg = registry().lock().unwrap();
            let stored = reg.cancel_flags.get("job-cancel").unwrap().clone();
            fire_cancellation(&stored);
        }
        assert!(
            flag_handle.load(std::sync::atomic::Ordering::SeqCst),
            "firing via the stored clone should be visible through the returned clone"
        );
        release_corpus(corpus, "job-cancel");
    }

    // ─── O1: reverse lookup + ranker + synthesizer tests ────────────

    #[test]
    fn reserve_corpus_populates_reverse_lookup_and_release_clears_it() {
        // `enrich_get_active_job` depends on job_id_by_corpus being
        // maintained symmetrically with `active_corpora`. A future
        // refactor could accidentally skip one side; pin the
        // contract.
        let corpus = "reverse_lookup_test_corpus";
        let job = "reverse-lookup-job";
        let _flag = reserve_corpus(corpus, job).expect("reserve");
        {
            let reg = registry().lock().unwrap();
            assert_eq!(
                reg.job_id_by_corpus.get(corpus).map(String::as_str),
                Some(job),
                "reverse lookup should map corpus → job_id"
            );
        }
        release_corpus(corpus, job);
        let reg = registry().lock().unwrap();
        assert!(
            !reg.job_id_by_corpus.contains_key(corpus),
            "release should clear the reverse lookup"
        );
    }

    #[test]
    fn release_corpus_does_not_wipe_reverse_entry_for_different_job() {
        // A stale release call on an already-replaced job shouldn't
        // knock out the current registration — otherwise a fast
        // cancel+restart sequence would lose the live job_id.
        let corpus = "reverse_lookup_test_stale_release";
        let _flag1 = reserve_corpus(corpus, "job-old").expect("reserve first");
        release_corpus(corpus, "job-old");
        let _flag2 = reserve_corpus(corpus, "job-new").expect("reserve second");
        // Simulate a late-arriving release for the old job.
        release_corpus(corpus, "job-old");
        let reg = registry().lock().unwrap();
        assert_eq!(
            reg.job_id_by_corpus.get(corpus).map(String::as_str),
            Some("job-new"),
            "stale release should not clobber the newer job mapping"
        );
        drop(reg);
        release_corpus(corpus, "job-new");
    }

    #[test]
    fn starter_question_ranker_prefers_thematic_then_interpretive() {
        use corpus_engine::enrichment::atlas::{
            AtomEnvelope, AtomId, ChunkRef, Question, ResolutionStatus,
        };
        use corpus_engine::enrichment::pipeline::{EnrichmentDepth, QuestionType};
        let mk = |id: usize, text: &str, qtype: QuestionType, section: &str| {
            AtomEnvelope::Question(Question {
                id: AtomId::question(id),
                content: text.into(),
                question_type: qtype,
                addressed_by: Vec::new(),
                raised_at: vec![ChunkRef::new(section.to_string(), None)],
                resolution_status: ResolutionStatus::Open,
                enrichment_depth: EnrichmentDepth::Extracted,
            })
        };
        let atoms = vec![
            mk(
                1,
                "What is the factual date of the encounter between the brothers?",
                QuestionType::Factual,
                "sec_0001",
            ),
            mk(
                2,
                "How does faith change when grief meets doubt across chapters?",
                QuestionType::Thematic,
                "sec_0002",
            ),
            mk(
                3,
                "Does the ending dissolve or resolve the central question posed here?",
                QuestionType::Interpretive,
                "sec_0003",
            ),
        ];
        let picks = rank_starter_questions(&atoms, 3);
        assert_eq!(picks.len(), 3, "all three should pass length gate");
        assert_eq!(picks[0].question_type, "thematic", "thematic wins tier 0");
        assert_eq!(
            picks[1].question_type, "interpretive",
            "interpretive wins tier 1"
        );
        assert_eq!(picks[2].question_type, "factual", "factual in tier 3");
    }

    #[test]
    fn starter_question_ranker_diversifies_by_section() {
        use corpus_engine::enrichment::atlas::{
            AtomEnvelope, AtomId, ChunkRef, Question, ResolutionStatus,
        };
        use corpus_engine::enrichment::pipeline::{EnrichmentDepth, QuestionType};
        let mk = |id: usize, text: &str, section: &str| {
            AtomEnvelope::Question(Question {
                id: AtomId::question(id),
                content: text.into(),
                question_type: QuestionType::Thematic,
                addressed_by: Vec::new(),
                raised_at: vec![ChunkRef::new(section.to_string(), None)],
                resolution_status: ResolutionStatus::Open,
                enrichment_depth: EnrichmentDepth::Extracted,
            })
        };
        // Three questions from the same section and two from different
        // sections. Limit=3 should pull at most one from sec_0001
        // before falling back to leftovers.
        let atoms = vec![
            mk(
                1,
                "A first long enough thematic question from section one opening?",
                "sec_0001",
            ),
            mk(
                2,
                "A second long enough thematic question from section one opening?",
                "sec_0001",
            ),
            mk(
                3,
                "A third long enough thematic question from section one opening?",
                "sec_0001",
            ),
            mk(
                4,
                "A long enough thematic question from section two probing meaning?",
                "sec_0002",
            ),
            mk(
                5,
                "A long enough thematic question from section three probing nuance?",
                "sec_0003",
            ),
        ];
        let picks = rank_starter_questions(&atoms, 3);
        let sections: Vec<Option<String>> =
            picks.iter().map(|p| p.source_section.clone()).collect();
        let distinct_sections: HashSet<_> = picks
            .iter()
            .filter_map(|p| p.source_section.clone())
            .collect();
        assert_eq!(picks.len(), 3);
        assert_eq!(
            distinct_sections.len(),
            3,
            "should cover three distinct sections before revisiting one; got {:?}",
            sections
        );
    }

    #[test]
    fn starter_question_ranker_rejects_too_short_and_too_long() {
        use corpus_engine::enrichment::atlas::{
            AtomEnvelope, AtomId, ChunkRef, Question, ResolutionStatus,
        };
        use corpus_engine::enrichment::pipeline::{EnrichmentDepth, QuestionType};
        let mk = |id: usize, text: String| {
            AtomEnvelope::Question(Question {
                id: AtomId::question(id),
                content: text,
                question_type: QuestionType::Thematic,
                addressed_by: Vec::new(),
                raised_at: vec![ChunkRef::new("sec_0001".to_string(), None)],
                resolution_status: ResolutionStatus::Open,
                enrichment_depth: EnrichmentDepth::Extracted,
            })
        };
        let atoms = vec![
            mk(1, "Why?".into()),   // too short
            mk(2, "a".repeat(300)), // too long
            mk(
                3,
                "What actually grounds a claim like this in the shipped corpus?".into(),
            ),
        ];
        let picks = rank_starter_questions(&atoms, 5);
        assert_eq!(picks.len(), 1, "only the middle-length question survives");
        assert!(picks[0].text.ends_with('?'));
    }

    #[test]
    fn starter_question_ranker_limit_zero_returns_empty() {
        let picks = rank_starter_questions(&[], 0);
        assert!(picks.is_empty());
    }

    #[test]
    fn synthesize_plaintext_from_jsonl_emits_section_headers_and_escapes_title() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let jsonl = dir.path().join("in.jsonl");
        let out = dir.path().join("source.txt");
        let content = [
            r#"{"id":"a","title":"The First Doc","content":"Alpha body.","source_path":"a.md"}"#,
            r#"{"id":"b","title":"Edge ===== Case","content":"Beta body.","source_path":"b.md"}"#,
            r#""#, // blank line — skipped
            r#"{"id":"c","title":"Empty","content":"","source_path":"c.md"}"#, // skipped
        ]
        .join("\n");
        std::fs::write(&jsonl, content).unwrap();
        let result = synthesize_plaintext_from_jsonl(&jsonl, &out, None).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(
            text.contains("===== The First Doc ====="),
            "header for first doc: {text}"
        );
        assert!(
            text.contains("===== Edge ooooo Case ====="),
            "title containing `=====` should be mangled to ooooo: {text}"
        );
        assert!(text.contains("Alpha body."));
        assert!(text.contains("Beta body."));
        // The empty-content record is dropped entirely (no header).
        assert!(
            !text.contains("===== Empty ====="),
            "empty content should not produce a header"
        );
        // SampledDocuments reflects only the two usable records.
        assert_eq!(result.titles.len(), 2);
        assert_eq!(result.total, 2);
        assert_eq!(result.titles[0], "The First Doc");
        assert_eq!(result.titles[1], "Edge ===== Case");
    }

    #[test]
    fn synthesize_plaintext_from_jsonl_errors_when_no_records_usable() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let jsonl = dir.path().join("empty.jsonl");
        let out = dir.path().join("source.txt");
        // Only blanks + empty-content records.
        std::fs::write(
            &jsonl,
            "\n{\"id\":\"a\",\"title\":\"T\",\"content\":\"   \",\"source_path\":\"a.md\"}\n",
        )
        .unwrap();
        let err = synthesize_plaintext_from_jsonl(&jsonl, &out, None).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn synthesize_plaintext_from_jsonl_respects_sample_size_and_records_total() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let jsonl = dir.path().join("in.jsonl");
        let out = dir.path().join("source.txt");
        // Five usable records; ask for a sample of 2. `titles` should
        // contain the first two, `total` should still be 5 so the UI
        // knows "atlas covers 2 of 5".
        let lines: Vec<String> = (0..5)
            .map(|i| {
                format!(
                    r#"{{"id":"d{i}","title":"Doc {i}","content":"Body {i}.","source_path":"d{i}.md"}}"#
                )
            })
            .collect();
        std::fs::write(&jsonl, lines.join("\n")).unwrap();
        let result = synthesize_plaintext_from_jsonl(&jsonl, &out, Some(2)).unwrap();
        assert_eq!(
            result.titles,
            vec!["Doc 0".to_string(), "Doc 1".to_string()]
        );
        assert_eq!(
            result.total, 5,
            "total should include beyond-sample records"
        );
        let text = std::fs::read_to_string(&out).unwrap();
        // Only Doc 0 + Doc 1 headers exist in the source.
        assert_eq!(text.matches("===== Doc ").count(), 2);
        assert!(!text.contains("Doc 2"));
        assert!(!text.contains("Doc 3"));
    }

    #[test]
    fn allowed_atlas_pipelines_matches_build_time_contract() {
        // build.rs requires `pipeline_id.ends_with("_atlas")`. Every
        // allowed id must honour that so the downstream build won't
        // reject what our UI produced.
        for id in ALLOWED_ATLAS_PIPELINES {
            assert!(
                id.ends_with("_atlas"),
                "pipeline id `{id}` must end with `_atlas` to match build.rs"
            );
        }
    }
}
