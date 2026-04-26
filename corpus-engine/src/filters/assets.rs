//! Bundled filter artefacts compiled into the crate via `include_bytes!`.
//!
//! These are committed under `corpus-engine/assets/` and referenced by
//! recipes through `@bundled:<key>` paths (resolved by
//! [`crate::filters::loader`]).
//!
//! ## Updating the bundled data
//!
//! - **Pageview ranks**: regenerate via
//!   `sovereign-recipes/wikipedia/scripts/build_pageview_ranks.py` against a
//!   fresh Wikimedia pageview dump, gzip the resulting CSV
//!   (`gzip -9 pageview_ranks_YYYYMM.csv`), drop it into
//!   `corpus-engine/assets/`, and bump the constant name + the recipe's
//!   `rank_file` reference in lockstep.
//! - **Vital articles**: scrape `Wikipedia:Vital_articles/Level/5`
//!   subpages, dedupe + sort, write one title per line. The list is
//!   small enough to keep uncompressed.
//!
//! The repository ships with **placeholder** assets (a few sentinel
//! entries) so the crate compiles and tests pass on a clean checkout.
//! Production binaries pick up the real data via PR — the bundling
//! mechanism stays the same.

/// Gzip-compressed CSV mapping `title,rank`. See the module docs for
/// regeneration instructions.
pub const PAGEVIEW_RANKS_202311: &[u8] =
    include_bytes!("../../assets/pageview_ranks_202311.csv.gz");

/// Newline-delimited Wikipedia Vital Articles Level 5 titles.
pub const VITAL_ARTICLES_L5: &[u8] = include_bytes!("../../assets/vital_articles_l5.txt");

/// Look up a bundled asset by its `@bundled:<key>` shorthand.
pub fn lookup_bundled(key: &str) -> Option<&'static [u8]> {
    match key {
        "pageview_ranks_202311" => Some(PAGEVIEW_RANKS_202311),
        "vital_articles_l5" => Some(VITAL_ARTICLES_L5),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_bundled_keys_resolve() {
        assert!(lookup_bundled("pageview_ranks_202311").is_some());
        assert!(lookup_bundled("vital_articles_l5").is_some());
        assert!(lookup_bundled("not_a_key").is_none());
    }
}
