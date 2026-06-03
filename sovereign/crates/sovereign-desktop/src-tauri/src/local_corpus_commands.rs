//! Tauri command surface for local corpora (Folder Drop + Obsidian).
//!
//! One job_id-scoped event channel per invocation:
//! `local-corpus://progress/{job_id}`. The UI listens with
//! `listen<LocalCorpusProgress>(channel, handler)`.
//!
//! Commands are thin — they translate TS-friendly shapes into
//! `LocalCorpusManager` calls and forward progress events via
//! `AppHandle::emit`. All heavy lifting happens in `sovereign-tools`.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use sovereign_tools::local_corpus::{
    clusterer::ClusterConfig,
    git::GitStatus,
    manager::{IncompleteJob, ProgressCallback},
    ocr::{OcrCtx, OcrEngineKind},
    pre_scanner::PreScanResult,
    preview::VaultPreview,
    progress::LocalCorpusProgress,
    writeback::{CleanResult, RollbackResult, SnapshotMeta, WriteBackResult},
    LocalCorpusConfig, LocalCorpusManager,
};
use std::path::PathBuf as StdPathBuf;

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

    // Spawn so the command doesn't block. The UI drives the progress
    // panel off the emit channel; failure propagates via
    // `LocalCorpusProgress::Error`.
    tokio::spawn(async move {
        match manager
            .ingest(&corpus_id, with_ocr, Some(progress.clone()))
            .await
        {
            Ok(_stats) => {
                // The manager already emits Complete; nothing to do
                // here.
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
