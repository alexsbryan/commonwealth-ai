// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri command surface for local corpora (Folder Drop + Obsidian).
//!
//! One job_id-scoped event channel per invocation:
//! `local-corpus://progress/{job_id}`. The UI listens with
//! `listen<LocalCorpusProgress>(channel, handler)`.
//!
//! Commands are thin — they translate TS-friendly shapes into
//! `LocalCorpusManager` calls and forward progress events via
//! `AppHandle::emit`. All heavy lifting happens in `sovereign-tools`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use sovereign_tools::local_corpus::{
    clusterer::ClusterConfig,
    git::GitStatus,
    manager::{IncompleteJob, IngestStats, ProgressCallback},
    ocr::{OcrCtx, OcrEngineKind},
    pre_scanner::PreScanResult,
    preview::VaultPreview,
    progress::{CompletionResult, LocalCorpusProgress},
    writeback::{CleanResult, RollbackResult, SnapshotMeta, WriteBackResult},
    LocalCorpusConfig, LocalCorpusManager,
};
use std::path::PathBuf as StdPathBuf;

use sovereign_core::traits::InferenceProvider;
use sovereign_workflow::Workflow;
use sovereign_workflow_host::{
    resolve_workflow_source, run_workflow_with_provider, HttpCorpusInstaller, StepObserver,
    WorkflowProgress,
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

async fn require_manager(
    state: &State<'_, Arc<AppState>>,
) -> Result<Arc<LocalCorpusManager>, String> {
    state
        .local_corpus
        .read()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| {
            "Local corpus manager not ready. Finish setup (model + embedding model) first."
                .to_string()
        })
}

// ─── Command: lc_ocr_available ───────────────────────────────────────

/// Whether the OCR pipeline is wired up for this build of the
/// desktop app. Driven by the presence of a Tesseract sidecar that
/// boot-time setup successfully resolved into the `LocalCorpusManager`.
///
/// The frontend hides the "Read them with OCR" affordance when this
/// returns `false`, so users on a build without bundled binaries
/// don't see a button that would error if clicked.
#[tauri::command]
pub async fn lc_ocr_available(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    let manager = match state.local_corpus.read().await.as_ref().cloned() {
        Some(m) => m,
        None => return Ok(false),
    };
    Ok(manager.ocr_available().await)
}

/// Install an OCR runtime context onto the running
/// `LocalCorpusManager`. Called once at desktop boot after the manager
/// is up. Resolves the bundled Tesseract binary via predictable paths,
/// points pdfium at the bundled dynamic library (if present), and
/// points cleanup at the local daemon.
///
/// Resolution order for the Tesseract binary:
///   1. `SOVEREIGN_TESSERACT_BIN` env var — escape hatch for dev.
///   2. `<resource_dir>/binaries/tesseract[-<target_triple>][.exe]`
///      — what `tauri.conf.json`'s `bundle.externalBin` produces.
///   3. `<exe_dir>/tesseract` — what bundled apps land at next to the
///      main binary.
///   4. Skip — leaving OCR unavailable. `lc_ocr_available` reports
///      this so the UI hides the offer entirely.
///
/// Failure is non-fatal: a build without bundled binaries simply
/// degrades to "OCR not offered" rather than erroring on boot.
pub async fn install_ocr_ctx_for_app(
    app: &AppHandle,
    manager: &Arc<LocalCorpusManager>,
    daemon_base_url: String,
    cleanup_model: String,
) {
    use tauri::Manager;

    let resource_dir = app.path().resource_dir().ok();

    // pdfium rasterizes PDFs to page images regardless of which OCR
    // engine reads them — required for BOTH paddle and tesseract. If we
    // can locate the bundled dylib we pin pdfium-render to it; otherwise
    // we fall back to its system-library probe (which surfaces a clear
    // error at OCR-time).
    let pdfium_lib_path = resolve_pdfium_lib(resource_dir.as_deref());

    // Prefer PaddleOCR. It drives the ONNX Runtime already linked for
    // GLiNER and needs NO external build dependency — unlike tesseract,
    // which users must `brew/apt install` or we must statically build.
    // The 2026-05-27 bake-off put paddle at/above tesseract quality once
    // `det_limit_side_len` was raised to 1600 (now the engine default).
    // Use it whenever its models resolve (bundled in the .app, or in
    // ~/.sovereign for a dev machine); fall back to tesseract otherwise.
    #[cfg(feature = "paddle-ocr")]
    {
        if let Some(model_root) = resolve_paddle_model_dir(resource_dir.as_deref()) {
            // The engine resolves models via SOVEREIGN_PADDLE_OCR_MODEL_DIR
            // → `paddle::models_root()`. Point it at whatever we found so a
            // packaged app uses the bundled copy and a dev box uses
            // ~/.sovereign — one code path, no per-build special-casing.
            std::env::set_var("SOVEREIGN_PADDLE_OCR_MODEL_DIR", &model_root);
            let ctx = OcrCtx {
                // tesseract_* are inert when engine = Paddle.
                tesseract_bin: PathBuf::from("tesseract"),
                tessdata_dir: PathBuf::new(),
                pdfium_lib_path,
                daemon_base_url,
                cleanup_model,
                dpi: 300,
                tesseract_timeout_secs: 30,
                cleanup_timeout_secs: 30,
                engine: OcrEngineKind::Paddle,
            };
            tracing::info!(
                paddle_model_root = %model_root.display(),
                pdfium = ?ctx.pdfium_lib_path,
                cleanup_model = %ctx.cleanup_model,
                "OCR context installed (PaddleOCR) — folder drop will offer OCR for scanned PDFs"
            );
            manager.set_ocr_ctx(ctx).await;
            return;
        }
        tracing::info!(
            "PaddleOCR models not found (.app bundle or ~/.sovereign/models/paddle-ocr) \
             — falling back to tesseract"
        );
    }

    // Fallback: the tesseract subprocess (needs a system/bundled binary).
    let tesseract_bin = match resolve_tesseract_path(app) {
        Some(p) => p,
        None => {
            tracing::info!("OCR not available: no PaddleOCR models and no tesseract sidecar");
            return;
        }
    };
    let tessdata_dir = match resolve_tessdata_dir(resource_dir.as_deref()) {
        Some(p) => p,
        None => {
            tracing::warn!(
                "OCR not available: tessdata/eng.traineddata missing — \
                 expected under <resource_dir>/tessdata/ or alongside the tesseract binary"
            );
            return;
        }
    };
    let ctx = OcrCtx {
        tesseract_bin,
        tessdata_dir,
        pdfium_lib_path,
        daemon_base_url,
        cleanup_model,
        dpi: 300,
        tesseract_timeout_secs: 30,
        cleanup_timeout_secs: 30,
        engine: OcrEngineKind::Tesseract,
    };
    tracing::info!(
        tesseract = %ctx.tesseract_bin.display(),
        tessdata = %ctx.tessdata_dir.display(),
        pdfium = ?ctx.pdfium_lib_path,
        cleanup_model = %ctx.cleanup_model,
        "OCR context installed (tesseract) — folder drop will offer OCR for scanned PDFs"
    );
    manager.set_ocr_ctx(ctx).await;
}

/// Compile-time absolute path to the `src-tauri/binaries/` directory
/// inside this crate. Lets `cargo tauri dev` find the same binaries
/// that release bundles ship via `externalBin`/`resources`, without
/// needing the user to set env vars or relying on Tauri's runtime
/// `resource_dir()` (which doesn't surface those entries in dev).
const DEV_BINARIES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/binaries");

fn resolve_tesseract_path(app: &AppHandle) -> Option<PathBuf> {
    use tauri::Manager;

    if let Ok(env_path) = std::env::var("SOVEREIGN_TESSERACT_BIN") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Some(p);
        }
    }

    // Tauri externalBin layout: under `<resource_dir>/binaries/`
    // Tauri may suffix the target triple — try the bare name and a
    // couple of common triples before giving up.
    let mut probes: Vec<PathBuf> = Vec::new();
    let mut bin_dirs: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = app.path().resource_dir() {
        bin_dirs.push(rd.join("binaries"));
        // macOS resource_dir is `Contents/Resources`, but
        // externalBin sidecars land in `Contents/MacOS` next to
        // the main exe — probe that too.
        if let Some(parent) = rd.parent() {
            bin_dirs.push(parent.join("MacOS"));
        }
    }
    // Dev fallback: the canonical `src-tauri/binaries/` directory.
    // Baked at compile time so it survives the working-directory
    // changes Tauri's dev runner does on macOS.
    bin_dirs.push(PathBuf::from(DEV_BINARIES_DIR));

    for bins in &bin_dirs {
        probes.push(bins.join("tesseract"));
        probes.push(bins.join("tesseract.exe"));
        for triple in [
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-gnu",
            "x86_64-pc-windows-msvc",
        ] {
            probes.push(bins.join(format!("tesseract-{triple}")));
            probes.push(bins.join(format!("tesseract-{triple}.exe")));
        }
    }
    if let Ok(rd) = app.path().resource_dir() {
        probes.push(rd.join("tesseract"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            probes.push(parent.join("tesseract"));
            probes.push(parent.join("tesseract.exe"));
        }
    }
    if let Some(p) = probes.into_iter().find(|p| p.exists()) {
        return Some(p);
    }
    // $PATH fallback — covers Homebrew (`/opt/homebrew/bin/tesseract`)
    // on Apple Silicon, `/usr/local/bin/tesseract` on Intel macOS,
    // distro packages on Linux, and any operator who installed
    // tesseract themselves. Without this, dev builds with no
    // `binaries/tesseract` symlink silently report "OCR not available"
    // even though the system can clearly run it.
    //
    // Tauri's launchd-spawned env has a minimal `PATH` (typically
    // `/usr/bin:/bin:/usr/sbin:/sbin`), so we splice in the standard
    // Homebrew + Linux locations before searching. The env var still
    // takes precedence — operators with a hand-rolled `PATH` win.
    let mut path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for extra in [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/usr/local/sbin",
        "/opt/local/bin",
    ] {
        let pb = PathBuf::from(extra);
        if !path_dirs.contains(&pb) {
            path_dirs.push(pb);
        }
    }
    for dir in path_dirs {
        for name in ["tesseract", "tesseract.exe"] {
            let candidate = dir.join(name);
            if candidate.is_file() {
                tracing::info!(
                    path = %candidate.display(),
                    "OCR: tesseract located via PATH fallback"
                );
                return Some(candidate);
            }
        }
    }
    None
}

fn resolve_tessdata_dir(resource_dir: Option<&std::path::Path>) -> Option<PathBuf> {
    let mut probes: Vec<PathBuf> = Vec::new();
    if let Ok(env_path) = std::env::var("SOVEREIGN_TESSDATA_DIR") {
        probes.push(PathBuf::from(env_path));
    }
    if let Some(rd) = resource_dir {
        probes.push(rd.join("tessdata"));
        probes.push(rd.join("binaries").join("tessdata"));
    }
    // Dev fallback — same compile-time-baked binaries dir.
    let dev_bins = PathBuf::from(DEV_BINARIES_DIR);
    probes.push(dev_bins.join("tessdata"));
    // System-install fallback. Tesseract's tessdata ships next to its
    // binary on Homebrew + most Linux distros at predictable paths.
    // Without these, a system-installed tesseract from the PATH probe
    // above would be found but tessdata would still come up empty.
    for p in [
        "/opt/homebrew/share/tessdata",           // Homebrew Apple Silicon
        "/usr/local/share/tessdata",              // Homebrew Intel
        "/opt/local/share/tessdata",              // MacPorts
        "/usr/share/tessdata",                    // Debian/Ubuntu
        "/usr/share/tesseract-ocr/4.00/tessdata", // Older Debian
        "/usr/share/tesseract-ocr/5/tessdata",    // Newer Debian
        "/usr/share/tesseract/tessdata",          // RHEL/Fedora
    ] {
        probes.push(PathBuf::from(p));
    }
    probes
        .into_iter()
        .find(|p| p.join("eng.traineddata").exists())
}

fn resolve_pdfium_lib(resource_dir: Option<&std::path::Path>) -> Option<PathBuf> {
    let mut probes: Vec<PathBuf> = Vec::new();
    if let Ok(env_path) = std::env::var("SOVEREIGN_PDFIUM_LIB") {
        probes.push(PathBuf::from(env_path));
    }
    let mut search_roots: Vec<PathBuf> = Vec::new();
    if let Some(rd) = resource_dir {
        search_roots.push(rd.to_path_buf());
        search_roots.push(rd.join("binaries"));
    }
    // Dev fallback — same compile-time-baked binaries dir.
    search_roots.push(PathBuf::from(DEV_BINARIES_DIR));
    for root in &search_roots {
        for lib in ["libpdfium.dylib", "pdfium.dll", "libpdfium.so"] {
            probes.push(root.join("pdfium").join(lib));
            probes.push(root.join(lib));
        }
    }
    probes.into_iter().find(|p| p.exists())
}

/// Locate the PaddleOCR models ROOT — the directory that contains the
/// `<model_id>/` set (`det.onnx` + `rec.onnx` + `dict.txt`). Returned so
/// it can be handed straight to `SOVEREIGN_PADDLE_OCR_MODEL_DIR`, which
/// the engine's `paddle::models_root()` reads. A match is only returned
/// when all three model files actually exist, so the caller can trust
/// "Some" to mean "Paddle can run" rather than discovering a missing
/// model per-document later.
///
/// Probe order: explicit env → bundled (`<resource>/binaries/paddle-ocr`)
/// → dev binaries dir → the CLI/user models root (`~/.sovereign`).
#[cfg(feature = "paddle-ocr")]
fn resolve_paddle_model_dir(resource_dir: Option<&std::path::Path>) -> Option<PathBuf> {
    use sovereign_tools::local_corpus::ocr::paddle::DEFAULT_MODEL_ID;

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(env_path) = std::env::var("SOVEREIGN_PADDLE_OCR_MODEL_DIR") {
        roots.push(PathBuf::from(env_path));
    }
    if let Some(rd) = resource_dir {
        roots.push(rd.join("binaries").join("paddle-ocr"));
        roots.push(rd.join("paddle-ocr"));
    }
    roots.push(PathBuf::from(DEV_BINARIES_DIR).join("paddle-ocr"));
    // Dev/user fallback: the root the CLI fetch populates.
    if let Ok(home) = std::env::var("HOME") {
        roots.push(
            PathBuf::from(home)
                .join(".sovereign")
                .join("models")
                .join("paddle-ocr"),
        );
    }
    roots.into_iter().find(|root| {
        let set = root.join(DEFAULT_MODEL_ID);
        set.join("det.onnx").is_file()
            && set.join("rec.onnx").is_file()
            && set.join("dict.txt").is_file()
    })
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
/// type, then run a pre-scan. Returns the classification and the new
/// corpus_id. Progress events are emitted on
/// `local-corpus://progress/{job_id}` but the command is synchronous
/// end-to-end — callers await the return value.
///
/// `source_type` is `"obsidian"` or `"folder"`. `display_name` defaults
/// to the folder's basename.
#[tauri::command]
pub async fn lc_pre_scan(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    path: String,
    source_type: String,
    display_name: Option<String>,
) -> Result<PreScanResponse, String> {
    let manager = require_manager(&state).await?;
    let p = PathBuf::from(&path);
    if !p.exists() || !p.is_dir() {
        return Err(format!("Path does not exist or is not a directory: {path}"));
    }

    let config = match source_type.as_str() {
        "obsidian" => {
            let snap = manager.snapshot_root().to_path_buf();
            LocalCorpusConfig::obsidian_vault(p, snap)
        }
        "folder" => {
            let name = display_name.unwrap_or_else(|| {
                std::path::Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Documents")
                    .to_string()
            });
            LocalCorpusConfig::document_folder(PathBuf::from(&path), name)
        }
        other => return Err(format!("Unknown source_type: {other}")),
    };

    let job_id = new_job_id();
    let progress = Some(make_emitter(app.clone(), job_id.clone()));
    let corpus_id = manager
        .register(config.clone())
        .await
        .map_err(|e| format!("register: {e}"))?;

    let result = manager
        .pre_scan(&corpus_id, progress)
        .await
        .map_err(|e| format!("pre_scan: {e}"))?;

    Ok(PreScanResponse {
        job_id,
        result,
        corpus_id,
        display_name: config.display_name,
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
    let manager = require_manager(&state).await?;
    let job_id = new_job_id();
    let progress = make_emitter(app.clone(), job_id.clone());
    let daemon_url = state.client_base_url();

    // One-shot document folder OR Obsidian vault → the daemon owns ingest AND
    // enrich (it holds the tiered providers; the desktop's manager doesn't).
    // Ingesting here and enriching in the daemon deadlocks on the cross-process
    // index handoff, so hand the whole job over: a single in-process ingest +
    // tiered enrich, no CLI subprocess. The daemon does NOT add it to the sweep
    // scheduler ⇒ no ongoing watch — "a watched folder without the watching".
    // WatchedFolder is excluded (its reconciliation worker owns enrichment).
    if with_ocr != Some(true) {
        if let Some(cfg) = manager.get(&corpus_id).await {
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
        let inference = state.inference.read().await.as_ref().map(Arc::clone);
        let cfg = manager.list().await.into_iter().find(|c| c.id == corpus_id);
        match (inference, cfg) {
            // A daemon-routed (or in-process) provider + a non-OCR corpus → Runner.
            (Some(inference), Some(cfg)) if !cfg.ocr_pdfs => {
                let progress = progress.clone();
                tokio::spawn(run_ingest_via_runner(inference, cfg, corpus_id, progress));
                return Ok(job_id);
            }
            // Missing provider, unknown corpus, or OCR wanted → fall through to bespoke.
            _ => {
                tracing::info!(
                    %corpus_id,
                    "SOVEREIGN_RUNNER_INGEST set but Runner ingest unavailable \
                     (no provider / unknown corpus / OCR) — using bespoke ingest"
                );
            }
        }
    }

    // Bespoke path (default). Spawn so the command doesn't block; the UI drives the
    // progress panel off the emit channel; failure propagates via
    // `LocalCorpusProgress::Error`.
    //
    // Enrichment is daemon-side (tiered providers are wired only in the daemon),
    // so a one-shot document folder is handed to the daemon after ingest — see
    // the Ok branch below. The corpus-watch HTTP surface (incl. enrich-once) is
    // mounted on the CLIENT port, same router the watched-folder commands hit.
    // Capture it now; the Tauri `state` guard can't cross the spawn boundary.
    let daemon_url = state.client_base_url();
    tokio::spawn(async move {
        match manager
            .ingest(&corpus_id, with_ocr, Some(progress.clone()))
            .await
        {
            Ok(_stats) => {
                // The manager already emits Complete. Enrichment follows
                // ingest, but the tiered providers (RAPTOR + entity extraction)
                // live only in the daemon — the desktop's own manager can't run
                // them. So hand a one-shot document folder to the daemon:
                // register-without-watch + tiered build ("a watched folder
                // without the watching"). Watched folders and vaults already
                // enrich via the reconciliation worker, so this is gated to
                // DocumentFolder. Glassbox-logged for soak/chaos runs.
                if let Some(cfg) = manager.get(&corpus_id).await {
                    if matches!(
                        cfg.source_type,
                        sovereign_tools::local_corpus::config::LocalCorpusSourceType::DocumentFolder
                    ) {
                        let url = format!("{daemon_url}/internal/corpus/enrich-once");
                        tracing::info!(
                            %corpus_id,
                            "lc_ingest: document folder — requesting daemon-side tiered enrichment"
                        );
                        let client = reqwest::Client::new();
                        match client.post(&url).json(&cfg).send().await {
                            Ok(r) if r.status().is_success() => tracing::info!(
                                %corpus_id,
                                "lc_ingest: daemon accepted one-shot enrichment"
                            ),
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
                }
            }
            Err(e) => {
                let err = LocalCorpusProgress::Error {
                    message: e.to_string(),
                    recoverable: false,
                };
                progress(err);
            }
        }
    });
    Ok(job_id)
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
    let manager = require_manager(&state).await?;
    let cfg = manager
        .get(&corpus_id)
        .await
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

/// Ingest a folder corpus by running the shipped `notebook` workflow on the
/// Runner (the substrate adoption path), translating the Runner's
/// `WorkflowProgress` into the `LocalCorpusProgress` phases the desktop UI already
/// renders — so the progress panel needs no change. Emits the terminal
/// `Complete { Ingest(stats) }` / `Error` itself (the headless run is silent).
async fn run_ingest_via_runner(
    inference: Arc<dyn InferenceProvider>,
    cfg: LocalCorpusConfig,
    corpus_id: String,
    progress: ProgressCallback,
) {
    let started = std::time::Instant::now();

    let wf = match resolve_workflow_source("notebook")
        .and_then(|(toml, _)| Workflow::parse(&toml).map_err(|e| e.to_string()))
    {
        Ok(w) => w,
        Err(e) => {
            progress(LocalCorpusProgress::Error {
                message: format!("notebook workflow: {e}"),
                recoverable: false,
            });
            return;
        }
    };

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

    // Observer: map each Runner event onto a `LocalCorpusProgress::Ingesting` phase.
    let acc = Arc::new(Mutex::new(IngestAccumulator::default()));
    let observer: StepObserver = {
        let progress = progress.clone();
        let acc = Arc::clone(&acc);
        Arc::new(move |ev: WorkflowProgress| {
            if let Some(local) = workflow_progress_to_local(ev, &mut acc.lock().unwrap()) {
                progress(local);
            }
        })
    };

    let installer = Arc::new(HttpCorpusInstaller::new());
    match run_workflow_with_provider(
        &wf,
        Some(inference),
        Some(installer),
        4,
        false,
        params,
        // Preserve the full tool surface: the corpus/atlas tools moved out of the
        // host's base registry, so inject them here (the desktop links
        // sovereign-tools).
        sovereign_tools::workflow_corpus_tools(),
        Some(observer),
    )
    .await
    {
        Ok(report) => {
            // `chunks_written` parsed from each item's `tool:corpus_store` output
            // ("stored N chunks into corpus …"); best-effort, contributes 0 if the
            // shape ever changes. Excerpts + per-file failure detail are deferred
            // with the full cutover (the field's documented M1 default is empty).
            let chunks_written: u64 = report
                .items
                .iter()
                .filter_map(|it| it.result.as_ref().ok())
                .filter_map(|txt| txt.strip_prefix("stored "))
                .filter_map(|rest| rest.split_whitespace().next())
                .filter_map(|n| n.parse::<u64>().ok())
                .sum();
            let stats = IngestStats {
                corpus_id: corpus_id.clone(),
                files_indexed: report.ok_count(),
                chunks_written,
                runtime_failures: Vec::new(),
                excerpt_chunks: Vec::new(),
                duration_secs: started.elapsed().as_secs(),
            };
            progress(LocalCorpusProgress::Complete {
                result: CompletionResult::Ingest(stats),
            });
        }
        Err(e) => progress(LocalCorpusProgress::Error {
            message: e,
            recoverable: false,
        }),
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

/// Map a Runner [`WorkflowProgress`] event onto the desktop's
/// [`LocalCorpusProgress`] phase model. Returns `None` for events that don't move
/// the UI bar (`RunFinished` — the caller emits the terminal `Complete` after
/// computing stats; `ElementSkipped` — a per-element warning).
fn workflow_progress_to_local(
    ev: WorkflowProgress,
    acc: &mut IngestAccumulator,
) -> Option<LocalCorpusProgress> {
    match ev {
        WorkflowProgress::RunStarted { items, .. } => {
            acc.total = items as u64;
            acc.done = 0;
            Some(LocalCorpusProgress::Ingesting {
                done: 0,
                total: acc.total,
                phase_label: "Reading your documents".to_string(),
                current_file: None,
            })
        }
        WorkflowProgress::StepDone { item, uses, .. } => Some(LocalCorpusProgress::Ingesting {
            done: acc.done,
            total: acc.total,
            phase_label: friendly_phase(&uses).to_string(),
            current_file: (item != "·").then_some(item),
        }),
        WorkflowProgress::ItemDone { .. } => {
            acc.done = (acc.done + 1).min(acc.total.max(1));
            Some(LocalCorpusProgress::Ingesting {
                done: acc.done,
                total: acc.total,
                phase_label: "Building the index".to_string(),
                current_file: None,
            })
        }
        WorkflowProgress::RunFinished { .. } | WorkflowProgress::ElementSkipped { .. } => None,
    }
}

// ─── Command: lc_list ────────────────────────────────────────────────

#[tauri::command]
pub async fn lc_list(state: State<'_, Arc<AppState>>) -> Result<Vec<LocalCorpusConfig>, String> {
    let manager = require_manager(&state).await?;
    Ok(manager.list().await)
}

// ─── Command: lc_remove ──────────────────────────────────────────────

#[tauri::command]
pub async fn lc_remove(state: State<'_, Arc<AppState>>, corpus_id: String) -> Result<(), String> {
    let manager = require_manager(&state).await?;
    manager
        .remove(&corpus_id)
        .await
        .map_err(|e| format!("remove: {e}"))
}

// ─── Command: lc_incomplete_jobs ────────────────────────────────────

#[tauri::command]
pub async fn lc_incomplete_jobs(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<IncompleteJob>, String> {
    let manager = require_manager(&state).await?;
    Ok(manager.incomplete_jobs().await)
}

// ─── Command: lc_cancel ──────────────────────────────────────────────

/// Signal a running ingest (or cluster) for `corpus_id` to stop
/// cooperatively. Returns `true` when a flag was found and flipped.
/// The progress channel emits its final `Error { recoverable: true }`
/// once the engine loop exits.
#[tauri::command]
pub async fn lc_cancel(state: State<'_, Arc<AppState>>, corpus_id: String) -> Result<bool, String> {
    let manager = require_manager(&state).await?;
    Ok(manager.cancel(&corpus_id))
}

// ─── Command: lc_check_git ───────────────────────────────────────────

#[tauri::command]
pub async fn lc_check_git(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Option<GitStatus>, String> {
    let manager = require_manager(&state).await?;
    manager
        .check_git(&corpus_id)
        .await
        .map_err(|e| format!("check_git: {e}"))
}

// ─── Command: lc_write_tags ──────────────────────────────────────────

#[tauri::command]
pub async fn lc_write_tags(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    git_commit: Option<bool>,
) -> Result<WriteBackResult, String> {
    let manager = require_manager(&state).await?;
    manager
        .write_tags(&corpus_id, git_commit.unwrap_or(false))
        .await
        .map_err(|e| format!("write_tags: {e}"))
}

// ─── Command: lc_list_snapshots ──────────────────────────────────────

#[tauri::command]
pub async fn lc_list_snapshots(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<Vec<SnapshotMeta>, String> {
    let manager = require_manager(&state).await?;
    manager
        .list_snapshots(&corpus_id)
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
    let manager = require_manager(&state).await?;
    let path = StdPathBuf::from(snapshot_path);
    manager
        .rollback(&corpus_id, &path)
        .await
        .map_err(|e| format!("rollback: {e}"))
}

// ─── Command: lc_clean ───────────────────────────────────────────────

#[tauri::command]
pub async fn lc_clean(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
) -> Result<CleanResult, String> {
    let manager = require_manager(&state).await?;
    manager
        .clean(&corpus_id)
        .await
        .map_err(|e| format!("clean: {e}"))
}

// ─── Command: lc_search ──────────────────────────────────────────────

#[derive(Serialize)]
pub struct LocalSearchHit {
    pub content: String,
    pub title: Option<String>,
    pub corpus_id: String,
    pub score: f32,
}

// ─── Command: lc_cluster ─────────────────────────────────────────────

/// Begin clustering + LLM labelling for an already-ingested Obsidian
/// vault. Returns a `job_id` immediately; caller subscribes to the
/// progress channel as with ingestion.
#[tauri::command]
pub async fn lc_cluster(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    config: Option<ClusterConfig>,
) -> Result<String, String> {
    let manager = require_manager(&state).await?;
    let cfg = config.unwrap_or_default();
    let job_id = new_job_id();
    let progress = make_emitter(app.clone(), job_id.clone());

    tokio::spawn(async move {
        match manager.cluster(&corpus_id, &cfg, progress.clone()).await {
            Ok(_) => {
                // Emit a terminal Complete event. The UI calls
                // `lc_get_preview` next to fetch the renderable
                // shape; we don't inline it here because the preview
                // blob can be large (per-note assignments) and
                // progress events are meant to be cheap.
                progress(LocalCorpusProgress::Complete {
                    result: sovereign_tools::local_corpus::progress::CompletionResult::Ingest(
                        sovereign_tools::local_corpus::manager::IngestStats {
                            corpus_id: corpus_id.clone(),
                            files_indexed: 0,
                            chunks_written: 0,
                            runtime_failures: Vec::new(),
                            excerpt_chunks: Vec::new(),
                            duration_secs: 0,
                        },
                    ),
                });
            }
            Err(e) => {
                progress(LocalCorpusProgress::Error {
                    message: e.to_string(),
                    recoverable: false,
                });
            }
        }
    });
    Ok(job_id)
}

// ─── Command: lc_get_preview ─────────────────────────────────────────

/// Fetch the computed preview for a corpus that has had `lc_cluster`
/// run recently. Returns `NotFound` if no cluster result is cached.
#[tauri::command]
pub async fn lc_get_preview(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    config: Option<ClusterConfig>,
) -> Result<VaultPreview, String> {
    let manager = require_manager(&state).await?;
    let cfg = config.unwrap_or_default();
    manager
        .get_preview(&corpus_id, &cfg)
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
    let manager = require_manager(&state).await?;
    let hits = manager
        .search(&corpus_id, &query, limit.unwrap_or(10))
        .await
        .map_err(|e| format!("search: {e}"))?;
    Ok(hits
        .into_iter()
        .map(|c| LocalSearchHit {
            content: c.content,
            title: c.title,
            corpus_id: c.corpus_id,
            score: c.score,
        })
        .collect())
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
            WorkflowProgress::RunStarted {
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
            WorkflowProgress::StepDone {
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
                WorkflowProgress::ItemDone {
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
            WorkflowProgress::RunFinished { ok: 2, failed: 0 },
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
