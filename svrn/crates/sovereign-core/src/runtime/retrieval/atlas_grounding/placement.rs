// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where late-appended summaries land in the pool: reserved to the head, or
//! ranked against the leaves by similarity (RAPTOR's collapsed tree).
//!
//! The head reserve gives every summary a seat ahead of every leaf, so the
//! row's Summary quota alone sets the pool's non-leaf share (8 of the 10-slot
//! eval pool on the literary rows, 80%; the paper reports 23-57%). Collapsed
//! placement makes each summary earn its seat: its index is the number of
//! leaves closer to the question than it is, so under any top-k or char-budget
//! cut a summary survives iff it would in a cosine ranking of all nodes, as in
//! Sarthi et al. 2024 §3. Leaves keep the pipeline's order; only the summaries
//! are placed.
//!
//! The leaf scale is not clean: a leaf's `vector_distance` is measured against
//! the query that retrieved it, and entity-boost searches the question's named
//! entities ("The Pilot", "His Wife") on their own, so those leaves carry
//! cosine to the entity string (0.69-0.71 on the pilot's `arc`, where no leaf
//! exceeds 0.667 against the question). Nothing on the chunk says which query
//! it was. Measured 2026-10-02 on the pilot essay bank it does not change the
//! verdict: ranking all 258 nodes against the question vector alone, the best
//! summary places 27th-124th and the top 20 hold no summary on any of the 12
//! questions.

use corpus_index::types::ScoredChunk;

/// The one switch (feature-fidelity R0.1). A closed set of two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SummaryPlacement {
    /// Every summary ahead of every leaf.
    Head,
    /// Each summary after the leaves closer to the question than it is.
    Collapsed,
}

impl SummaryPlacement {
    /// `SOVEREIGN_SUMMARY_PLACEMENT=collapsed`; unset means `Head`. Any other
    /// value is named at warn and read as `Head`, never guessed at.
    pub(crate) fn from_env() -> Self {
        match std::env::var("SOVEREIGN_SUMMARY_PLACEMENT") {
            Err(_) => Self::Head,
            Ok(v) => match v.trim().to_ascii_lowercase().as_str() {
                "" | "head" => Self::Head,
                "collapsed" => Self::Collapsed,
                other => {
                    tracing::warn!(
                        value = other,
                        "SOVEREIGN_SUMMARY_PLACEMENT is not head|collapsed; using head"
                    );
                    Self::Head
                }
            },
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Collapsed => "collapsed",
        }
    }
}

/// Merge `summaries` into `pool` by cosine distance to the question.
///
/// A summary's slot is the count of pool entries with a distance no greater
/// than its own (a tie goes to the leaf), and it is inserted before the next
/// distance-bearing entry. Entries with no distance (FTS-only hits, pinned
/// atom-enum chunks) are not on the scale: they keep their place and never
/// count against a summary. A summary with no distance lands at the tail.
///
/// Returns the merged pool and, per summary, `(landed index, cosine)` for the
/// trace.
pub(crate) fn place_collapsed(
    pool: Vec<ScoredChunk>,
    mut summaries: Vec<ScoredChunk>,
) -> (Vec<ScoredChunk>, Vec<(usize, Option<f32>)>) {
    let dist = |c: &ScoredChunk| c.vector_distance.filter(|d| d.is_finite());
    summaries.sort_by(|a, b| {
        dist(a)
            .unwrap_or(f32::INFINITY)
            .total_cmp(&dist(b).unwrap_or(f32::INFINITY))
    });
    // Pool indices of the entries that are on the scale, in pool order.
    let on_scale: Vec<usize> = (0..pool.len())
        .filter(|&i| dist(&pool[i]).is_some())
        .collect();
    let mut on_scale_dists: Vec<f32> = on_scale.iter().filter_map(|&i| dist(&pool[i])).collect();
    on_scale_dists.sort_by(f32::total_cmp);
    let slot = |s: &ScoredChunk| -> usize {
        let Some(d) = dist(s) else {
            return pool.len();
        };
        let beaten_by = on_scale_dists.partition_point(|&leaf| leaf <= d);
        on_scale.get(beaten_by).copied().unwrap_or(pool.len())
    };
    let slots: Vec<usize> = summaries.iter().map(slot).collect();

    let mut out: Vec<ScoredChunk> = Vec::with_capacity(pool.len() + summaries.len());
    let mut landed: Vec<(usize, Option<f32>)> = Vec::with_capacity(summaries.len());
    let mut pending = summaries.into_iter().zip(slots).peekable();
    for (i, leaf) in pool.into_iter().enumerate() {
        while let Some((s, _)) = pending.next_if(|(_, at)| *at <= i) {
            landed.push((out.len(), dist(&s).map(|d| 1.0 - d)));
            out.push(s);
        }
        out.push(leaf);
    }
    for (s, _) in pending {
        landed.push((out.len(), dist(&s).map(|d| 1.0 - d)));
        out.push(s);
    }
    (out, landed)
}

#[cfg(test)]
mod tests {
    use super::place_collapsed;
    use corpus_index::{index::ChunkProvenance, types::ScoredChunk};

    fn chunk(name: &str, distance: Option<f32>, summary: bool) -> ScoredChunk {
        ScoredChunk {
            content: name.into(),
            title: None,
            url: None,
            corpus_id: "c".into(),
            score: 0.0,
            metadata: Default::default(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: distance,
            provenance: if summary {
                ChunkProvenance::manufactured_summary("atlas_summary")
            } else {
                ChunkProvenance::acquired_from_estate("c")
            },
        }
    }

    fn names(pool: &[ScoredChunk]) -> Vec<&str> {
        pool.iter().map(|c| c.content.as_str()).collect()
    }

    /// Leaves arrive in RERANK order, not cosine order. A summary at 0.5
    /// cosine is beaten by three leaves (`a`, `c`, `d`) wherever they sit, so
    /// its index is 3: a top-3 cut drops it and a top-4 cut keeps it, exactly
    /// as a cosine ranking of all six would. Inserting before the first weaker
    /// leaf (`b`, index 1) would seat it ahead of two closer leaves.
    #[test]
    fn a_summary_index_is_the_count_of_closer_leaves() {
        let pool = vec![
            chunk("a", Some(0.4), false),
            chunk("b", Some(0.7), false),
            chunk("c", Some(0.3), false),
            chunk("d", Some(0.35), false),
            chunk("e", Some(0.8), false),
        ];
        let (out, landed) = place_collapsed(pool, vec![chunk("S", Some(0.5), true)]);
        assert_eq!(names(&out), vec!["a", "b", "c", "S", "d", "e"]);
        assert_eq!(landed, vec![(3, Some(0.5))]);
    }

    /// Both extremes, and summaries ordered among themselves: one closer than
    /// every leaf takes the head, one further than every leaf takes the tail,
    /// so a top-k cut keeps the first and drops the second.
    #[test]
    fn summaries_compete_rather_than_being_reserved() {
        let pool = vec![chunk("a", Some(0.4), false), chunk("b", Some(0.5), false)];
        let (out, _) = place_collapsed(
            pool,
            vec![
                chunk("far", Some(0.9), true),
                chunk("near", Some(0.1), true),
            ],
        );
        assert_eq!(names(&out), vec!["near", "a", "b", "far"]);
    }

    /// A leaf with no distance (FTS-only, pinned) is off the scale: it keeps
    /// its place and does not count against a summary. A tie goes to the leaf.
    #[test]
    fn off_scale_leaves_hold_their_place_and_ties_go_to_the_leaf() {
        let pool = vec![
            chunk("pinned", None, false),
            chunk("a", Some(0.5), false),
            chunk("fts", None, false),
            chunk("b", Some(0.6), false),
        ];
        let (out, _) = place_collapsed(pool, vec![chunk("S", Some(0.5), true)]);
        assert_eq!(names(&out), vec!["pinned", "a", "fts", "S", "b"]);
    }

    /// The leaf order and set are untouched; only summaries are inserted.
    #[test]
    fn the_leaf_order_is_preserved() {
        let pool: Vec<ScoredChunk> = [0.9, 0.2, 0.6, 0.4]
            .iter()
            .enumerate()
            .map(|(i, d)| chunk(&format!("l{i}"), Some(*d), false))
            .collect();
        let (out, _) = place_collapsed(
            pool,
            vec![chunk("S1", Some(0.5), true), chunk("S2", None, true)],
        );
        let leaves: Vec<&str> = out
            .iter()
            .filter(|c| c.provenance.grain() == kernel_types::Grain::Leaf)
            .map(|c| c.content.as_str())
            .collect();
        assert_eq!(leaves, vec!["l0", "l1", "l2", "l3"]);
        assert_eq!(out.len(), 6);
        assert_eq!(out.last().unwrap().content, "S2");
    }
}
