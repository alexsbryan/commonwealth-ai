//! PaddleOCR engine — in-process OCR via ONNX Runtime (`ort`).
//!
//! An alternative to the tesseract subprocess that drives the SAME
//! ONNX Runtime already statically linked for GLiNER, so it adds no new
//! ML runtime to the binary. Two ONNX models, loaded once and held for
//! the engine's life:
//!
//! ```text
//! page image
//!   │  detect::run_detection        (DBNet → text-line boxes)
//!   ▼
//! Vec<Quad> (sorted top→bottom, left→right)
//!   │  for each box: geometry::crop → recognize::run_recognition
//!   ▼                                 (CRNN/SVTR → CTC decode → line text)
//! lines joined with '\n'  →  raw page text  →  (cleanup pass, as tesseract)
//! ```
//!
//! Why driven directly (not an off-the-shelf PaddleOCR crate): the
//! GLiNER stack hard-pins `ort =2.0.0-rc.9`, and every off-the-shelf
//! PP-OCR crate wants rc.10+ — which is build-verified to break the
//! GLiNER stack (`orp` fails to compile). Pinning ort to rc.9 here
//! unifies to a single onnxruntime. See `Cargo.toml`'s `paddle-ocr`
//! feature comment and the `project_ort_pin_locked_rc9` note.

mod detect;
mod dict;
mod geometry;
mod recognize;

use std::path::PathBuf;
use std::sync::Mutex;

use image::DynamicImage;
use ort::session::Session;
use tracing::{debug, info, info_span};

use super::engine::{OcrEngine, OcrError};
use super::OcrCtx;

pub use geometry::Quad;

/// Default model set id. Maps to a directory under the models root
/// holding `det.onnx`, `rec.onnx`, and `dict.txt`.
pub const DEFAULT_MODEL_ID: &str = "ppocr-en-v4v5";

/// Models root, mirroring `gliner_ner::models_root`. Falls back to
/// `~/.sovereign/models/paddle-ocr` when the env var is unset.
pub fn models_root() -> PathBuf {
    if let Ok(p) = std::env::var("SOVEREIGN_PADDLE_OCR_MODEL_DIR") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .map(|h| h.join(".sovereign").join("models").join("paddle-ocr"))
        .unwrap_or_else(|| PathBuf::from(".sovereign/models/paddle-ocr"))
}

/// Resolve `(det, rec, dict)` paths for a model id, validating each
/// exists with a clear "go fetch these" message — same ergonomics as
/// `gliner_ner::resolve_model_paths`.
pub fn resolve_model_paths(model_id: &str) -> Result<(PathBuf, PathBuf, PathBuf), PaddleError> {
    let root = models_root().join(model_id);
    let det = root.join("det.onnx");
    let rec = root.join("rec.onnx");
    let dict = root.join("dict.txt");
    for (p, what) in [(&det, "det.onnx"), (&rec, "rec.onnx"), (&dict, "dict.txt")] {
        if !p.is_file() {
            return Err(PaddleError::Model(format!(
                "PaddleOCR {what} not found at {}\n\
                 Populate {}/ with det.onnx + rec.onnx + dict.txt \
                 (see scripts/fetch-desktop-binaries.sh / RELEASING.md).",
                p.display(),
                root.display(),
            )));
        }
    }
    Ok((det, rec, dict))
}

/// Errors from PaddleOCR engine construction and inference. Construction
/// errors (`Model`, `Session`) surface at `build_engine` time so a
/// missing/corrupt model fails the document once. Per-page inference
/// errors are mapped to [`OcrError`] by [`OcrEngine::recognize`].
#[derive(Debug)]
pub enum PaddleError {
    /// A model/dict file is missing or unreadable.
    Model(String),
    /// `ort` session build or inference failed.
    Session(String),
    /// A tensor had an unexpected shape (logged with the actual shape).
    Shape(String),
}

impl std::fmt::Display for PaddleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PaddleError::Model(m) => write!(f, "model: {m}"),
            PaddleError::Session(m) => write!(f, "session: {m}"),
            PaddleError::Shape(m) => write!(f, "shape: {m}"),
        }
    }
}
impl std::error::Error for PaddleError {}

/// Tunables for the PP-OCR pipeline. Defaults track RapidOCR / the
/// PaddleOCR `DistillationDBPostProcess` mobile defaults; the bake-off
/// sweeps `unclip_ratio`/`box_thresh`.
#[derive(Debug, Clone)]
pub struct PaddleConfig {
    /// Detection: cap the longer image side at this many px before the
    /// multiple-of-32 round (RapidOCR uses 960).
    pub det_limit_side_len: u32,
    /// Detection: probability-map binarization threshold.
    pub det_thresh: f32,
    /// Detection: drop boxes whose mean probability is below this.
    pub det_box_thresh: f32,
    /// Detection: polygon expansion ratio (`area*ratio/perimeter`).
    pub det_unclip_ratio: f32,
    /// Recognition: fixed input height (PP-OCRv4/v5 mobile = 48).
    pub rec_img_height: u32,
    /// Recognition: drop decoded lines below this mean confidence.
    pub rec_min_score: f32,
    /// ORT intra-op thread count per session.
    pub intra_threads: usize,
}

impl Default for PaddleConfig {
    fn default() -> Self {
        Self {
            det_limit_side_len: 960,
            det_thresh: 0.3,
            det_box_thresh: 0.6,
            det_unclip_ratio: 1.5,
            rec_img_height: 48,
            rec_min_score: 0.5,
            intra_threads: 1,
        }
    }
}

/// The loaded PaddleOCR engine. Both sessions live behind a `Mutex`
/// (mirroring `GlinerExtractor`) so `recognize(&self, …)` stays `&self`
/// while honouring ort's `&self`-but-not-`Sync`-run model; OCR is
/// sequential per page so contention is a non-issue.
pub struct PaddleEngine {
    det: Mutex<Session>,
    rec: Mutex<Session>,
    /// Recognition input tensor name (read off the session, not
    /// hardcoded — exports vary between `"x"` and `"images"`).
    det_input_name: String,
    rec_input_name: String,
    /// CTC label table: index 0 is the blank, the rest map argmax→char.
    dict: Vec<String>,
    cfg: PaddleConfig,
}

impl PaddleEngine {
    /// Build the engine from an [`OcrCtx`]. Resolves the model set
    /// (env / `~/.sovereign/models/paddle-ocr`), loads both ONNX
    /// sessions and the dictionary. Fails loudly if anything is missing.
    pub fn from_ctx(_ctx: &OcrCtx) -> Result<Self, PaddleError> {
        Self::with_config(DEFAULT_MODEL_ID, PaddleConfig::default())
    }

    /// Construction with an explicit model id + config — used by the
    /// bake-off harness to point at a specific model set and sweep
    /// detection tunables.
    pub fn with_config(model_id: &str, cfg: PaddleConfig) -> Result<Self, PaddleError> {
        let (det_path, rec_path, dict_path) = resolve_model_paths(model_id)?;
        let dict = dict::load_dict(&dict_path)?;
        info!(
            model_id,
            det = %det_path.display(),
            rec = %rec_path.display(),
            dict_len = dict.len(),
            "paddleocr: loading models"
        );

        let det = build_session(&det_path, cfg.intra_threads)?;
        let rec = build_session(&rec_path, cfg.intra_threads)?;

        // Read input/output names once; these vary by export and are the
        // load-bearing keys for `inputs!`/output extraction (risk R4/R5).
        let det_input_name = first_input_name(&det);
        let rec_input_name = first_input_name(&rec);
        info!(
            det_in = %det_input_name,
            det_out = ?output_names(&det),
            rec_in = %rec_input_name,
            rec_out = ?output_names(&rec),
            "paddleocr: session i/o names"
        );

        Ok(Self {
            det: Mutex::new(det),
            rec: Mutex::new(rec),
            det_input_name,
            rec_input_name,
            dict,
            cfg,
        })
    }
}

impl OcrEngine for PaddleEngine {
    fn name(&self) -> &'static str {
        "paddleocr"
    }

    fn recognize(&self, image: &DynamicImage) -> Result<String, OcrError> {
        let span = info_span!("paddle.recognize", w = image.width(), h = image.height());
        let _g = span.enter();

        // 1. Detect text-line boxes.
        let boxes = detect::run_detection(self, image)
            .map_err(|e| OcrError::Page(format!("detection: {e}")))?;
        debug!(boxes = boxes.len(), "paddleocr: detected text regions");
        if boxes.is_empty() {
            return Ok(String::new());
        }

        // 2. Recognize each crop; collect (text, score) for survivors.
        let mut lines: Vec<String> = Vec::with_capacity(boxes.len());
        for quad in &boxes {
            let crop = geometry::crop(image, quad);
            match recognize::run_recognition(self, &crop) {
                Ok((text, score)) => {
                    if score >= self.cfg.rec_min_score && !text.trim().is_empty() {
                        lines.push(text);
                    } else {
                        debug!(score, text = %text, "paddleocr: dropped low-confidence line");
                    }
                }
                Err(e) => {
                    // Per-line failure is soft: skip the line, keep the
                    // page. The page placeholder is only for whole-page
                    // failures (handled by the caller).
                    debug!(error = %e, "paddleocr: line recognition failed; skipping");
                }
            }
        }

        Ok(lines.join("\n"))
    }
}

// ─── ort session helpers ─────────────────────────────────────────────

fn build_session(path: &std::path::Path, intra_threads: usize) -> Result<Session, PaddleError> {
    use ort::session::builder::GraphOptimizationLevel;
    Session::builder()
        .map_err(|e| PaddleError::Session(e.to_string()))?
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .map_err(|e| PaddleError::Session(e.to_string()))?
        .with_intra_threads(intra_threads)
        .map_err(|e| PaddleError::Session(e.to_string()))?
        .commit_from_file(path)
        .map_err(|e| PaddleError::Session(format!("commit {}: {e}", path.display())))
}

fn first_input_name(s: &Session) -> String {
    s.inputs
        .first()
        .map(|i| i.name.clone())
        .unwrap_or_else(|| "x".to_string())
}

fn output_names(s: &Session) -> Vec<String> {
    s.outputs.iter().map(|o| o.name.clone()).collect()
}

// Accessors for the sibling stage modules (they live in this module's
// privacy scope but read these fields).
impl PaddleEngine {
    pub(super) fn det_session(&self) -> &Mutex<Session> {
        &self.det
    }
    pub(super) fn rec_session(&self) -> &Mutex<Session> {
        &self.rec
    }
    pub(super) fn det_input(&self) -> &str {
        &self.det_input_name
    }
    pub(super) fn rec_input(&self) -> &str {
        &self.rec_input_name
    }
    pub(super) fn dict(&self) -> &[String] {
        &self.dict
    }
    pub(super) fn cfg(&self) -> &PaddleConfig {
        &self.cfg
    }
}
