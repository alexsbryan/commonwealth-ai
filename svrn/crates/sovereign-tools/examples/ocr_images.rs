// SPDX-License-Identifier: AGPL-3.0-or-later
//! Headless PaddleOCR over a list of image files — the data-prep OCR
//! entrypoint for the UAP hero set (per-case NARA Blue Book page JPGs →
//! text). Skips PDF rasterization entirely: NARA already provides
//! per-case page images, so each JPG goes straight to
//! `PaddleEngine::recognize`.
//!
//! Build/run (release + the paddle-ocr feature):
//!   cargo run --release --example ocr_images --features paddle-ocr \
//!     -p sovereign-tools -- <img1.jpg> <img2.jpg> ...
//!
//! Emits each image's recognized text to stdout, prefixed by a
//! `<<<IMAGE path>>>` delimiter so a driver can split per page. Errors go
//! to stderr; a failed page is skipped, not fatal (microfilm OCR is
//! best-effort). Env: SOVEREIGN_PADDLE_OCR_MODEL / _MODEL_DIR override
//! the model id / root (default ppocr-en-v4v5 in ~/.svrnmesh/models).
//!
//! Compiles only with `paddle-ocr` (see `required-features` in Cargo.toml).

use sovereign_tools::local_corpus::ocr::paddle::{PaddleConfig, PaddleEngine, DEFAULT_MODEL_ID};
use sovereign_tools::local_corpus::ocr::OcrEngine;

fn main() {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: ocr_images <image.jpg> [<image.jpg> ...]");
        std::process::exit(2);
    }
    let model_id = std::env::var("SOVEREIGN_PADDLE_OCR_MODEL")
        .unwrap_or_else(|_| DEFAULT_MODEL_ID.to_string());
    let engine = match PaddleEngine::with_config(&model_id, PaddleConfig::default()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("paddle engine init ({model_id}) failed: {e}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "paddle engine loaded ({model_id}); OCR'ing {} image(s)",
        paths.len()
    );
    for p in &paths {
        let img = match image::open(p) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("open {p}: {e}");
                continue;
            }
        };
        match engine.recognize(&img) {
            Ok(text) => {
                println!("<<<IMAGE {p}>>>");
                println!("{text}");
            }
            Err(e) => eprintln!("ocr {p}: {e:?}"),
        }
    }
}
