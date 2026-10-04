// SPDX-License-Identifier: AGPL-3.0-or-later
//! Dominance is a share of the pool, not an absolute repeat count
//! (`is_dominant_source_pool`): the bars that pin the 1/5 floor.

use super::{
    build_test_evidence_shape, decide_expansion_strategy, ExpansionStrategy, SynthesisRoute,
};
use crate::types::Intent;

#[test]
fn two_hits_under_one_section_title_do_not_collapse_a_diverse_pool() {
    // The measured defect (ei7-ans faaec2a84712, 2026-09-23): a
    // 20-chunk pool across 16 sources where the top SECTION title
    // repeated twice collapsed to 7 chunks and evicted the rank-4
    // answering passage. Two repeats of anything is not a fifth of
    // this pool; the merge-selected set survives untouched.
    let shape = build_test_evidence_shape(20, 16, true, 2);
    let (strategy, _) =
        decide_expansion_strategy(&Intent::KnowledgeQuery, SynthesisRoute::FastFocused, &shape);
    assert_eq!(strategy, ExpansionStrategy::NoExpansion);
    // The boundary is the share, not the absolute repeat: 3/20 (15%)
    // is still below a fifth and must not collapse either.
    let shape = build_test_evidence_shape(20, 16, true, 3);
    let (strategy, _) =
        decide_expansion_strategy(&Intent::KnowledgeQuery, SynthesisRoute::FastFocused, &shape);
    assert_eq!(strategy, ExpansionStrategy::NoExpansion);
}

#[test]
fn a_fifth_share_still_expands_dominant() {
    // 4 of 20 is exactly the share floor: deepening stays available
    // at the same strength the 3/10 pinned case exercises.
    let shape = build_test_evidence_shape(20, 8, true, 4);
    let (strategy, _) =
        decide_expansion_strategy(&Intent::KnowledgeQuery, SynthesisRoute::FastFocused, &shape);
    assert_eq!(strategy, ExpansionStrategy::DominantSource);
}
