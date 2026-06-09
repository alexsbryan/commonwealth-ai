// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tesseract-vs-PaddleOCR bake-off harness.
//!
//! Rasterizes a PDF via the existing `PdfiumRasterizer`, runs BOTH the
//! tesseract engine and the PaddleOCR engine on each page, and emits a
//! side-by-side comparison: char/word counts, per-page latency, the
//! first ~200 recognized chars, and — when ground truth is available —
//! CER/WER for each engine.
//!
//! ## Ground truth without a hand-labelled scan
//!
//! No scanned PDF with known text ships in-repo. Instead we exploit a
//! born-digital PDF as its own oracle: `pdf-extract` reads the embedded
//! text layer (exact), while pdfium rasterizes the *same* pages to
//! images that both OCR engines then read. The embedded text is the
//! ground truth; the OCR output is the hypothesis. Both engines see the
//! identical rasterized input, so the comparison is fair even if the
//! absolute scores are optimistic (clean digital render, no scan skew or
//! noise). Pass `--truth <file.txt>` to override with an external truth.
//!
//! ```sh
//! SOVEREIGN_PDFIUM_LIB=/path/to/libpdfium.dylib \
//! cargo run --example paddle_bakeoff --features paddle-ocr -p sovereign-tools -- \
//!     --pdf <doc.pdf> [--truth <truth.txt>] [--dpi 300] [--max-pages 3] \
//!     [--unclip 1.6] [--box-thresh 0.5] [--no-tesseract]
//! ```
//!
//! Env resolution (so it runs on any machine without flags):
//!   SOVEREIGN_PDFIUM_LIB        libpdfium dylib/so/dll (required — pdfium has no system install here)
//!   SOVEREIGN_TESSERACT_BIN     tesseract binary             (default: `tesseract` on PATH)
//!   TESSDATA_PREFIX             dir holding eng.traineddata  (default: /opt/homebrew/share/tessdata)
//!   SOVEREIGN_PADDLE_OCR_MODEL  paddle model-set id          (default: ppocr-en-v4v5)
//!   SOVEREIGN_PADDLE_OCR_MODEL_DIR  models root              (default: ~/.sovereign/models/paddle-ocr)
//!
//! Compiles only with the `paddle-ocr` feature (see `required-features`
//! in Cargo.toml).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use image::DynamicImage;
use sovereign_tools::local_corpus::ocr::paddle::{PaddleConfig, PaddleEngine, DEFAULT_MODEL_ID};
use sovereign_tools::local_corpus::ocr::rasterize::{PdfiumRasterizer, Rasterizer};
use sovereign_tools::local_corpus::ocr::tesseract::TesseractEngine;
use sovereign_tools::local_corpus::ocr::{OcrCtx, OcrEngine, OcrEngineKind};

/// Parsed CLI args. Hand-rolled — one binary, no clap dependency.
struct Args {
    pdf: PathBuf,
    truth: Option<PathBuf>,
    dpi: u32,
    max_pages: usize,  // 0 = all
    skip_pages: usize, // drop this many leading pages (skip front matter)
    unclip: Option<f32>,
    box_thresh: Option<f32>,
    det_limit: Option<u32>, // det_limit_side_len override (line-separation knob)
    run_tesseract: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut pdf = None;
    let mut truth = None;
    let mut dpi = 300u32;
    let mut max_pages = 3usize;
    let mut skip_pages = 0usize;
    let mut unclip = None;
    let mut box_thresh = None;
    let mut det_limit = None;
    let mut run_tesseract = true;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--pdf" => pdf = Some(PathBuf::from(next(&mut it, "--pdf")?)),
            "--truth" => truth = Some(PathBuf::from(next(&mut it, "--truth")?)),
            "--dpi" => {
                dpi = next(&mut it, "--dpi")?
                    .parse()
                    .map_err(|e| format!("--dpi: {e}"))?
            }
            "--max-pages" => {
                max_pages = next(&mut it, "--max-pages")?
                    .parse()
                    .map_err(|e| format!("--max-pages: {e}"))?
            }
            "--unclip" => {
                unclip = Some(
                    next(&mut it, "--unclip")?
                        .parse()
                        .map_err(|e| format!("--unclip: {e}"))?,
                )
            }
            "--box-thresh" => {
                box_thresh = Some(
                    next(&mut it, "--box-thresh")?
                        .parse()
                        .map_err(|e| format!("--box-thresh: {e}"))?,
                )
            }
            "--det-limit" => {
                det_limit = Some(
                    next(&mut it, "--det-limit")?
                        .parse()
                        .map_err(|e| format!("--det-limit: {e}"))?,
                )
            }
            "--skip-pages" => {
                skip_pages = next(&mut it, "--skip-pages")?
                    .parse()
                    .map_err(|e| format!("--skip-pages: {e}"))?
            }
            "--no-tesseract" => run_tesseract = false,
            "-h" | "--help" => return Err("help".into()),
            other => return Err(format!("unknown arg: {other}")),
        }
    }

    Ok(Args {
        pdf: pdf.ok_or("missing required --pdf <path>")?,
        truth,
        dpi,
        max_pages,
        skip_pages,
        unclip,
        box_thresh,
        det_limit,
        run_tesseract,
    })
}

fn next(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} requires a value"))
}

/// Per-engine accumulator across all pages: concatenated text (for
/// doc-level CER/WER), total latency, and per-page timings.
#[derive(Default)]
struct EngineStats {
    name: String,
    full_text: String,
    page_ms: Vec<u128>,
    failures: usize,
}

impl EngineStats {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            ..Default::default()
        }
    }
    fn record_page(&mut self, text: String, elapsed: Duration) {
        if !self.full_text.is_empty() {
            self.full_text.push('\n');
        }
        self.full_text.push_str(&text);
        self.page_ms.push(elapsed.as_millis());
    }
    fn total_ms(&self) -> u128 {
        self.page_ms.iter().sum()
    }
}

fn main() {
    // Glassbox: surface the engine's internal tracing when RUST_LOG is set
    // (e.g. RUST_LOG=sovereign_tools=debug). Default stays quiet so the
    // side-by-side report isn't drowned out.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = match parse_args() {
        Ok(a) => a,
        Err(e) if e == "help" => {
            eprintln!("{}", include_help());
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("paddle_bakeoff: {e}\n\n{}", include_help());
            std::process::exit(2);
        }
    };

    if let Err(e) = run(args) {
        eprintln!("paddle_bakeoff: {e}");
        std::process::exit(1);
    }
}

fn include_help() -> &'static str {
    "usage: paddle_bakeoff --pdf <path> [--truth <txt>] [--dpi 300] \
[--max-pages 3] [--unclip 1.5] [--box-thresh 0.6] [--no-tesseract]\n\
     set SOVEREIGN_PDFIUM_LIB to your libpdfium; see file header for all env vars."
}

fn run(args: Args) -> Result<(), String> {
    // ── Resolve the runtime context ─────────────────────────────────
    let pdfium_lib_path = std::env::var("SOVEREIGN_PDFIUM_LIB")
        .ok()
        .map(PathBuf::from);
    if pdfium_lib_path.is_none() {
        eprintln!(
            "WARN: SOVEREIGN_PDFIUM_LIB unset — falling back to system pdfium search \
             (likely to fail; export it to the bundled libpdfium)."
        );
    }
    let tesseract_bin = std::env::var("SOVEREIGN_TESSERACT_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("tesseract"));
    let tessdata_dir = std::env::var("TESSDATA_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/opt/homebrew/share/tessdata"));
    let model_id = std::env::var("SOVEREIGN_PADDLE_OCR_MODEL")
        .unwrap_or_else(|_| DEFAULT_MODEL_ID.to_string());

    let ctx = OcrCtx {
        tesseract_bin,
        tessdata_dir,
        pdfium_lib_path,
        daemon_base_url: "http://127.0.0.1:9741".into(),
        cleanup_model: "unused-in-bakeoff".into(),
        dpi: args.dpi,
        tesseract_timeout_secs: 120,
        cleanup_timeout_secs: 30,
        engine: OcrEngineKind::Paddle,
    };

    // ── Rasterize ───────────────────────────────────────────────────
    println!("== bake-off: {} ==", args.pdf.display());
    println!(
        "dpi={} max_pages={}",
        args.dpi,
        if args.max_pages == 0 {
            "all".into()
        } else {
            args.max_pages.to_string()
        }
    );
    let raster = PdfiumRasterizer::new(&ctx).map_err(|e| format!("rasterizer: {e}"))?;
    let t0 = Instant::now();
    let mut pages = raster
        .pdf_to_pages(&args.pdf)
        .map_err(|e| format!("rasterize: {e}"))?;
    println!("rasterized {} page(s) in {:?}", pages.len(), t0.elapsed());
    if pages.is_empty() {
        return Err("no pages rendered (encrypted/corrupt PDF?)".into());
    }
    if args.skip_pages > 0 {
        let drop = args.skip_pages.min(pages.len());
        pages.drain(0..drop);
        println!("skipped first {drop} page(s) (front matter)");
    }
    if args.max_pages > 0 && pages.len() > args.max_pages {
        pages.truncate(args.max_pages);
    }

    // ── Build engines (once — amortizes the ONNX session load) ──────
    let mut cfg = PaddleConfig::default();
    if let Some(u) = args.unclip {
        cfg.det_unclip_ratio = u;
    }
    if let Some(b) = args.box_thresh {
        cfg.det_box_thresh = b;
    }
    if let Some(l) = args.det_limit {
        cfg.det_limit_side_len = l;
    }
    println!(
        "paddle cfg: det_limit_side_len={} unclip_ratio={} box_thresh={} det_thresh={} rec_min_score={}",
        cfg.det_limit_side_len, cfg.det_unclip_ratio, cfg.det_box_thresh, cfg.det_thresh, cfg.rec_min_score
    );
    let t_load = Instant::now();
    let paddle = PaddleEngine::with_config(&model_id, cfg)
        .map_err(|e| format!("paddle engine init ({model_id}): {e}"))?;
    println!("paddle models loaded in {:?}", t_load.elapsed());

    let tesseract = if args.run_tesseract {
        Some(TesseractEngine::from_ctx(&ctx))
    } else {
        None
    };

    // ── Per-page comparison ─────────────────────────────────────────
    let mut paddle_stats = EngineStats::new("paddleocr");
    let mut tess_stats = EngineStats::new("tesseract");

    for (idx, image) in pages.iter().enumerate() {
        let page_no = idx + 1;
        println!(
            "\n── page {page_no} ({}×{}) ──",
            image.width(),
            image.height()
        );

        run_engine_on_page(&paddle, image, page_no, &mut paddle_stats);
        if let Some(t) = tesseract.as_ref() {
            run_engine_on_page(t, image, page_no, &mut tess_stats);
        }
    }

    // ── Summary ─────────────────────────────────────────────────────
    println!("\n========== SUMMARY ==========");
    summarize(&paddle_stats);
    if tesseract.is_some() {
        summarize(&tess_stats);
    }

    // ── Ground truth + CER/WER ──────────────────────────────────────
    let truth = load_truth(&args)?;
    match truth {
        Some(truth) => {
            println!("\n----- CER / WER (lower is better) -----");
            println!(
                "ground-truth chars={} words={}",
                truth.chars().count(),
                word_count(&truth)
            );
            report_accuracy(&paddle_stats, &truth);
            if tesseract.is_some() {
                report_accuracy(&tess_stats, &truth);
            }
        }
        None => {
            println!(
                "\n(no ground truth — pass --truth <txt> or use a born-digital PDF \
                 whose text layer pdf-extract can read for CER/WER)"
            );
        }
    }

    Ok(())
}

fn run_engine_on_page(
    engine: &dyn OcrEngine,
    image: &DynamicImage,
    page_no: usize,
    stats: &mut EngineStats,
) {
    let t = Instant::now();
    match engine.recognize(image) {
        Ok(text) => {
            let elapsed = t.elapsed();
            let chars = text.chars().count();
            let words = word_count(&text);
            println!(
                "  [{:>10}] {:>5} chars  {:>4} words  {:>6} ms  | {}",
                engine.name(),
                chars,
                words,
                elapsed.as_millis(),
                preview(&text, 200),
            );
            stats.record_page(text, elapsed);
        }
        Err(e) => {
            println!(
                "  [{:>10}] FAILED page {page_no}: {}",
                engine.name(),
                e.detail()
            );
            stats.failures += 1;
            stats.page_ms.push(t.elapsed().as_millis());
        }
    }
}

fn summarize(s: &EngineStats) {
    let n = s.page_ms.len().max(1);
    let avg = s.total_ms() / n as u128;
    println!(
        "{:>10}: {:>6} total chars  {:>5} ms total  {:>5} ms/page avg  {} failure(s)",
        s.name,
        s.full_text.chars().count(),
        s.total_ms(),
        avg,
        s.failures,
    );
}

fn report_accuracy(s: &EngineStats, truth: &str) {
    let (cer, wer) = score(&s.full_text, truth);
    println!("{:>10}: CER={:.4}  WER={:.4}", s.name, cer, wer);
}

/// Load ground truth: explicit `--truth` file, else the born-digital
/// PDF's embedded text layer via pdf-extract. Returns `None` if neither
/// yields usable text (e.g. a genuine image-only scan with no text
/// layer and no --truth).
fn load_truth(args: &Args) -> Result<Option<String>, String> {
    if let Some(p) = &args.truth {
        let raw =
            std::fs::read_to_string(p).map_err(|e| format!("read --truth {}: {e}", p.display()))?;
        return Ok(Some(normalize(&raw)));
    }
    // Born-digital fallback: extract the PDF's own text layer as oracle.
    // Per-PAGE so we can align the oracle to exactly the pages OCR ran —
    // comparing N OCR'd pages against the whole-doc text layer would score
    // ~0.99 CER (the un-OCR'd remainder reads as deletions).
    match pdf_extract::extract_text_by_pages(&args.pdf) {
        Ok(mut pages) => {
            if args.skip_pages > 0 {
                pages.drain(0..args.skip_pages.min(pages.len()));
            }
            if args.max_pages > 0 && pages.len() > args.max_pages {
                pages.truncate(args.max_pages);
            }
            let text = pages.join("\n");
            let norm = normalize(&text);
            if norm.chars().filter(|c| !c.is_whitespace()).count() < 32 {
                // Too little embedded text → almost certainly a real scan.
                println!(
                    "(pdf-extract found ~{} chars of embedded text — too little to be a \
                     reliable oracle; treating as image-only)",
                    norm.chars().count()
                );
                Ok(None)
            } else {
                println!("(ground truth: extracted PDF text layer via pdf-extract)");
                Ok(Some(norm))
            }
        }
        Err(e) => {
            println!("(pdf-extract could not read a text layer: {e})");
            Ok(None)
        }
    }
}

/// CER (char error rate) and WER (word error rate) of `hyp` vs `truth`.
/// Both normalized (whitespace-collapsed, trimmed) before comparison so
/// line-wrapping differences don't dominate the distance.
fn score(hyp: &str, truth: &str) -> (f64, f64) {
    let hyp = normalize(hyp);
    let truth = normalize(truth);

    let truth_chars: Vec<char> = truth.chars().collect();
    let hyp_chars: Vec<char> = hyp.chars().collect();
    let cer = if truth_chars.is_empty() {
        0.0
    } else {
        strsim::generic_levenshtein(&hyp_chars, &truth_chars) as f64 / truth_chars.len() as f64
    };

    let truth_words: Vec<&str> = truth.split_whitespace().collect();
    let hyp_words: Vec<&str> = hyp.split_whitespace().collect();
    let wer = if truth_words.is_empty() {
        0.0
    } else {
        strsim::generic_levenshtein(&hyp_words, &truth_words) as f64 / truth_words.len() as f64
    };

    (cer, wer)
}

/// Collapse runs of whitespace to a single space and trim. Keeps CER/WER
/// focused on character/word recognition rather than layout reflow.
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// First `n` chars of `text` on one line (newlines → ⏎) for the table.
fn preview(text: &str, n: usize) -> String {
    let flat: String = text
        .chars()
        .take(n)
        .map(|c| if c == '\n' { '⏎' } else { c })
        .collect();
    if text.chars().count() > n {
        format!("{flat}…")
    } else {
        flat
    }
}
