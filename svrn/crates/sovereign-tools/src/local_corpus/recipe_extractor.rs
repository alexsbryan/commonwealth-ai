// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wire `local_corpus`'s PDF reading into the ingest engine (through
//! `IngestPluginPort`) so recipes declaring
//! `extract = { type = "custom", kind = "pdf" }` can ingest PDFs
//! downloaded by `http_api + follow` (or any other acquirer).
//!
//! A recipe's PDF reads as a watched folder's does: the text layer, and
//! PaddleOCR when the layer is too thin (`ocr::text_layer_needs_ocr`) and
//! the daemon has an `OcrCtx`. Without one, a scanned PDF yields its thin
//! text layer, as it always has; the registration log says which posture
//! this daemon took.
//!
//! Why this lives in `sovereign-tools` and not in `corpus-engine`:
//! `pdf-extract` is a ~30 MB dep and corpus-engine deliberately stays
//! lean so cloud-build deployments that never see a PDF don't pay the
//! weight. The daemon registers it at boot
//! (`sovereign-daemon/src/bootstrap.rs::register_recipe_extractors`); a
//! recipe ingest on an engine nobody registered it on panics at install.
use std::path::Path;
use std::sync::Arc;

use corpus_index::ingest_port::{CustomExtractorFn, IngestPluginPort};
use corpus_index::{Error, Result};

use super::extract_stage::{safe_extract_pdf_text, SafeExtractError};
use super::ocr::{extract_pdf_via_ocr, text_layer_needs_ocr, OcrCtx};

/// Register the `"pdf"` per-file extractor on `engine`, reading scanned
/// PDFs through `ocr` when it is `Some`. Idempotent: re-registering
/// overwrites.
pub fn register_pdf_extractor<P: IngestPluginPort + ?Sized>(engine: &P, ocr: Option<OcrCtx>) {
    tracing::info!(
        ocr = ocr.is_some(),
        "recipe_extractor:pdf_registered — recipes of kind \"pdf\" read the text layer{}",
        if ocr.is_some() {
            ", and OCR a layer too thin to index"
        } else {
            " only; a scanned PDF indexes its thin text layer (no OcrCtx installed)"
        }
    );
    let extractor: CustomExtractorFn = Arc::new(move |path| extract_recipe_pdf(path, ocr.as_ref()));
    engine.register_extractor("pdf", extractor);
}

/// One PDF's text: its text layer, or OCR when the layer is too thin or
/// unreadable and `ocr` is installed. An encrypted PDF is refused either
/// way; OCR cannot read past a password.
fn extract_recipe_pdf(path: &Path, ocr: Option<&OcrCtx>) -> Result<String> {
    let layer = safe_extract_pdf_text(path);
    let Some(ctx) = ocr else {
        if let Ok(text) = &layer {
            if text_layer_needs_ocr(text) {
                tracing::debug!(
                    path = %path.display(),
                    chars = text.trim().len(),
                    "recipe_extractor:pdf_thin_layer_no_ocr"
                );
            }
        }
        return layer.map_err(|e| extract_error(path, e));
    };
    let reason = match layer {
        Ok(text) if !text_layer_needs_ocr(&text) => return Ok(text),
        Err(SafeExtractError::Encrypted) => {
            return Err(extract_error(path, SafeExtractError::Encrypted))
        }
        Ok(text) => format!("text layer of {} chars", text.trim().len()),
        Err(e) => format!("text layer unreadable: {e:?}"),
    };
    tracing::debug!(path = %path.display(), %reason, "recipe_extractor:pdf_ocr_fallback");
    ocr_blocking(path, ctx)
}

/// Run the async OCR pipeline from the engine's synchronous extractor
/// call. The extractor may be driven on a runtime worker thread, where
/// `block_on` panics, so the pipeline gets a thread and a current-thread
/// runtime of its own.
fn ocr_blocking(path: &Path, ctx: &OcrCtx) -> Result<String> {
    let display = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("(unnamed)")
        .to_string();
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| Error::Extraction(format!("pdf ocr runtime: {e}")))?;
                runtime
                    .block_on(extract_pdf_via_ocr(path, ctx, &display, 1, 1, None))
                    .map_err(|e| {
                        Error::Extraction(format!("pdf ocr failed on {}: {e}", path.display()))
                    })
            })
            .join()
            .unwrap_or_else(|_| {
                Err(Error::Extraction(format!(
                    "pdf ocr panicked on {}",
                    path.display()
                )))
            })
    })
}

fn extract_error(path: &Path, e: SafeExtractError) -> Error {
    Error::Extraction(match e {
        SafeExtractError::Encrypted => {
            format!("pdf is encrypted/password-protected: {}", path.display())
        }
        SafeExtractError::Parse(msg) => format!("pdf parse error in {}: {msg}", path.display()),
        SafeExtractError::Panic(msg) => {
            format!("pdf-extract panicked on {}: {msg}", path.display())
        }
        SafeExtractError::Other(msg) => format!("pdf extract error on {}: {msg}", path.display()),
    })
}

#[cfg(test)]
#[path = "recipe_extractor_tests.rs"]
mod tests;
