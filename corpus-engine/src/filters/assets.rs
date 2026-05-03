//! Bundled filter artefacts compiled into the crate via `include_bytes!`.
//!
//! Source files live in the sibling `sovereign-recipes` repo (where
//! their generator scripts live), copied into Cargo's `OUT_DIR` by
//! [`build.rs`](../../build.rs) at compile time. corpus-engine itself
//! tracks no generated artefacts — single source of truth in
//! `sovereign-recipes/wikipedia/data/`.
//!
//! ## What ships bundled
//!
//! - **`vital_articles_l5`** — Wikipedia Vital Articles Level 5. ~51K
//!   curated mainspace titles representing "what an encyclopedia
//!   should cover." Regenerate via
//!   `sovereign-recipes/wikipedia/scripts/build_vital_articles.py`
//!   when the curators add new entries (rare — the list is editorial-
//!   pace, not viral-pace).
//! - **`vital_articles_l1` … `vital_articles_l4`** — the four narrower
//!   tiers (10 / 100 / 1,000 / 10,000 articles). Used by the atlas
//!   post-install triage step as a centrality prior so Tier-2
//!   enrichment burns its budget on the articles a curator-chosen
//!   ranking already says are most encyclopedic. Regenerate via
//!   `sovereign-recipes/wikipedia/scripts/build_vital_articles_tiered.py`
//!   (parses wikitext list items rather than `prop=links` so prose
//!   chrome doesn't sneak in for the small-tier pages).
//!
//! ## What does NOT ship bundled
//!
//! - **Pageview ranks**: the build script
//!   (`sovereign-recipes/wikipedia/scripts/build_pageview_ranks.py`)
//!   still works and recipes can reference a generated rank file via
//!   path, but bundling it would tie the crate to a specific month's
//!   Wikipedia traffic. That data ages out within ~6 months and the
//!   benefit over Vital-Articles-alone is small enough that the
//!   freshness debt isn't worth carrying. See
//!   [`crate::filters::pageview_rank::PageviewRankFilter`] for the
//!   filter implementation — still generic and future-proof, just not
//!   bundled with stale data.
//!
//! ## Build-time resolution
//!
//! The `build.rs` looks for source files in this order:
//!   1. `$CORPUS_ENGINE_DATA_DIR` (escape hatch for standalone /
//!      airgapped builds — set this to a directory containing the
//!      filenames in `BUNDLED_ASSETS` in `build.rs`).
//!   2. `<corpus-engine>/../sovereign-recipes/wikipedia/data/` — the
//!      common workspace-sibling path.
//! If neither resolves, the build fails with a remediation hint
//! pointing at the regenerator script.

/// Newline-delimited Wikipedia Vital Articles Level 5 titles. The
/// `OUT_DIR`-relative include path is what makes this consume the
/// build-script-copied file rather than a tracked-in-crate artefact.
pub const VITAL_ARTICLES_L5: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/vital_articles_l5.txt"));

/// Wikipedia Vital Articles Level 1 — the curator-canonical "ten
/// most vital" articles. Used as the highest-priority tier in the
/// atlas triage prior.
pub const VITAL_ARTICLES_L1: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/vital_articles_l1.txt"));

/// Wikipedia Vital Articles Level 2 — ~100 curator-canonical titles.
pub const VITAL_ARTICLES_L2: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/vital_articles_l2.txt"));

/// Wikipedia Vital Articles Level 3 — ~1,000 curator-canonical titles.
pub const VITAL_ARTICLES_L3: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/vital_articles_l3.txt"));

/// Wikipedia Vital Articles Level 4 — ~10,000 curator-canonical titles.
pub const VITAL_ARTICLES_L4: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/vital_articles_l4.txt"));

/// Look up a bundled asset by its `@bundled:<key>` shorthand.
pub fn lookup_bundled(key: &str) -> Option<&'static [u8]> {
    match key {
        "vital_articles_l1" => Some(VITAL_ARTICLES_L1),
        "vital_articles_l2" => Some(VITAL_ARTICLES_L2),
        "vital_articles_l3" => Some(VITAL_ARTICLES_L3),
        "vital_articles_l4" => Some(VITAL_ARTICLES_L4),
        "vital_articles_l5" => Some(VITAL_ARTICLES_L5),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_bundled_keys_resolve() {
        assert!(lookup_bundled("vital_articles_l1").is_some());
        assert!(lookup_bundled("vital_articles_l2").is_some());
        assert!(lookup_bundled("vital_articles_l3").is_some());
        assert!(lookup_bundled("vital_articles_l4").is_some());
        assert!(lookup_bundled("vital_articles_l5").is_some());
        assert!(lookup_bundled("not_a_key").is_none());
    }

    /// Sanity-check tier sizes. Curator quotas: L1=10, L2=100,
    /// L3=1000, L4=10,000. The fetch script's wikitext heuristic
    /// can come in slightly under the quota when the curators have
    /// open slots, but it should never balloon — if a future
    /// regeneration produces wildly different counts the fetcher
    /// likely picked up navbar/prose chrome and needs a tighter
    /// list-item match.
    #[test]
    fn vital_article_tier_sizes_within_curator_quota() {
        let count = |bytes: &[u8]| {
            std::str::from_utf8(bytes)
                .unwrap()
                .lines()
                .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
                .count()
        };
        let l1 = count(VITAL_ARTICLES_L1);
        let l2 = count(VITAL_ARTICLES_L2);
        let l3 = count(VITAL_ARTICLES_L3);
        let l4 = count(VITAL_ARTICLES_L4);
        // Allow ±20% slack for curator drift; widen if real curator
        // re-quotas exceed it.
        assert!((8..=12).contains(&l1), "L1 expected ~10, got {l1}");
        assert!((80..=120).contains(&l2), "L2 expected ~100, got {l2}");
        assert!((900..=1200).contains(&l3), "L3 expected ~1000, got {l3}");
        assert!((9000..=12000).contains(&l4), "L4 expected ~10000, got {l4}");
    }

    /// Pin the deliberate decision to NOT bundle pageview rank data.
    /// Adding it back means committing to a regeneration cadence the
    /// project doesn't currently fund. If you find yourself adding the
    /// const, also schedule the regeneration (see the module docs).
    #[test]
    fn pageview_ranks_are_intentionally_unbundled() {
        assert!(lookup_bundled("pageview_ranks_202311").is_none());
        assert!(lookup_bundled("pageview_ranks").is_none());
    }
}
