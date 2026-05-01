//! Bridge `WatchedDiff` → `corpus_engine::update::CorpusUpdater::apply_update`.
//!
//! Builds a `VersionManifest` from the fresh walk snapshot, builds a
//! `ManifestDiff` from the per-doc verdict, and constructs the
//! `fetch_content` closure that re-stages a single file through
//! `extract_stage::extract_one` (which already wraps the
//! `safe_extract_pdf_text` panic guard).
//!
//! Idempotency: `CorpusUpdater::apply_update` checkpoints every
//! committed doc into `_update_progress.json`. A daemon crash
//! mid-phase resumes on the next sweep tick because `is_complete`
//! short-circuits or the per-phase loops skip already-done ids.

use std::pin::Pin;
use std::sync::Arc;

use sovereign_core::error::{Error, Result};
use tokio::sync::mpsc;

use corpus_engine::update::delta::{
    CorpusUpdater, ManifestDiff, UpdatePhase, UpdateProgress, VersionManifest,
};
use corpus_engine::CorpusEngine;

use super::diff::WatchedDiff;
use super::events::{EventSink, WatchedFolderEvent};
use super::status::SweepPhase;
use super::walker::WalkSnapshot;
use crate::local_corpus::config::LocalCorpusConfig;
use crate::local_corpus::extract_stage;
use crate::local_corpus::ocr::OcrCtx;

/// Apply a watched-folder diff through the engine's three-phase
/// updater. Emits `PhaseProgress` events on `sink` as the updater
/// moves through deletions → updates → additions.
///
/// `now_unix` is captured up front so the `version` field of the
/// `VersionManifest` is reproducible across restarts of the same
/// sweep — the engine uses it as a sentinel only, but we want the
/// log line to match what the user sees in the status file.
///
/// `ocr_ctx`: optional OCR context. When `Some` AND `cfg.ocr_pdfs`
/// is true, the per-file fetch closure dispatches scanned PDFs
/// through `extract_pdf_via_ocr` instead of the plain text-layer
/// extraction. When `None`, scanned PDFs return empty/short text
/// from `extract_one` — they still pass through but contribute
/// nothing useful to the index. The worker filters scanned PDFs out
/// of the diff in that case via `collect_failed_files`.
pub async fn apply_watched_diff(
    engine: Arc<CorpusEngine>,
    cfg: &LocalCorpusConfig,
    diff: &WatchedDiff,
    snapshot: &WalkSnapshot,
    ocr_ctx: Option<OcrCtx>,
    sink: &EventSink,
    now_unix: u64,
) -> Result<()> {
    // 1. Build the new VersionManifest from the snapshot.
    let entries: std::collections::HashMap<String, String> = snapshot
        .iter()
        .map(|(k, v)| (k.clone(), v.content_hash.clone()))
        .collect();
    let new_manifest = VersionManifest {
        corpus_id: cfg.id.clone(),
        version: format!("watched-{now_unix}"),
        entries,
    };

    // 2. Translate WatchedDiff → ManifestDiff (1:1 field rename).
    let mdiff = ManifestDiff {
        new_documents: diff.added.clone(),
        updated_documents: diff.modified.clone(),
        deleted_documents: diff.removed.clone(),
    };

    // 3. Build the fetch_content closure. Each call re-extracts one
    //    file from disk via the same extract_stage path the initial
    //    ingest uses, with one branch: when the file is a PDF, OCR
    //    is enabled, and an OcrCtx is installed, dispatch through
    //    the OCR pipeline (rasterize → tesseract → cleanup) for
    //    scanned PDFs. The pipeline transparently handles
    //    born-digital PDFs too (it OCRs every page regardless), so
    //    we only take the OCR branch when the plain extractor would
    //    produce empty text — otherwise we'd burn cycles OCR'ing
    //    pages that already have a clean text layer.
    let snapshot_arc = Arc::new(snapshot.clone());
    let cfg_arc = Arc::new(cfg.clone());
    let ocr_ctx_arc = Arc::new(ocr_ctx);
    let fetch = move |doc_id: &str| {
        let snap = snapshot_arc.clone();
        let cfg = cfg_arc.clone();
        let ocr_ctx = ocr_ctx_arc.clone();
        let id = doc_id.to_owned();
        let fut = async move {
            let entry = snap.get(&id).ok_or_else(|| {
                corpus_engine::error::Error::Extraction(format!(
                    "watched_folder: doc_id '{id}' missing from sweep snapshot"
                ))
            })?;
            let path = entry.absolute_path.clone();
            let cfg_inner = (*cfg).clone();
            // First pass: plain text-layer extraction. Cheap when
            // the file is markdown/txt/born-digital PDF.
            let plain_path = path.clone();
            let plain = tokio::task::spawn_blocking(move || {
                extract_stage::extract_one(&plain_path, &cfg_inner)
            })
            .await
            .map_err(|e| {
                corpus_engine::error::Error::Extraction(format!(
                    "watched_folder: extract task: {e}"
                ))
            })?;

            let is_pdf = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.eq_ignore_ascii_case("pdf"))
                .unwrap_or(false);
            let extracted = match plain {
                Ok(text) => text,
                Err(e) => {
                    if is_pdf && cfg.ocr_pdfs && ocr_ctx.is_some() {
                        // Plain extractor failed on a PDF and OCR is
                        // available — fall through to OCR. Common
                        // for scanned PDFs where pdf-extract panics.
                        String::new()
                    } else {
                        return Err(corpus_engine::error::Error::Extraction(format!(
                            "watched_folder: extract '{id}': {e}"
                        )));
                    }
                }
            };

            // Decide whether to fall through to OCR. Trigger when
            // the file is a PDF, OCR is enabled, an OcrCtx is
            // installed, AND the plain text is short enough to
            // suggest a scanned-without-text-layer document. The
            // 32-character threshold matches the spirit of the
            // pre-scan classifier (`pre_scanner.rs::classify_pdf_blocking`
            // looks for `< 20` words in the first 4KB).
            let needs_ocr = is_pdf
                && cfg.ocr_pdfs
                && ocr_ctx.as_ref().as_ref().is_some()
                && extracted.trim().len() < 32;

            if needs_ocr {
                let ctx = ocr_ctx
                    .as_ref()
                    .as_ref()
                    .expect("checked Some above")
                    .clone();
                let display = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("(unnamed)")
                    .to_string();
                tracing::debug!(
                    doc_id = %id,
                    path = %path.display(),
                    "watched_folder:ocr_fallback"
                );
                return crate::local_corpus::ocr::extract_pdf_via_ocr(
                    &path, &ctx, &display, 1, 1, None,
                )
                .await
                .map_err(|e| {
                    corpus_engine::error::Error::Extraction(format!(
                        "watched_folder: ocr '{id}': {e}"
                    ))
                });
            }

            Ok(extracted)
        };
        Box::pin(fut) as Pin<Box<dyn std::future::Future<Output = corpus_engine::error::Result<String>> + Send>>
    };

    // 4. Bridge engine progress channel into our EventSink.
    let (tx, mut rx) = mpsc::channel::<UpdateProgress>(32);
    let sink_for_pump = sink.clone();
    let corpus_id = cfg.id.clone();
    let pump = tokio::spawn(async move {
        while let Some(p) = rx.recv().await {
            sink_for_pump(WatchedFolderEvent::PhaseProgress {
                corpus_id: corpus_id.clone(),
                phase: phase_to_local(p.phase),
                done: p.current,
                total: p.total,
            });
        }
    });

    let updater = CorpusUpdater::new(engine).with_progress_tx(tx);
    let result = updater
        .apply_update(&cfg.id, &mdiff, &new_manifest, fetch)
        .await
        .map_err(|e| Error::Execution(format!("watched_folder apply_update: {e}")));

    // Drop the sender (held inside `updater`) so the pump exits.
    drop(updater);
    let _ = pump.await;
    result
}

fn phase_to_local(p: UpdatePhase) -> SweepPhase {
    match p {
        UpdatePhase::Deletions => SweepPhase::Deleting,
        UpdatePhase::Updates => SweepPhase::Updating,
        UpdatePhase::Additions => SweepPhase::Adding,
    }
}
