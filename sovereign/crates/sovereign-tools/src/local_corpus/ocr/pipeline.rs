//! Per-file orchestration for the OCR path.
//!
//! `extract_pdf_via_ocr(path, ctx, on_progress)` is the single entry
//! point used by `extract_stage::extract_one` when it sees a PDF and
//! the corpus's `ocr_pdfs` flag is set. It:
//!
//!   1. Builds a `PdfiumRasterizer` (paying the dynamic-library bind
//!      cost once per file). For batch ingest we expose a lower-level
//!      `extract_pdf_with_rasterizer` so the caller can amortize the
//!      bind across the whole queue.
//!   2. Renders every page to a `DynamicImage`.
//!   3. For each page: tesseract → cleanup → push the result. On
//!      per-page failure, push the placeholder marker.
//!   4. Concatenates pages with `\n\n---\n<!-- page N -->\n\n`
//!      between them, so downstream chunking can attribute chunks
//!      back to a page.
//!
//! Per-page failures NEVER fail the document. The placeholder is the
//! contract; it gives the user something to point at when they search
//! for content from a page that didn't OCR cleanly.

use std::path::Path;

use super::cleanup::{cleanup_page, CleanupError};
use super::rasterize::{PdfiumRasterizer, Rasterizer};
use super::tesseract::{recognize_page, TesseractError};
use super::{OcrCtx, PageProgress, PageProgressCallback};

/// One-shot entry point: rasterize, OCR, clean, assemble. Builds a
/// fresh `PdfiumRasterizer` per call. For batch ingest, prefer
/// `extract_pdf_with_rasterizer` so the rasterizer can be reused.
pub async fn extract_pdf_via_ocr(
    path: &Path,
    ctx: &OcrCtx,
    file_display_name: &str,
    file_idx: u32,
    file_total: u32,
    on_progress: Option<PageProgressCallback>,
) -> Result<String, String> {
    let rasterizer = PdfiumRasterizer::new(ctx)?;
    extract_pdf_with_rasterizer(
        &rasterizer,
        path,
        ctx,
        file_display_name,
        file_idx,
        file_total,
        on_progress,
    )
    .await
}

/// Lower-level entry point that takes a pre-built rasterizer. The
/// rasterizer is `dyn Rasterizer` so tests pass a `StubRasterizer`
/// that returns hand-built images and exercises the per-page fallout
/// (cleanup, placeholders, assembly).
pub async fn extract_pdf_with_rasterizer<R: Rasterizer + ?Sized>(
    rasterizer: &R,
    path: &Path,
    ctx: &OcrCtx,
    file_display_name: &str,
    file_idx: u32,
    file_total: u32,
    on_progress: Option<PageProgressCallback>,
) -> Result<String, String> {
    let pages = rasterizer.pdf_to_pages(path)?;
    let total_pages = pages.len() as u32;

    if pages.is_empty() {
        return Err(format!(
            "no pages rendered from {} — may be encrypted or corrupt",
            path.display()
        ));
    }

    let mut assembled = String::with_capacity(8 * 1024);

    for (idx, image) in pages.into_iter().enumerate() {
        let page_no = (idx as u32) + 1;
        if let Some(cb) = on_progress.as_ref() {
            cb(PageProgress {
                file_display_name: file_display_name.to_string(),
                current_page: page_no,
                total_pages,
                file_idx,
                file_total,
            });
        }

        // Tesseract is CPU-bound; hop onto a blocking thread so the
        // async cleanup HTTP call below doesn't block the runtime.
        let ctx_for_blocking = ctx.clone();
        let raw = tokio::task::spawn_blocking(move || {
            recognize_page(&image, &ctx_for_blocking)
        })
        .await;

        let cleaned = match raw {
            Ok(Ok(raw_text)) => match cleanup_page(&raw_text, ctx).await {
                Ok(text) => text,
                Err(e) => {
                    // Cleanup is a quality polish, not a correctness
                    // gate. When it fails (daemon down, model not
                    // loaded, etc.), keep the raw tesseract text — it
                    // still indexes and searches, just with broken
                    // lines and OCR artefacts. Better than a black
                    // hole. The marker tells the user this page used
                    // raw OCR so they understand any roughness.
                    tracing::warn!(
                        path = %path.display(),
                        page = page_no,
                        "OCR cleanup failed; falling back to raw OCR text: {}",
                        e.user_message()
                    );
                    if raw_text.trim().is_empty() {
                        placeholder_for(page_no, &cleanup_failure_label(&e))
                    } else {
                        format!(
                            "<!-- page {page_no}: raw OCR (cleanup unavailable: {}) -->\n\n{}",
                            cleanup_failure_label(&e),
                            raw_text.trim_end(),
                        )
                    }
                }
            },
            Ok(Err(e)) => {
                tracing::warn!(
                    path = %path.display(),
                    page = page_no,
                    "tesseract failed: {}",
                    e.user_message()
                );
                placeholder_for(page_no, &tesseract_failure_label(&e))
            }
            Err(join_err) => {
                tracing::warn!(
                    path = %path.display(),
                    page = page_no,
                    "tesseract join error: {join_err}"
                );
                placeholder_for(page_no, "engine task crashed")
            }
        };

        if idx > 0 {
            assembled.push_str(&format!("\n\n---\n<!-- page {page_no} -->\n\n"));
        }
        assembled.push_str(cleaned.trim_end());
    }

    Ok(assembled)
}

/// Render the standard "this page couldn't be read" marker. Embedded
/// in the document content so search results can show the user which
/// pages we lost vs which produced text.
pub(crate) fn placeholder_for(page: u32, reason: &str) -> String {
    format!("<!-- page {page}: could not be read ({reason}) -->")
}

fn tesseract_failure_label(e: &TesseractError) -> String {
    match e {
        TesseractError::Timeout => "OCR timed out".into(),
        TesseractError::NonZero { .. } => "OCR engine error".into(),
        TesseractError::Spawn(_) => "OCR engine missing".into(),
        TesseractError::Io(_) => "page image staging failed".into(),
    }
}

fn cleanup_failure_label(e: &CleanupError) -> String {
    match e {
        CleanupError::Timeout => "cleanup timed out".into(),
        CleanupError::Unreachable(_) => "inference daemon unreachable".into(),
        CleanupError::Http { status, .. } => format!("daemon error {status}"),
        CleanupError::Malformed(_) => "daemon response malformed".into(),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use image::{DynamicImage, ImageBuffer, Rgb};

    use crate::local_corpus::ocr::rasterize::Rasterizer;

    /// Stub that returns canned page images.
    struct StubRasterizer {
        pages: Vec<DynamicImage>,
    }

    impl Rasterizer for StubRasterizer {
        fn pdf_to_pages(&self, _: &Path) -> Result<Vec<DynamicImage>, String> {
            Ok(self.pages.clone())
        }
    }

    fn solid_white(w: u32, h: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(ImageBuffer::from_fn(w, h, |_, _| Rgb([255u8, 255, 255])))
    }

    #[tokio::test]
    async fn no_pages_yields_typed_error() {
        let stub = StubRasterizer { pages: vec![] };
        let ctx = OcrCtx::for_test(
            PathBuf::from("/this/binary/does/not/exist"),
            PathBuf::from("/no-tessdata"),
            "http://127.0.0.1:1".into(),
        );
        let res = extract_pdf_with_rasterizer(
            &stub,
            Path::new("/fake.pdf"),
            &ctx,
            "fake",
            1,
            1,
            None,
        )
        .await;
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("no pages rendered"));
    }

    #[tokio::test]
    async fn missing_tesseract_inserts_placeholder_per_page() {
        // No tesseract binary → every page falls back to the
        // placeholder. Daemon URL never gets hit because tesseract
        // fails first.
        let stub = StubRasterizer {
            pages: vec![solid_white(8, 8), solid_white(8, 8)],
        };
        let ctx = OcrCtx::for_test(
            PathBuf::from("/this/binary/does/not/exist/tesseract"),
            PathBuf::from("/no-tessdata"),
            "http://127.0.0.1:1".into(),
        );
        let out = extract_pdf_with_rasterizer(
            &stub,
            Path::new("/fake.pdf"),
            &ctx,
            "fake",
            1,
            1,
            None,
        )
        .await
        .unwrap();
        // Two pages, two placeholders, separator between them.
        assert!(out.contains("page 1: could not be read"));
        assert!(out.contains("page 2: could not be read"));
        assert!(out.contains("---\n<!-- page 2 -->"));
    }

    #[tokio::test]
    async fn progress_callback_fires_once_per_page() {
        let stub = StubRasterizer {
            pages: vec![solid_white(8, 8), solid_white(8, 8), solid_white(8, 8)],
        };
        let ctx = OcrCtx::for_test(
            PathBuf::from("/this/binary/does/not/exist"),
            PathBuf::from("/no-tessdata"),
            "http://127.0.0.1:1".into(),
        );

        let calls: Arc<std::sync::Mutex<Vec<(u32, u32)>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_cb = Arc::clone(&calls);
        let cb: PageProgressCallback = Arc::new(move |p: PageProgress| {
            calls_cb
                .lock()
                .unwrap()
                .push((p.current_page, p.total_pages));
        });

        let _ = extract_pdf_with_rasterizer(
            &stub,
            Path::new("/fake.pdf"),
            &ctx,
            "fake",
            1,
            1,
            Some(cb),
        )
        .await
        .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(*calls, vec![(1, 3), (2, 3), (3, 3)]);
    }

    #[test]
    fn placeholder_format_is_stable() {
        let out = placeholder_for(7, "OCR timed out");
        assert_eq!(out, "<!-- page 7: could not be read (OCR timed out) -->");
    }
}
