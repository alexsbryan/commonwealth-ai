//! CRNN/SVTR text recognition: one text-line crop → string.
//!
//! Preprocess (fixed height, proportional width, `(x/255-0.5)/0.5`
//! normalize, NCHW) → run rec ONNX → CTC greedy decode against the
//! dictionary (argmax over the class axis, collapse repeats, drop the
//! blank at index 0). Returns `(text, mean_confidence)`.

use image::DynamicImage;
use ndarray::Ix3;
use ort::value::Tensor;
use tracing::{trace, warn};

use super::{PaddleEngine, PaddleError};

// PP-OCRv4/v5 recognition normalizes to [-1, 1].
const REC_MEAN: f32 = 0.5;
const REC_STD: f32 = 0.5;
// Cap recognition width so a freakishly wide crop can't blow up memory.
const REC_MAX_WIDTH: u32 = 2048;

/// Recognize the text in a single line-crop. Returns `(text, score)`
/// where score is the mean per-character max-probability.
pub fn run_recognition(
    engine: &PaddleEngine,
    crop: &DynamicImage,
) -> Result<(String, f32), PaddleError> {
    let cfg = engine.cfg();
    let target_h = cfg.rec_img_height;

    // ── preprocess: resize to fixed height, proportional width ──
    let (cw, ch) = (crop.width().max(1), crop.height().max(1));
    let target_w =
        (((target_h as f32) * cw as f32 / ch as f32).round() as u32).clamp(1, REC_MAX_WIDTH);
    let resized = crop
        .resize_exact(target_w, target_h, image::imageops::FilterType::Triangle)
        .to_rgb8();

    let mut data = vec![0f32; (3 * target_h * target_w) as usize];
    let plane = (target_h * target_w) as usize;
    for (x, y, px) in resized.enumerate_pixels() {
        let idx = (y * target_w + x) as usize;
        for c in 0..3 {
            data[c * plane + idx] = (px[c] as f32 / 255.0 - REC_MEAN) / REC_STD;
        }
    }
    let shape: Vec<i64> = vec![1, 3, target_h as i64, target_w as i64];
    let tensor = Tensor::from_array((shape, data))
        .map_err(|e| PaddleError::Session(format!("rec input tensor: {e}")))?;

    // ── run ──
    let (logits, t, c) = {
        let sess = engine
            .rec_session()
            .lock()
            .map_err(|_| PaddleError::Session("rec mutex poisoned".into()))?;
        let outputs = sess
            .run(
                ort::inputs![engine.rec_input() => tensor]
                    .map_err(|e| PaddleError::Session(format!("rec inputs!: {e}")))?,
            )
            .map_err(|e| PaddleError::Session(format!("rec run: {e}")))?;
        let view = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| PaddleError::Session(format!("rec extract: {e}")))?;
        // Expect [1, T, C].
        let arr = view
            .into_dimensionality::<Ix3>()
            .map_err(|e| PaddleError::Shape(format!("rec output not 3-D: {e}")))?;
        let (t, c) = (arr.shape()[1], arr.shape()[2]);
        let mut flat = vec![0f32; t * c];
        for ti in 0..t {
            for ci in 0..c {
                flat[ti * c + ci] = arr[[0, ti, ci]];
            }
        }
        (flat, t, c)
    };

    Ok(ctc_decode(&logits, t, c, engine.dict()))
}

/// CTC greedy decode. `logits` is row-major `[T, C]`. Index 0 is the
/// blank. `dict[k]` maps class `k`→char; the model may have one extra
/// trailing class (a space) beyond the dict — reconciled here with a
/// one-time warn rather than a silent panic (plan risk R2).
pub fn ctc_decode(logits: &[f32], t: usize, c: usize, dict: &[String]) -> (String, f32) {
    if c != dict.len() {
        // Off-by-one is expected for some exports (trailing space class).
        // Anything larger is a real dict/model mismatch — surface it.
        let delta = c as isize - dict.len() as isize;
        if delta.abs() > 1 {
            warn!(
                logits_c = c,
                dict_len = dict.len(),
                "paddle.recognize: dict/model class-count mismatch — output will be garbled"
            );
        } else {
            trace!(
                logits_c = c,
                dict_len = dict.len(),
                "paddle.recognize: +1 trailing class"
            );
        }
    }

    let mut out = String::new();
    let mut scores: Vec<f32> = Vec::new();
    let mut last = usize::MAX;
    for step in 0..t {
        let row = &logits[step * c..step * c + c];
        let (best, &best_v) = row
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or((0, &0.0));
        // Drop blank (0) and collapse consecutive duplicates.
        if best != 0 && best != last {
            out.push_str(class_to_str(best, dict));
            scores.push(best_v);
        }
        last = best;
    }
    let conf = if scores.is_empty() {
        0.0
    } else {
        scores.iter().sum::<f32>() / scores.len() as f32
    };
    (out, conf)
}

/// Map a CTC class index to its string. Indices within the dict use it
/// directly; a single class beyond the dict is PaddleOCR's trailing
/// space; anything further is out-of-range (empty, already warned).
fn class_to_str(k: usize, dict: &[String]) -> &str {
    if k < dict.len() {
        &dict[k]
    } else if k == dict.len() {
        " "
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict() -> Vec<String> {
        // index 0 = blank, then a,b,c
        ["<blank>", "a", "b", "c"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    /// Build a [T,C] logit row-major buffer where each step's argmax is
    /// the given class index (one-hot-ish: 1.0 at the class, 0 elsewhere).
    fn logits_from_argmax(seq: &[usize], c: usize) -> Vec<f32> {
        let mut v = vec![0f32; seq.len() * c];
        for (t, &k) in seq.iter().enumerate() {
            v[t * c + k] = 1.0;
        }
        v
    }

    #[test]
    fn decode_drops_blanks_and_collapses_repeats() {
        let d = dict();
        let c = d.len(); // 4
                         // blank, a, a, blank, b, c, c  →  "abc"
        let seq = [0usize, 1, 1, 0, 2, 3, 3];
        let logits = logits_from_argmax(&seq, c);
        let (text, conf) = ctc_decode(&logits, seq.len(), c, &d);
        assert_eq!(text, "abc");
        assert!((conf - 1.0).abs() < 1e-6);
    }

    #[test]
    fn decode_keeps_distinct_adjacent_via_blank() {
        let d = dict();
        let c = d.len();
        // a, a (collapsed), then a again after blank → "aa"
        let seq = [1usize, 1, 0, 1];
        let logits = logits_from_argmax(&seq, c);
        let (text, _) = ctc_decode(&logits, seq.len(), c, &d);
        assert_eq!(text, "aa");
    }

    #[test]
    fn decode_handles_trailing_space_class() {
        let d = dict(); // len 4
        let c = 5; // model has one extra class = space at index 4
        let seq = [1usize, 4, 2]; // a, <space>, b
        let logits = logits_from_argmax(&seq, c);
        let (text, _) = ctc_decode(&logits, seq.len(), c, &d);
        assert_eq!(text, "a b");
    }
}
