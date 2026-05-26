//! The OCR engine seam.
//!
//! `recognize_page` (tesseract subprocess) was the original single OCR
//! implementation, called directly by `pipeline.rs`. To let us swap in
//! an alternative engine (PaddleOCR via ONNX, behind the `paddle-ocr`
//! feature) without touching the per-page orchestration, recognition is
//! now expressed as a trait:
//!
//! ```text
//! OcrCtx.engine  ──build_engine()──▶  Arc<dyn OcrEngine>
//!                                          │  recognize(&page) per page
//!                                          ▼
//!                                     raw page text → cleanup → assemble
//! ```
//!
//! The engine is built ONCE per ingest (like `PdfiumRasterizer`) so a
//! stateful engine — PaddleOCR holds two loaded ONNX sessions — pays its
//! load cost a single time, not per page. Tesseract is stateless, so its
//! engine is a thin handle over the resolved binary/tessdata paths.
//!
//! Failure model mirrors what `pipeline.rs` already does with
//! `TesseractError`: a per-page `OcrError` becomes the
//! `<!-- could not be read -->` placeholder for that page, never a
//! document-level failure. Engine *construction* failure (a missing
//! model) is surfaced by `build_engine` instead, so it fails the
//! document once rather than placeholder-ing every page.

use image::DynamicImage;

use super::OcrCtx;

/// One page image → its recognized text. Implementors hold whatever
/// state they need (subprocess paths, loaded models) and are built once
/// per ingest via [`build_engine`]. `Send + Sync` so the pipeline can
/// share an `Arc<dyn OcrEngine>` across the `spawn_blocking` per-page
/// calls.
pub trait OcrEngine: Send + Sync {
    /// Stable engine name for glassbox logging (`"tesseract"`,
    /// `"paddleocr"`). Shows up in the per-page tracing so a reader of
    /// the logs can tell which engine produced a given page.
    fn name(&self) -> &'static str;

    /// Recognize every line of text on one rasterized page. Returns the
    /// raw text in the engine's native formatting (line breaks,
    /// hyphenation, headers all preserved) — the downstream cleanup pass
    /// is what reformats it into searchable paragraphs.
    fn recognize(&self, image: &DynamicImage) -> Result<String, OcrError>;
}

/// A per-page recognition failure. Variants mirror the failure classes
/// `pipeline.rs` already distinguishes for tesseract so the placeholder
/// labels stay stable across engines.
#[derive(Debug)]
pub enum OcrError {
    /// The engine ran but reported an error recognizing this page.
    Page(String),
    /// The engine did not finish within its per-page timeout.
    Timeout,
    /// Staging the page image for the engine failed (temp file, encode).
    Io(String),
    /// The engine could not run at all (binary missing / not executable).
    /// Distinct from `Page` so the placeholder can say "engine missing"
    /// rather than implying the page itself was unreadable.
    Unavailable(String),
}

impl OcrError {
    /// Short, user-facing label embedded in the page placeholder. Kept
    /// in lock-step with the labels `pipeline.rs` historically produced
    /// from `TesseractError` so existing snapshots/searches are stable.
    pub fn placeholder_label(&self) -> String {
        match self {
            OcrError::Timeout => "OCR timed out".into(),
            OcrError::Page(_) => "OCR engine error".into(),
            OcrError::Unavailable(_) => "OCR engine missing".into(),
            OcrError::Io(_) => "page image staging failed".into(),
        }
    }

    /// Full message for logging (includes the underlying detail the
    /// terse `placeholder_label` drops).
    pub fn detail(&self) -> String {
        match self {
            OcrError::Timeout => "OCR engine timed out".into(),
            OcrError::Page(e) => format!("OCR engine failed: {e}"),
            OcrError::Unavailable(e) => format!("OCR engine could not run: {e}"),
            OcrError::Io(e) => format!("could not stage page for OCR: {e}"),
        }
    }
}

/// Build the OCR engine selected by `ctx.engine`, once per ingest.
///
/// Tesseract construction is infallible (it's a thin path handle;
/// per-page spawn failures surface later as `OcrError::Unavailable`).
/// PaddleOCR construction loads ONNX models and CAN fail — that failure
/// returns `Err` here so the document fails once with a clear message,
/// rather than every page producing a placeholder.
pub fn build_engine(ctx: &OcrCtx) -> Result<std::sync::Arc<dyn OcrEngine>, String> {
    match ctx.engine {
        OcrEngineKind::Tesseract => Ok(std::sync::Arc::new(
            super::tesseract::TesseractEngine::from_ctx(ctx),
        )),
        OcrEngineKind::Paddle => {
            #[cfg(feature = "paddle-ocr")]
            {
                let engine = super::paddle::PaddleEngine::from_ctx(ctx)
                    .map_err(|e| format!("paddleocr engine init: {e}"))?;
                Ok(std::sync::Arc::new(engine))
            }
            #[cfg(not(feature = "paddle-ocr"))]
            {
                Err("OcrCtx requested the PaddleOCR engine but this build \
                     does not have the `paddle-ocr` feature enabled"
                    .to_string())
            }
        }
    }
}

/// Which recognition engine an [`OcrCtx`] selects. Defaults to
/// `Tesseract` — the engine v1 ships — so existing construction sites
/// that don't set it keep their current behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OcrEngineKind {
    /// Tesseract subprocess sidecar (the v1 default).
    #[default]
    Tesseract,
    /// PaddleOCR via ONNX Runtime, in-process. Requires the `paddle-ocr`
    /// cargo feature; `build_engine` errors clearly if it's absent.
    Paddle,
}
