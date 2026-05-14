//! Built-in recipe catalog — extracted out of `crate::recipe`.
//!
//! `RecipeId` is the type-safe identifier for every bundled recipe.
//! `bundled_recipe_toml` is the wire-string adapter used by
//! `crate::registry::RecipeRegistry::fetch_recipe` as a last-resort
//! fallback when the live URL is unreachable.

/// Type-safe identifier for every bundled recipe in this crate.
///
/// The wire form (TOML files, the `registry_snapshot.toml` catalog,
/// `sovereign corpus install <id>` CLI args) stays string-typed — the
/// strings are the API contract per ARCH_PRINCIPLES.md §2.2. This
/// enum is the source of truth on the Rust side: adding a new bundled
/// recipe means adding a variant here, which gives compile-time
/// exhaustiveness across the dispatch + accessor surface, and the
/// `bundled_recipe_covers_every_snapshot_entry` test keeps the
/// enum aligned with the catalog.
///
/// To add a new bundled recipe:
///   1. Drop `recipes/<id>/recipe.toml` in the crate.
///   2. Add a `RecipeId` variant here.
///   3. Extend `RecipeId::id()`, `RecipeId::bundled_toml()`, and
///      `RecipeId::from_id()` — `rustc` flags any of these you miss.
///   4. Add the catalog entry to `registry_snapshot.toml` +
///      `sovereign-recipes/registry.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecipeId {
    Wikipedia,
    WikipediaSimple,
    StackExchange,
    StackExchangeKnowledge,
    OpenAlex,
    Gutenberg,
    GutenbergWork,
    WikipediaCatalog,
    WikipediaArticle,
    WikipediaNewsworthy,
    Alignment,
    Sep,
    CrsReports,
    FederalRegisterPresidential,
    UsCode,
    OlcOpinions,
    ScotusOpinions,
}

impl RecipeId {
    /// Wire-form string id for this recipe. Stable contract — these
    /// strings appear in `registry_snapshot.toml`, recipe TOML
    /// frontmatter, the `corpus install <id>` CLI surface, mesh
    /// gossip, and on-disk state. Don't rename without coordinating
    /// across all of those.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Wikipedia => "wikipedia",
            Self::WikipediaSimple => "wikipedia-simple",
            Self::StackExchange => "stackexchange",
            Self::StackExchangeKnowledge => "stackexchange-knowledge",
            Self::OpenAlex => "openalex",
            Self::Gutenberg => "gutenberg",
            Self::GutenbergWork => "gutenberg-work",
            Self::WikipediaCatalog => "wikipedia-catalog",
            Self::WikipediaArticle => "wikipedia-article",
            Self::WikipediaNewsworthy => "wikipedia-newsworthy",
            Self::Alignment => "alignment",
            Self::Sep => "sep",
            Self::CrsReports => "crs_reports",
            Self::FederalRegisterPresidential => "federal-register-presidential",
            Self::UsCode => "us-code",
            Self::OlcOpinions => "olc-opinions",
            Self::ScotusOpinions => "scotus-opinions",
        }
    }

    /// Parse a wire-form id. `None` for unknown ids — caller falls
    /// back to its remote-fetch / error path.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "wikipedia" => Some(Self::Wikipedia),
            "wikipedia-simple" => Some(Self::WikipediaSimple),
            "stackexchange" => Some(Self::StackExchange),
            "stackexchange-knowledge" => Some(Self::StackExchangeKnowledge),
            "openalex" => Some(Self::OpenAlex),
            "gutenberg" => Some(Self::Gutenberg),
            "gutenberg-work" => Some(Self::GutenbergWork),
            "wikipedia-catalog" => Some(Self::WikipediaCatalog),
            "wikipedia-article" => Some(Self::WikipediaArticle),
            "wikipedia-newsworthy" => Some(Self::WikipediaNewsworthy),
            "alignment" => Some(Self::Alignment),
            "sep" => Some(Self::Sep),
            "crs_reports" => Some(Self::CrsReports),
            "federal-register-presidential" => Some(Self::FederalRegisterPresidential),
            "us-code" => Some(Self::UsCode),
            "olc-opinions" => Some(Self::OlcOpinions),
            "scotus-opinions" => Some(Self::ScotusOpinions),
            _ => None,
        }
    }

    /// The compile-time bundled recipe TOML for this id. Each arm is
    /// an `include_str!` of the recipe file in `recipes/<id>/`.
    pub fn bundled_toml(self) -> &'static str {
        match self {
            Self::Wikipedia => include_str!("../recipes/wikipedia/recipe.toml"),
            Self::WikipediaSimple => include_str!("../recipes/wikipedia-simple/recipe.toml"),
            Self::StackExchange => include_str!("../recipes/stackexchange/recipe.toml"),
            Self::StackExchangeKnowledge => {
                include_str!("../recipes/stackexchange-knowledge/recipe.toml")
            }
            Self::OpenAlex => include_str!("../recipes/openalex/recipe.toml"),
            Self::Gutenberg => include_str!("../recipes/gutenberg/recipe.toml"),
            Self::GutenbergWork => include_str!("../recipes/gutenberg-work/recipe.toml"),
            Self::WikipediaCatalog => include_str!("../recipes/wikipedia-catalog/recipe.toml"),
            Self::WikipediaArticle => include_str!("../recipes/wikipedia-article/recipe.toml"),
            Self::WikipediaNewsworthy => {
                include_str!("../recipes/wikipedia-newsworthy/recipe.toml")
            }
            Self::Alignment => include_str!("../recipes/alignment/recipe.toml"),
            Self::Sep => include_str!("../recipes/sep/recipe.toml"),
            Self::CrsReports => include_str!("../recipes/crs_reports/recipe.toml"),
            Self::FederalRegisterPresidential => {
                include_str!("../recipes/federal-register-presidential/recipe.toml")
            }
            Self::UsCode => include_str!("../recipes/us-code/recipe.toml"),
            Self::OlcOpinions => include_str!("../recipes/olc-opinions/recipe.toml"),
            Self::ScotusOpinions => {
                include_str!("../recipes/scotus-opinions/recipe.toml")
            }
        }
    }

    /// Every bundled `RecipeId`, in declaration order. Used by tests
    /// to assert the enum stays paired with `registry_snapshot.toml`
    /// without manually enumerating in two places.
    pub const ALL: &'static [RecipeId] = &[
        Self::Wikipedia,
        Self::WikipediaSimple,
        Self::StackExchange,
        Self::StackExchangeKnowledge,
        Self::OpenAlex,
        Self::Gutenberg,
        Self::GutenbergWork,
        Self::WikipediaCatalog,
        Self::WikipediaArticle,
        Self::WikipediaNewsworthy,
        Self::Alignment,
        Self::Sep,
        Self::CrsReports,
        Self::FederalRegisterPresidential,
        Self::UsCode,
        Self::OlcOpinions,
        Self::ScotusOpinions,
    ];
}

/// Bundled recipe TOML for well-known corpora, embedded at compile
/// time. Used as a **last-resort fallback** by
/// `RecipeRegistry::fetch_recipe()` so a corpus listed in the snapshot
/// catalog still installs even when:
///
/// - the registry's `toml_url` 404s (recipe not pushed to GitHub yet —
///   common during development);
/// - the user has no internet; or
/// - the user is running an air-gapped build.
///
/// Returns `None` for unknown ids; the caller falls back to its prior
/// error message in that case.
///
/// Per ARCH_PRINCIPLES.md §2.1+§2.2: the dispatch is now type-safe
/// via [`RecipeId`]; this function stays as the string-keyed
/// adapter for callers that already hold a `&str` from the wire
/// (registry catalog id, CLI arg). New Rust callsites should prefer
/// `RecipeId::<variant>.bundled_toml()` directly.
pub fn bundled_recipe_toml(id: &str) -> Option<&'static str> {
    RecipeId::from_id(id).map(|r| r.bundled_toml())
}
