// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic geometry for the fieldglass page.
//!
//! Everything here is a pure function of its inputs — no clock, no RNG, no
//! I/O — because the page's whole value is gestalt stability: the operator's
//! eye learns yesterday's shape, and anomaly perception is delta-from-familiar.
//! A force layout (capability-graph's choice) reshuffles on every open and is
//! disqualified for this surface.
//!
//! The treemap is a STRIP layout, not classic squarify: items are placed in a
//! FIXED caller-supplied order (crates by (layer, name); files by path), never
//! re-sorted by size. Squarify's size-sort gives prettier aspect ratios but
//! lets one file's growth reshuffle its whole crate; strip keeps neighborhoods
//! stable so growth reads as growth, not as motion.

/// One laid-out rectangle. `key` is the stable identity (path), which the
/// renderer uses for hover/joins and the tests use for stability assertions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LaidRect {
    pub key: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Strip-treemap `items` (key, weight) into `(x, y, w, h)`, preserving item
/// order. Rows fill left→right, top→bottom; each row's height is chosen so the
/// row's items get roughly square cells at the current width. Zero/negative
/// weights are clamped to the smallest positive weight so every item stays
/// visible (an invisible file is a lie of omission).
pub fn strip_treemap(items: &[(String, f64)], x: f64, y: f64, w: f64, h: f64) -> Vec<LaidRect> {
    let mut out = Vec::with_capacity(items.len());
    if items.is_empty() || w <= 0.0 || h <= 0.0 {
        return out;
    }
    let min_pos = items
        .iter()
        .map(|(_, v)| *v)
        .filter(|v| *v > 0.0)
        .fold(f64::INFINITY, f64::min);
    let floor = if min_pos.is_finite() { min_pos } else { 1.0 };
    let weights: Vec<f64> = items.iter().map(|(_, v)| v.max(floor)).collect();
    let total: f64 = weights.iter().sum();
    let scale = (w * h) / total; // area per unit weight

    let mut cur_y = y;
    let mut i = 0;
    while i < items.len() {
        // Grow the row while adding the next item improves (or keeps
        // acceptable) the worst aspect ratio at this width.
        let mut row_end = i + 1;
        let mut row_sum = weights[i];
        let mut best = row_aspect(&weights[i..row_end], row_sum, w, scale);
        while row_end < items.len() {
            let cand_sum = row_sum + weights[row_end];
            let cand = row_aspect(&weights[i..=row_end], cand_sum, w, scale);
            if cand <= best {
                best = cand;
                row_sum = cand_sum;
                row_end += 1;
            } else {
                break;
            }
        }
        let row_h = (row_sum * scale / w).min(y + h - cur_y).max(0.0);
        let mut cur_x = x;
        for j in i..row_end {
            let cell_w = if row_sum > 0.0 { w * weights[j] / row_sum } else { 0.0 };
            out.push(LaidRect {
                key: items[j].0.clone(),
                x: cur_x,
                y: cur_y,
                w: cell_w,
                h: row_h,
            });
            cur_x += cell_w;
        }
        cur_y += row_h;
        i = row_end;
    }
    out
}

/// Worst aspect ratio of a candidate row (≥1.0; lower is better).
fn row_aspect(row: &[f64], row_sum: f64, width: f64, scale: f64) -> f64 {
    if row_sum <= 0.0 || width <= 0.0 {
        return f64::INFINITY;
    }
    let row_h = row_sum * scale / width;
    row.iter()
        .map(|wt| {
            let cell_w = width * wt / row_sum;
            if row_h <= 0.0 || cell_w <= 0.0 {
                f64::INFINITY
            } else {
                (cell_w / row_h).max(row_h / cell_w)
            }
        })
        .fold(1.0_f64, f64::max)
}

/// Barycenter seriation for the ISP matrices: reorder rows and columns so
/// that co-used method/caller groups become contiguous — a stapled-together
/// trait then RENDERS as block-diagonal instead of requiring the eye to
/// mentally permute a scrambled matrix. Two alternating passes are enough to
/// surface block structure; more buys little and this must stay deterministic
/// (stable sort, index tie-break). Returns (row_order, col_order) as index
/// permutations into the input matrix.
pub fn seriate(cells: &[Vec<u32>]) -> (Vec<usize>, Vec<usize>) {
    let n_rows = cells.len();
    let n_cols = cells.first().map_or(0, Vec::len);
    let mut rows: Vec<usize> = (0..n_rows).collect();
    let mut cols: Vec<usize> = (0..n_cols).collect();
    for _ in 0..2 {
        // Rows by barycenter of their column positions.
        let col_pos: Vec<usize> = invert(&cols);
        rows.sort_by(|&a, &b| {
            let ba = barycenter(&cells[a], &col_pos);
            let bb = barycenter(&cells[b], &col_pos);
            ba.partial_cmp(&bb).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b))
        });
        // Columns by barycenter of their row positions.
        let row_pos: Vec<usize> = invert(&rows);
        cols.sort_by(|&a, &b| {
            let col = |c: usize| -> f64 {
                let (mut num, mut den) = (0.0, 0.0);
                for (r, row) in cells.iter().enumerate() {
                    let v = f64::from(row[c]);
                    num += v * row_pos[r] as f64;
                    den += v;
                }
                if den > 0.0 { num / den } else { f64::MAX }
            };
            let (ba, bb) = (col(a), col(b));
            ba.partial_cmp(&bb).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b))
        });
    }
    (rows, cols)
}

fn barycenter(row: &[u32], col_pos: &[usize]) -> f64 {
    let (mut num, mut den) = (0.0, 0.0);
    for (c, v) in row.iter().enumerate() {
        let v = f64::from(*v);
        num += v * col_pos[c] as f64;
        den += v;
    }
    if den > 0.0 { num / den } else { f64::MAX }
}

/// Permutation → position lookup (`pos[original_index] = display_position`).
fn invert(order: &[usize]) -> Vec<usize> {
    let mut pos = vec![0usize; order.len()];
    for (display, &orig) in order.iter().enumerate() {
        pos[orig] = display;
    }
    pos
}

/// Union-find over string keys — used for SRP co-change communities and for
/// grouping duplication-arc endpoints. Deterministic: ids assigned in
/// insertion order, and `communities()` returns components sorted by their
/// lexicographically-smallest member.
#[derive(Default)]
pub struct UnionFind {
    ids: std::collections::BTreeMap<String, usize>,
    parent: Vec<usize>,
}

impl UnionFind {
    fn id(&mut self, key: &str) -> usize {
        if let Some(&i) = self.ids.get(key) {
            return i;
        }
        let i = self.parent.len();
        self.ids.insert(key.to_string(), i);
        self.parent.push(i);
        i
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            self.parent[i] = self.parent[self.parent[i]];
            i = self.parent[i];
        }
        i
    }

    pub fn union(&mut self, a: &str, b: &str) {
        let (ia, ib) = (self.id(a), self.id(b));
        let (ra, rb) = (self.find(ia), self.find(ib));
        if ra != rb {
            self.parent[ra.max(rb)] = ra.min(rb);
        }
    }

    /// Components with ≥2 members, sorted by smallest member; members sorted.
    pub fn communities(&mut self) -> Vec<Vec<String>> {
        let entries: Vec<(String, usize)> =
            self.ids.iter().map(|(k, &v)| (k.clone(), v)).collect();
        let mut by_root: std::collections::BTreeMap<usize, Vec<String>> = Default::default();
        for (key, id) in entries {
            let root = self.find(id);
            by_root.entry(root).or_default().push(key);
        }
        let mut out: Vec<Vec<String>> = by_root
            .into_values()
            .filter(|members| members.len() >= 2)
            .collect();
        for m in &mut out {
            m.sort();
        }
        out.sort_by(|a, b| a[0].cmp(&b[0]));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(spec: &[(&str, f64)]) -> Vec<(String, f64)> {
        spec.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn strip_treemap_is_deterministic_and_order_stable() {
        let input = items(&[("a", 100.0), ("b", 40.0), ("c", 260.0), ("d", 10.0)]);
        let r1 = strip_treemap(&input, 0.0, 0.0, 400.0, 300.0);
        let r2 = strip_treemap(&input, 0.0, 0.0, 400.0, 300.0);
        assert_eq!(format!("{r1:?}"), format!("{r2:?}"), "same input, same bytes");
        // Order preserved — the strip never re-sorts by size.
        let keys: Vec<&str> = r1.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, vec!["a", "b", "c", "d"]);
        // Area is proportional to weight (within float tolerance).
        let area: f64 = r1.iter().map(|r| r.w * r.h).sum();
        assert!((area - 400.0 * 300.0).abs() < 1.0, "areas tile the canvas: {area}");
        let a = &r1[0];
        let c = &r1[2];
        assert!(
            ((c.w * c.h) / (a.w * a.h) - 2.6).abs() < 0.05,
            "relative areas track weights"
        );
    }

    #[test]
    fn strip_treemap_clamps_zero_weights_visible() {
        let input = items(&[("a", 0.0), ("b", 50.0)]);
        let r = strip_treemap(&input, 0.0, 0.0, 100.0, 100.0);
        assert!(r[0].w * r[0].h > 0.0, "zero-weight items must remain visible");
    }

    #[test]
    fn seriate_surfaces_block_structure() {
        // Two interleaved blocks: rows {0,2} use cols {0,2}; rows {1,3} use
        // cols {1,3}. After seriation each block's rows and cols must be
        // contiguous — that contiguity IS the "two interfaces stapled
        // together" rendering.
        let cells = vec![
            vec![9, 0, 8, 0],
            vec![0, 7, 0, 9],
            vec![8, 0, 9, 0],
            vec![0, 9, 0, 8],
        ];
        let (rows, cols) = seriate(&cells);
        let row_block: Vec<bool> = rows.iter().map(|&r| r % 2 == 0).collect();
        let col_block: Vec<bool> = cols.iter().map(|&c| c % 2 == 0).collect();
        assert!(
            row_block.windows(2).filter(|w| w[0] != w[1]).count() == 1,
            "row blocks contiguous after seriation: {rows:?}"
        );
        assert!(
            col_block.windows(2).filter(|w| w[0] != w[1]).count() == 1,
            "col blocks contiguous after seriation: {cols:?}"
        );
    }

    #[test]
    fn union_find_communities_are_deterministic() {
        let mut uf = UnionFind::default();
        uf.union("b.rs", "a.rs");
        uf.union("c.rs", "a.rs");
        uf.union("z.rs", "y.rs");
        uf.union("solo.rs", "solo.rs");
        let comms = uf.communities();
        assert_eq!(
            comms,
            vec![
                vec!["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()],
                vec!["y.rs".to_string(), "z.rs".to_string()],
            ]
        );
    }
}
