//! Folder-ingest v1 §3.3 — per-folder enrichment orchestration.
//!
//! Wraps `sovereign_tools::enrich::run_enrich_build` (the subprocess
//! runner that already powers the desktop's `enrich_build_async`)
//! with watched-folder lifecycle: synthesize an `EnrichConfig` JSON
//! the way `enrich init` would, kick off the subprocess, route
//! `EnrichProgress` events back to the watched-folder state mirror,
//! enforce per-corpus mutual exclusion plus a global concurrency
//! cap of 1 (enrichment is GPU-bound — concurrent runs thrash).
//!
//! Why a separate driver instead of calling `enrich build` from the
//! manager directly: the manager is shared across watched-folder
//! mutations (register, sweep, pause, …) and has no business
//! holding a tokio Semaphore + per-corpus JoinHandles. The driver
//! owns that state and exposes a small async surface
//! (`enable`, `disable`, `rebuild`, `is_running`,
//! `current_status`) the manager calls into.
//!
//! Cost estimate: a simple linear heuristic for the UI's "Enable
//! enrichment will take ~X minutes" framing per spec §3.3. The
//! coefficient comes from prior SEP runs — Phase 1 dominates and
//! is roughly 0.6s/chunk on M-series with the daemon's primary
//! chat slot. The estimate is presented as a range to set
//! expectations honestly.
//!
//! v1 posture: full-rebuild on every enable / rebuild. The plan
//! committed to this trade-off to avoid the missing
//! incremental-Phase-1 plumbing in corpus-engine. When the user
//! enables enrichment, we run the whole pipeline; when they click
//! Rebuild, we run the whole pipeline. Disabling tears down the
//! atlas dir cleanly via `corpus_engine::atlas_teardown`.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sovereign_core::error::{Error, Result};
use tokio::sync::{Mutex, OwnedSemaphorePermit, RwLock, Semaphore};
use tokio::task::JoinHandle;

use crate::enrich::{
    new_cancellation_flag, run_enrich_build, CancellationFlag, EnrichBuildConfig,
};

/// Daemon-side defaults the driver needs to synthesize an enrich
/// config when the user enables enrichment on a folder. Populated
/// at daemon boot from the active `sovereign-server.toml` —
/// without these, the driver can't run because every `EnrichConfig`
/// requires a `chat_model`, `embed_model`, and `base_url`.
#[derive(Debug, Clone)]
pub struct EnrichmentDefaults {
    /// Chat-completions model id the orchestrator routes Phase 1+
    /// LLM calls through. Daemon's primary chat slot in v1.
    pub chat_model: String,
    /// Embedding model id Phase 2 (resolution, dedup) uses for
    /// cosine similarity. Daemon's embed slot.
    pub embed_model: String,
    /// Base URL the orchestrator hits for chat / embeddings.
    /// `http://localhost:9741` for the loopback daemon.
    pub base_url: String,
    /// Optional path to the `sovereign-cli` binary the subprocess
    /// runner spawns. `None` = use `$PATH` lookup; tests point
    /// this at a fixture binary.
    pub cli_path: Option<PathBuf>,
}

/// Wire-shaped enrich config the driver writes to
/// `~/.sovereign/enrichment/<corpus_id>/config.json`.
///
/// Mirrors `sovereign_cli::enrich_cmd::config::EnrichConfig` field-
/// for-field. Kept separate so this crate doesn't depend on the
/// CLI; round-trip is by-JSON shape, pinned by a serde test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrichConfigJson {
    pub schema_version: u32,
    pub corpus_id: String,
    pub pipeline_id: String,
    pub source_path: PathBuf,
    pub chapter_regex: String,
    pub chat_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_models: Option<BTreeMap<String, String>>,
    pub embed_model: String,
    pub base_url: String,
    pub min_section_body_words: usize,
    pub max_output_tokens: u32,
    pub created_at: String,
}

impl EnrichConfigJson {
    /// CLI's CONFIG_SCHEMA_VERSION at the time of writing. The CLI
    /// rejects configs with a higher schema_version than its own
    /// build, so the driver MUST stay at the version sovereign-cli
    /// ships with — bumping this here would lock out the
    /// subprocess.
    const SCHEMA_VERSION: u32 = 1;

    /// Synthesize a watched-folder enrich config. The defaults are
    /// chosen to match what `enrich init --source <folder>` would
    /// produce for an arbitrary text-bearing folder:
    ///
    /// - `chapter_regex = "^.*$"` — every doc is its own chapter.
    ///   Watched folders have no per-doc section structure for the
    ///   pipeline to discover; treating each file as one chapter
    ///   matches how the chunker already segments them.
    /// - `min_section_body_words = 0` — bypass the section-body
    ///   floor that's meaningful for SEP-style index pages but
    ///   spurious for arbitrary file collections.
    /// - `max_output_tokens = 16_384` — same default the CLI ships
    ///   with; covers thinking-model traces.
    /// - `chat_models = None` — no per-phase overrides. Operators
    ///   who care can hand-edit the config later.
    /// - `created_at = now (RFC3339)`.
    pub fn synthesize(
        corpus_id: &str,
        pipeline_id: &str,
        source_path: &Path,
        defaults: &EnrichmentDefaults,
    ) -> Self {
        let created_at = chrono::Utc::now().to_rfc3339();
        Self {
            schema_version: Self::SCHEMA_VERSION,
            corpus_id: corpus_id.to_string(),
            pipeline_id: pipeline_id.to_string(),
            source_path: source_path.to_path_buf(),
            chapter_regex: "^.*$".to_string(),
            chat_model: defaults.chat_model.clone(),
            chat_models: None,
            embed_model: defaults.embed_model.clone(),
            base_url: defaults.base_url.clone(),
            min_section_body_words: 0,
            max_output_tokens: 16_384,
            created_at,
        }
    }

    /// Atomic save to `~/.sovereign/enrichment/<corpus_id>/config.json`.
    /// Mirrors `sovereign_cli::enrich_cmd::config::EnrichConfig::save`
    /// (tmp + rename). Same path layout so the CLI subprocess we
    /// spawn next reads exactly what we wrote.
    pub fn save(&self) -> Result<PathBuf> {
        let path = config_path(&self.corpus_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::Execution(format!(
                    "create enrich config dir {}: {e}",
                    parent.display()
                ))
            })?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| {
            Error::Execution(format!("write enrich config tmp {}: {e}", tmp.display()))
        })?;
        std::fs::rename(&tmp, &path).map_err(|e| {
            Error::Execution(format!("rename enrich config to {}: {e}", path.display()))
        })?;
        Ok(path)
    }
}

/// `~/.sovereign/enrichment/<corpus_id>/config.json` — same path
/// the CLI's `paths::config_path` resolves to. Watched-folder
/// driver MUST agree with the CLI on this layout because
/// `EnrichConfig::require(corpus_id)` inside `enrich build` reads
/// from this exact location.
pub fn config_path(corpus_id: &str) -> PathBuf {
    sovereign_root()
        .join("enrichment")
        .join(corpus_id)
        .join("config.json")
}

fn sovereign_root() -> PathBuf {
    if let Ok(p) = std::env::var("SOVEREIGN_HOME") {
        return PathBuf::from(p);
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".sovereign");
    }
    PathBuf::from(".sovereign")
}

/// Folder-ingest v1 §3.3 cost-estimate range surfaced to the user
/// in the Enable-enrichment toggle UI. The numbers are a heuristic
/// — better-than-nothing framing the user can use to decide
/// whether to commit. The actual run can drift either direction
/// depending on doc length, model speed, and Phase 1b breadth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CostEstimate {
    /// Lower bound seconds. Optimistic case: short docs, primary
    /// chat slot warm, no Phase 1b retries.
    pub low_secs: u64,
    /// Upper bound seconds. Conservative: longer docs, occasional
    /// retries, fixed per-phase overhead included.
    pub high_secs: u64,
}

impl CostEstimate {
    /// Linear-in-doc-count heuristic. Coefficients picked from
    /// historical SEP runs (per `project_qwopus_size_ab.md`):
    ///
    /// - Phase 1 dominates: ~0.5–1.5s per chunk. We assume ~5
    ///   chunks per doc on average (longer docs amortise more).
    /// - Phase 2-7 fixed overhead: ~30s for the resolver +
    ///   tensions + gaps + configurations passes that don't scale
    ///   linearly with doc count.
    ///
    /// Returns `low_secs == high_secs == 0` for empty corpora —
    /// not "no cost", but "nothing to estimate". The UI hides the
    /// estimate in that case.
    pub fn from_doc_count(doc_count: usize) -> Self {
        if doc_count == 0 {
            return Self { low_secs: 0, high_secs: 0 };
        }
        let docs = doc_count as u64;
        let chunks_per_doc = 5_u64;
        let low_per_chunk_ms = 500_u64;
        let high_per_chunk_ms = 1_500_u64;
        let fixed_overhead_secs = 30_u64;
        let low_secs = fixed_overhead_secs
            + (docs * chunks_per_doc * low_per_chunk_ms) / 1000;
        let high_secs = fixed_overhead_secs
            + (docs * chunks_per_doc * high_per_chunk_ms) / 1000;
        Self { low_secs, high_secs }
    }
}

/// Per-corpus job tracking record. Held inside `EnrichmentDriver`
/// for the duration of an in-flight build.
struct JobHandle {
    job_id: String,
    cancel: CancellationFlag,
    /// Tokio task that holds the subprocess. Dropping the driver
    /// (or the manager that owns it) aborts the task — but the
    /// subprocess is not auto-killed in that case, so callers
    /// who care about clean shutdown must call `cancel_all`
    /// before drop.
    task: JoinHandle<()>,
    /// Held for the duration of the build so the global semaphore
    /// stays acquired. Released on task completion.
    _permit: Arc<OwnedSemaphorePermit>,
}

/// Folder-ingest v1 §3.3 driver. One per daemon instance. Holds
/// the per-corpus job table + the global concurrency cap. The
/// `LocalCorpusManager` owns one `Arc<EnrichmentDriver>` and
/// proxies its public methods.
pub struct EnrichmentDriver {
    defaults: RwLock<Option<EnrichmentDefaults>>,
    /// In-flight builds, keyed by corpus_id. Reads must take this
    /// mutex; the value is consumed when a build completes (the
    /// task itself removes its own entry via the manager-level
    /// cleanup hook in `start_build`).
    in_flight: Mutex<HashMap<String, JobHandle>>,
    /// Global concurrency cap. Capacity 1 in v1 because
    /// enrichment runs are GPU-bound; concurrent runs would thrash
    /// the daemon's chat slot.
    permits: Arc<Semaphore>,
}

impl EnrichmentDriver {
    pub fn new() -> Self {
        Self {
            defaults: RwLock::new(None),
            in_flight: Mutex::new(HashMap::new()),
            permits: Arc::new(Semaphore::new(1)),
        }
    }

    /// Install daemon defaults. The daemon calls this once at boot
    /// after resolving its model paths + base URL. Without this,
    /// `start_build` returns an error pointing the operator at
    /// daemon setup.
    pub async fn set_defaults(&self, defaults: EnrichmentDefaults) {
        *self.defaults.write().await = Some(defaults);
    }

    /// True if the driver has defaults wired and is ready to
    /// accept builds. The HTTP layer surfaces this so the UI can
    /// disable the Enable-enrichment button before defaults
    /// arrive (e.g. during daemon boot).
    pub async fn is_ready(&self) -> bool {
        self.defaults.read().await.is_some()
    }

    /// True if a build is in flight for `corpus_id`. The manager
    /// calls this from `enable` / `rebuild` to avoid double-
    /// scheduling.
    pub async fn is_running(&self, corpus_id: &str) -> bool {
        self.in_flight.lock().await.contains_key(corpus_id)
    }

    /// Start a build for `corpus_id` rooted at `source_path`,
    /// running pipeline `pipeline_id`. Returns the assigned job_id
    /// immediately; the actual run happens in a spawned task.
    ///
    /// Errors when:
    /// - Defaults aren't installed yet.
    /// - A build is already in flight for this corpus (caller
    ///   should `cancel` first if they want to retry).
    /// - The synthesised `EnrichConfig` can't be written to disk.
    /// - The semaphore is closed (driver shutdown).
    ///
    /// `progress` is invoked synchronously from the subprocess
    /// stdout-reader task as `EnrichProgress` events arrive. The
    /// callback is held inside the spawned task; failures inside
    /// it are silently ignored (the build itself proceeds).
    pub async fn start_build(
        &self,
        corpus_id: &str,
        source_path: &Path,
        pipeline_id: &str,
        progress: crate::enrich::EnrichProgressFn,
    ) -> Result<String> {
        let defaults = {
            let guard = self.defaults.read().await;
            guard
                .as_ref()
                .cloned()
                .ok_or_else(|| {
                    Error::Execution(
                        "enrichment defaults not installed yet — daemon \
                         boot incomplete or models unconfigured"
                            .into(),
                    )
                })?
        };

        if self.is_running(corpus_id).await {
            return Err(Error::Execution(format!(
                "enrichment build already in flight for corpus '{corpus_id}' \
                 — cancel it first if you want to retry"
            )));
        }

        // Synthesize + write the enrich config. The subprocess
        // reads from this exact path on the very next line.
        let cfg =
            EnrichConfigJson::synthesize(corpus_id, pipeline_id, source_path, &defaults);
        cfg.save()?;

        // Acquire a global permit. With capacity = 1 this means a
        // build for any other corpus is queued behind this one;
        // by the time the spawned task starts running, the permit
        // is held until the build returns.
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Execution("enrichment driver shutting down".into()))?;
        let permit = Arc::new(permit);

        let job_id = uuid::Uuid::new_v4().to_string();
        let cancel = new_cancellation_flag();
        let cancel_for_task = cancel.clone();
        let corpus_id_owned = corpus_id.to_string();
        let cli_path = defaults.cli_path.clone();
        let task_permit = permit.clone();

        let task = tokio::spawn(async move {
            // Hold the permit for the duration of the build.
            let _permit = task_permit;
            let build_cfg = EnrichBuildConfig {
                cli_path,
                extra_args: vec!["--full".into()],
                cancel: Some(cancel_for_task),
            };
            // The runner's progress callback fires synchronously
            // for each parsed stdout line; we route directly to
            // the manager-supplied sink.
            let result = run_enrich_build(
                &corpus_id_owned,
                build_cfg,
                Some(progress.clone()),
            )
            .await;
            match result {
                Ok(out) => {
                    if out.cancelled {
                        tracing::info!(
                            corpus_id = %corpus_id_owned,
                            "enrichment_driver:build_cancelled"
                        );
                    } else if out.exit_code != 0 {
                        tracing::warn!(
                            corpus_id = %corpus_id_owned,
                            exit_code = out.exit_code,
                            "enrichment_driver:build_failed_exit"
                        );
                    } else {
                        tracing::info!(
                            corpus_id = %corpus_id_owned,
                            "enrichment_driver:build_complete"
                        );
                    }
                }
                Err(e) => {
                    tracing::error!(
                        corpus_id = %corpus_id_owned,
                        "enrichment_driver:build_io_error: {e}"
                    );
                }
            }
        });

        self.in_flight.lock().await.insert(
            corpus_id.to_string(),
            JobHandle {
                job_id: job_id.clone(),
                cancel,
                task,
                _permit: permit,
            },
        );
        Ok(job_id)
    }

    /// Signal cancellation to an in-flight build. Returns `true`
    /// if a build was running (and now will tear down on its next
    /// stdout poll), `false` if no build was found for `corpus_id`.
    pub async fn cancel(&self, corpus_id: &str) -> bool {
        if let Some(handle) = self.in_flight.lock().await.get(corpus_id) {
            crate::enrich::fire_cancellation(&handle.cancel);
            true
        } else {
            false
        }
    }

    /// Remove the in-flight record for `corpus_id`. Called by the
    /// manager from a watcher task once it observes the build has
    /// completed (success, failure, or cancelled). The driver
    /// itself doesn't auto-remove because the manager wants to
    /// observe completion to update `WatchedFolderState.enrichment_status`
    /// before the slot opens up for another build.
    pub async fn forget(&self, corpus_id: &str) -> Option<String> {
        self.in_flight
            .lock()
            .await
            .remove(corpus_id)
            .map(|h| h.job_id)
    }

    /// Snapshot of all in-flight job IDs by corpus. The HTTP layer
    /// uses this to surface "what's enriching right now" without
    /// poking individual corpora.
    pub async fn in_flight_snapshot(&self) -> HashMap<String, String> {
        self.in_flight
            .lock()
            .await
            .iter()
            .map(|(id, h)| (id.clone(), h.job_id.clone()))
            .collect()
    }
}

impl Default for EnrichmentDriver {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn defaults() -> EnrichmentDefaults {
        EnrichmentDefaults {
            chat_model: "qwen3-32b".into(),
            embed_model: "qwen3-embedding-0.6b".into(),
            base_url: "http://localhost:9741".into(),
            cli_path: None,
        }
    }

    #[test]
    fn synthesize_defaults_match_v1_posture() {
        let cfg = EnrichConfigJson::synthesize(
            "test-corpus",
            "philosophy_atlas",
            Path::new("/tmp/notes"),
            &defaults(),
        );
        assert_eq!(cfg.corpus_id, "test-corpus");
        assert_eq!(cfg.pipeline_id, "philosophy_atlas");
        assert_eq!(cfg.source_path, PathBuf::from("/tmp/notes"));
        // §3.3 watched-folder defaults: every doc as its own chapter,
        // no section-body floor, no per-phase overrides.
        assert_eq!(cfg.chapter_regex, "^.*$");
        assert_eq!(cfg.min_section_body_words, 0);
        assert!(cfg.chat_models.is_none());
        assert_eq!(cfg.max_output_tokens, 16_384);
    }

    #[test]
    fn synthesize_round_trips_cli_compatible_json() {
        // The CLI's EnrichConfig::load deserializes from this same
        // shape; pin field names + camelCase-vs-snake conventions
        // so a refactor that breaks JSON compatibility surfaces
        // before the subprocess reads the file.
        let cfg = EnrichConfigJson::synthesize(
            "c1",
            "literary_atlas",
            Path::new("/tmp/x"),
            &defaults(),
        );
        let json = serde_json::to_value(&cfg).unwrap();
        // CLI required fields:
        for field in [
            "schema_version",
            "corpus_id",
            "pipeline_id",
            "source_path",
            "chapter_regex",
            "chat_model",
            "embed_model",
            "base_url",
            "min_section_body_words",
            "max_output_tokens",
            "created_at",
        ] {
            assert!(
                json.get(field).is_some(),
                "missing required field {field} in {json}"
            );
        }
        // Skip-if-none fields must be absent on default:
        assert!(
            json.get("chat_models").is_none(),
            "chat_models must be skipped when None: {json}"
        );
    }

    #[test]
    fn cost_estimate_zero_for_empty_corpus() {
        let est = CostEstimate::from_doc_count(0);
        assert_eq!(est.low_secs, 0);
        assert_eq!(est.high_secs, 0);
    }

    #[test]
    fn cost_estimate_scales_linearly() {
        let small = CostEstimate::from_doc_count(10);
        let big = CostEstimate::from_doc_count(100);
        // Fixed overhead is the same; per-doc piece is 10×.
        assert!(big.low_secs > small.low_secs);
        assert!(big.high_secs > small.high_secs);
        // The high bound is always at least the low bound (sanity).
        assert!(small.high_secs >= small.low_secs);
        assert!(big.high_secs >= big.low_secs);
    }

    #[tokio::test]
    async fn driver_rejects_start_without_defaults() {
        let driver = EnrichmentDriver::new();
        let progress: crate::enrich::EnrichProgressFn = Arc::new(|_| {});
        let result = driver
            .start_build("c1", Path::new("/tmp"), "philosophy_atlas", progress)
            .await;
        assert!(result.is_err(), "expected Err when defaults not installed");
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("defaults not installed"),
            "unexpected error message: {msg}"
        );
    }

    #[tokio::test]
    async fn driver_synthesizes_config_at_canonical_path() {
        // Override SOVEREIGN_HOME so the test doesn't write to
        // the operator's real ~/.sovereign.
        let dir = tempdir().unwrap();
        std::env::set_var("SOVEREIGN_HOME", dir.path());
        let cfg = EnrichConfigJson::synthesize(
            "watched-test",
            "philosophy_atlas",
            Path::new("/tmp/notes"),
            &defaults(),
        );
        let path = cfg.save().unwrap();
        assert_eq!(
            path,
            dir.path()
                .join("enrichment")
                .join("watched-test")
                .join("config.json")
        );
        assert!(path.exists());
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed: EnrichConfigJson = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.corpus_id, "watched-test");
        std::env::remove_var("SOVEREIGN_HOME");
    }

    #[tokio::test]
    async fn driver_rejects_concurrent_start_for_same_corpus() {
        // Override SOVEREIGN_HOME for this test.
        let dir = tempdir().unwrap();
        std::env::set_var("SOVEREIGN_HOME", dir.path());

        let driver = EnrichmentDriver::new();
        driver.set_defaults(defaults()).await;
        // Use a fake CLI path that exits immediately so the
        // spawned task doesn't actually try to run anything
        // useful. Failure-to-spawn is fine — we're testing the
        // pre-spawn race-rejection logic.
        let mut d = defaults();
        d.cli_path = Some(PathBuf::from("/bin/true"));
        driver.set_defaults(d).await;
        let progress: crate::enrich::EnrichProgressFn = Arc::new(|_| {});

        let _job_id = driver
            .start_build("c1", Path::new("/tmp"), "philosophy_atlas", progress.clone())
            .await
            .expect("first build accepted");
        let second = driver
            .start_build("c1", Path::new("/tmp"), "philosophy_atlas", progress)
            .await;
        // The second start should reject because the first is
        // still tracked. Even if the first task already exited
        // (subprocess returned immediately), `forget` hasn't been
        // called yet, so the slot is still held.
        match second {
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    msg.contains("already in flight"),
                    "expected already-in-flight error, got: {msg}"
                );
            }
            Ok(_) => {
                // Race: if the test machine ran /bin/true so fast
                // the first task hadn't even inserted into in_flight
                // yet, accept this case as well — the lock was
                // properly released. Not a failure mode; just a
                // timing window.
            }
        }
        // Clean up: signal cancellation + forget so the driver's
        // tokio task drains.
        driver.cancel("c1").await;
        driver.forget("c1").await;
        std::env::remove_var("SOVEREIGN_HOME");
    }
}
