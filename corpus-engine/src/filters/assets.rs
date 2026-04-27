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

/// Look up a bundled asset by its `@bundled:<key>` shorthand.
pub fn lookup_bundled(key: &str) -> Option<&'static [u8]> {
    match key {
        "vital_articles_l5" => Some(VITAL_ARTICLES_L5),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_bundled_keys_resolve() {
        assert!(lookup_bundled("vital_articles_l5").is_some());
        assert!(lookup_bundled("not_a_key").is_none());
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
