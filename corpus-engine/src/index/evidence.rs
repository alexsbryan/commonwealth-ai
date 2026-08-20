// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`Evidence`] — what corpus-engine publishes when it is asked what it knows.
//!
//! Minted 2026-08-20 for noun-convergence rung `nc-4-evidence`.
//!
//! # The load-bearing property is the door, not the struct
//!
//! Four fields is the easy half. The half that does work is that **nothing
//! outside this crate can construct one**: every field is private, there is no
//! `pub fn new`, and [`Evidence::acquired`] — the only constructor — is
//! `pub(crate)`. So the only way a caller in `sovereign` or `commonwealth`
//! obtains an `Evidence` is to have been handed one by an acquisition door,
//! and every door must supply an [`Origin`] and a [`Custody`] BY VALUE.
//!
//! That is the difference between this type and the [`ScoredChunk`] it is
//! drawn from. `ScoredChunk` has nine public fields and is a mutable
//! accumulator: measured on this tree 2026-08-20, sovereign reassigns `score`
//! at twelve production sites, overwrites `content`/`title`/`url` at six, and
//! calls `metadata.insert` after construction at sixteen. It is also
//! MANUFACTURED outside the engine — nine production sites in seven
//! `sovereign-core` files build one from something that never came out of a
//! corpus index. None of that is reachable here.
//!
//! [`ScoredChunk`]: crate::types::ScoredChunk
//!
//! # Why there is no `Deserialize`
//!
//! `Deserialize` is a constructor. Deriving it would put a public door back on
//! the type — `serde_json::from_str::<Evidence>("…")` would mint one with any
//! origin and any custody the caller cared to type — and that is precisely the
//! invariant this rung exists to make structural (ARCH §7, principle 10).
//!
//! This costs nothing, which was checked rather than assumed: `ScoredChunk`
//! derives `Deserialize` and the derive is VESTIGIAL. There is no production
//! deserialize of it anywhere in the repo; the mesh wire type is OICP
//! `KnowledgeResult` and the conversion is field-by-field at
//! `commonwealth-api/src/routes_knowledge.rs:200`, which drops `metadata` and
//! `vector_distance` outright. `commonwealth-api/src/server.rs:142` carries a
//! comment asserting "no `corpus_engine` types on the wire". `Serialize` IS
//! derived — it is not a constructor, and the glassbox JSON payloads want it.
//!
//! # What this closes
//!
//! `ScoredChunk` carries provenance in `metadata: HashMap<String, String>`,
//! and two of the keys in that bag decide whether a claim may be made:
//!
//! - `metadata["custody"]` — read at `sovereign-core/src/runtime/grounding/
//!   mod.rs:459` through `Custody::parse_wire`, and it sets the egress floor.
//! - `metadata["source"] == "raptor"` — compared at `grounding/mod.rs:483` to
//!   decide `EvidenceSource::Summary` vs `Leaf`, i.e. **whether a chunk may
//!   ground a claim verbatim is a string compare on an untyped map**, written
//!   at one site and read at fifteen with no type in between.
//!
//! Here both are fields: [`Custody`] is an enum whose `Unknown` variant
//! refuses rather than defaults, and the summary/leaf question is
//! [`Grain::may_be_quoted`] on the [`Origin`]. Neither can be misspelled, and
//! neither can be absent.

use kernel_types::{ContentHash, Custody, Grain, Locator, Origin, Server, Source};
use serde::Serialize;

use crate::error::Result;
use crate::index::CorpusIndex;
use crate::types::ScoredChunk;

/// The legacy `metadata` key carrying a chunk's custody class. The wire
/// spelling is `sovereign-contracts`' `CUSTODY_META_KEY`; corpus-engine cannot
/// depend on that crate (it is a product domain, and the edge would run the
/// wrong way), so the constant is restated here and pinned by a test rather
/// than left as a bare literal at the read site.
const LEGACY_CUSTODY_KEY: &str = "custody";

/// The legacy `metadata` key carrying a chunk's acquisition tag.
const LEGACY_SOURCE_KEY: &str = "source";

/// The `metadata["source"]` value marking a RAPTOR rollup — model-authored
/// prose ABOUT source text, which may orient retrieval but may not be quoted.
/// Today this literal is compared at three sites with no constant behind it
/// (`grounding/mod.rs:483`, `merge_select.rs:119`).
const LEGACY_SOURCE_RAPTOR: &str = "raptor";

/// One piece of knowledge corpus-engine is prepared to stand behind, with
/// where it came from and where it stands for sharing.
///
/// Constructible only inside this crate — see the module docs. Read through
/// the accessors; there are no public fields and no setters, so an `Evidence`
/// that exists is an `Evidence` some acquisition door vouched for.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Evidence {
    content: String,
    origin: Origin,
    custody: Custody,
    score: f32,
}

impl Evidence {
    /// Mint an `Evidence` at an acquisition site. `pub(crate)` — the whole
    /// point of the rung is that this cannot be called from `sovereign` or
    /// `commonwealth` (ARCH §7).
    ///
    /// `origin` and `custody` are by value and are not `Option`: a door that
    /// cannot say where content came from cannot mint evidence for it. Where
    /// provenance genuinely cannot be determined the honest value is
    /// [`Custody::Unknown`], which refuses downstream — never a permissive
    /// default (ARCH §18.3, principle 6).
    pub(crate) fn acquired(
        content: impl Into<String>,
        origin: Origin,
        custody: Custody,
        score: f32,
    ) -> Self {
        Self {
            content: content.into(),
            origin,
            custody,
            score,
        }
    }

    /// The text itself, as acquired.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Where it came from — store, machine, and grain.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Where it stands for sharing.
    pub fn custody(&self) -> Custody {
        self.custody
    }

    /// Relevance assigned by the door that produced it. Comparable only
    /// within one result set.
    pub fn score(&self) -> f32 {
        self.score
    }

    /// May this be quoted verbatim as source text?
    ///
    /// ONE decider (ARCH §10.6) for the question `grounding/mod.rs:483`
    /// currently answers with `metadata.get("source") == Some("raptor")`. A
    /// caller asking it here cannot get the spelling wrong, and cannot forget
    /// that a summary is not a source.
    pub fn may_be_quoted(&self) -> bool {
        self.origin.grain.may_be_quoted()
    }
}

impl CorpusIndex {
    /// Retrieve evidence for a query — the published door.
    ///
    /// Wraps [`CorpusIndex::search`] and stamps each hit with an [`Origin`]
    /// and a [`Custody`] drawn from the index's OWN facts at the moment of
    /// acquisition, which is the only place those facts exist. Downstream code
    /// reads them off the type instead of re-deriving them from a string map.
    ///
    /// Rows that cannot be cited are DROPPED rather than given a fabricated
    /// locator: an empty locator "is a citation that points at nothing, which
    /// is worse than no citation because it renders as one" (`kernel_types::
    /// origin::Locator`). The count is traced, never silently swallowed.
    pub async fn retrieve(
        &self,
        query_embedding: &[f32],
        query_text: &str,
        limit: usize,
    ) -> Result<Vec<Evidence>> {
        let hits = self.search(query_embedding, query_text, limit).await?;
        let found = hits.len();
        let mut out = Vec::with_capacity(found);
        let mut uncitable = 0usize;
        let mut unstamped = 0usize;
        for hit in hits {
            match evidence_from_hit(&hit) {
                Some(ev) => {
                    if ev.custody() == Custody::Unknown {
                        unstamped += 1;
                    }
                    out.push(ev);
                }
                None => uncitable += 1,
            }
        }
        tracing::debug!(
            corpus = %self.corpus_id(),
            found,
            published = out.len(),
            uncitable,
            unstamped,
            "retrieve: published evidence"
        );
        Ok(out)
    }
}

/// Convert one search hit into published evidence, or `None` when the hit
/// carries no citable locator.
///
/// The ONE place the legacy `metadata` bag is read on the retrieval path.
/// Every provenance fact it holds becomes a typed field here and is never
/// re-parsed downstream.
fn evidence_from_hit(hit: &ScoredChunk) -> Option<Evidence> {
    let corpus = kernel_types::CorpusId::new(hit.corpus_id.clone())?;
    let locator = locator_of(hit)?;

    // The citable unit at retrieval grain is the CHUNK, and a chunk is its
    // bytes (ARCH §7.5). corpus-engine does not hold the whole document at
    // search time, so `document` carries the content address of the retrieved
    // chunk rather than of the file it was cut from. Named rather than
    // silently substituted (principle 6); the index does persist a
    // `content_hash` column and surfacing it is the honest upgrade, but that
    // means widening `search`'s projection, which is a separate change.
    let document = ContentHash::of(hit.content.as_bytes());

    // Provenance cannot be determined => Unknown, which refuses. Never
    // `Personal` or `PublicWeb` by default: an unstamped chunk and a chunk
    // stamped as estate material must not be the same value.
    let custody = hit
        .metadata
        .get(LEGACY_CUSTODY_KEY)
        .and_then(|v| Custody::parse_wire(v))
        .unwrap_or(Custody::Unknown);

    let grain = match hit.metadata.get(LEGACY_SOURCE_KEY).map(String::as_str) {
        Some(LEGACY_SOURCE_RAPTOR) => Grain::Summary,
        _ => Grain::Leaf,
    };

    Some(Evidence::acquired(
        hit.content.clone(),
        Origin {
            source: Source::Corpus {
                corpus,
                document,
                locator,
            },
            // `search` runs against a local LanceDB table. A peer-served hit
            // arrives through the mesh client and is not this door's output,
            // so `Local` here is a fact, not an assumption.
            served_by: Server::Local,
            grain,
        },
        custody,
        hit.score,
    ))
}

/// The citation handle for a hit: the LanceDB row id when present, else the
/// source document id. `None` when neither exists — such a row cannot be
/// cited and does not become evidence.
fn locator_of(hit: &ScoredChunk) -> Option<Locator> {
    if let Some(id) = hit.chunk_id {
        return Locator::new(format!("chunk:{id}"));
    }
    hit.source_doc_id
        .as_deref()
        .and_then(|d| Locator::new(format!("doc:{d}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn hit(meta: &[(&str, &str)]) -> ScoredChunk {
        ScoredChunk {
            content: "the text".into(),
            title: Some("A title".into()),
            url: None,
            corpus_id: "wikipedia".into(),
            score: 0.5,
            metadata: meta
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
            chunk_id: Some(42),
            source_doc_id: None,
            vector_distance: None,
        }
    }

    #[test]
    fn an_unstamped_chunk_is_unknown_and_never_a_permissive_default() {
        // The defect this replaces: absence read as agreement. An unstamped
        // chunk must not arrive as `personal` or `public-web`.
        let ev = evidence_from_hit(&hit(&[])).unwrap();
        assert_eq!(ev.custody(), Custody::Unknown);
        assert!(!ev.custody().is_released_class());
    }

    #[test]
    fn the_custody_stamp_is_read_through_the_one_parser() {
        let ev = evidence_from_hit(&hit(&[("custody", "personal")])).unwrap();
        assert_eq!(ev.custody(), Custody::Personal);
        // A typo is absence, not a new class — `parse_wire` is exact.
        let typo = evidence_from_hit(&hit(&[("custody", "public_web")])).unwrap();
        assert_eq!(typo.custody(), Custody::Unknown);
    }

    #[test]
    fn the_legacy_custody_key_matches_the_contract_spelling() {
        // Pinned rather than left as a bare literal: this constant restates
        // `sovereign-contracts`' CUSTODY_META_KEY across a crate boundary
        // corpus-engine may not take a dependency on.
        assert_eq!(LEGACY_CUSTODY_KEY, "custody");
    }

    #[test]
    fn a_raptor_rollup_is_a_summary_and_may_not_be_quoted() {
        // Replaces `metadata.get("source") == Some("raptor")` compared inside
        // the grounding gate.
        let summary = evidence_from_hit(&hit(&[("source", "raptor")])).unwrap();
        assert_eq!(summary.origin().grain, Grain::Summary);
        assert!(!summary.may_be_quoted());

        let leaf = evidence_from_hit(&hit(&[])).unwrap();
        assert_eq!(leaf.origin().grain, Grain::Leaf);
        assert!(leaf.may_be_quoted());
    }

    #[test]
    fn a_hit_with_no_citable_handle_does_not_become_evidence() {
        let mut h = hit(&[]);
        h.chunk_id = None;
        h.source_doc_id = None;
        assert!(evidence_from_hit(&h).is_none());
    }

    #[test]
    fn the_row_id_is_preferred_and_the_doc_id_is_the_fallback() {
        let ev = evidence_from_hit(&hit(&[])).unwrap();
        let Source::Corpus { locator, .. } = &ev.origin().source else {
            panic!("a corpus door must mint a corpus source");
        };
        assert_eq!(locator.as_str(), "chunk:42");

        let mut h = hit(&[]);
        h.chunk_id = None;
        h.source_doc_id = Some("enwiki-0007".into());
        let ev = evidence_from_hit(&h).unwrap();
        let Source::Corpus { locator, .. } = &ev.origin().source else {
            panic!("a corpus door must mint a corpus source");
        };
        assert_eq!(locator.as_str(), "doc:enwiki-0007");
    }

    #[test]
    fn an_empty_corpus_id_cannot_mint_evidence() {
        let mut h = hit(&[]);
        h.corpus_id = String::new();
        assert!(evidence_from_hit(&h).is_none());
    }

    #[test]
    fn the_corpus_door_always_stamps_local() {
        // A peer-served hit does not come out of this door, so `Local` is a
        // fact rather than a default. Pinned so a future mesh path cannot
        // quietly reuse this constructor.
        let ev = evidence_from_hit(&hit(&[])).unwrap();
        assert!(!ev.origin().served_by.is_peer());
    }

    #[test]
    fn content_and_score_survive_the_crossing() {
        let ev = evidence_from_hit(&hit(&[])).unwrap();
        assert_eq!(ev.content(), "the text");
        assert_eq!(ev.score(), 0.5);
    }
}
