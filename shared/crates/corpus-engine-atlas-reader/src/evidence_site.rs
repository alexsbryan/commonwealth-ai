// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where an atom's evidence passage lives, and how to select it.
//!
//! # The bug this type exists to make unsayable
//!
//! An atlas has TWO identities, and until 2026-09-03 the code carried them in
//! one `String`:
//!
//! 1. **Its own id** (`sep-freewill`) — an EXTRACTION address: the directory
//!    the atlas was written to.
//! 2. **The corpus holding the chunks it cites** (`sep`) — a RETRIEVAL
//!    address: the index you actually search.
//!
//! They are equal for a whole-corpus atlas (wikipedia, enron) and they DIVERGE
//! for SEP's 1,770 per-article atlases. `ChunkRequest.corpus_id` carried #1,
//! and `apply_atlas_grounding` scoped its FTS fetch with it as though it were
//! #2 — so every SEP fetch searched `sep-freewill`, an index that holds an
//! `atlas/` directory and no `chunks.lance` at all. Atlas grounding therefore
//! contributed exactly **zero** chunks to every SEP answer, and did so
//! silently: a zero yield is also what "nothing was relevant" looks like.
//!
//! Wikipedia was unaffected, because its two ids are the same string. That is
//! why the one measured runtime number came from the case that happens to
//! work, and why no bench caught it (note `81feaf78`).
//!
//! # Why a type rather than a fixed call site
//!
//! The point fix — swap one field at one call site — leaves the class alive.
//! Two independent axes were both being recovered by PARSING, from data whose
//! shape the producer knew and threw away:
//!
//! | Axis | Recovered by | Correct for | Wrong for |
//! |---|---|---|---|
//! | which corpus holds the chunk | `article_slug` happening to equal the corpus id | self-hosted atlases | per-article atlases |
//! | how to select the chunk | `chunk_id.parse::<u64>()` | numeric row ids | section slugs |
//!
//! Neither branch read a fact that STATED the layout; each recovered it by
//! accident, and each accident held for exactly one of the two layouts. So the
//! two axes become two values that the producer — the only party that knows —
//! mints once, and the consumer executes without interpreting.
//!
//! A consumer cannot ask an [`EvidenceSite`] for "the corpus id" and get an
//! atlas id back, because the atlas id is not reachable from here at all: the
//! only corpus this type will yield is the one that holds chunks.
//!
//! # Not `kernel_types::Locator`
//!
//! Adjacent noun, different job, checked before minting (ARCH §19). `Locator`
//! is an opaque `String` handle (`"chunk:42"`) carried by `Origin` for
//! CITATION provenance. It cannot answer "which corpus do I search" or "how do
//! I select the chunk", which is this type's entire purpose. Renamed apart
//! rather than converged.

use std::fmt;

use kernel_types::CorpusId;
use serde::{Deserialize, Serialize};

/// Atlas-id prefixes that mark a per-article child atlas, mapped to the parent
/// corpus that holds the chunks.
///
/// The COMPAT path, and deliberately a table rather than a split-on-dash: an
/// id like `wikipedia-newsworthy` is its own corpus, not an article of
/// `wikipedia`, and a generic rule would mis-parent it. New atlases declare
/// their site in `_summary.json` (see [`EvidenceSite::declared_or_derived`])
/// and never reach this table; it exists for the 1,770 SEP atlases already on
/// disk, which predate the declaration.
const PARENTED_PREFIXES: &[(&str, &str)] = &[("sep-", "sep")];

/// Where the chunks an atlas cites actually live.
///
/// Minted by the atlas that owns the atom — it alone knows its own layout —
/// and executed by the retrieval side. See the module doc for why this is a
/// type and not a fixed call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceSite {
    /// The atlas and the chunks it cites share one index: the atlas id IS the
    /// chunk corpus id. Wikipedia, enron, conversations, vaults.
    SelfHosted { corpus: CorpusId },
    /// The atlas is a per-article child; its chunks live in a PARENT corpus
    /// and are addressed within it by article title. SEP: 1,770 atlases named
    /// `sep-<article>`, all citing chunks in the single `sep` index.
    ArticleOfParent { parent: CorpusId, article: String },
}

impl EvidenceSite {
    /// The corpus to search. There is no other answer to this question and no
    /// caller may compute it themselves — ARCH §10.6, one decider.
    pub fn chunk_corpus(&self) -> &CorpusId {
        match self {
            EvidenceSite::SelfHosted { corpus } => corpus,
            EvidenceSite::ArticleOfParent { parent, .. } => parent,
        }
    }

    /// The article a fetch must be filtered to, or `None` when the atlas spans
    /// its whole corpus and no title filter applies.
    ///
    /// `None` is load-bearing: the old code filtered `hit.title == article_slug`
    /// unconditionally, and for a self-hosted atlas `article_slug` was the
    /// CORPUS id, so the filter could only ever match an article that happened
    /// to be named after its corpus.
    pub fn article(&self) -> Option<&str> {
        match self {
            EvidenceSite::SelfHosted { .. } => None,
            EvidenceSite::ArticleOfParent { article, .. } => Some(article),
        }
    }

    /// The human label for this site — what the old `AtlasGraph.article_slug`
    /// field rendered. Display and grouping only; never a corpus to search.
    pub fn label(&self) -> &str {
        match self {
            EvidenceSite::SelfHosted { corpus } => corpus.as_str(),
            EvidenceSite::ArticleOfParent { article, .. } => article,
        }
    }

    /// Infer the site from an atlas id alone — the COMPAT path for atlases
    /// written before the site was declared. Prefer
    /// [`Self::declared_or_derived`], which reads the declaration when it is
    /// present.
    ///
    /// An id whose prefix is not in [`PARENTED_PREFIXES`] is self-hosted, which
    /// is the correct reading for every non-SEP atlas in the tree.
    pub fn derive(atlas_corpus_id: &str) -> Self {
        for (prefix, parent) in PARENTED_PREFIXES {
            if let Some(article) = atlas_corpus_id.strip_prefix(prefix) {
                // An empty remainder means the id IS the bare prefix
                // (`"sep-"`); that is not an article, so fall through to
                // self-hosted rather than minting an unaddressable site.
                if !article.is_empty() {
                    if let Some(parent) = CorpusId::new(*parent) {
                        return EvidenceSite::ArticleOfParent {
                            parent,
                            article: article.to_string(),
                        };
                    }
                }
            }
        }
        match CorpusId::new(atlas_corpus_id) {
            Some(corpus) => EvidenceSite::SelfHosted { corpus },
            // `CorpusId` refuses an empty id on purpose (principle 6). This
            // arm undoes that refusal, so it says so at ERROR rather than
            // defaulting quietly — a NAMED substitution, which is what
            // ARCH §18.3 allows in place of a refusal. Unreachable in
            // practice (an atlas id is a directory name), and "unreachable"
            // is exactly the claim §18.3 exists to distrust.
            None => {
                tracing::error!(
                    "evidence site: atlas has an EMPTY id; substituting an \
                     unaddressable sentinel corpus. This atlas will ground \
                     nothing and the fetch ledger will report every candidate \
                     dropped. Fix the caller that passed an unnamed atlas."
                );
                EvidenceSite::SelfHosted {
                    corpus: CorpusId::new("<unnamed-atlas>").expect("literal is non-empty"),
                }
            }
        }
    }

    /// The site an atlas declared in its `_summary.json`, falling back to
    /// [`Self::derive`] when the atlas predates the declaration.
    ///
    /// Declared data beats a convention re-derived by every reader (ARCH §6),
    /// but not at the cost of a mandatory backfill across 1,770 directories —
    /// so both paths exist and the fallback is traced, never silent.
    pub fn declared_or_derived(atlas_corpus_id: &str, declared: Option<EvidenceSite>) -> Self {
        match declared {
            Some(site) => site,
            None => {
                let site = Self::derive(atlas_corpus_id);
                tracing::debug!(
                    atlas = atlas_corpus_id,
                    site = %site,
                    "evidence site: no declaration on disk, derived from the atlas id"
                );
                site
            }
        }
    }
}

impl fmt::Display for EvidenceSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvidenceSite::SelfHosted { corpus } => write!(f, "{corpus}"),
            EvidenceSite::ArticleOfParent { parent, article } => write!(f, "{parent}#{article}"),
        }
    }
}

/// How to pick one chunk out of the corpus an [`EvidenceSite`] names.
///
/// The second axis. Independent of the site: a per-article atlas could carry
/// row ids and a self-hosted one could carry sections. Today's corpora happen
/// to pair `SelfHosted`+`RowId` (wikipedia) and `ArticleOfParent`+`Section`
/// (SEP), and the old code collapsed that coincidence into a
/// `chunk_id.parse::<u64>()` branch that read as a shape test but was really a
/// layout test.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChunkSelector {
    /// A LanceDB row id — a direct key, no search. Wikipedia, conversations,
    /// vaults.
    RowId(u64),
    /// A section slug (`sec_0001`) within an article. Resolved by search
    /// scoped to the site's corpus and filtered to its article.
    Section(String),
}

impl ChunkSelector {
    /// Read a selector off an atom's `first_appearance.chunk_id`.
    ///
    /// Still a parse, because that is what is on disk in every atlas written
    /// to date — but it happens ONCE, at the producer, and its result is
    /// carried as a value. The consumer no longer re-derives it, which is the
    /// half that was wrong.
    pub fn parse(chunk_id: &str) -> Self {
        match chunk_id.trim().parse::<u64>() {
            Ok(row) => ChunkSelector::RowId(row),
            Err(_) => ChunkSelector::Section(chunk_id.trim().to_string()),
        }
    }

    /// The raw on-disk spelling — for logs and for the FTS query text.
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        match self {
            ChunkSelector::RowId(row) => std::borrow::Cow::Owned(row.to_string()),
            ChunkSelector::Section(s) => std::borrow::Cow::Borrowed(s),
        }
    }
}

impl fmt::Display for ChunkSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cid(s: &str) -> CorpusId {
        CorpusId::new(s).unwrap()
    }

    #[test]
    fn sep_article_atlas_parents_to_the_chunk_corpus() {
        // The regression this module exists for: the atlas is `sep-freewill`,
        // the chunks are in `sep`, and `chunk_corpus()` must say `sep`.
        let site = EvidenceSite::derive("sep-freewill");
        assert_eq!(
            site,
            EvidenceSite::ArticleOfParent {
                parent: cid("sep"),
                article: "freewill".into()
            }
        );
        assert_eq!(site.chunk_corpus(), &cid("sep"));
        assert_eq!(site.article(), Some("freewill"));
    }

    #[test]
    fn self_hosted_atlas_is_its_own_chunk_corpus() {
        let site = EvidenceSite::derive("wikipedia");
        assert_eq!(site.chunk_corpus(), &cid("wikipedia"));
        // No title filter applies — the old code compared `hit.title` against
        // the CORPUS id here, which could only match by coincidence.
        assert_eq!(site.article(), None);
    }

    #[test]
    fn a_dashed_id_that_is_not_a_known_parent_stays_self_hosted() {
        // `wikipedia-newsworthy` is its own corpus, not an article of
        // `wikipedia`. A split-on-first-dash rule would mis-parent it.
        let site = EvidenceSite::derive("wikipedia-newsworthy");
        assert_eq!(site.chunk_corpus(), &cid("wikipedia-newsworthy"));
        assert_eq!(site.article(), None);
    }

    #[test]
    fn bare_prefix_is_not_an_article() {
        let site = EvidenceSite::derive("sep-");
        assert_eq!(site.chunk_corpus(), &cid("sep-"));
        assert_eq!(site.article(), None);
    }

    #[test]
    fn declaration_wins_over_the_prefix_table() {
        // An atlas whose id looks parented but which declares otherwise is
        // read as it declares — the whole point of moving to declared data.
        let declared = EvidenceSite::SelfHosted {
            corpus: cid("sep-freewill"),
        };
        let site = EvidenceSite::declared_or_derived("sep-freewill", Some(declared.clone()));
        assert_eq!(site, declared);
        assert_eq!(site.chunk_corpus(), &cid("sep-freewill"));
    }

    #[test]
    fn absent_declaration_falls_back_to_derivation() {
        let site = EvidenceSite::declared_or_derived("sep-freewill", None);
        assert_eq!(site.chunk_corpus(), &cid("sep"));
    }

    #[test]
    fn selector_distinguishes_row_ids_from_section_slugs() {
        // Wikipedia atoms: 400/400 numeric. SEP atoms: 0/214.
        assert_eq!(ChunkSelector::parse("594413"), ChunkSelector::RowId(594413));
        assert_eq!(
            ChunkSelector::parse("sec_0001"),
            ChunkSelector::Section("sec_0001".into())
        );
    }

    #[test]
    fn site_and_selector_are_independent_axes() {
        // Neither combination is expressible in the old encoding, and both are
        // legitimate: the layout and the addressing scheme are orthogonal.
        let parented_rowid = (
            EvidenceSite::derive("sep-freewill"),
            ChunkSelector::parse("42"),
        );
        let selfhosted_section = (
            EvidenceSite::derive("wikipedia"),
            ChunkSelector::parse("sec_0007"),
        );
        assert_eq!(parented_rowid.0.chunk_corpus(), &cid("sep"));
        assert_eq!(parented_rowid.1, ChunkSelector::RowId(42));
        assert_eq!(selfhosted_section.0.chunk_corpus(), &cid("wikipedia"));
        assert_eq!(
            selfhosted_section.1,
            ChunkSelector::Section("sec_0007".into())
        );
    }

    #[test]
    fn display_is_unambiguous_between_the_two_shapes() {
        assert_eq!(EvidenceSite::derive("wikipedia").to_string(), "wikipedia");
        assert_eq!(
            EvidenceSite::derive("sep-freewill").to_string(),
            "sep#freewill"
        );
    }
}
