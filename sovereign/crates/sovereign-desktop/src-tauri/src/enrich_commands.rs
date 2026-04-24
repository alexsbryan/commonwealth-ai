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
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use sovereign_tools::enrich::{
    fire_cancellation, new_cancellation_flag, run_enrich_build, CancellationFlag,
    EnrichBuildConfig, EnrichProgressFn,
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
}

static REGISTRY: OnceLock<Mutex<EnrichJobRegistry>> = OnceLock::new();

fn registry() -> &'static Mutex<EnrichJobRegistry> {
    REGISTRY.get_or_init(|| Mutex::new(EnrichJobRegistry::default()))
}

/// Attempt to reserve a corpus for a build. Returns the
/// cancellation flag for the new job on success, `Err` if the
/// corpus already has a build in flight.
fn reserve_corpus(
    corpus_id: &str,
    job_id: &str,
) -> Result<CancellationFlag, String> {
    let mut reg = registry().lock().expect("enrich job registry lock");
    if reg.active_corpora.contains(corpus_id) {
        return Err(format!(
            "A build is already running for `{corpus_id}`. Cancel it first or wait for it to finish."
        ));
    }
    let flag = new_cancellation_flag();
    reg.active_corpora.insert(corpus_id.to_string());
    reg.cancel_flags.insert(job_id.to_string(), flag.clone());
    Ok(flag)
}

/// Drop a job's reservation + cancel flag. Safe to call multiple
/// times (the second call is a no-op); the subprocess-spawned
/// task calls it on every exit path.
fn release_corpus(corpus_id: &str, job_id: &str) {
    let mut reg = registry().lock().expect("enrich job registry lock");
    reg.active_corpora.remove(corpus_id);
    reg.cancel_flags.remove(job_id);
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
        let result = run_enrich_build(
            &corpus_id_for_task,
            config,
            Some(progress.clone()),
        )
        .await;

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
    let reg = registry().lock().map_err(|e| format!("registry lock: {e}"))?;
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
pub async fn enrich_errors(
    corpus_id: String,
) -> Result<serde_json::Value, String> {
    use tokio::process::Command;
    use tokio::time::{timeout, Duration};

    /// Hard cap on the subprocess call. The aggregator is
    /// expected to finish in well under a second for any real
    /// corpus (reads JSON from disk, groups in-memory); 10s is
    /// comfortably over the P99 while still bounding "something
    /// is wrong" scenarios like a disk stall or a filesystem
    /// hang.
    const ERRORS_TIMEOUT: Duration = Duration::from_secs(10);

    let fut = async {
        Command::new("sovereign-cli")
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
            "invalid SEP slug `{slug}`: slugs must match `[a-z0-9-]+` \
             (lowercase, digits, or hyphens only — SEP category slugs \
             follow this shape on plato.stanford.edu)"
        ));
    }

    const SEP_INGEST_TIMEOUT: Duration = Duration::from_secs(60);

    let mut cmd = Command::new("sovereign-cli");
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
/// `recursive-functions`, `18thgerman-prekant`, …). Reject
/// everything else — defensive clamp before a CLI shell-out
/// that writes a file named after the slug.
fn is_valid_sep_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > 64 {
        return false;
    }
    slug.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
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
    let entries = std::fs::read_dir(&root)
        .map_err(|e| format!("reading {}: {e}", root.display()))?;
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
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".sovereign")
        .join("enrichment")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_channel_format_is_stable() {
        // The UI's `listen` call hardcodes this shape; a rename
        // would silently break the subscription.
        assert_eq!(
            progress_channel("abc-123"),
            "enrich://progress/abc-123"
        );
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
        let err = reserve_corpus(corpus, "job-2")
            .expect_err("second reserve should fail");
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
        let _f2 =
            reserve_corpus(corpus, "job-2").expect("reserve after release");
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
        let _f2 =
            reserve_corpus(b, "job-b").expect("different corpus should pass");
        release_corpus(a, "job-a");
        release_corpus(b, "job-b");
    }

    #[test]
    fn sep_slug_validator_accepts_real_plato_slugs() {
        assert!(is_valid_sep_slug("compatibilism"));
        assert!(is_valid_sep_slug("recursive-functions"));
        assert!(is_valid_sep_slug("18thgerman-prekant"));
    }

    #[test]
    fn sep_slug_validator_rejects_path_traversal_and_weird_chars() {
        assert!(!is_valid_sep_slug("../etc/passwd"));
        assert!(!is_valid_sep_slug("slug with spaces"));
        assert!(!is_valid_sep_slug("slug/with/slashes"));
        assert!(!is_valid_sep_slug("UPPERCASE"));
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
        let flag_handle =
            reserve_corpus(corpus, "job-cancel").expect("reserve");
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
}
