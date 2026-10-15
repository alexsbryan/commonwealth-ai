// SPDX-License-Identifier: AGPL-3.0-or-later
//! The exhaustive scan: a query's unit vector against every row of a scope,
//! the best `k` kept. Threaded across the host's cores, because the deployed
//! daemon is a debug build and one core scans about 180k 1024-wide rows a
//! second there (measured with this kernel before it was written).

use std::ops::Range;

use super::store::CorpusSentences;

/// One scored row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scored {
    pub row: usize,
    pub score: f32,
}

/// Cosine of two unit vectors, eight lanes at a time so an unoptimised
/// build still keeps its accumulators in registers.
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let tail: f32 = ca
        .remainder()
        .iter()
        .zip(cb.remainder())
        .map(|(x, y)| x * y)
        .sum();
    for (x, y) in ca.zip(cb) {
        for k in 0..8 {
            acc[k] += x[k] * y[k];
        }
    }
    acc.iter().sum::<f32>() + tail
}

/// The best `k` of `rows` by cosine with `query`, best first, ties by row.
pub fn top_k(
    index: &CorpusSentences,
    rows: &[Range<usize>],
    query: &[f32],
    k: usize,
) -> Vec<Scored> {
    let all: Vec<usize> = rows.iter().flat_map(|r| r.clone()).collect();
    if all.is_empty() || k == 0 || query.len() != index.dims {
        return Vec::new();
    }
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(all.len().div_ceil(2048))
        .max(1);
    let per = all.len().div_ceil(threads);
    let mut found: Vec<Scored> = std::thread::scope(|s| {
        let handles: Vec<_> = all
            .chunks(per)
            .map(|part| {
                s.spawn(move || {
                    let mut best: Vec<Scored> = Vec::with_capacity(k + 1);
                    for &row in part {
                        let score = dot(query, index.vector(row));
                        if best.len() < k || score > best[best.len() - 1].score {
                            let at = best.partition_point(|b| b.score >= score);
                            best.insert(at, Scored { row, score });
                            best.truncate(k);
                        }
                    }
                    best
                })
            })
            .collect();
        handles
            .into_iter()
            // A scan thread's panic is the caller's, never an empty part.
            .flat_map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    found.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.row.cmp(&b.row)));
    found.truncate(k);
    found
}

/// The first `ceiling` rows of `rows`, in order: the scope a scan past its
/// ceiling scores.
pub fn capped(rows: &[Range<usize>], ceiling: usize) -> Vec<Range<usize>> {
    let mut left = ceiling;
    let mut out = Vec::new();
    for r in rows {
        if left == 0 {
            break;
        }
        let take = r.len().min(left);
        out.push(r.start..r.start + take);
        left -= take;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::store::{split, unit, CorpusSentences};
    use super::*;
    use kernel_types::Sha256Hash;

    fn index(vectors: &[[f32; 2]]) -> CorpusSentences {
        let text: String = (0..vectors.len())
            .map(|i| format!("Sentence {i}. "))
            .collect();
        let mut s =
            CorpusSentences::default().rebuilt(None, vec![(Sha256Hash::of_str("t"), split(&text))]);
        s.stamp(unit(vec![1.0, 1.0]));
        for (r, v) in vectors.iter().enumerate() {
            s.put(r, &unit(v.to_vec()));
        }
        s
    }

    /// The best rows come back best first, whichever thread scored them, and
    /// a capped scope scores only its first rows.
    #[test]
    fn the_scan_keeps_the_best_rows_best_first_and_a_cap_is_a_prefix() {
        let mut vs = vec![[0.0, 1.0]; 5000];
        vs[4321] = [1.0, 0.0];
        vs[17] = [1.0, 0.2];
        let s = index(&vs);
        let all = s.scoped(None);
        let top = top_k(&s, &all, &[1.0, 0.0], 2);
        assert_eq!(
            top.iter().map(|t| t.row).collect::<Vec<_>>(),
            vec![4321, 17]
        );
        let first = capped(&all, 100);
        assert_eq!(first, vec![0..100]);
        let top = top_k(&s, &first, &[1.0, 0.0], 1);
        assert_eq!(top[0].row, 17);
    }

    /// The scan's cost on this build, for the data file's ceiling: run with
    /// `--ignored --nocapture` on the host whose daemon it sets.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn measure_the_scan_per_sentence() {
        let dims = 1024;
        for n in [22_000usize, 100_000, 200_000, 400_000] {
            let mut s = CorpusSentences {
                dims,
                vectors: (0..n * dims)
                    .map(|i| ((i * 7919) % 1000) as f32 / 1000.0)
                    .collect(),
                ..Default::default()
            };
            s.embedded = vec![true; n];
            let q: Vec<f32> = (0..dims).map(|i| (i % 13) as f32).collect();
            let rows = vec![0..n];
            let t = std::time::Instant::now();
            for _ in 0..3 {
                std::hint::black_box(top_k(&s, &rows, &q, 50));
            }
            println!("scan n={n}: {:?} each", t.elapsed() / 3);
        }
    }
}
