// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bridge `WatchedDiff` → ingest's `LocalCorpusPort::apply_watched_update`
//! (the engine's `CorpusUpdater::apply_update`).
//!
//! Builds the new version's entries from the fresh walk snapshot and the
//! delta from the per-doc verdict, and constructs the
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

use corpus_index::ingest_port::{
    DocFetchFn, FetchedDoc, LocalCorpusPort, WatchedUpdate, WatchedUpdateProgressFn,
    WatchedUpdateStage,
};

use super::diff::WatchedDiff;
use super::events::{EventSink, WatchedFolderEvent};
use super::status::SweepPhase;
use super::walker::WalkSnapshot;
use crate::local_corpus::config::LocalCorpusConfig;
use crate::local_corpus::extract_stage;
use crate::local_corpus::ocr::OcrCtx;

/// A fetched document with the same source statement the initial ingest's
/// staging writes ([`extract_stage::stated_source`]), so the delta stores the
/// record the first ingest did.
fn fetched(content: String, path: &std::path::Path, ocr: bool) -> corpus_index::Result<FetchedDoc> {
    let (sha256, extractor) = extract_stage::stated_source(path, ocr)?;
    let source = corpus_index::index::DocSource::Hashed { sha256, extractor };
    Ok(FetchedDoc { content, source })
}

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
    engine: Arc<dyn LocalCorpusPort>,
    cfg: &LocalCorpusConfig,
    diff: &WatchedDiff,
    snapshot: &WalkSnapshot,
    ocr_ctx: Option<OcrCtx>,
    sink: &EventSink,
    now_unix: u64,
) -> Result<()> {
    // 1 + 2. The new version's entries from the snapshot, and the
    //    WatchedDiff as the update's delta (1:1 field rename).
    let entries: std::collections::HashMap<String, String> = snapshot
        .iter()
        .map(|(k, v)| (k.clone(), v.content_hash.clone()))
        .collect();
    let update = WatchedUpdate {
        corpus_id: cfg.id.clone(),
        version: format!("watched-{now_unix}"),
        entries,
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
    let fetch: DocFetchFn = Arc::new(move |doc_id: &str| {
        let snap = snapshot_arc.clone();
        let cfg = cfg_arc.clone();
        let ocr_ctx = ocr_ctx_arc.clone();
        let id = doc_id.to_owned();
        let fut = async move {
            let entry = snap.get(&id).ok_or_else(|| {
                corpus_index::error::Error::Extraction(format!(
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
                corpus_index::error::Error::Extraction(format!("watched_folder: extract task: {e}"))
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
                        return Err(corpus_index::error::Error::Extraction(format!(
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
                let content = crate::local_corpus::ocr::extract_pdf_via_ocr(
                    &path, &ctx, &display, 1, 1, None,
                )
                .await
                .map_err(|e| {
                    corpus_index::error::Error::Extraction(format!(
                        "watched_folder: ocr '{id}': {e}"
                    ))
                })?;
                return fetched(content, &path, true);
            }

            fetched(extracted, &path, false)
        };
        Box::pin(fut)
            as Pin<
                Box<
                    dyn std::future::Future<Output = corpus_index::error::Result<FetchedDoc>>
                        + Send,
                >,
            >
    });

    // 4. Bridge engine progress into our EventSink.
    let sink_for_pump = sink.clone();
    let corpus_id = cfg.id.clone();
    let progress: WatchedUpdateProgressFn = Box::new(move |stage, done, total| {
        sink_for_pump(WatchedFolderEvent::PhaseProgress {
            corpus_id: corpus_id.clone(),
            phase: phase_to_local(stage),
            done,
            total,
        });
    });

    engine
        .apply_watched_update(&update, fetch, progress)
        .await
        .map_err(|e| Error::Execution(format!("watched_folder apply_update: {e}")))
}

fn phase_to_local(p: WatchedUpdateStage) -> SweepPhase {
    match p {
        WatchedUpdateStage::Deletions => SweepPhase::Deleting,
        WatchedUpdateStage::Updates => SweepPhase::Updating,
        WatchedUpdateStage::Additions => SweepPhase::Adding,
    }
}
