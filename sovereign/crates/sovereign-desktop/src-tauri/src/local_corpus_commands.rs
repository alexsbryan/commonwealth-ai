// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri command surface for local corpora (Folder Drop + Obsidian).
//!
//! One job_id-scoped event channel per invocation:
//! `local-corpus://progress/{job_id}`. The UI listens with
//! `listen<LocalCorpusProgress>(channel, handler)`.
//!
//! Commands are thin — they translate TS-friendly shapes into
//! local-corpus calls and forward progress events via
//! `AppHandle::emit`. All heavy lifting happens in `sovereign-tools`.
//!
//! # Where the manager lives (sv-surface D5, completed in D8)
//!
//! Eleven commands hold NO manager: `lc_list`, `lc_remove`,
//! `lc_incomplete_jobs`, `lc_check_git`, `lc_list_snapshots`,
//! `lc_rollback`, `lc_clean`, `lc_search` (D5) and now `lc_ingest`,
//! `lc_cancel` and `lc_ocr_available` (D8) are one call each onto
//! `sovereign_mesh::lc_http`'s `/internal/corpus/local/` routes over
//! [`lc_client`], and so are the config reads inside `lc_ingest` and
//! `lc_enrich_now`. Return types are unchanged, so the webview sees the
//! same bytes. Two defaults moved DOWN to the route: `lc_search`'s 10 and
//! `lc_get`'s absence semantics.
//!
//! THE RULE THAT DECIDED WHICH ONES CROSSED, because it is not the
//! route list: a command crosses when its answer is a function of the
//! REGISTRY ON DISK — state both managers see. A command stays when its
//! answer is a function of ONE manager instance's in-memory state and
//! the producer of that state has not crossed. In Local mode there is
//! only one instance (`state.rs` hands its manager to
//! `WatchedSubsystem::install`, so `watched_folder_runtime::manager()`
//! IS this manager) and the distinction is invisible; in attach mode
//! there are two, and crossing a consumer while its producer stays is
//! how a working pane starts answering "not found".
//!
//! D8 crossed the PRODUCER the last three stays were pinned to — the
//! bespoke ingest job — and took them with it:
//!
//! | Crossed in D8 | In-memory state it read | Producer, now daemon-side |
//! |---|---|---|
//! | `lc_cancel` | the engine's cancellation registry | the ingest job |
//! | `lc_ocr_available` | an instance's `OcrCtx` | the ingest job's OCR arm |
//!
//! The cluster pair crossed on 2026-09-11: `lc_cluster` is now `POST
//! /internal/corpus/local/{c}/cluster` (a job, followed over
//! `…/cluster/progress` and re-emitted frame for frame), so
//! `lc_get_preview` and `lc_write_tags` — which read the
//! `cluster_results` cache that job fills — cross with it. The same day
//! `lc_pre_scan` crossed WHOLE over `POST /internal/corpus/local/pre-scan`
//! (register + scan on the daemon's manager, with the daemon's snapshot
//! root). No command in this file holds a manager any more; what stays
//! local is `lc_validate_path`, a pre-corpus path probe.
//!
//! # D9c: `lc_pre_scan`'s REGISTRATION crosses (2026-09-10)
//!
//! The parenthesis above used to read "`pre_scan` also registers", and
//! that clause was doing load-bearing work it could not carry. A path
//! probe is app-local; a registration is not. `register` writes the
//! registry ON DISK — the very state the rule above says both managers
//! see — and it is the PRODUCER for every consumer D8 crossed. The
//! real-mode harness measured the cost on run 5: the desktop registered
//! `folder-corpus-2918e9ebc0b5` into this process's manager and the
//! daemon answered the ingest that followed
//! `404 … corpus 'folder-corpus-2918e9ebc0b5' is not registered
//! locally`. The registration now goes over `POST
//! /internal/corpus/local` in both modes, and `local_corpus_wire_census`
//! pins the pairing so a consumer cannot cross alone again.
//!
//! # What crossing the ingest job changed for a user (ARCH §18.3)
//!
//! The OCR that runs is the HOST's — and since 2026-09-11 this file
//! installs none. `install_ocr_ctx_for_app` used to resolve
//! Paddle/Tesseract/PDFium at boot and `set_ocr_ctx` it on
//! `state.local_corpus`, under the belief that "in Local mode that
//! manager IS the daemon's". It is not: `WatchedSubsystem::install` has
//! ONE production caller, `sovereign-cli-daemon`'s bootstrap
//! (`attach_construction_census` pins this process's count at zero), so
//! the manager this app builds serves no ingest and the context it was
//! handed read nothing. The daemon that ingests installs its own
//! (`daemon_cmd/ocr_install.rs`, feature `ocr`), probing
//! `SOVEREIGN_PADDLE_OCR_MODEL_DIR`, `{data_dir}/models/paddle-ocr` and
//! `~/.svrnmesh/models/paddle-ocr`; `lc_ocr_available` reports THAT
//! context, so the "Read them with OCR" affordance reflects the engine
//! that would actually do the reading. `lc_ocr_available` is an `Err`
//! when the daemon has no local-corpus runtime, where it used to degrade
//! to `false`: "OCR is unavailable" and "nobody could be asked" want
//! different remedies from the pane.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use sovereign_tools::local_corpus::{
    clusterer::ClusterConfig,
    git::GitStatus,
    manager::{IncompleteJob, IngestStats, ProgressCallback},
    pre_scanner::PreScanResult,
    preview::VaultPreview,
    progress::{CompletionResult, LocalCorpusProgress},
    writeback::{CleanResult, RollbackResult, SnapshotMeta, WriteBackResult},
    LocalCorpusConfig,
};
use sovereign_workflow_host::workflow_http::{
    JobResponse, RunRequest, RunResponse, WorkflowJobEvent,
};

use crate::state::AppState;

// ─── Channel helpers ─────────────────────────────────────────────────

fn progress_channel(job_id: &str) -> String {
    format!("local-corpus://progress/{job_id}")
}

/// Build a progress callback that emits every `LocalCorpusProgress`
/// event on the job-scoped Tauri channel. `_ = emit(...)` because
/// a failed emit (window closed, e.g.) should not abort the long
/// running ingest — UI re-subscription will catch the terminal event
/// via the ingest result.
fn make_emitter(app: AppHandle, job_id: String) -> ProgressCallback {
    let channel = progress_channel(&job_id);
    Arc::new(move |evt: LocalCorpusProgress| {
        let _ = app.emit(&channel, &evt);
    })
}

fn new_job_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ─── Shared guards ───────────────────────────────────────────────────

/// The client for the daemon's local-corpus surface — the SAME
/// `LocalCorpusManager` this file used to reach through
/// `state.local_corpus`, reached over loopback instead (sv-surface D5).
/// ONE path in both boot modes: Local means the daemon is in-process and
/// `watched_folder_runtime::manager()` holds the very Arc `state.rs`
/// handed to `WatchedSubsystem::install`.
fn lc_client(state: &AppState) -> sovereign_turn_client::TurnClient {
    sovereign_turn_client::TurnClient::new(state.client_base_url())
}

// ─── Command: lc_ocr_available ───────────────────────────────────────

/// Whether the OCR pipeline is wired up on the daemon that would run
/// the ingest — the one whose `OcrCtx` decides whether a scanned PDF
/// can actually be read (sv-surface D8, paired with the ingest job it
/// crossed with).
///
/// The frontend hides the "Read them with OCR" affordance when this
/// returns `false`, so users on a build without bundled binaries don't
/// see a button that would error if clicked. A daemon with no
/// local-corpus runtime is an `Err`, not a `false`.
#[tauri::command]
pub async fn lc_ocr_available(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    let avail = lc_client(&state)
        .lc_ocr_available::<sovereign_contracts::daemon_wire::OcrAvailability>()
        .await
        .map_err(|e| format!("lc_ocr_available: {e}"))?;
    Ok(avail.available)
}

// ─── Command: lc_validate_path ───────────────────────────────────────

#[derive(Serialize)]
pub struct PathValidation {
    pub exists: bool,
    pub is_dir: bool,
    pub readable: bool,
    pub canonical_path: Option<String>,
}

/// Validate a user-supplied path. Returns readable metadata without
/// registering anything. Used by both the "Browse..." file dialog and
/// the file-drop handler before prompting confirmation.
#[tauri::command]
pub async fn lc_validate_path(path: String) -> Result<PathValidation, String> {
    let p = PathBuf::from(&path);
    let exists = p.exists();
    let is_dir = p.is_dir();
    let readable = p.metadata().and_then(|_| std::fs::read_dir(&p)).is_ok();
    let canonical_path = p
        .canonicalize()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    Ok(PathValidation {
        exists,
        is_dir,
        readable,
        canonical_path,
    })
}

// ─── Command: lc_pre_scan ────────────────────────────────────────────

#[derive(Serialize)]
pub struct PreScanResponse {
    pub job_id: String,
    pub result: PreScanResult,
    pub corpus_id: String,
    pub display_name: String,
}

/// Register (or re-register) a corpus for the supplied path + source
/// type, then run a pre-scan. Returns the classification and the
/// corpus id + display name AS REGISTERED.
///
/// ONE call since 2026-09-11: `POST /internal/corpus/local/pre-scan`
/// builds the config (the Obsidian arm with the DAEMON's snapshot root —
/// this command's last local manager read), registers it on the manager
/// that will run the ingest, and scans the config the registry kept.
/// D9c had crossed the registration alone; the scan stayed because the
/// path was "user-picked", which stopped being a reason once the engine
/// reading that path was the daemon's.
///
/// `job_id` is kept on the response for the TS shape (`LcPreScanResponse`)
/// only. No listener subscribes to it — `FolderDropFlow.svelte` reads
/// `corpus_id`, `display_name` and `result` and nothing else (checked
/// 2026-09-11) — and the scan no longer narrates `Scanning` frames from
/// this process, so the id names no channel anything emits on.
#[tauri::command]
pub async fn lc_pre_scan(
    state: State<'_, Arc<AppState>>,
    path: String,
    source_type: String,
    display_name: Option<String>,
) -> Result<PreScanResponse, String> {
    let answer = lc_client(&state)
        .lc_pre_scan::<serde_json::Value, sovereign_mesh::lc_http::PreScanAnswer>(
            &serde_json::json!({
                "path": path,
                "source_type": source_type,
                "display_name": display_name,
            }),
        )
        .await
        .map_err(|e| format!("pre_scan: {e}"))?;
    Ok(PreScanResponse {
        job_id: new_job_id(),
        result: answer.result,
        corpus_id: answer.corpus_id,
        display_name: answer.display_name,
    })
}

// ─── Command: lc_ingest ──────────────────────────────────────────────

/// Begin ingestion for an already-registered corpus. Returns a
/// `job_id` immediately; callers listen on
/// `local-corpus://progress/{job_id}` for phase events and the
/// terminal `Complete { result: Ingest(stats) }` payload.
///
/// Ingestion runs in a spawned task so the command itself can return
/// promptly — the UI progress panel is driven entirely by events.
#[tauri::command]
pub async fn lc_ingest(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    with_ocr: Option<bool>,
) -> Result<String, String> {
    let job_id = new_job_id();
    let progress = make_emitter(app.clone(), job_id.clone());
    let daemon_url = state.client_base_url();
    // The corpus's config is a REGISTRY read and crosses (sv-surface D5).
    // Both dispatch arms below branch on it; no arm holds a manager.
    let registered = lc_client(&state)
        .lc_get::<LocalCorpusConfig>(&corpus_id)
        .await
        .map_err(|e| format!("lc_ingest: {e}"))?;

    // One-shot document folder OR Obsidian vault → the daemon owns ingest AND
    // enrich (it holds the tiered providers; the desktop's manager doesn't).
    // Ingesting here and enriching in the daemon deadlocks on the cross-process
    // index handoff, so hand the whole job over: a single in-process ingest +
    // tiered enrich, no CLI subprocess. The daemon does NOT add it to the sweep
    // scheduler ⇒ no ongoing watch — "a watched folder without the watching".
    // WatchedFolder is excluded (its reconciliation worker owns enrichment).
    if with_ocr != Some(true) {
        if let Some(cfg) = registered.clone() {
            use sovereign_tools::local_corpus::config::LocalCorpusSourceType;
            if matches!(
                cfg.source_type,
                LocalCorpusSourceType::DocumentFolder | LocalCorpusSourceType::ObsidianVault { .. }
            ) {
                let cid = corpus_id.clone();
                let progress = progress.clone();
                tokio::spawn(async move {
                    let client = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(600))
                        .build()
                        .unwrap_or_else(|_| reqwest::Client::new());
                    let url = format!("{daemon_url}/internal/corpus/enrich-once");
                    tracing::info!(
                        corpus_id = %cid,
                        "lc_ingest: handing one-shot corpus to the daemon (ingest + tiered enrich)"
                    );
                    match client.post(&url).json(&cfg).send().await {
                        Ok(r) if r.status().is_success() => {
                            let body: serde_json::Value = r.json().await.unwrap_or_default();
                            let files = body
                                .get("files_indexed")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as usize;
                            let chunks = body
                                .get("chunks_written")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0);
                            tracing::info!(
                                corpus_id = %cid, files, chunks,
                                "lc_ingest: daemon ingested; tiered enrichment building in background"
                            );
                            progress(LocalCorpusProgress::Complete {
                                result: CompletionResult::Ingest(IngestStats {
                                    corpus_id: cid.clone(),
                                    files_indexed: files,
                                    chunks_written: chunks,
                                    runtime_failures: Vec::new(),
                                    excerpt_chunks: Vec::new(),
                                    duration_secs: 0,
                                }),
                            });
                        }
                        Ok(r) => {
                            let status = r.status();
                            let msg = r.text().await.unwrap_or_default();
                            progress(LocalCorpusProgress::Error {
                                message: format!("daemon ingest+enrich failed ({status}): {msg}"),
                                recoverable: false,
                            });
                        }
                        Err(e) => progress(LocalCorpusProgress::Error {
                            message: format!("could not reach daemon for ingest+enrich: {e}"),
                            recoverable: false,
                        }),
                    }
                });
                return Ok(job_id);
            }
        }
    }

    // Opt-in: route folder ingest through the workflow Runner (the substrate
    // adoption path — same `notebook` definition the CLI + Run view use) when
    // `SOVEREIGN_RUNNER_INGEST` is set and the corpus needs no OCR (`tool:extract`
    // has none). Bespoke stays the default and still owns OCR + enrichment.
    if std::env::var("SOVEREIGN_RUNNER_INGEST").is_ok() && with_ocr != Some(true) {
        match registered.clone() {
            // A non-OCR corpus -> the daemon's notebook job.
            Some(cfg) if !cfg.ocr_pdfs => {
                let progress = progress.clone();
                tokio::spawn(run_ingest_via_runner(daemon_url, cfg, corpus_id, progress));
                return Ok(job_id);
            }
            // Unknown corpus, or OCR wanted -> fall through to bespoke.
            _ => {
                tracing::info!(
                    %corpus_id,
                    "SOVEREIGN_RUNNER_INGEST set but Runner ingest unavailable \
                     (unknown corpus / OCR) — using bespoke ingest"
                );
            }
        }
    }

    // The daemon base, captured before the spawn: the Tauri `state` guard
    // cannot cross that boundary.
    let daemon_url = state.client_base_url();
    // THE ARM THAT CROSSED IN D8. `POST /internal/corpus/local/{c}/ingest`
    // runs exactly this call on the DAEMON's manager and answers 202 with
    // an `IngestJobAck`. 069660fd9 could not consume it: the ack named
    // `/internal/corpus/watch/status/{c}`, whose handler requires a
    // reconcilable watched folder (a 404 for the DocumentFolder / OCR
    // corpora this arm serves) and which carries no `IngestStats` — so a
    // poller could only have finished by fabricating the counts the
    // Folder-drop flow renders (ARCH §18.3). The route the ack names now is
    // `.../ingest/progress`, over a terminal receipt written by ONE
    // recorder from both ingest sites, carrying `IngestStats` verbatim.
    //
    // This command's contract is unchanged and is not the returned id: it
    // is the Tauri channel `local-corpus://progress/{job_id}`, whose
    // terminal frame is `Complete { result: Ingest(stats) }`. The frames
    // below are that contract, filled from the route's own numbers.
    //
    // The id returned IS the host's job id (ARCH §7.5): the job is the
    // daemon's, so minting a second id here for the same job would be two
    // names for one thing.
    let ack = lc_client(&state)
        .lc_ingest::<sovereign_contracts::daemon_wire::IngestJobAck>(&corpus_id, with_ocr)
        .await
        .map_err(|e| format!("lc_ingest: {e}"))?;
    tracing::info!(
        %corpus_id,
        job_id = %ack.job_id,
        progress_route = %ack.progress_route,
        "lc_ingest: daemon accepted the ingest job"
    );
    let job_id = ack.job_id;
    let progress = make_emitter(app.clone(), job_id.clone());
    tokio::spawn(follow_ingest_job(
        daemon_url,
        corpus_id,
        job_id.clone(),
        registered,
        progress,
    ));
    Ok(job_id)
}

/// How often the ingest job is polled. The host's stamper throttles its
/// phase writes to ~50 across a whole embed pass, so a tighter interval
/// would re-read the same numbers; a looser one would visibly lag the
/// progress bar.
const INGEST_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(750);

/// How many CONSECUTIVE poll failures end the follow with an error frame.
/// One failure is a hiccup; ten in a row (~7.5s) is a daemon that is not
/// coming back, and a progress panel that spins forever is worse than one
/// that says so.
const INGEST_POLL_MAX_FAILURES: u32 = 10;

/// Follow a daemon-side ingest job to its terminal receipt, emitting the
/// desktop's own progress frames as it goes.
///
/// Every number here is the host's. `Ingesting.current_file` is `None`
/// because the phase file carries no filename — the local arm had one and
/// this does not, and inventing one would be a fabricated detail on an
/// otherwise honest frame (ARCH §18.3). The terminal frame is the
/// route's `outcome`: `stats` verbatim into `Complete`, or `error` into
/// `Error`. Exactly one of the two is set by the recorder, so the
/// `finished`-with-neither branch is a host contradiction and says so
/// rather than closing the panel on an invented success.
async fn follow_ingest_job(
    daemon_url: String,
    corpus_id: String,
    job_id: String,
    registered: Option<LocalCorpusConfig>,
    progress: ProgressCallback,
) {
    let client = sovereign_turn_client::TurnClient::new(daemon_url.clone());
    let mut failures = 0u32;
    let mut last_step: Option<(u64, u64)> = None;
    loop {
        tokio::time::sleep(INGEST_POLL_INTERVAL).await;
        let p = match client
            .lc_ingest_progress::<sovereign_contracts::daemon_wire::IngestProgressView<
                sovereign_tools::local_corpus::manager::IngestStats,
            >>(&corpus_id)
            .await
        {
            Ok(p) => {
                failures = 0;
                p
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(
                    %corpus_id, %job_id, failures,
                    "lc_ingest: progress poll failed: {e}"
                );
                if failures >= INGEST_POLL_MAX_FAILURES {
                    progress(LocalCorpusProgress::Error {
                        message: format!(
                            "lost contact with the daemon while ingesting \
                             '{corpus_id}' ({failures} consecutive failures): {e}"
                        ),
                        recoverable: false,
                    });
                    return;
                }
                continue;
            }
        };

        // Phase frames, only when the numbers actually moved.
        if let Some(st) = &p.state {
            let step = (st.step_current, st.step_total);
            if st.step_total > 0 && last_step != Some(step) {
                last_step = Some(step);
                progress(LocalCorpusProgress::Ingesting {
                    done: st.step_current,
                    total: st.step_total,
                    phase_label: st
                        .message
                        .clone()
                        .unwrap_or_else(|| "Reading and embedding your notes".to_string()),
                    current_file: None,
                });
            }
        }

        if !p.finished {
            continue;
        }
        match p.outcome {
            Some(o) => match (o.stats, o.error) {
                (Some(stats), _) => {
                    tracing::info!(
                        %corpus_id, %job_id,
                        files_indexed = stats.files_indexed,
                        chunks_written = stats.chunks_written,
                        "lc_ingest: job finished"
                    );
                    progress(LocalCorpusProgress::Complete {
                        result: CompletionResult::Ingest(stats),
                    });
                    hand_one_shot_to_enrichment(&daemon_url, &corpus_id, registered.as_ref()).await;
                }
                (None, Some(err)) => progress(LocalCorpusProgress::Error {
                    message: err,
                    recoverable: false,
                }),
                (None, None) => progress(LocalCorpusProgress::Error {
                    message: format!(
                        "the daemon reported ingest of '{corpus_id}' finished with \
                         neither counts nor an error — nothing can be said about \
                         what it indexed"
                    ),
                    recoverable: false,
                }),
            },
            // `finished` is defined as `outcome.is_some()` on the host, so
            // this is unreachable unless the two disagree. Report the
            // disagreement; do not paper it over with a zero-count Complete.
            None => progress(LocalCorpusProgress::Error {
                message: format!(
                    "the daemon reported ingest of '{corpus_id}' finished with no receipt"
                ),
                recoverable: false,
            }),
        }
        return;
    }
}

/// Hand a one-shot DOCUMENT FOLDER to the daemon for tiered enrichment
/// after its ingest ends — register-without-watch + tiered build ("a
/// watched folder without the watching").
///
/// Reached only by the OCR path: the non-OCR document folders and vaults
/// are served by the `enrich-once` arm above, which ingests AND enriches
/// in one call. Watched folders and vaults enrich via the reconciliation
/// worker, so this is gated to `DocumentFolder`. Best-effort and
/// glassbox-logged: a failure here leaves an ingested-but-unenriched
/// corpus, which the Explore pane reports on its own.
async fn hand_one_shot_to_enrichment(
    daemon_url: &str,
    corpus_id: &str,
    registered: Option<&LocalCorpusConfig>,
) {
    use sovereign_tools::local_corpus::config::LocalCorpusSourceType;
    let Some(cfg) = registered else { return };
    if !matches!(cfg.source_type, LocalCorpusSourceType::DocumentFolder) {
        return;
    }
    let url = format!("{daemon_url}/internal/corpus/enrich-once");
    tracing::info!(
        %corpus_id,
        "lc_ingest: document folder — requesting daemon-side tiered enrichment"
    );
    match reqwest::Client::new().post(&url).json(cfg).send().await {
        Ok(r) if r.status().is_success() => {
            tracing::info!(%corpus_id, "lc_ingest: daemon accepted one-shot enrichment")
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            tracing::warn!(
                %corpus_id, %status, body,
                "lc_ingest: daemon rejected one-shot enrichment"
            );
        }
        Err(e) => tracing::warn!(
            %corpus_id,
            "lc_ingest: could not reach daemon for enrichment: {e}"
        ),
    }
}

/// Make an already-ingested local corpus explorable by building its atlas
/// via the daemon's IN-PROCESS tiered enrichment (RAPTOR + entity graph +
/// motifs) — the same path document folders take at ingest.
///
/// Replaces the legacy `sovereign-cli enrich init/build` subprocess, which is
/// not bundled with the desktop (it needs the `sovereign-cli-llm` sibling) and
/// is redundant with the daemon that already holds the models. Hands the
/// corpus config to `POST /internal/corpus/enrich-once` (register-without-watch
/// + tiered build). Fire-and-forget: returns as soon as the request is
/// dispatched; the UI polls `lc_enrichment_status` for phase/percent and the
/// corpus-progress banner shows any (re-)ingest the daemon runs. Enrichment
/// runs in the daemon so writer and reader share one process (a cross-process
/// index handoff deadlocks `enable_enrichment`).
#[tauri::command]
pub async fn lc_enrich_now(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let cfg = lc_client(&state)
        .lc_get::<LocalCorpusConfig>(&corpus_id)
        .await
        .map_err(|e| format!("lc_enrich_now: {e}"))?
        .ok_or_else(|| format!("corpus '{corpus_id}' is not registered locally"))?;
    let daemon_url = state.client_base_url();
    let cid = corpus_id.clone();
    tokio::spawn(async move {
        let url = format!("{daemon_url}/internal/corpus/enrich-once");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3600))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        match client.post(&url).json(&cfg).send().await {
            Ok(r) if r.status().is_success() => {
                tracing::info!(corpus_id = %cid, "lc_enrich_now: daemon accepted tiered enrichment")
            }
            Ok(r) => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                tracing::warn!(corpus_id = %cid, %status, body, "lc_enrich_now: daemon rejected enrichment")
            }
            Err(e) => {
                tracing::warn!(corpus_id = %cid, "lc_enrich_now: could not reach daemon: {e}")
            }
        }
    });
    Ok(())
}

/// Tauri command: clear a "zombie" enrichment / watched-folder status —
/// a build stuck at "Preparing to build the map" that never advanced
/// (crashed / killed / stalled), or a sticky `Errored` watched-folder
/// sweep. Drops the corpus back to "no map yet" so the user can rebuild.
/// Awaited (unlike `lc_enrich_now`) so the caller can immediately re-poll
/// `lc_enrichment_status` and see the cleared state. Does NOT delete the
/// index or the atlas — only the status surfaces.
#[tauri::command]
pub async fn lc_enrich_reset(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<(), String> {
    let daemon_url = state.client_base_url();
    let url = format!("{daemon_url}/internal/corpus/enrich-reset");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "corpus_id": corpus_id }))
        .send()
        .await
        .map_err(|e| format!("POST /internal/corpus/enrich-reset: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("daemon enrich-reset returned {status}: {body}"));
    }
    Ok(())
}

/// Tauri command: the "flag a wrong summary → re-enrich just this note"
/// revision loop (`docs/specs/SUMMARY_REVISION_LOOP.md`). Persists the
/// user's correction to the ledger (status `pending`), then asks the
/// daemon to re-enrich that ONE note; the provider reads the pending
/// correction, forces past the content-hash checkpoint, regenerates the
/// summary with the hint injected, and flips the row to `applied`.
/// Awaited (the ~1-min single-note build) so the caller can re-fetch the
/// corrected summary on return. `correction_hint` / `original_summary`
/// may be empty strings (stored as NULL).
#[tauri::command]
pub async fn lc_reenrich_note(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    source_doc_id: String,
    correction_hint: String,
    original_summary: String,
) -> Result<(), String> {
    // 1. Persist the correction so the provider sees it during the build.
    //    Same sqlite file the embedded daemon's provider reads.
    {
        let guard = state.sqlite_store.read().await;
        let store = guard
            .as_ref()
            .ok_or_else(|| "enrichment store not ready".to_string())?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let hint = Some(correction_hint.trim()).filter(|s| !s.is_empty());
        let original = Some(original_summary.trim()).filter(|s| !s.is_empty());
        store
            .upsert_summary_correction(&corpus_id, &source_doc_id, hint, original, "pending", now)
            .await
            .map_err(|e| format!("record correction: {e}"))?;
    }

    // 2. Ask the daemon to re-enrich just this note (awaits the build).
    let daemon_url = state.client_base_url();
    let url = format!("{daemon_url}/internal/corpus/watch/{corpus_id}/enrich/reenrich-note");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "source_doc_id": source_doc_id }))
        .send()
        .await
        .map_err(|e| format!("POST reenrich-note: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("daemon reenrich-note returned {status}: {body}"));
    }
    Ok(())
}

/// Ingest a folder corpus by submitting the shipped `notebook` workflow to
/// the daemon's job surface (sv-surface rung 5 — the daemon executes), then
/// bridging the job's events onto the `LocalCorpusProgress` phases the
/// desktop UI already renders — so the progress panel needs no change.
/// Emits the terminal `Complete { Ingest(stats) }` / `Error` itself from the
/// job's terminal event.
async fn run_ingest_via_runner(
    daemon_url: String,
    cfg: LocalCorpusConfig,
    corpus_id: String,
    progress: ProgressCallback,
) {
    let started = std::time::Instant::now();

    // Params from the corpus config: the source folder + a comma-glob of its
    // configured extensions (empty = every file, which `notebook` extracts by type).
    let glob = cfg
        .extensions
        .iter()
        .map(|e| format!("*.{e}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut params = BTreeMap::new();
    params.insert(
        "folder".to_string(),
        cfg.root_path.to_string_lossy().into_owned(),
    );
    params.insert("corpus".to_string(), corpus_id.clone());
    params.insert("glob".to_string(), glob);

    // Submit the job; resolution + execution are the daemon's.
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            progress(LocalCorpusProgress::Error {
                message: format!("build daemon client: {e}"),
                recoverable: false,
            });
            return;
        }
    };
    let run: RunResponse = match client
        .post(format!("{daemon_url}/internal/workflows/run"))
        .json(&RunRequest {
            name_or_path: "notebook".to_string(),
            params,
            toml: None,
            concurrency: None,
            no_cache: None,
        })
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => match r.json().await {
            Ok(run) => run,
            Err(e) => {
                progress(LocalCorpusProgress::Error {
                    message: format!("parse notebook run ack: {e}"),
                    recoverable: false,
                });
                return;
            }
        },
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            progress(LocalCorpusProgress::Error {
                message: format!("daemon notebook run returned {status}: {body}"),
                recoverable: false,
            });
            return;
        }
        Err(e) => {
            progress(LocalCorpusProgress::Error {
                message: format!("could not reach daemon for notebook run: {e}"),
                recoverable: false,
            });
            return;
        }
    };

    // Poll bridge: job events -> LocalCorpusProgress phases.
    let acc = Arc::new(Mutex::new(IngestAccumulator::default()));
    let mut after = 0u64;
    loop {
        let resp = client
            .get(format!(
                "{daemon_url}/internal/workflows/jobs/{}?after={after}",
                run.job_id
            ))
            .send()
            .await;
        let job = match resp {
            Ok(r) if r.status().is_success() => match r.json::<JobResponse>().await {
                Ok(job) => job,
                Err(e) => {
                    progress(LocalCorpusProgress::Error {
                        message: format!("parse notebook job status: {e}"),
                        recoverable: false,
                    });
                    return;
                }
            },
            Ok(r) => {
                let status = r.status();
                progress(LocalCorpusProgress::Error {
                    message: format!(
                        "notebook job status returned {status} — run may still be in flight"
                    ),
                    recoverable: false,
                });
                return;
            }
            Err(e) => {
                tracing::warn!(job = %run.job_id, error = %e, "notebook poll: retrying");
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                continue;
            }
        };

        let terminal = job.status != sovereign_workflow_host::workflow_http::JobStatus::Running;
        for event in job.events {
            after = after.max(event.seq);
            match event.event {
                WorkflowJobEvent::Complete { ok, items, .. } => {
                    // `chunks_written` parsed from each item's `tool:corpus_store`
                    // output ("stored N chunks into corpus ..."); best-effort,
                    // contributes 0 if the shape ever changes.
                    let chunks_written: u64 = items
                        .iter()
                        .filter_map(|it| it.output.as_deref())
                        .filter_map(|txt| txt.strip_prefix("stored "))
                        .filter_map(|rest| rest.split_whitespace().next())
                        .filter_map(|n| n.parse::<u64>().ok())
                        .sum();
                    let stats = IngestStats {
                        corpus_id: corpus_id.clone(),
                        files_indexed: ok,
                        chunks_written,
                        runtime_failures: Vec::new(),
                        excerpt_chunks: Vec::new(),
                        duration_secs: started.elapsed().as_secs(),
                    };
                    progress(LocalCorpusProgress::Complete {
                        result: CompletionResult::Ingest(stats),
                    });
                    return;
                }
                WorkflowJobEvent::Failed { error } => {
                    progress(LocalCorpusProgress::Error {
                        message: error,
                        recoverable: false,
                    });
                    return;
                }
                ev => {
                    if let Some(local) = workflow_progress_to_local(ev, &mut acc.lock().unwrap()) {
                        progress(local);
                    }
                }
            }
        }
        if terminal {
            // Terminal status without a terminal event in this window (e.g. a
            // daemon restart lost the job) — surface it rather than polling
            // forever.
            progress(LocalCorpusProgress::Error {
                message: "notebook job ended without a terminal event".to_string(),
                recoverable: false,
            });
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
}

/// Running tally for the progress bar: the item total (from `RunStarted`) and how
/// many have finished (`ItemDone`), so each emitted phase carries `done/total`.
#[derive(Default)]
struct IngestAccumulator {
    total: u64,
    done: u64,
}

/// The desktop UI's friendly phase label for a workflow step's `uses`.
fn friendly_phase(uses: &str) -> &'static str {
    if uses.starts_with("tool:extract") {
        "Reading your documents"
    } else if uses.starts_with("tool:chunk") {
        "Chunking"
    } else if uses.starts_with("embed:") {
        "Embedding"
    } else if uses.starts_with("tool:corpus_store") {
        "Building the index"
    } else {
        "Working"
    }
}

/// Map a workflow job event (the daemon's wire enum) onto the desktop's
/// [`LocalCorpusProgress`] phase model. Returns `None` for events that don't move
/// the UI bar (`RunFinished` — the caller emits the terminal `Complete` after
/// computing stats; `ElementSkipped` — a per-element warning).
fn workflow_progress_to_local(
    ev: WorkflowJobEvent,
    acc: &mut IngestAccumulator,
) -> Option<LocalCorpusProgress> {
    match ev {
        WorkflowJobEvent::RunStarted { items, .. } => {
            acc.total = items as u64;
            acc.done = 0;
            Some(LocalCorpusProgress::Ingesting {
                done: 0,
                total: acc.total,
                phase_label: "Reading your documents".to_string(),
                current_file: None,
            })
        }
        WorkflowJobEvent::StepDone { item, uses, .. } => Some(LocalCorpusProgress::Ingesting {
            done: acc.done,
            total: acc.total,
            phase_label: friendly_phase(&uses).to_string(),
            current_file: (item != "·").then_some(item),
        }),
        WorkflowJobEvent::ItemDone { .. } => {
            acc.done = (acc.done + 1).min(acc.total.max(1));
            Some(LocalCorpusProgress::Ingesting {
                done: acc.done,
                total: acc.total,
                phase_label: "Building the index".to_string(),
                current_file: None,
            })
        }
        WorkflowJobEvent::RunFinished { .. } | WorkflowJobEvent::ElementSkipped { .. } => None,
        // Terminal events are handled by the poll bridge itself.
        WorkflowJobEvent::Complete { .. } | WorkflowJobEvent::Failed { .. } => None,
    }
}

// ─── Command: lc_list ────────────────────────────────────────────────

/// Every registered local corpus. An empty vec is a real answer (a
/// fresh install); a daemon with no local-corpus runtime is an `Err`,
/// because the pane renders an empty list as "you have no vaults".
#[tauri::command]
pub async fn lc_list(state: State<'_, Arc<AppState>>) -> Result<Vec<LocalCorpusConfig>, String> {
    lc_client(&state)
        .lc_list::<LocalCorpusConfig>()
        .await
        .map_err(|e| format!("lc_list: {e}"))
}

// ─── Command: lc_remove ──────────────────────────────────────────────

#[tauri::command]
pub async fn lc_remove(state: State<'_, Arc<AppState>>, corpus_id: String) -> Result<(), String> {
    lc_client(&state)
        .lc_remove(&corpus_id)
        .await
        .map_err(|e| format!("remove: {e}"))
}

// ─── Command: lc_incomplete_jobs ────────────────────────────────────

#[tauri::command]
pub async fn lc_incomplete_jobs(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<IncompleteJob>, String> {
    lc_client(&state)
        .lc_incomplete_jobs::<IncompleteJob>()
        .await
        .map_err(|e| format!("lc_incomplete_jobs: {e}"))
}

// ─── Command: lc_cancel ──────────────────────────────────────────────

/// Signal a running ingest for `corpus_id` to stop cooperatively.
/// Returns `true` when a flag was found and flipped.
///
/// Crossed with the ingest job it cancels (sv-surface D8): the flag lives
/// in the cancellation registry of the engine RUNNING the job, so a
/// cancel sent anywhere else cannot stop it. The progress channel emits
/// its final frame once the engine loop exits.
#[tauri::command]
pub async fn lc_cancel(state: State<'_, Arc<AppState>>, corpus_id: String) -> Result<bool, String> {
    let ack = lc_client(&state)
        .lc_cancel::<sovereign_contracts::daemon_wire::CancelAck>(&corpus_id)
        .await
        .map_err(|e| format!("lc_cancel: {e}"))?;
    // `cancelled` is "there WAS a job and it is now cancelled", which the
    // route keeps apart from "the call succeeded". Both are true for a
    // cancel that found nothing, and collapsing them would tell the pane a
    // job was stopped when none was running.
    Ok(ack.cancelled)
}

// ─── Command: lc_check_git ───────────────────────────────────────────

#[tauri::command]
pub async fn lc_check_git(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Option<GitStatus>, String> {
    lc_client(&state)
        .lc_check_git::<GitStatus>(&corpus_id)
        .await
        .map_err(|e| format!("check_git: {e}"))
}

// ─── Command: lc_write_tags ──────────────────────────────────────────

/// Crossed with `lc_cluster`, the producer that pinned it: `write_tags`
/// reaches the `cluster_results` cache through `get_preview`, and that
/// cache is filled on the manager that RAN the cluster job — the
/// daemon's, since 2026-09-11.
#[tauri::command]
pub async fn lc_write_tags(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    git_commit: Option<bool>,
) -> Result<WriteBackResult, String> {
    lc_client(&state)
        .lc_write_tags::<WriteBackResult>(&corpus_id, git_commit.unwrap_or(false))
        .await
        .map_err(|e| format!("write_tags: {e}"))
}

// ─── Command: lc_list_snapshots ──────────────────────────────────────

#[tauri::command]
pub async fn lc_list_snapshots(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Vec<SnapshotMeta>, String> {
    lc_client(&state)
        .lc_snapshots::<SnapshotMeta>(&corpus_id)
        .await
        .map_err(|e| format!("list_snapshots: {e}"))
}

// ─── Command: lc_rollback ────────────────────────────────────────────

#[tauri::command]
pub async fn lc_rollback(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    snapshot_path: String,
) -> Result<RollbackResult, String> {
    lc_client(&state)
        .lc_rollback::<RollbackResult>(&corpus_id, &snapshot_path)
        .await
        .map_err(|e| format!("rollback: {e}"))
}

// ─── Command: lc_clean ───────────────────────────────────────────────

#[tauri::command]
pub async fn lc_clean(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<CleanResult, String> {
    lc_client(&state)
        .lc_clean::<CleanResult>(&corpus_id)
        .await
        .map_err(|e| format!("clean: {e}"))
}

// ─── Command: lc_search ──────────────────────────────────────────────

/// The hit shape the frontend already matches — the ROUTE's own type, not
/// a twin of it. `lc_search` below both parses the route's answer with this
/// and returns it to the webview, so one definition owns both ends and the
/// serialized bytes cannot drift (ARCH principle 8). It is the SAME
/// definition `lc_http`'s route emits: both sides name
/// `sovereign_contracts::daemon_wire`, so this client no longer links the
/// serving host to parse four primitives (sv-surface svt-3).
pub use sovereign_contracts::daemon_wire::LocalSearchHit;

// ─── Command: lc_cluster ─────────────────────────────────────────────

/// Begin clustering + LLM labelling for an already-ingested Obsidian
/// vault. Returns a `job_id` immediately; caller subscribes to the
/// progress channel as with ingestion.
///
/// THE PRODUCER THAT CROSSED (2026-09-11). `POST
/// /internal/corpus/local/{c}/cluster` runs exactly the `manager.cluster`
/// call this command used to make, on the DAEMON's manager — the one
/// whose `cluster_results` cache `…/preview` and `…/write-tags` read, so
/// `lc_get_preview` and `lc_write_tags` cross with it. The contract is
/// unchanged and is not the returned id: it is the Tauri channel
/// `local-corpus://progress/{job_id}`, and every frame on it is the
/// host's own `LocalCorpusProgress`, re-emitted verbatim from the poll
/// — including the terminal `Complete { result: Ingest(zero stats) }`
/// this command used to mint itself. The id returned IS the host's job
/// id, as for `lc_ingest`.
#[tauri::command]
pub async fn lc_cluster(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    config: Option<ClusterConfig>,
) -> Result<String, String> {
    let daemon_url = state.client_base_url();
    let ack = lc_client(&state)
        .lc_cluster::<ClusterConfig, sovereign_contracts::daemon_wire::IngestJobAck>(
            &corpus_id,
            config.as_ref(),
        )
        .await
        .map_err(|e| format!("cluster: {e}"))?;
    tracing::info!(
        %corpus_id,
        job_id = %ack.job_id,
        progress_route = %ack.progress_route,
        "lc_cluster: daemon accepted the cluster job"
    );
    let job_id = ack.job_id;
    let progress = make_emitter(app.clone(), job_id.clone());
    tokio::spawn(follow_cluster_job(
        daemon_url,
        corpus_id,
        job_id.clone(),
        progress,
    ));
    Ok(job_id)
}

/// Follow a daemon-side cluster job, re-emitting every frame the host
/// appended on the desktop's channel. Same cadence and give-up rule as
/// [`follow_ingest_job`]; the frames need no translation because the
/// route serves the manager's `LocalCorpusProgress` verbatim, and the
/// terminal frame is the host's too.
async fn follow_cluster_job(
    daemon_url: String,
    corpus_id: String,
    job_id: String,
    progress: ProgressCallback,
) {
    let client = sovereign_turn_client::TurnClient::new(daemon_url);
    let mut failures = 0u32;
    let mut cursor = 0usize;
    loop {
        tokio::time::sleep(INGEST_POLL_INTERVAL).await;
        let p = match client
            .lc_cluster_progress::<sovereign_mesh::lc_http::ClusterProgress>(&corpus_id, cursor)
            .await
        {
            Ok(p) => {
                failures = 0;
                p
            }
            Err(e) => {
                failures += 1;
                tracing::warn!(
                    %corpus_id, %job_id, failures,
                    "lc_cluster: progress poll failed: {e}"
                );
                if failures >= INGEST_POLL_MAX_FAILURES {
                    progress(LocalCorpusProgress::Error {
                        message: format!(
                            "lost contact with the daemon while clustering \
                             '{corpus_id}' ({failures} consecutive failures): {e}"
                        ),
                        recoverable: false,
                    });
                    return;
                }
                continue;
            }
        };
        cursor = p.next;
        for frame in p.frames {
            progress(frame);
        }
        if p.finished {
            tracing::info!(%corpus_id, %job_id, "lc_cluster: job finished");
            return;
        }
    }
}

// ─── Command: lc_get_preview ─────────────────────────────────────────

/// Fetch the computed preview for a corpus that has had `lc_cluster`
/// run recently. The route answers a named 500 ("no clustering run on
/// record") when nothing is cached — on the DAEMON's manager, which is
/// the one `lc_cluster` now fills. `None` config is the host's
/// `ClusterConfig::default()`, one decider.
#[tauri::command]
pub async fn lc_get_preview(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    config: Option<ClusterConfig>,
) -> Result<VaultPreview, String> {
    lc_client(&state)
        .lc_preview::<ClusterConfig, VaultPreview>(&corpus_id, config.as_ref())
        .await
        .map_err(|e| format!("get_preview: {e}"))
}

// ─── Command: lc_search ─────────────────────────────────────────────

#[tauri::command]
pub async fn lc_search(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<LocalSearchHit>, String> {
    // `limit: None` is the HOST's 10 now — the same 10 this command
    // defaulted to, moved down to the one decider (ARCH §10.6).
    lc_client(&state)
        .lc_search::<LocalSearchHit>(&corpus_id, &query, limit)
        .await
        .map_err(|e| format!("search: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Runner→desktop progress mapping for a notebook run over two files:
    /// the right phase labels and a monotonic `done/total` bar.
    #[test]
    fn workflow_progress_maps_to_ingesting_phases() {
        let mut acc = IngestAccumulator::default();

        let start = workflow_progress_to_local(
            WorkflowJobEvent::RunStarted {
                workflow: "notebook".into(),
                items: 2,
                steps: 4,
            },
            &mut acc,
        );
        assert!(matches!(
            start,
            Some(LocalCorpusProgress::Ingesting {
                done: 0,
                total: 2,
                ..
            })
        ));

        let step = workflow_progress_to_local(
            WorkflowJobEvent::StepDone {
                item: "notes.md".into(),
                step: "embed".into(),
                uses: "embed:default".into(),
                for_each: true,
                cached: false,
                step_index: 2,
                total_steps: 4,
            },
            &mut acc,
        );
        match step {
            Some(LocalCorpusProgress::Ingesting {
                phase_label,
                current_file,
                done,
                total,
            }) => {
                assert_eq!(phase_label, "Embedding");
                assert_eq!(current_file.as_deref(), Some("notes.md"));
                assert_eq!((done, total), (0, 2)); // not yet item-complete
            }
            other => panic!("expected Ingesting, got {other:?}"),
        }

        // Two items finish → done climbs to 2 and never past the total.
        for expected in [1u64, 2] {
            let done = workflow_progress_to_local(
                WorkflowJobEvent::ItemDone {
                    item: "x".into(),
                    ok: true,
                    ran: 4,
                    cached: 0,
                },
                &mut acc,
            );
            assert!(matches!(
                done,
                Some(LocalCorpusProgress::Ingesting { done, total: 2, .. }) if done == expected
            ));
        }

        // Terminal + per-element events don't move the bar (the caller owns Complete).
        assert!(workflow_progress_to_local(
            WorkflowJobEvent::RunFinished { ok: 2, failed: 0 },
            &mut acc,
        )
        .is_none());
    }

    #[test]
    fn friendly_phase_covers_the_notebook_steps() {
        assert_eq!(friendly_phase("tool:extract"), "Reading your documents");
        assert_eq!(friendly_phase("tool:chunk"), "Chunking");
        assert_eq!(friendly_phase("embed:default"), "Embedding");
        assert_eq!(friendly_phase("tool:corpus_store"), "Building the index");
    }
}
