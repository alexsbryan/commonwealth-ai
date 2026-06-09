// SPDX-License-Identifier: AGPL-3.0-or-later
//! OCR pipeline for scanned PDFs in the folder-drop flow.
//!
//! Wired in front of `extract_stage::extract_one` when the per-corpus
//! `ocr_pdfs` flag is set. Three stages, each a sibling module:
//!
//! ```text
//! Scanned PDF
//!   │  rasterize::pdf_to_pages       (pdfium-render → PNG images at N DPI)
//!   ▼
//! Page images
//!   │  tesseract::recognize_page     (sidecar subprocess: image → raw text)
//!   ▼
//! Raw OCR text per page
//!   │  cleanup::cleanup_page         (daemon /v1/chat/completions, fast slot)
//!   ▼
//! Cleaned markdown per page
//!   │  pipeline::assemble_pages      (concat with `---` page break markers)
//!   ▼
//! Single markdown blob → existing extract_stage JSONL line
//! ```
//!
//! Per-page failures (tesseract crash, cleanup HTTP error, timeout) are
//! soft — we insert a `<!-- page N: could not be read -->` marker and
//! continue with the rest of the document. Per-document failures bubble
//! up and land in the existing `runtime_failures` collector that the UI
//! already surfaces on the completion screen.
//!
//! Why the architecture: all LLM work in this codebase flows through
//! the daemon (`/v1/chat/completions`), never bundled in-process. That
//! rules out Qianfan-OCR / vision-LLM GGUFs in v1. Tesseract gives us
//! raw text; the daemon's already-resident fast slot gives us the
//! cleanup pass that turns column-flow garbage into searchable
//! paragraphs. Retrieval quality is the gain; tesseract alone produces
//! text noisy enough to *degrade* the index.

pub mod cleanup;
pub mod engine;
pub mod pipeline;
pub mod rasterize;
pub mod tesseract;

/// PaddleOCR (ONNX) engine — an alternative to the tesseract subprocess
/// that drives the ONNX Runtime already linked for GLiNER. Gated so
/// headless/default builds don't pull the OCR model-inference surface.
#[cfg(feature = "paddle-ocr")]
pub mod paddle;

use std::path::PathBuf;

/// Runtime configuration for the OCR pipeline. One instance is built
/// per ingest and threaded through every stage. Built at the manager
/// boundary so tests can stub each piece independently.
///
/// Defaults are deliberately *not* embedded here — the desktop layer
/// resolves real paths (Tauri sidecar, pdfium dylib) and passes them
/// in. Tests construct `OcrCtx` directly with stubbed paths.
#[derive(Debug, Clone)]
pub struct OcrCtx {
    /// Absolute path to the `tesseract` binary. Resolved by the desktop
    /// from the bundled Tauri sidecar; in CLI / tests, can point at a
    /// system install.
    pub tesseract_bin: PathBuf,
    /// Directory containing `eng.traineddata` (and any other language
    /// packs). Passed to tesseract via `TESSDATA_PREFIX`.
    pub tessdata_dir: PathBuf,
    /// Path to the PDFium dynamic library (`libpdfium.dylib` on macOS,
    /// `pdfium.dll` on Windows, `libpdfium.so` on Linux). When `None`,
    /// pdfium-render falls back to its bundled-or-system search.
    pub pdfium_lib_path: Option<PathBuf>,
    /// Daemon base URL for the cleanup pass — typically
    /// `http://127.0.0.1:9741`. The cleanup module appends
    /// `/v1/chat/completions`.
    pub daemon_base_url: String,
    /// Model id to request for the cleanup pass. Must be a name the
    /// daemon's `/v1/chat/completions` route can resolve — typically
    /// the file stem of the chat slot's gguf (e.g.
    /// `"Qwen3.5-9B.Q8_0"`), since the daemon registers each loaded
    /// slot under its file stem. The desktop layer reads this from
    /// `AppConfig.model_path` and passes it in. There's no "fast"
    /// alias in the routing layer today — passing `"fast"` 503s on
    /// CLI-daemon setups.
    pub cleanup_model: String,
    /// DPI for rasterization. Tesseract's documented optimum for
    /// English printed text is 300.
    pub dpi: u32,
    /// Per-page tesseract timeout. Pages that time out get the
    /// `<!-- could not be read -->` placeholder.
    pub tesseract_timeout_secs: u64,
    /// Per-page cleanup HTTP request timeout. Same fallback behaviour
    /// as tesseract.
    pub cleanup_timeout_secs: u64,
    /// Which recognition engine to build for this ingest. Defaults to
    /// `Tesseract` (the v1 engine); `Paddle` selects the ONNX PaddleOCR
    /// engine, which `build_engine` constructs only when the
    /// `paddle-ocr` feature is compiled in. The tesseract-specific
    /// fields above are ignored when this is `Paddle`; the paddle model
    /// paths are resolved by the engine itself (env / `~/.sovereign`).
    pub engine: engine::OcrEngineKind,
}

impl OcrCtx {
    /// Construction helper for tests — every field stubbed except
    /// the ones the caller wants to vary. Production code uses the
    /// desktop-built ctx, never this.
    #[cfg(test)]
    pub fn for_test(
        tesseract_bin: PathBuf,
        tessdata_dir: PathBuf,
        daemon_base_url: String,
    ) -> Self {
        Self {
            tesseract_bin,
            tessdata_dir,
            pdfium_lib_path: None,
            daemon_base_url,
            cleanup_model: "fast".into(),
            dpi: 300,
            tesseract_timeout_secs: 30,
            cleanup_timeout_secs: 30,
            engine: engine::OcrEngineKind::Tesseract,
        }
    }
}

/// Page-level progress callback signature, shared by `pipeline` and
/// the manager glue that surfaces it onto the
/// `local-corpus://progress/{job_id}` channel.
pub type PageProgressCallback = std::sync::Arc<dyn Fn(PageProgress) + Send + Sync>;

/// One page-level event for the UI.
#[derive(Debug, Clone)]
pub struct PageProgress {
    /// File whose pages are being read, e.g. "FOIA response final v2".
    pub file_display_name: String,
    /// 1-based current page within this file.
    pub current_page: u32,
    /// Total pages in this file.
    pub total_pages: u32,
    /// 1-based current file index across the OCR queue.
    pub file_idx: u32,
    /// Total files in the OCR queue.
    pub file_total: u32,
}

pub use cleanup::cleanup_page;
pub use engine::{build_engine, OcrEngine, OcrEngineKind, OcrError};
pub use pipeline::extract_pdf_via_ocr;
pub use rasterize::{pdf_to_pages, Rasterizer};
pub use tesseract::recognize_page;
