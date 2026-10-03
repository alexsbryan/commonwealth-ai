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
//! - `metadata["custody"]` — set the egress floor, through
//!   `Custody::parse_wire` at the gate's evidence builder.
//! - `metadata["source"] == "raptor"` — decided `Summary` vs `Leaf`, i.e.
//!   **whether a chunk may ground a claim verbatim was a string compare on an
//!   untyped map**, written at one site and read at several with no type in
//!   between.
//!
//! Here both are fields: [`Custody`] is an enum whose `Unknown` variant
//! refuses rather than defaults, and the summary/leaf question is
//! [`Grain::may_be_quoted`] on the [`Origin`]. Neither can be misspelled, and
//! neither can be absent.
//!
//! **Both keys are gone from the retrieval path as of 2026-08-26**
//! (TOPOLOGY §10 rung 9.1, second half). [`ChunkProvenance`] carries the two
//! facts on `ScoredChunk` itself, the five sites that read them read the
//! typed stamp, and `CUSTODY_META_KEY` has no production writer left. The
//! paragraph above is kept in the past tense because it is the argument for
//! the shape, not a description of the tree.

use kernel_types::{ContentHash, CorpusId, Custody, Locator, Origin, Seal, Server, Source};
use serde::Serialize;

use crate::error::Result;
use crate::index::CorpusIndex;
use crate::types::ScoredChunk;

// The three `LEGACY_*` metadata-key constants that used to live here moved to
// `index/provenance.rs` on 2026-08-26 (TOPOLOGY §10 rung 9.1) along with the
// parse that read them. They were dead the moment `evidence_from_hit` started
// reading the typed stamp, and a dead constant beside a live one is how a
// second spelling gets born (ARCH §10.6).

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

/// A sealed body of evidence for one turn — what `retrieve` hands out.
///
/// **Two seals, both carried**, which is the totality
/// `quality/CONCEPTS.toml` states for this noun: the CHUNK SET composition
/// reads, and the CORPUS SCOPE retrieval was bound to. The 2026-08-17
/// verification found neither existed — the grounding gate received a
/// filtered, reordered projection under two env vars, and the re-search
/// "seal" was a list of corpus ids RECONSTRUCTED from the chunks themselves
/// when none was passed. A scope derived from its own contents cannot
/// constrain anything, which is why [`EvidenceSet::scope`] is an input to the
/// seal here and never computed from `members`.
///
/// Constructible only inside this crate, for the same reason [`Evidence`] is:
/// a set anyone can assemble is not a seal. It is the [`Seal`] implementor the
/// kernel's [`kernel_types::Citation`] requires, so a citation minted against
/// it cannot quote what this set does not hold.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvidenceSet {
    members: Vec<Evidence>,
    scope: Vec<CorpusId>,
}

impl EvidenceSet {
    /// Seal a retrieval's output against the scope it was bound to.
    ///
    /// `pub(crate)` — the seal is minted at the acquisition door and nowhere
    /// else. `scope` is what retrieval was ALLOWED to search, which is a fact
    /// only the door knows; deriving it from `members` would reconstruct the
    /// defect this type replaces.
    pub(crate) fn sealed(members: Vec<Evidence>, scope: Vec<CorpusId>) -> Self {
        Self { members, scope }
    }

    /// The chunk set — what composition may read and what support is judged
    /// against.
    pub fn members(&self) -> &[Evidence] {
        &self.members
    }

    /// The corpus scope retrieval was bound to. Verification may search
    /// WITHIN this; it may never widen it.
    pub fn scope(&self) -> &[CorpusId] {
        &self.scope
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}

impl Seal for EvidenceSet {
    /// Where `quote` came from, if one sealed member contains it as a
    /// contiguous verbatim run.
    ///
    /// Substring containment, deliberately: "the quoted text appears in the
    /// cited chunk" is the invariant `sovereign-meshapp` states in prose, and
    /// a fuzzier match would make a citation that points somewhere plausible
    /// rather than somewhere true. A quote spanning two chunks matches
    /// neither and produces no citation — refusal over guess, the same rule
    /// `locator_of` already follows.
    ///
    /// First match wins. Members arrive in relevance order from `retrieve`,
    /// so the first is the highest-scoring chunk containing the passage.
    fn locate(&self, quote: &str) -> Option<(&Origin, Custody)> {
        self.members
            .iter()
            .find(|e| e.content().contains(quote))
            .map(|e| (e.origin(), e.custody()))
    }

    fn sealed_len(&self) -> usize {
        self.members.len()
    }
}

impl CorpusIndex {
    /// Retrieve evidence for a query — the published door, and the only
    /// producer of [`Evidence`].
    ///
    /// Returns a SEALED set (rung nc-11-answer): the chunk set plus the corpus
    /// scope it was bound to. A caller holding one can mint a
    /// [`kernel_types::Citation`] against it and cannot mint one against
    /// anything else.
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
    ) -> Result<EvidenceSet> {
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
        // The scope is this index's own corpus — `search` runs against one
        // LanceDB table, so that is a fact the door knows, not a list
        // reconstructed from what came back.
        let scope: Vec<CorpusId> = CorpusId::new(self.corpus_id()).into_iter().collect();
        tracing::debug!(
            corpus = %self.corpus_id(),
            found,
            published = out.len(),
            uncitable,
            unstamped,
            scope = scope.len(),
            "retrieve: sealed evidence"
        );
        Ok(EvidenceSet::sealed(out, scope))
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

    // Read off the TYPED stamp the acquisition door already applied, not
    // re-parsed from the metadata bag (TOPOLOGY §10 rung 9.1). Behaviour is
    // identical by construction — `search` computes the stamp from this same
    // bag at the moment of acquisition — and what changes is that the parse
    // happens once instead of here and at fifteen other sites (ARCH §10.6).
    //
    // A chunk that never passed a door is `Manufactured`, which yields
    // `Custody::Unknown` — it refuses. It reaches this function only if some
    // caller hands a manufactured chunk to an evidence builder; the honest
    // reading of one is "nothing vouched for this", and Unknown is exactly
    // that (ARCH §18.3). Grain comes off the same stamp for both arms, so a
    // manufactured rollup is a summary here too rather than silently a leaf.
    let custody = hit.provenance.custody();
    let grain = hit.provenance.grain();

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
    use kernel_types::Grain;
    use std::collections::HashMap;

    fn hit(meta: &[(&str, &str)]) -> ScoredChunk {
        let md: HashMap<String, String> = meta
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        ScoredChunk {
            content: "the text".into(),
            title: Some("A title".into()),
            url: None,
            corpus_id: "wikipedia".into(),
            score: 0.5,
            metadata: md.clone(),
            chunk_id: Some(42),
            source_doc_id: None,
            vector_distance: None,
            // Stamped the way `CorpusIndex::search` stamps it, from this same
            // bag. That is the point of the fixture: these tests used to
            // exercise a parse inside `evidence_from_hit`, and since rung 9.1
            // the parse lives at the acquisition door, so the fixture has to
            // BE a door to keep testing the same property.
            provenance: crate::index::ChunkProvenance::Acquired(
                crate::index::Acquisition::stamped(
                    "wikipedia",
                    crate::index::custody_of(&md),
                    crate::index::grain_of(&md),
                ),
            ),
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

    fn sealed(meta: &[(&str, &str)]) -> EvidenceSet {
        let ev = evidence_from_hit(&hit(meta)).unwrap();
        EvidenceSet::sealed(vec![ev], vec![CorpusId::new("wikipedia").unwrap()])
    }

    #[test]
    fn a_seal_carries_both_the_chunk_set_and_the_corpus_scope() {
        // The 2026-08-17 verification found neither seal existed. Both are
        // fields here, and the scope is an INPUT — never derived from the
        // members, which is the defect the type replaces.
        let s = sealed(&[]);
        assert_eq!(s.len(), 1);
        assert!(!s.is_empty());
        assert_eq!(s.scope(), &[CorpusId::new("wikipedia").unwrap()]);
        assert_eq!(s.members()[0].content(), "the text");
    }

    #[test]
    fn a_citation_can_only_be_minted_from_what_the_seal_holds() {
        // The integration the rung exists for: corpus-engine seals, the
        // kernel's one door mints, and the prose invariant sovereign-meshapp
        // states at wrapped.rs:182 is now the constructor.
        let s = sealed(&[]);
        let c = kernel_types::Citation::pointing_into(&s, "the text").unwrap();
        assert_eq!(c.quote(), "the text");
        assert_eq!(c.custody(), Custody::Unknown);

        // A passage the seal does not hold cannot become a citation, however
        // plausible it looks.
        let err = kernel_types::Citation::pointing_into(&s, "the txet").unwrap_err();
        assert!(err.to_string().contains("does not contain"));
    }

    #[test]
    fn a_raptor_rollup_seals_but_cannot_be_quoted() {
        // A summary is IN the seal — it may orient retrieval — and still
        // refuses at the citation door, through `Grain::may_be_quoted`.
        let s = sealed(&[("source", "raptor")]);
        assert_eq!(s.len(), 1);
        let err = kernel_types::Citation::pointing_into(&s, "the text").unwrap_err();
        assert!(
            matches!(err, kernel_types::Refused::NotQuotable { .. }),
            "{err}"
        );
    }

    #[test]
    fn an_empty_seal_reports_its_own_size_in_the_refusal() {
        let empty = EvidenceSet::sealed(vec![], vec![]);
        let err = kernel_types::Citation::pointing_into(&empty, "anything").unwrap_err();
        assert_eq!(
            err,
            kernel_types::Refused::NotInSeal {
                quote: "anything".into(),
                sealed_len: 0
            }
        );
    }

    #[test]
    fn content_and_score_survive_the_crossing() {
        let ev = evidence_from_hit(&hit(&[])).unwrap();
        assert_eq!(ev.content(), "the text");
        assert_eq!(ev.score(), 0.5);
    }
}
