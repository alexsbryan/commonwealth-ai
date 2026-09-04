// SPDX-License-Identifier: AGPL-3.0-or-later
//! The filter that decides which atoms reach the atlas-context bag — and, via
//! [`AtlasContextFilter::signature`], the cache key that keeps a bag built
//! under one filter from being read under another.
//!
//! Its own module because it is config-as-data, not algorithm: env knobs, a
//! `Default`, and a signature string. It sat apart from the loader before the
//! two moved down here together (order ei-5a-build-cut) — in
//! `sovereign-tools`'s `atlas_context_manager.rs` while the loader had
//! `atlas_context_loader.rs` — and keeping them apart is what holds the
//! loader under the ARCH §3.1 800-line line rather than one 827-line file
//! carrying both.
//!
//! Re-exported from [`super::context_loader`], so
//! `context_loader::AtlasContextFilter` and the historical
//! `sovereign_tools::atlas_context_manager::AtlasContextFilter` both resolve
//! to this ONE definition (§10.6).

/// Filter applied during atlas-context loading. Mirrors the shape
/// of the eval CLI's `AtlasLoadFilter` so the cache key derived
/// here is comparable to what the CLI writes / reads.
#[derive(Debug, Clone)]
pub struct AtlasContextFilter {
    pub min_description_chars: usize,
    pub depth_allowlist: Vec<String>,
    pub max_entries: Option<usize>,
    pub top_k: usize,
    /// Path 2 (Phase A) — when true, the loader also emits virtual
    /// entries for `Claim` atoms in addition to `Entity` atoms. Each
    /// claim becomes one `AtlasEntry` whose `canonical_name` is the
    /// article slug (so retrieval-time `score_sources` matching by
    /// title still credits the source) and whose `embed_text`
    /// encodes the discourse_act + epistemic_status + content as
    /// `[Claim: <act>] <content>`. Default `false` for backwards
    /// compatibility with the entity-only cache. Cache key
    /// invalidates automatically via `signature()`.
    pub include_claims: bool,
    /// Path 2 (Phase B) — when true, the loader also emits virtual
    /// entries for `Tension` edges in `edges.json`. Each tension fuses
    /// its `sub_question` with both endpoint atoms into one embed text;
    /// `canonical_name` is the article slug. Default `false`. Cache
    /// key invalidates automatically via `signature()`. This is the
    /// only Path 2 surface that can move the `dialectical_breadth`
    /// essay axis — the substance lives on the edge, not on either
    /// endpoint atom by itself.
    pub include_tensions: bool,
    /// Path 2 (Phase C) — when true, the loader also emits virtual
    /// entries for `Configuration` atoms (spec §2.7). Each
    /// configuration becomes one `AtlasEntry` with `canonical_name`
    /// set to the article slug and embed text
    /// `[Configuration: <label>] <description>`. Default `false`.
    /// Should lift `argument_depth` on essay-readiness — Configurations
    /// articulate the interpretive shape the article enacts as a whole.
    pub include_configurations: bool,
    /// DARK (ontology-v1 P5, default **OFF**) —
    /// `SOVEREIGN_ATLAS_INCLUDE_DECLARED_CLAIMS`. Narrower than
    /// [`Self::include_claims`]: it admits only Claim atoms whose `claim_kind`
    /// names a type the corpus DECLARED, so an undeclared corpus admits
    /// nothing new however it is set. The declared claim is where a
    /// numismatics corpus keeps "who dated this coin to when, on what
    /// evidence" — content the entity bag cannot carry.
    ///
    /// Baked into [`Self::signature`], so a cache built with it off is
    /// correctly ignored when it flips on.
    pub include_declared_claim_types: bool,
}

impl Default for AtlasContextFilter {
    fn default() -> Self {
        // Defaults are tuned for Wikipedia/SEP-scale corpora where
        // Tier-2 extracted entities carry multi-sentence descriptions.
        // Small-corpus atom schemas (the `conversational` domain
        // produces ~0-150 char descriptions; arch-principles structural
        // atoms similarly short) would be filtered to zero here. Three
        // env knobs let the operator relax the filter at boot without
        // rebuilding:
        //   - SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS=<N> overrides the
        //     200-char floor. `0` admits every atom.
        //   - SOVEREIGN_ATLAS_INCLUDE_DEPTHS=<csv> overrides the
        //     `extracted`-only depth filter. `*` admits every depth.
        //   - SOVEREIGN_ATLAS_INCLUDE_CLAIMS=1|true surfaces Claim
        //     atoms as virtual chunks (default off).
        // The cache signature (`signature()`) bakes all three, so a cache
        // populated under one filter is correctly ignored under
        // another — no risk of cross-contaminating loaded atoms.
        // Floor on an atom's FULL embed signal (name + aliases + description),
        // not description alone — names are first-class grounding signal, so a
        // 10-char floor admits every real atom and drops only empty fragments.
        // (Was 200, which silently nuked name-rich/short-description atoms —
        // ~85% of SEP — and "filtered to zero" small-corpus schemas.)
        let min_chars = std::env::var("SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);
        let depth_allowlist = match std::env::var("SOVEREIGN_ATLAS_INCLUDE_DEPTHS") {
            Ok(v) if v.trim() == "*" => Vec::new(),
            Ok(v) => v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            Err(_) => vec!["extracted".to_string()],
        };
        // Claim atoms as virtual chunks (Path 2 Phase A). Off by default:
        // wiki/SEP-scale atlases lean on Entity grounding, and claims
        // multiply the embed count. For a small narrative atlas (a single
        // novel) the Claims ARE the substance — the entity descriptions are
        // short and the discriminating content lives in the Claim atoms — so
        // a literary grounding run sets this on. Baked into `signature()`,
        // so a cache built with claims off is ignored when it flips on.
        let include_claims = std::env::var("SOVEREIGN_ATLAS_INCLUDE_CLAIMS")
            .ok()
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true")
            })
            .unwrap_or(false);
        // DARK: declared-type claims as virtual chunks (ontology-v1 P5).
        // Off by default; see the `DEFAULTS_LEDGER.md` row for the flip
        // conditions.
        let include_declared_claim_types = std::env::var("SOVEREIGN_ATLAS_INCLUDE_DECLARED_CLAIMS")
            .ok()
            .map(|v| {
                let v = v.trim();
                v == "1" || v.eq_ignore_ascii_case("true")
            })
            .unwrap_or(false);
        Self {
            min_description_chars: min_chars,
            depth_allowlist,
            max_entries: None,
            top_k: 3,
            include_claims,
            include_tensions: false,
            include_configurations: false,
            include_declared_claim_types,
        }
    }
}

impl AtlasContextFilter {
    /// Stable signature used as the embeddings cache key. Must agree
    /// with `sovereign-cli::eval_cmd::runner::filter_signature` so a
    /// cache populated by either side is recognised by the other.
    pub fn signature(&self) -> String {
        let mut depths = self.depth_allowlist.clone();
        depths.sort();
        format!(
            "min_chars={};depth=[{}];max={};claims={};tensions={};configs={};declared_claims={}",
            self.min_description_chars,
            depths.join(","),
            self.max_entries
                .map(|n| n.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.include_claims,
            self.include_tensions,
            self.include_configurations,
            self.include_declared_claim_types,
        )
    }
}
