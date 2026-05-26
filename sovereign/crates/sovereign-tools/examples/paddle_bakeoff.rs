//! Tesseract-vs-PaddleOCR bake-off harness.
//!
//! Rasterizes a scanned PDF via the existing `PdfiumRasterizer`, runs
//! BOTH the tesseract engine and the PaddleOCR engine on each page, and
//! emits a side-by-side comparison (char/word counts, per-page latency,
//! and CER/WER against a ground-truth `.txt` when supplied).
//!
//! ```sh
//! cargo run --example paddle_bakeoff --features paddle-ocr -- \
//!     --pdf <scan.pdf> [--truth <truth.txt>] [--dpi 300]
//! ```
//!
//! Compiles only with the `paddle-ocr` feature (see `required-features`
//! in Cargo.toml). Skeleton — filled in once `PaddleEngine` lands.

fn main() {
    eprintln!("paddle_bakeoff: harness not yet implemented");
    std::process::exit(2);
}
