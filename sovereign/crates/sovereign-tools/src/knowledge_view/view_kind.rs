//! `ViewKind` — the type-safe handle for a KnowledgeView.
//!
//! Before this module existed, every caller that wanted to reason
//! about a view (assemble a digest, pick a budget, print a short
//! label for cross-view output) matched on `&str` view ids scattered
//! across `manager.rs`, `recipes.rs`, and `cross_view.rs`. Adding a
//! new view meant tracking down every match. Now the data lives on
//! the enum and each method is one line.
//!
//! The string ids (`"personal-knowledge"`, etc.) are still the
//! canonical persisted form — LanceDB paths and `_corpus_meta.json`
//! both key on them — so the enum ↔ id mapping is the contract here.

/// One of the four KnowledgeView perspectives. `CrossView` is
/// synthetic: it has no recipe, no index, and no ingest path — it
/// exists only as a key for the assembled resonance digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewKind {
    /// Personal memories — recurring concerns, held positions, open
    /// questions drawn from the user's own memory store.
    Personal,
    /// 180-day conversation history — active domains, unresolved
    /// threads, questions that crossed multiple sessions.
    Conversational,
    /// Institutional notes — architectural consensus, live tensions,
    /// open decisions drawn from the NoteStore.
    Institutional,
    /// Cross-view resonance digest. Synthetic (no backing index);
    /// assembled by `cross_view::build_cross_view_digest` from the
    /// other three views' field skeletons.
    CrossView,
}

impl ViewKind {
    /// Canonical on-disk id. Used as the corpus id in `corpus-engine`
    /// (index path, metadata, gossip) and as the `view_id` tag on
    /// spliced `LandscapeDigest`s.
    pub const fn id(&self) -> &'static str {
        match self {
            Self::Personal => "personal-knowledge",
            Self::Conversational => "conversation-history",
            Self::Institutional => "institutional-notes",
            Self::CrossView => "cross-view",
        }
    }

    /// Human-readable heading used at the top of a landscape digest
    /// ("Personal knowledge:", "Conversational knowledge:", …).
    pub const fn title(&self) -> &'static str {
        match self {
            Self::Personal => "Personal knowledge",
            Self::Conversational => "Conversational knowledge",
            Self::Institutional => "Institutional knowledge",
            Self::CrossView => "Cross-view connections",
        }
    }

    /// Short form used inside the cross-view digest to label where
    /// each theme came from: "(personal)", "(conversations)",
    /// "(institutional)".
    pub const fn short_label(&self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Conversational => "conversations",
            Self::Institutional => "institutional",
            Self::CrossView => "cross-view",
        }
    }

    /// Default per-turn token budget. Chosen to total ~600 tokens
    /// when all three primary views are spliced together, with
    /// cross-view adding another ~100.
    pub const fn default_budget_tokens(&self) -> usize {
        match self {
            Self::Personal => 300,
            Self::Conversational => 200,
            Self::Institutional => 100,
            Self::CrossView => 100,
        }
    }

    /// Parse a canonical id back to a `ViewKind`. Returns `None` for
    /// anything that isn't one of the four known ids — callers that
    /// have to reason about unknown views (e.g. during logging) can
    /// still pass the string along untyped.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "personal-knowledge" => Some(Self::Personal),
            "conversation-history" => Some(Self::Conversational),
            "institutional-notes" => Some(Self::Institutional),
            "cross-view" => Some(Self::CrossView),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_id() {
        for k in [
            ViewKind::Personal,
            ViewKind::Conversational,
            ViewKind::Institutional,
            ViewKind::CrossView,
        ] {
            assert_eq!(ViewKind::from_id(k.id()), Some(k));
        }
    }

    #[test]
    fn unknown_id_is_none() {
        assert_eq!(ViewKind::from_id("not-a-real-view"), None);
    }

    #[test]
    fn default_budgets_sum_to_spec() {
        // Spec §11 budget contract: three primary views together
        // stay within 600 tokens; cross-view adds at most 100 more.
        let primary = ViewKind::Personal.default_budget_tokens()
            + ViewKind::Conversational.default_budget_tokens()
            + ViewKind::Institutional.default_budget_tokens();
        assert_eq!(primary, 600);
        assert_eq!(ViewKind::CrossView.default_budget_tokens(), 100);
    }
}
