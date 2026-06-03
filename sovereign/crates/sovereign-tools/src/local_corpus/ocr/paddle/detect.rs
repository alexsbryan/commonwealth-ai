//! DBNet text detection: page image → text-line boxes.
//!
//! Stages: preprocess (resize to a multiple of 32, ImageNet normalize,
//! NCHW f32) → run det ONNX → post-process the probability map
//! (binarize → `imageproc` contours → axis-aligned box → score filter →
//! unclip → scale back to original coords → reading-order sort).
//!
//! Risk instrumentation (see plan §10): the tensor shape, box count, and
//! per-box score/geometry are traced so a bad unclip ratio or a NHWC/
//! NCHW slip is visible in one run rather than guessed at.

use image::{DynamicImage, GrayImage, Luma};
use ndarray::Ix4;
use ort::value::Tensor;
use tracing::{debug, trace};

use super::geometry::{sort_reading_order, Quad};
use super::{PaddleEngine, PaddleError};

// ImageNet normalization constants PP-OCR detection was trained with.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];
const STRIDE: u32 = 32;

/// Detect text-line boxes in `image`. Returns axis-aligned boxes in the
/// ORIGINAL image's pixel coordinates, in reading order.
pub fn run_detection(
    engine: &PaddleEngine,
    image: &DynamicImage,
) -> Result<Vec<Quad>, PaddleError> {
    let cfg = engine.cfg();
    let (orig_w, orig_h) = (image.width(), image.height());

    // ── preprocess ──
    let (resize_w, resize_h) = det_resize_dims(orig_w, orig_h, cfg.det_limit_side_len);
    let resized = image
        .resize_exact(resize_w, resize_h, image::imageops::FilterType::Triangle)
        .to_rgb8();
    trace!(orig_w, orig_h, resize_w, resize_h, "paddle.detect: resized");

    // NCHW [1,3,H,W], channel-first, normalized.
    let mut data = vec![0f32; (3 * resize_h * resize_w) as usize];
    let plane = (resize_h * resize_w) as usize;
    for (x, y, px) in resized.enumerate_pixels() {
        let idx = (y * resize_w + x) as usize;
        for c in 0..3 {
            let v = (px[c] as f32 / 255.0 - MEAN[c]) / STD[c];
            data[c * plane + idx] = v;
        }
    }
    let shape: Vec<i64> = vec![1, 3, resize_h as i64, resize_w as i64];
    let tensor = Tensor::from_array((shape, data))
        .map_err(|e| PaddleError::Session(format!("det input tensor: {e}")))?;

    // ── run ──
    let prob_map = {
        let sess = engine
            .det_session()
            .lock()
            .map_err(|_| PaddleError::Session("det mutex poisoned".into()))?;
        let outputs = sess
            .run(
                ort::inputs![engine.det_input() => tensor]
                    .map_err(|e| PaddleError::Session(format!("det inputs!: {e}")))?,
            )
            .map_err(|e| PaddleError::Session(format!("det run: {e}")))?;
        let view = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| PaddleError::Session(format!("det extract: {e}")))?;
        // Expect [1,1,H,W]. Own the data so we can drop the session lock.
        let arr = view
            .into_dimensionality::<Ix4>()
            .map_err(|e| PaddleError::Shape(format!("det output not 4-D: {e}")))?;
        let (_n, _c, ph, pw) = (
            arr.shape()[0],
            arr.shape()[1],
            arr.shape()[2],
            arr.shape()[3],
        );
        let mut m = vec![0f32; ph * pw];
        for y in 0..ph {
            for x in 0..pw {
                m[y * pw + x] = arr[[0, 0, y, x]];
            }
        }
        (m, pw as u32, ph as u32)
    };
    let (prob, map_w, map_h) = prob_map;

    // ── post-process ──
    // Binarize into a 0/255 mask for contour finding.
    let mut mask = GrayImage::new(map_w, map_h);
    for y in 0..map_h {
        for x in 0..map_w {
            let v = prob[(y * map_w + x) as usize];
            mask.put_pixel(x, y, Luma([if v > cfg.det_thresh { 255 } else { 0 }]));
        }
    }

    let contours = imageproc::contours::find_contours::<u32>(&mask);
    // Scale from the (possibly resized) det map back to original coords.
    let sx = orig_w as f32 / map_w as f32;
    let sy = orig_h as f32 / map_h as f32;

    let mut quads: Vec<Quad> = Vec::new();
    for contour in &contours {
        // Only outer borders enclose text regions.
        if contour.border_type != imageproc::contours::BorderType::Outer {
            continue;
        }
        let Some(bbox) = axis_aligned_bbox(&contour.points) else {
            continue;
        };
        // Discard slivers in the det map.
        if bbox.w < 3 || bbox.h < 3 {
            continue;
        }
        // Mean probability over the box region — PaddleOCR's
        // `box_score_fast` filter for false positives.
        let score = box_score(&prob, map_w, map_h, &bbox);
        if score < cfg.det_box_thresh {
            trace!(?bbox, score, "paddle.detect: dropped low-score box");
            continue;
        }
        // Unclip in det-map space, then scale to original coords.
        let unclipped = bbox.unclip(cfg.det_unclip_ratio, map_w, map_h);
        let scaled = scale_quad(&unclipped, sx, sy, orig_w, orig_h);
        if scaled.w >= 1 && scaled.h >= 1 {
            quads.push(scaled);
        }
    }

    debug!(
        contours = contours.len(),
        kept = quads.len(),
        unclip = cfg.det_unclip_ratio,
        "paddle.detect: boxes"
    );
    sort_reading_order(&mut quads);
    Ok(quads)
}

/// Resize so the longer side ≤ `limit`, both dims a multiple of 32.
fn det_resize_dims(w: u32, h: u32, limit: u32) -> (u32, u32) {
    let longer = w.max(h) as f32;
    let ratio = if longer > limit as f32 {
        limit as f32 / longer
    } else {
        1.0
    };
    let round32 = |v: f32| -> u32 {
        let r = (v / STRIDE as f32).round() as u32 * STRIDE;
        r.max(STRIDE)
    };
    (round32(w as f32 * ratio), round32(h as f32 * ratio))
}

/// Axis-aligned bounding box of a contour's points.
fn axis_aligned_bbox(points: &[imageproc::point::Point<u32>]) -> Option<Quad> {
    if points.is_empty() {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for p in points {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    Some(Quad {
        x: x0,
        y: y0,
        w: x1.saturating_sub(x0) + 1,
        h: y1.saturating_sub(y0) + 1,
    })
}

/// Mean probability over the box region (clamped to the map).
fn box_score(prob: &[f32], map_w: u32, map_h: u32, b: &Quad) -> f32 {
    let x1 = (b.x + b.w).min(map_w);
    let y1 = (b.y + b.h).min(map_h);
    let mut sum = 0f32;
    let mut n = 0u32;
    for y in b.y..y1 {
        for x in b.x..x1 {
            sum += prob[(y * map_w + x) as usize];
            n += 1;
        }
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f32
    }
}

fn scale_quad(q: &Quad, sx: f32, sy: f32, max_w: u32, max_h: u32) -> Quad {
    let x = ((q.x as f32) * sx).round() as u32;
    let y = ((q.y as f32) * sy).round() as u32;
    let w = ((q.w as f32) * sx).round() as u32;
    let h = ((q.h as f32) * sy).round() as u32;
    Quad {
        x: x.min(max_w.saturating_sub(1)),
        y: y.min(max_h.saturating_sub(1)),
        w: w.min(max_w.saturating_sub(x)),
        h: h.min(max_h.saturating_sub(y)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_is_multiple_of_32_and_capped() {
        // 1000x500, limit 960 → ratio 0.96 → 960x480, both /32.
        let (w, h) = det_resize_dims(1000, 500, 960);
        assert_eq!(w % 32, 0);
        assert_eq!(h % 32, 0);
        assert!(w <= 960 && h <= 960);
        // Small image not upscaled past its rounded size.
        let (sw, sh) = det_resize_dims(40, 20, 960);
        assert_eq!(sw % 32, 0);
        assert!(sw >= 32 && sh >= 32);
    }

    #[test]
    fn bbox_of_points() {
        let pts = vec![
            imageproc::point::Point::new(5u32, 10),
            imageproc::point::Point::new(20, 30),
            imageproc::point::Point::new(8, 12),
        ];
        let b = axis_aligned_bbox(&pts).unwrap();
        assert_eq!((b.x, b.y), (5, 10));
        assert_eq!((b.w, b.h), (16, 21)); // inclusive +1
    }
}
