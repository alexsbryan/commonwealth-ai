//! Geometry for the detection→recognition handoff.
//!
//! For the prototype we use **axis-aligned** boxes: document text is
//! overwhelmingly horizontal, so the rotated-min-area-rect + perspective
//! warp that full PaddleOCR does buys little here and costs a lot of
//! fiddly code. `Quad` is therefore an axis-aligned rectangle in page
//! pixel coordinates. (Rotating-calipers + `imageproc` warp can be added
//! behind a flag later if the bake-off shows skew hurting quality.)

use image::DynamicImage;

/// An axis-aligned text-line box in page pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Quad {
    pub fn area(&self) -> f32 {
        (self.w as f32) * (self.h as f32)
    }
    pub fn perimeter(&self) -> f32 {
        2.0 * (self.w as f32 + self.h as f32)
    }

    /// Expand the box outward to recover the margin DBNet's shrunk
    /// probability map trims off the glyphs. PaddleOCR's `unclip`
    /// offsets the polygon by `distance = area * ratio / perimeter`; for
    /// an axis-aligned rect, uniform inflation by that distance on every
    /// side closely matches the Vatti offset of a near-rectangle. Result
    /// is clamped to the page bounds `[0,img_w] × [0,img_h]`.
    pub fn unclip(&self, ratio: f32, img_w: u32, img_h: u32) -> Quad {
        let peri = self.perimeter();
        if peri <= 0.0 {
            return *self;
        }
        let dist = (self.area() * ratio / peri).round() as i64;
        let x0 = (self.x as i64 - dist).max(0);
        let y0 = (self.y as i64 - dist).max(0);
        let x1 = (self.x as i64 + self.w as i64 + dist).min(img_w as i64);
        let y1 = (self.y as i64 + self.h as i64 + dist).min(img_h as i64);
        Quad {
            x: x0 as u32,
            y: y0 as u32,
            w: (x1 - x0).max(0) as u32,
            h: (y1 - y0).max(0) as u32,
        }
    }

    /// Top-left y/x for ordering. See [`sort_reading_order`].
    fn sort_key(&self, line_tol: u32) -> (u32, u32) {
        // Bucket the y coordinate so boxes on the same visual line group
        // together, then order left→right within the line.
        let bucket = if line_tol == 0 { self.y } else { self.y / line_tol };
        (bucket, self.x)
    }
}

/// Crop the page to an axis-aligned box. `crop_imm` clamps internally,
/// but we guard the degenerate zero-size case so recognition never sees
/// an empty image.
pub fn crop(page: &DynamicImage, quad: &Quad) -> DynamicImage {
    let w = quad.w.max(1).min(page.width().saturating_sub(quad.x).max(1));
    let h = quad.h.max(1).min(page.height().saturating_sub(quad.y).max(1));
    page.crop_imm(quad.x, quad.y, w, h)
}

/// Order detected boxes in reading order: top→bottom by line, then
/// left→right within a line. `line_tol` is a y-bucket size (~half the
/// median box height) so boxes that are vertically close count as the
/// same line. Matches RapidOCR's `sorted_boxes`.
pub fn sort_reading_order(quads: &mut [Quad]) {
    let line_tol = median_height(quads).max(1) / 2;
    quads.sort_by_key(|q| q.sort_key(line_tol));
}

fn median_height(quads: &[Quad]) -> u32 {
    if quads.is_empty() {
        return 1;
    }
    let mut hs: Vec<u32> = quads.iter().map(|q| q.h).collect();
    hs.sort_unstable();
    hs[hs.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unclip_expands_and_clamps() {
        // A 100x20 box well inside a 1000x1000 page.
        let q = Quad { x: 200, y: 200, w: 100, h: 20 };
        let u = q.unclip(1.5, 1000, 1000);
        // area=2000, peri=240, dist=round(2000*1.5/240)=round(12.5)=13.
        assert_eq!(u.x, 187);
        assert_eq!(u.y, 187);
        assert_eq!(u.w, 100 + 26);
        assert_eq!(u.h, 20 + 26);
    }

    #[test]
    fn unclip_clamps_to_page_bounds() {
        let q = Quad { x: 2, y: 2, w: 50, h: 50 };
        let u = q.unclip(2.0, 60, 60);
        // dist=round(2500*2/200)=25 → x0 = max(2-25,0)=0, clamped.
        assert_eq!(u.x, 0);
        assert_eq!(u.y, 0);
        assert!(u.x + u.w <= 60);
        assert!(u.y + u.h <= 60);
    }

    #[test]
    fn reading_order_groups_lines_then_left_to_right() {
        // Two lines; second line's left box should still come after both
        // first-line boxes. Heights ~20 → line_tol 10.
        let mut quads = vec![
            Quad { x: 300, y: 10, w: 50, h: 20 }, // line 1, right
            Quad { x: 10, y: 12, w: 50, h: 20 },  // line 1, left
            Quad { x: 10, y: 80, w: 50, h: 20 },  // line 2, left
        ];
        sort_reading_order(&mut quads);
        assert_eq!(quads[0].x, 10); // line1 left
        assert_eq!(quads[1].x, 300); // line1 right
        assert_eq!(quads[2].y, 80); // line2
    }
}
