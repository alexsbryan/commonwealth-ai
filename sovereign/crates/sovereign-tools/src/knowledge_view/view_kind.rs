// SPDX-License-Identifier: AGPL-3.0-or-later
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

/// One of the KnowledgeView perspectives. `CrossView`, `Relational`
/// and `Strategic` are **synthetic** — they have no recipes, no
/// indexes, and no ingest paths. They exist as keys for digest
/// blocks assembled at splice time:
///
/// - `CrossView` is built from the other views' field skeletons
///   (resonance matching).
/// - `Relational` is built from the `Person` / `Organization` atoms
///   in the personal + conversational atlases (entity timelines).
/// - `Strategic` is built from `Initiative` atoms plus goal notes.
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
    /// People + organisations the user has discussed recently.
    /// Synthetic: derived from `Person` and `Organization` atoms in
    /// the personal + conversational atlases. See
    /// `knowledge_view::relational` for the formatter and
    /// `knowledge_view::timeline` for assembly.
    Relational,
    /// Initiatives the user is organising work around. Synthetic:
    /// derived from `Initiative` atoms + goal notes. ATOS phase /
    /// charter status composed via [`crate::knowledge_view::timeline::AtosLookup`].
    Strategic,
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
            Self::Relational => "relational",
            Self::Strategic => "strategic",
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
            Self::Relational => "People on your radar",
            Self::Strategic => "Active initiatives",
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
            Self::Relational => "relational",
            Self::Strategic => "strategic",
        }
    }

    /// Default per-turn token budget. Chosen to total ~850 tokens
    /// across the five primary blocks, with cross-view adding
    /// another ~100. Splits per requirements §4.1:
    ///   Personal 300 / Conversational 200 / Institutional 100 /
    ///   Relational 150 / Strategic 100.
    pub const fn default_budget_tokens(&self) -> usize {
        match self {
            Self::Personal => 300,
            Self::Conversational => 200,
            Self::Institutional => 100,
            Self::CrossView => 100,
            Self::Relational => 150,
            Self::Strategic => 100,
        }
    }

    /// True when the view has its own LanceDB index + recipe +
    /// ingest path. False for synthetic views (`CrossView`,
    /// `Relational`, `Strategic`) that are assembled from other
    /// views' outputs at splice time.
    pub const fn has_own_index(&self) -> bool {
        matches!(
            self,
            Self::Personal | Self::Conversational | Self::Institutional
        )
    }

    /// Parse a canonical id back to a `ViewKind`. Returns `None` for
    /// anything that isn't one of the known ids — callers that
    /// have to reason about unknown views (e.g. during logging) can
    /// still pass the string along untyped.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "personal-knowledge" => Some(Self::Personal),
            "conversation-history" => Some(Self::Conversational),
            "institutional-notes" => Some(Self::Institutional),
            "cross-view" => Some(Self::CrossView),
            "relational" => Some(Self::Relational),
            "strategic" => Some(Self::Strategic),
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
            ViewKind::Relational,
            ViewKind::Strategic,
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
        // Requirements §4.1: total budget across the five primary
        // blocks is 850 tokens; cross-view adds at most 100 more.
        let primary = ViewKind::Personal.default_budget_tokens()
            + ViewKind::Conversational.default_budget_tokens()
            + ViewKind::Institutional.default_budget_tokens()
            + ViewKind::Relational.default_budget_tokens()
            + ViewKind::Strategic.default_budget_tokens();
        assert_eq!(primary, 850);
        assert_eq!(ViewKind::CrossView.default_budget_tokens(), 100);
    }

    #[test]
    fn synthetic_views_declare_no_own_index() {
        assert!(ViewKind::Personal.has_own_index());
        assert!(ViewKind::Conversational.has_own_index());
        assert!(ViewKind::Institutional.has_own_index());
        assert!(!ViewKind::CrossView.has_own_index());
        assert!(!ViewKind::Relational.has_own_index());
        assert!(!ViewKind::Strategic.has_own_index());
    }
}
