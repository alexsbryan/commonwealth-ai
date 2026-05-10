//! PDF → page images via `pdfium-render`.
//!
//! pdfium-render binds to the same PDFium engine Chrome's PDF viewer
//! uses, so it's robust on the long tail of weird PDFs. The dynamic
//! library (`libpdfium.dylib` / `pdfium.dll` / `libpdfium.so`) is NOT
//! linked at compile time — the Tauri build ships it as a bundled
//! resource and `OcrCtx::pdfium_lib_path` points to it at runtime.
//!
//! The `Rasterizer` trait exists so the higher stages can be tested
//! without a working PDFium install. Production code calls
//! `PdfiumRasterizer::new(&ctx)?.pdf_to_pages(path)`; tests pass a
//! `StubRasterizer` that returns a hand-built `DynamicImage` per page.

use std::path::Path;

use image::DynamicImage;

use super::OcrCtx;

/// Trait abstraction so the pipeline can be tested without pdfium.
pub trait Rasterizer {
    /// Render every page of `path` to a `DynamicImage` at the
    /// configured DPI. The returned vec is in page order; pages that
    /// fail to render are NOT included — caller treats a missing page
    /// as a per-page failure.
    fn pdf_to_pages(&self, path: &Path) -> Result<Vec<DynamicImage>, String>;
}

// ─── PDFium-backed rasterizer ────────────────────────────────────────

/// Production rasterizer. Holds the loaded `Pdfium` instance so we
/// pay the dynamic-library bind cost once per ingest.
pub struct PdfiumRasterizer {
    pdfium: pdfium_render::prelude::Pdfium,
    target_width_px: i32,
}

impl PdfiumRasterizer {
    /// Build a rasterizer from the OCR context. Loads the PDFium
    /// dynamic library at `ctx.pdfium_lib_path` if set, otherwise
    /// falls back to pdfium-render's bundled-or-system search.
    pub fn new(ctx: &OcrCtx) -> Result<Self, String> {
        use pdfium_render::prelude::*;

        let bindings = match &ctx.pdfium_lib_path {
            Some(path) => Pdfium::bind_to_library(path)
                .map_err(|e| format!("pdfium: bind to {}: {e}", path.display()))?,
            None => Pdfium::bind_to_system_library()
                .map_err(|e| format!("pdfium: bind to system library: {e}"))?,
        };
        let pdfium = Pdfium::new(bindings);

        // Convert DPI into a target image width. A4 at 1× is
        // ≈ 8.27 inches wide, so target_width_px = dpi * 8.27.
        // pdfium-render's PdfRenderConfig sizes by target dimensions,
        // not DPI directly, so we approximate and let the height fall
        // out from the page aspect ratio.
        let target_width_px = (ctx.dpi as f32 * 8.27).round() as i32;

        Ok(Self {
            pdfium,
            target_width_px,
        })
    }
}

impl Rasterizer for PdfiumRasterizer {
    fn pdf_to_pages(&self, path: &Path) -> Result<Vec<DynamicImage>, String> {
        use pdfium_render::prelude::*;

        let document = self
            .pdfium
            .load_pdf_from_file(path, None)
            .map_err(|e| format!("pdfium: load {}: {e}", path.display()))?;

        let render_config = PdfRenderConfig::new()
            .set_target_width(self.target_width_px)
            // Cap height at ~3× width so a freakishly tall page
            // (e.g. a stitched receipt) doesn't run pdfium out of
            // memory. Real document pages stay well under this.
            .set_maximum_height(self.target_width_px.saturating_mul(3));

        let pages = document.pages();
        let mut out = Vec::with_capacity(pages.len() as usize);
        for (idx, page) in pages.iter().enumerate() {
            match page.render_with_config(&render_config) {
                Ok(bitmap) => match bitmap.as_image() {
                    Ok(img) => out.push(img),
                    Err(e) => {
                        tracing::warn!(
                            path = %path.display(),
                            page = idx + 1,
                            "pdfium bitmap → image conversion failed: {e:?}"
                        );
                    }
                },
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        page = idx + 1,
                        "pdfium render failed: {e:?}"
                    );
                    // Skip this page; the pipeline inserts the
                    // `<!-- page N: could not be read -->` marker for
                    // any page index missing from the output vec.
                }
            }
        }
        Ok(out)
    }
}

// ─── Convenience free function ───────────────────────────────────────

/// One-shot rasterize using the pdfium-backed rasterizer. Builds the
/// rasterizer fresh per call — fine for one-off CLI usage but the
/// pipeline holds onto a single instance to amortize the library bind.
pub fn pdf_to_pages(ctx: &OcrCtx, path: &Path) -> Result<Vec<DynamicImage>, String> {
    let r = PdfiumRasterizer::new(ctx)?;
    r.pdf_to_pages(path)
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    /// Stub used by higher-stage tests to avoid loading PDFium.
    pub struct StubRasterizer {
        pub pages: Vec<DynamicImage>,
    }

    impl Rasterizer for StubRasterizer {
        fn pdf_to_pages(&self, _path: &Path) -> Result<Vec<DynamicImage>, String> {
            Ok(self.pages.clone())
        }
    }

    fn solid_color_page(w: u32, h: u32, rgb: [u8; 3]) -> DynamicImage {
        let buf = ImageBuffer::from_fn(w, h, |_, _| Rgb(rgb));
        DynamicImage::ImageRgb8(buf)
    }

    #[test]
    fn stub_rasterizer_returns_configured_pages() {
        let stub = StubRasterizer {
            pages: vec![
                solid_color_page(8, 8, [255, 255, 255]),
                solid_color_page(8, 8, [0, 0, 0]),
            ],
        };
        let pages = stub.pdf_to_pages(Path::new("/nonexistent.pdf")).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].width(), 8);
    }
}
