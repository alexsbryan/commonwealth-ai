// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wikipedia Vital Articles tier classifier.
//!
//! Wraps the bundled `vital_articles_l{1..5}.txt` lists in a single
//! `vital_tier(canonical_name) -> Option<u8>` entry point used by the
//! atlas post-install triage step as a curator-canonical prior.
//!
//! ## Why a tier prior matters
//!
//! Pure inbound + outbound centrality on the structural atlas is a
//! noisy proxy for "what an encyclopedia user actually needs covered
//! deeply." Disambiguation pages and template-wrapped lists rack up
//! enormous degree without being content-rich; specialized topics
//! rank low despite mattering more than their link footprint says.
//!
//! Vital Articles is a community-curated answer to the same question:
//! given a fixed budget, what should an encyclopedia cover? Bundling
//! tiers L1-L5 as a centrality prior lets the triage scorer prefer
//! curator-canonical articles without throwing centrality away.
//!
//! ## Tiers (curator-defined, nested)
//!
//! Each tier is a strict superset of the one above:
//! `L1 ⊂ L2 ⊂ L3 ⊂ L4 ⊂ L5`.
//!
//! | Tier | Curator quota | This bundle |
//! |------|--------------:|------------:|
//! | L1   |            10 |          10 |
//! | L2   |           100 |         100 |
//! | L3   |         1,000 |       1,003 |
//! | L4   |        10,000 |      10,007 |
//! | L5   |        50,000 |      51,286 |
//!
//! `vital_tier` returns the *narrowest* tier a title belongs to —
//! a title in L1 is also in L2..L5, but `vital_tier` returns 1 so
//! the caller can rank by tier-sharpness directly.
//!
//! ## Matching semantics
//!
//! Title strings get [`crate::filters::normalize_title`] treatment
//! (lowercase + underscore→space + whitespace collapse), so the same
//! article matches whether it arrives as `"Albert Einstein"`,
//! `"albert_einstein"`, or `"ALBERT  EINSTEIN"`. Disambiguation
//! suffixes like `(disambiguation)` are preserved — they are
//! semantically distinct articles.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::filters::{assets, normalize_title};

/// Set of normalized titles for one Vital Articles tier.
struct TierSet {
    titles: HashSet<String>,
}

impl TierSet {
    fn from_bytes(bytes: &[u8]) -> Self {
        let text = std::str::from_utf8(bytes).expect("bundled vital_articles file is UTF-8");
        let mut titles = HashSet::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            titles.insert(normalize_title(trimmed));
        }
        Self { titles }
    }

    fn contains(&self, normalized: &str) -> bool {
        self.titles.contains(normalized)
    }

    fn len(&self) -> usize {
        self.titles.len()
    }
}

/// Lazily-loaded view over all five tiers. Tier index is `tier - 1`,
/// so `tiers()[0]` is L1 and `tiers()[4]` is L5.
fn tiers() -> &'static [TierSet; 5] {
    static SETS: OnceLock<[TierSet; 5]> = OnceLock::new();
    SETS.get_or_init(|| {
        [
            TierSet::from_bytes(assets::VITAL_ARTICLES_L1),
            TierSet::from_bytes(assets::VITAL_ARTICLES_L2),
            TierSet::from_bytes(assets::VITAL_ARTICLES_L3),
            TierSet::from_bytes(assets::VITAL_ARTICLES_L4),
            TierSet::from_bytes(assets::VITAL_ARTICLES_L5),
        ]
    })
}

/// Return the narrowest Vital Articles tier `name` belongs to, or
/// `None` if it isn't on any tier list.
///
/// Tiers are nested (L1 ⊂ L2 ⊂ … ⊂ L5), so a title in L1 is also
/// technically in L2..L5; this function returns `Some(1)` because L1
/// membership is the highest-priority signal a caller can act on.
///
/// Matching uses [`normalize_title`] so case + underscores ↔ spaces
/// don't matter.
pub fn vital_tier(name: &str) -> Option<u8> {
    let key = normalize_title(name);
    let sets = tiers();
    for (idx, set) in sets.iter().enumerate() {
        if set.contains(&key) {
            return Some((idx + 1) as u8);
        }
    }
    None
}

/// Number of distinct titles bundled per tier. Useful for diagnostic
/// surfaces (`sovereign corpus status`, `sovereign enrich
/// triage-candidates`).
pub fn tier_sizes() -> [usize; 5] {
    let sets = tiers();
    [
        sets[0].len(),
        sets[1].len(),
        sets[2].len(),
        sets[3].len(),
        sets[4].len(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_1_titles_classify_as_l1() {
        // Pulled from the L1 bundle — these are the curator-canonical
        // ten. If the upstream list rotates members, update here.
        for name in [
            "Earth",
            "Human",
            "Science",
            "Mathematics",
            "Philosophy",
            "Society",
            "Technology",
            "Life",
            "Human history",
            "The arts",
        ] {
            assert_eq!(vital_tier(name), Some(1), "{name} should be L1");
        }
    }

    #[test]
    fn level_2_titles_classify_as_l2_not_l1() {
        // Sample from L2 that's not in L1 — `Universe`, `Religion`
        // appear in L2 but not L1 per the fetched lists.
        assert_eq!(vital_tier("Universe"), Some(2));
        assert_eq!(vital_tier("Religion"), Some(2));
    }

    #[test]
    fn underscore_and_case_normalize() {
        assert_eq!(vital_tier("earth"), Some(1));
        assert_eq!(vital_tier("the_arts"), Some(1));
        assert_eq!(vital_tier("THE  ARTS"), Some(1));
    }

    #[test]
    fn unknown_title_returns_none() {
        assert_eq!(vital_tier("Some Random Garage Band"), None);
    }

    #[test]
    fn tier_sizes_match_quotas() {
        let s = tier_sizes();
        assert!((8..=12).contains(&s[0]), "L1 size {}", s[0]);
        assert!((80..=120).contains(&s[1]), "L2 size {}", s[1]);
        assert!((900..=1200).contains(&s[2]), "L3 size {}", s[2]);
        assert!((9000..=12000).contains(&s[3]), "L4 size {}", s[3]);
        assert!((40000..=60000).contains(&s[4]), "L5 size {}", s[4]);
    }
}
