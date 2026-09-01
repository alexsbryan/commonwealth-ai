// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`SealedEvidence`] — the gate's leaf evidence view, as a [`Seal`].
//!
//! Minted 2026-08-21 for noun-convergence rung `nc-20-turn-adoption`. This is
//! the adapter that lets the live grounding gate mint a
//! [`kernel_types::Citation`] instead of hand-filling a struct.
//!
//! # Why the adapter exists rather than an `impl Seal for EvidenceContext`
//!
//! [`Seal::locate`] returns `&Origin`, so the seal has to OWN an [`Origin`] per
//! row. `EvidenceContext` does not hold one: it holds six parallel arrays
//! (`chunks`, `chunk_sources`, `chunk_custodies`, `chunk_targets`,
//! `chunk_locators`, `chunk_urls`) whose alignment is the invariant
//! `GateEvidenceParts` exists to protect. Folding those into `Vec<Origin>` at
//! the point of use is exactly the conversion this rung is for — one row, one
//! provenance, no index arithmetic downstream.
//!
//! It is built from the LEAF view (`gate_answer_inner`'s `chunks` / `targets` /
//! `custodies` / `grains`), not from the raw context, because that is the view
//! the ladder is allowed to quote from: a passage living only inside a derived
//! RAPTOR summary must not ground a release, and the filter that enforces that
//! runs upstream. The grain rides along rather than being assumed, so a later
//! change to that filter cannot make this seal quietly lie.
//!
//! # A row without a `(corpus, chunk)` handle is not in the seal
//!
//! That is not a new rule. `grounding/mod.rs`'s released-citation fold already
//! drops a quote whose `target` is `None` — a citation row is a promise that
//! clicking it opens the passage quoted. Here the same rule becomes the seal's
//! MEMBERSHIP: a chunk with no handle cannot supply a [`Source::Corpus`], so
//! it is not a member, and a quote that matches only such a chunk comes back
//! as [`kernel_types::Refused::NotInSeal`] — a named value the caller traces,
//! rather than a `None` that vanishes into a `filter_map`.
//!
//! [`engine_attribution`] lives here for the same reason: it is the other half
//! of the kernel vocabulary the release needs, and the gate is the only caller.

use crate::oicp::Speed;
use kernel_types::{
    Attribution, ContentHash, CorpusId, Custody, Grain, Locator, Origin, Seal, Server, Source,
};

use crate::traits::InferenceProvider;
use crate::types::CitationTarget;

/// Which engine computed the text this gate is about to release.
///
/// Every field is read from something the runtime already knows, or reported
/// absent — nothing here is invented (principle 6):
///
/// * `model` — [`InferenceProvider::model_id_for`], the provider's own answer
///   for the slot this turn was routed to. Providers that cannot say return
///   the string `"unknown"`, which is the trait's own absence marker and is
///   passed through rather than papered over.
/// * `build` — this crate's package version, which IS the engine build that
///   ran the ladder.
/// * `quantization` — parsed from the model id by
///   [`crate::models_manifest::parse_quant`], the one parser for that token.
///   `None` means the id carries no quant segment, not that the weights are
///   known to be full precision — the same reading `ModelAttribution` gives it.
/// * `host` — [`Server::Local`]. `model_id_for` is the SYNCHRONOUS accessor and
///   reports the local slot; the peer-attributed id only ever comes out of
///   `complete_stream_with_id`, so `Local` is a fact about this accessor rather
///   than an assumption about the turn.
pub(crate) fn engine_attribution(inference: &dyn InferenceProvider, speed: Speed) -> Attribution {
    let model = inference.model_id_for(speed);
    Attribution {
        quantization: crate::models_manifest::parse_quant(&model),
        model,
        build: env!("CARGO_PKG_VERSION").to_string(),
        host: Server::Local,
    }
}

/// One sealed passage: the text the ladder may quote, where it came from, and
/// where it stands for sharing.
struct SealedRow {
    text: String,
    target: CitationTarget,
    origin: Origin,
    custody: Custody,
}

/// The seal narrowed to the ONE member an upstream quote-match already
/// attributed a quote to.
///
/// This exists because there are two questions and they have two owners.
/// WHICH passage a quote came from is decided upstream by
/// `citation::locate_quote_in_chunks`, on NORMALISED text, and the section
/// heading rides that same decision. WHETHER the passage may be cited — is the
/// quote verbatim, is the grain quotable — is the kernel's door. Handing the
/// whole seal to `Citation::pointing_into` would let a raw first-match pick a
/// DIFFERENT member than the normalised match did, and the released row would
/// then carry one chunk's handle beside another chunk's heading. That is
/// precisely the failure `GroundedQuote`'s doc calls out: a citation pointing
/// at the wrong chapter is worse than one pointing nowhere.
///
/// So the chunk choice stays where it was (§10.6 — one decider), and only the
/// citing rule moves into the kernel.
struct MemberSeal<'a> {
    row: Option<&'a SealedRow>,
}

impl Seal for MemberSeal<'_> {
    fn locate(&self, quote: &str) -> Option<(&Origin, Custody)> {
        self.row
            .filter(|r| r.text.contains(quote))
            .map(|r| (&r.origin, r.custody))
    }

    /// One, or none. A refusal reading "the sealed evidence (0 member(s))"
    /// says the handle named no member of this seal; "(1 member(s))" says the
    /// member is there and does not hold the quote. Different facts, and the
    /// caller traces which.
    fn sealed_len(&self) -> usize {
        usize::from(self.row.is_some())
    }
}

/// The turn's quotable evidence, sealed — what a released
/// [`kernel_types::Citation`] must point into.
///
/// Constructed only by [`SealedEvidence::over`], which is called at the one
/// place the leaf view exists (`gate_answer_inner`). There is no `push`: a
/// seal that can grow after the draft was written is not a seal.
pub(crate) struct SealedEvidence {
    rows: Vec<SealedRow>,
    /// Chunks the leaf view held that could NOT become members, because they
    /// carry no `(corpus, chunk)` handle. Traced by the caller so "the quote
    /// is not in these 12 chunks" stays distinguishable from "4 of the 12
    /// were unciteable and never entered the seal".
    unhandled: usize,
}

impl SealedEvidence {
    /// Seal the leaf view. The four slices are index-parallel; shorter ones
    /// read as absent rather than panicking, matching
    /// [`crate::runtime::grounding::EvidenceContext::source_of`]'s
    /// conservative degradation for late-appended evidence.
    pub(crate) fn over(
        chunks: &[String],
        targets: &[Option<CitationTarget>],
        custodies: &[Option<Custody>],
        grains: &[Grain],
    ) -> Self {
        let mut rows = Vec::with_capacity(chunks.len());
        let mut unhandled = 0usize;
        for (i, text) in chunks.iter().enumerate() {
            let Some(target) = targets.get(i).and_then(Option::as_ref) else {
                unhandled += 1;
                continue;
            };
            let Some(corpus) = CorpusId::new(target.corpus_id.clone()) else {
                unhandled += 1;
                continue;
            };
            // The kernel `Locator` is the MACHINE handle — which span inside
            // the document — so the chunk id goes here. The human section
            // heading ("CHAPTER VII") is a different fact with a different
            // failure mode (it can be missing when the handle is present) and
            // it stays on `EvidenceContext::chunk_locators`, where the release
            // reads it. Two words, two concepts, adjudicated rather than
            // collapsed.
            let Some(locator) = Locator::new(target.chunk_id.to_string()) else {
                unhandled += 1;
                continue;
            };
            rows.push(SealedRow {
                text: text.clone(),
                target: target.clone(),
                origin: Origin {
                    source: Source::Corpus {
                        corpus,
                        // The citable unit at gate grain is the CHUNK, and a
                        // chunk is its bytes (ARCH §7.5). Same convention as
                        // `corpus_engine::index::evidence::evidence_from_hit`,
                        // and named there for the same reason: the gate does
                        // not hold the whole document.
                        document: ContentHash::of(text.as_bytes()),
                        locator,
                    },
                    // NAMED SUBSTITUTION (principle 6), not a default. A
                    // peer-served hit is tagged `metadata["peer"] = <name>` at
                    // `retrieval_pipeline.rs:2267`, and that name is a DISPLAY
                    // label — `Server::Peer` needs a `NodeId`, and deriving one
                    // from a name would be identity from an address (ARCH
                    // §7.5). The metadata bag does not survive
                    // `gate_evidence_with_sources` either, so at this seam the
                    // fact is absent rather than merely unformatted. Carrying a
                    // `Server` through `GateEvidenceParts` is the honest
                    // upgrade and it needs a `NodeId` on the mesh hit first.
                    served_by: Server::Local,
                    grain: grains.get(i).copied().unwrap_or(Grain::Leaf),
                },
                // Unstamped => `Unknown`, which refuses downstream. Never
                // `PublicWeb` by default: an unstamped chunk and a chunk
                // stamped as estate material must not be the same value
                // (ARCH §18.3).
                custody: custodies
                    .get(i)
                    .copied()
                    .flatten()
                    .unwrap_or(Custody::Unknown),
            });
        }
        SealedEvidence { rows, unhandled }
    }

    /// Members that entered the seal.
    pub(crate) fn len(&self) -> usize {
        self.rows.len()
    }

    /// Leaf-view chunks that could not become members for want of a
    /// `(corpus, chunk)` handle.
    pub(crate) fn unhandled(&self) -> usize {
        self.unhandled
    }

    /// Mint the citation for a quote the upstream match already attributed to
    /// `target`.
    ///
    /// The door is still [`kernel_types::Citation::pointing_into`] — this only
    /// says which member it is allowed to look in. See [`MemberSeal`] for why
    /// that narrowing is the correct division of labour rather than a
    /// convenience.
    pub(crate) fn cite(
        &self,
        target: &CitationTarget,
        quote: &str,
    ) -> Result<kernel_types::Citation, kernel_types::Refused> {
        let member = MemberSeal {
            row: self.rows.iter().find(|r| &r.target == target),
        };
        kernel_types::Citation::pointing_into(&member, quote)
    }
}

impl Seal for SealedEvidence {
    /// Substring containment against ONE member, first match wins.
    ///
    /// The same predicate `corpus_engine::EvidenceSet` answers with, and the
    /// same one the quote-first path already enforces upstream
    /// (`QuoteMatch::Exact` — a `Some(target)` on a `GroundedQuote` means the
    /// span is one contiguous run of one chunk). Members arrive in relevance
    /// order, so the first match is the highest-scoring chunk holding the
    /// passage.
    fn locate(&self, quote: &str) -> Option<(&Origin, Custody)> {
        self.rows
            .iter()
            .find(|r| r.text.contains(quote))
            .map(|r| (&r.origin, r.custody))
    }

    fn sealed_len(&self) -> usize {
        self.rows.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(corpus: &str, chunk: u64) -> Option<CitationTarget> {
        Some(CitationTarget {
            corpus_id: corpus.to_string(),
            chunk_id: chunk,
        })
    }

    #[test]
    fn a_chunk_with_a_handle_becomes_a_citable_member() {
        let seal = SealedEvidence::over(
            &["the cold lantern stood empty".to_string()],
            &[target("inns", 7)],
            &[Some(Custody::PublicWeb)],
            &[Grain::Leaf],
        );
        assert_eq!(seal.len(), 1);
        let c = kernel_types::Citation::pointing_into(&seal, "cold lantern").unwrap();
        assert_eq!(c.quote(), "cold lantern");
        assert_eq!(c.custody(), Custody::PublicWeb);
        match &c.source().source {
            Source::Corpus {
                corpus, locator, ..
            } => {
                assert_eq!(corpus.as_str(), "inns");
                // The chunk id, not the chapter heading.
                assert_eq!(locator.as_str(), "7");
            }
            other => panic!("expected a corpus origin, got {other:?}"),
        }
    }

    #[test]
    fn a_chunk_with_no_handle_is_not_a_member() {
        // The live rule the released-citation fold already applied by
        // dropping a `target: None` quote — here it is the seal's membership,
        // so the refusal is a value rather than a vanished row.
        let seal = SealedEvidence::over(
            &["synthetic evidence".to_string()],
            &[None],
            &[None],
            &[Grain::Leaf],
        );
        assert_eq!(seal.len(), 0);
        assert_eq!(seal.unhandled(), 1);
        let err = kernel_types::Citation::pointing_into(&seal, "synthetic").unwrap_err();
        assert!(
            matches!(err, kernel_types::Refused::NotInSeal { sealed_len: 0, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_blank_corpus_id_leaves_half_a_handle_out_of_the_seal() {
        let seal = SealedEvidence::over(
            &["text".to_string()],
            &[target("   ", 3)],
            &[None],
            &[Grain::Leaf],
        );
        assert_eq!(seal.len(), 0);
        assert_eq!(seal.unhandled(), 1);
    }

    /// covers: X-PR-2
    #[test]
    fn a_summary_member_may_not_be_quoted() {
        // Belt to the leaf filter's braces: the grain rides with the row, so
        // even if a summary reached this seam the kernel door refuses it.
        let seal = SealedEvidence::over(
            &["a rollup about the source".to_string()],
            &[target("c", 1)],
            &[Some(Custody::PublicWeb)],
            &[Grain::Summary],
        );
        let err = kernel_types::Citation::pointing_into(&seal, "rollup").unwrap_err();
        assert!(
            matches!(
                err,
                kernel_types::Refused::NotQuotable {
                    grain: Grain::Summary,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn an_unstamped_chunk_reads_as_unknown_not_as_public() {
        let seal = SealedEvidence::over(
            &["unstamped passage".to_string()],
            &[target("c", 1)],
            &[None],
            &[Grain::Leaf],
        );
        let c = kernel_types::Citation::pointing_into(&seal, "unstamped").unwrap();
        assert_eq!(c.custody(), Custody::Unknown);
    }

    #[test]
    fn cite_looks_only_in_the_member_the_upstream_match_chose() {
        // Both chunks hold the passage. The upstream quote-match attributed it
        // to chunk 2, so the released citation must name chunk 2 — a raw
        // first-match over the whole seal would have named chunk 1 and paired
        // it with chunk 2's chapter heading.
        let seal = SealedEvidence::over(
            &[
                "shared passage here".to_string(),
                "shared passage here".to_string(),
            ],
            &[target("c", 1), target("c", 2)],
            &[Some(Custody::PublicWeb); 2],
            &[Grain::Leaf; 2],
        );
        let chosen = CitationTarget {
            corpus_id: "c".to_string(),
            chunk_id: 2,
        };
        let c = seal.cite(&chosen, "shared passage").unwrap();
        match &c.source().source {
            Source::Corpus { locator, .. } => assert_eq!(locator.as_str(), "2"),
            other => panic!("expected a corpus origin, got {other:?}"),
        }
    }

    /// covers: GR-17
    #[test]
    fn cite_refuses_a_quote_the_chosen_member_does_not_hold() {
        let seal = SealedEvidence::over(
            &["the real passage".to_string()],
            &[target("c", 1)],
            &[Some(Custody::PublicWeb)],
            &[Grain::Leaf],
        );
        let chosen = CitationTarget {
            corpus_id: "c".to_string(),
            chunk_id: 1,
        };
        let err = seal.cite(&chosen, "a fabricated passage").unwrap_err();
        assert!(
            matches!(err, kernel_types::Refused::NotInSeal { sealed_len: 1, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn cite_refuses_a_handle_that_names_no_member() {
        let seal = SealedEvidence::over(
            &["text".to_string()],
            &[target("c", 1)],
            &[Some(Custody::PublicWeb)],
            &[Grain::Leaf],
        );
        let absent = CitationTarget {
            corpus_id: "c".to_string(),
            chunk_id: 99,
        };
        let err = seal.cite(&absent, "text").unwrap_err();
        assert!(
            matches!(err, kernel_types::Refused::NotInSeal { sealed_len: 0, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_quote_spanning_two_members_matches_neither() {
        let seal = SealedEvidence::over(
            &["first half".to_string(), "second half".to_string()],
            &[target("c", 1), target("c", 2)],
            &[Some(Custody::PublicWeb); 2],
            &[Grain::Leaf; 2],
        );
        let err =
            kernel_types::Citation::pointing_into(&seal, "first half second half").unwrap_err();
        assert!(
            matches!(err, kernel_types::Refused::NotInSeal { sealed_len: 2, .. }),
            "{err:?}"
        );
    }
}
