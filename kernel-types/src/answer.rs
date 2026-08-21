// SPDX-License-Identifier: AGPL-3.0-or-later
//! The released turn: *what the user is shown, and what it stands on.*
//!
//! Minted 2026-08-20 for noun-convergence rung `nc-11-answer`. This module
//! makes three of the five constructions `nc-thesis` declares illegal
//! un-writable rather than merely audited:
//!
//! | declared illegal construction | what refuses it |
//! |---|---|
//! | an `Answer` with no `Judgement` | private fields; the only doors take one by value |
//! | a `Citation` not pointing into a sealed `EvidenceSet` | private fields; [`Citation::pointing_into`] is the one door and it takes a [`Seal`] |
//! | a non-shareable `Evidence` in a peer-bound reply | [`PeerAnswer`] is the only thing the mesh accepts, and [`PeerAnswer::bound_for_peer`] returns a `Result` |
//!
//! # Why these live in the kernel and not in `sovereign-contracts`
//!
//! `quality/CONCEPTS.toml` writes the canonical home as
//! `sovereign_contracts::answer::Answer`. That home **cannot hold this type**,
//! and the reason is the same one that stopped `Judgement.quote: Evidence` at
//! rung 10: `sovereign-contracts` is declared in the layer-0 `contract` layer
//! of `quality/ARCH_LAYERS.toml`, *below* `corpus-engine` in the `knowledge`
//! layer. Anything naming evidence from there inverts the one edge the bottom
//! of the stack exists to forbid.
//!
//! The resolution is not to move the noun up but to notice what it is made
//! of. Every field of `Answer` is already kernel vocabulary or a primitive:
//! `text` is a `String`, `provenance` is an [`Attribution`], `judgement` is a
//! [`Judgement`], and `citations` are [`Citation`]s — a quote plus an
//! [`Origin`] plus a [`Custody`]. **`Answer` never names `Evidence`.** What it
//! names is where a quote came from, which is exactly what layer 0 is for.
//!
//! The seal itself stays where the evidence is. [`Seal`] is a trait this crate
//! defines and `corpus_engine::EvidenceSet` implements: the kernel states the
//! question a seal must answer, the crate that owns the evidence answers it,
//! and no arrow points up. Same shape as rung 10's finding, applied one level
//! out.
//!
//! # It carries work, because shape does not get adopted
//!
//! §10.3 of `quality/NOUN_CONVERGENCE.md` is a control experiment: adoption is
//! monotone in work carried and in nothing else (`Recipe` ~100%, `Report` 1%
//! of 105, `Args` 0% of 58). A four-field struct is shape. So the three jobs
//! every call site was doing by hand live here, once:
//!
//! 1. **the containment check** — [`Citation::pointing_into`] refuses a quote
//!    the seal does not hold verbatim. `sovereign-meshapp`'s `Citation`
//!    (`wrapped.rs:182`) states this in a doc comment — *"the audited
//!    invariant is that `text` appears verbatim in chunk `chunk_id`"* — and
//!    audits it nowhere. Here it is the constructor.
//! 2. **the fold** — [`Draft::release`] reduces `&[Judgement]` by ONE named
//!    policy. That policy is [`Judgement::roll_up`], which rung 10 already
//!    shipped and tested; this module reuses it rather than minting a second
//!    reduce (ARCH §10.6, principle 11).
//! 3. **the custody sweep** — [`PeerAnswer::bound_for_peer`] asks
//!    [`Custody::released_by`] once per citation. `grep -rn Custody
//!    commonwealth/` returns **zero hits** on this tree: the mesh boundary
//!    has no custody check at all today, which is the defect this row names.
//!    `sovereign-core/src/egress.rs` guards a different boundary (third-party
//!    providers) and gained no mesh arm; the two now share one ordering
//!    instead of growing a second.
//!
//! # No `Deserialize`, on any type here
//!
//! `Deserialize` is a constructor. Deriving it would put a public door back on
//! every type in this file — `serde_json::from_str::<Answer>("…")` would mint
//! one with any judgement the caller cared to type, and
//! `from_str::<Citation>` would mint a citation pointing at nothing. That is
//! precisely what these totalities exist to make unrepresentable (ARCH §7,
//! principle 10). `Serialize` IS derived: it is not a constructor, and the
//! surfaces that render an answer want it. The rule is inherited verbatim from
//! `corpus_engine::Evidence`, where it was checked rather than assumed.

use serde::Serialize;

use crate::attribution::Attribution;
use crate::custody::{join_custody, Custody};
use crate::judgement::{Judgement, Reason};
use crate::origin::{Grain, Origin};

/// The subject every turn-level judgement is filed under.
///
/// One spelling (ARCH §10.6). It is what `Judgement::subject` reads on an
/// answer's rolled-up verdict, so a reader grepping for turn verdicts has one
/// string to grep for rather than one per release site.
pub const TURN_SUBJECT: &str = "answer";

/// A sealed body of evidence, as far as the kernel needs to know about one.
///
/// The kernel states the question; the crate that owns the evidence answers
/// it. `corpus_engine::EvidenceSet` is the implementor — this crate cannot
/// name it, and must not, because that edge runs the wrong way.
///
/// Implementors must answer [`Seal::locate`] **only** from members the seal
/// actually holds. A seal that widens its own scope to satisfy a lookup is
/// not a seal, and no type can stop it — which is why `retrieve` is the only
/// producer of evidence and the set is sealed at that moment.
pub trait Seal {
    /// Where `quote` came from, if some sealed member contains it as one
    /// contiguous verbatim run.
    ///
    /// `None` means the seal does not hold it. Not "probably not", not "close
    /// enough": a citation that points at a passage the reader cannot find is
    /// worse than no citation, because it renders as one.
    fn locate(&self, quote: &str) -> Option<(&Origin, Custody)>;

    /// How many members the seal holds. Reported in a refusal so a caller can
    /// tell "the quote is not in these 12 chunks" from "the seal is empty".
    fn sealed_len(&self) -> usize;
}

/// Why a citation or a peer-bound release refused.
///
/// Refusals are values, not `None` and not a log line. Each variant names the
/// object of the decision, so the caller can render it, trace it, or turn it
/// into a [`Reason`] via the [`From`] impl below and file it as a
/// [`Judgement`] — which is how an abstention comes to carry the reason it
/// abstained (ARCH §18.3, principle 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "refused")]
pub enum Refused {
    /// The seal does not contain this quote verbatim.
    NotInSeal {
        quote: String,
        /// How many members were searched. `0` distinguishes "nothing was
        /// retrieved" from "twelve chunks and none of them say this".
        sealed_len: usize,
    },
    /// The seal holds it, but in a member that may not be quoted — a RAPTOR
    /// rollup is model-authored prose ABOUT source text ([`Grain::Summary`]).
    /// It may orient retrieval; it may not be presented as a source.
    NotQuotable { quote: String, grain: Grain },
    /// The evidence behind this answer may not leave the machine at the
    /// floor the caller offered.
    Custody {
        quote: String,
        /// The most restrictive custody among the answer's citations.
        custody: Custody,
        /// The floor the caller offered.
        floor: Custody,
    },
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::NotInSeal { quote, sealed_len } => write!(
                f,
                "the sealed evidence ({sealed_len} member(s)) does not contain {:?} verbatim",
                elide(quote)
            ),
            Refused::NotQuotable { quote, grain } => write!(
                f,
                "{:?} lands in {grain} material, which may orient retrieval but may not be quoted",
                elide(quote)
            ),
            Refused::Custody {
                quote,
                custody,
                floor,
            } => write!(
                f,
                "evidence behind {:?} has {custody} custody, which a {floor} floor does not release",
                elide(quote)
            ),
        }
    }
}

impl From<Refused> for Reason {
    /// A refusal is always a reason — every rendering above names an object
    /// and a rule, so none of them is a placeholder.
    fn from(r: Refused) -> Reason {
        Reason::new(r.to_string()).expect("a refusal names its object and its rule")
    }
}

/// First 60 characters, so a refusal is readable in a table without carrying
/// a whole passage.
fn elide(s: &str) -> String {
    let mut out: String = s.chars().take(60).collect();
    if out.chars().count() < s.chars().count() {
        out.push('…');
    }
    out
}

/// A verbatim passage the answer stands on, and where it came from.
///
/// Fields are private and there is no `Default`, no `new`, and no
/// `Deserialize`. [`Citation::pointing_into`] is the only door and it takes a
/// [`Seal`], so **a `Citation` that exists is a citation some seal vouched
/// for**. That is the difference between this type and the three that carry
/// the name today (`sovereign-eval/judge.rs:79`,
/// `sovereign-meshapp/wrapped.rs:182`, `sovereign-server/projection.rs:95`)
/// plus the fourth under a different name (`ReleasedCitation`): all four are
/// plain public-field structs anyone can fill in.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Citation {
    quote: String,
    source: Origin,
    custody: Custody,
}

impl Citation {
    /// The ONE door: mint a citation by finding its quote inside a seal.
    ///
    /// Two rules, both structural because both are here and nowhere else:
    /// the quote must appear verbatim in a sealed member, and that member's
    /// [`Grain`] must permit quoting. A caller cannot skip either, cannot
    /// misspell either, and cannot mint a citation from a bare string.
    ///
    /// This is `sovereign-meshapp`'s audited invariant, promoted from a doc
    /// comment to a constructor.
    pub fn pointing_into<S>(seal: &S, quote: impl Into<String>) -> Result<Citation, Refused>
    where
        S: Seal + ?Sized,
    {
        let quote = quote.into();
        // An empty quote matches every member — `"anything".contains("")` is
        // true — so without this it would mint a citation pointing at whatever
        // the seal happened to hold first. Refused as absence, which is what
        // it is: the seal does not contain an empty string as a citable run.
        if quote.trim().is_empty() {
            return Err(Refused::NotInSeal {
                quote,
                sealed_len: seal.sealed_len(),
            });
        }
        let Some((origin, custody)) = seal.locate(&quote) else {
            return Err(Refused::NotInSeal {
                quote,
                sealed_len: seal.sealed_len(),
            });
        };
        if !origin.grain.may_be_quoted() {
            return Err(Refused::NotQuotable {
                quote,
                grain: origin.grain,
            });
        }
        Ok(Citation {
            quote,
            source: origin.clone(),
            custody,
        })
    }

    /// The passage, verbatim as it appears in the sealed member.
    pub fn quote(&self) -> &str {
        &self.quote
    }

    /// Where the passage came from — store, machine, and grain.
    pub fn source(&self) -> &Origin {
        &self.source
    }

    /// Where the passage stands for sharing.
    pub fn custody(&self) -> Custody {
        self.custody
    }
}

/// Composed text that has not been judged yet.
///
/// **A `Draft`'s text cannot be read.** There is no `text()`, no `Display`, no
/// `Serialize`, and the field is private — the only way to get at what was
/// composed is [`Draft::release`] or [`Draft::release_ungated`], each of which
/// requires saying what is known about it. A surface holding a `Draft` holds
/// something it cannot show anyone, which is what "no surface returns a
/// pre-release draft" means when it is a type rather than a review comment.
///
/// Tokens on the wire are NOT a `Draft` and not an `Answer`: they are frames.
/// The gate already buffers them (`streaming.rs` `held_tokens` /
/// `gate_held_answer`) and the user sees a token count, not text. This type
/// makes structural what that path already does procedurally; it does not ban
/// it.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    text: String,
    citations: Vec<Citation>,
}

impl Draft {
    /// What the composer produced, against the citations it could support.
    pub fn composed(text: impl Into<String>, citations: Vec<Citation>) -> Draft {
        Draft {
            text: text.into(),
            citations,
        }
    }

    /// The citations composed so far. Readable — a citation is already
    /// vouched for by a seal, so there is nothing to withhold. The TEXT is
    /// what cannot be read.
    pub fn citations(&self) -> &[Citation] {
        &self.citations
    }

    /// Release the draft against the judgements the verifier produced.
    ///
    /// The fold is [`Judgement::roll_up`] and it is the only fold: worst
    /// verdict wins, an empty set is `CouldNotJudge` rather than a pass, and
    /// the roll-up is never fresher than its stalest input. Rung 10 shipped
    /// and tested that policy; re-reducing here with a bespoke `min_by_key`
    /// would be the second implementation of one decider (ARCH §10.6).
    ///
    /// Note what an empty slice therefore means: **verification that produced
    /// no judgements is a could-not-judge, not a pass.** That is the whole
    /// §18.2 point, and it falls out of the reuse rather than being restated.
    pub fn release(self, provenance: Attribution, judgements: &[Judgement]) -> Answer {
        self.sealed_with(provenance, Judgement::roll_up(TURN_SUBJECT, judgements))
    }

    /// Release a draft on a turn where the gate did not run.
    ///
    /// An un-gated turn carries `Verdict::NeverRan` and its reason. This is
    /// not a convenience: ARCH §18.2 says a gate that did not execute is not
    /// a gate that abstained, and until this existed an un-gated turn was
    /// **indistinguishable at the type level from a gated-and-passed one**.
    /// Now it is a different word on the answer's own judgement.
    ///
    /// One fold, still: the judgement goes through the same private seal as
    /// [`Draft::release`].
    pub fn release_ungated(self, provenance: Attribution, reason: Reason) -> Answer {
        self.sealed_with(provenance, Judgement::never_ran(TURN_SUBJECT, reason))
    }

    /// The single place a `Draft`'s text becomes an `Answer`'s text.
    fn sealed_with(self, provenance: Attribution, judgement: Judgement) -> Answer {
        Answer {
            text: self.text,
            citations: self.citations,
            provenance,
            judgement,
        }
    }
}

/// What the user sees, and what it stands on.
///
/// Fields are private, there is no `Default` and no `Deserialize`, and every
/// door takes a [`Judgement`] by value — so **there is no way to make an
/// `Answer` without saying how much it should be trusted**. The doors are
/// [`Draft::release`], [`Draft::release_ungated`], and [`Answer::abstained`].
///
/// An honest abstention is an `Answer` too. That is why [`Answer::abstained`]
/// is a constructor here rather than an `Err` somewhere else: a refusal the
/// user should read is a result, not an error path, and routing it through
/// `Result` is how it comes to be rendered as a stack trace or swallowed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Answer {
    text: String,
    citations: Vec<Citation>,
    provenance: Attribution,
    judgement: Judgement,
}

impl Answer {
    /// The turn declined to answer, and says why.
    ///
    /// `Verdict::Failed` with the reason — the answer failed to establish
    /// what was asked, and the text tells the user that in words. No
    /// citations: an abstention that cited something would be an answer.
    pub fn abstained(text: impl Into<String>, provenance: Attribution, reason: Reason) -> Answer {
        Answer {
            text: text.into(),
            citations: Vec::new(),
            provenance,
            judgement: Judgement::failed(TURN_SUBJECT, reason),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn citations(&self) -> &[Citation] {
        &self.citations
    }

    /// Which engine computed this text. A field, not
    /// `metadata["provenance"]` — the untyped channel eight writer sites
    /// currently share with defensive readers on the other end.
    pub fn provenance(&self) -> &Attribution {
        &self.provenance
    }

    /// How much this answer should be trusted, and why.
    pub fn judgement(&self) -> &Judgement {
        &self.judgement
    }

    /// The most restrictive custody among the answer's citations.
    ///
    /// `None` when the answer cites nothing — reported, never defaulted
    /// (principle 6). An uncited answer does not have public-web evidence; it
    /// has no evidence, and those are different facts. Callers that treat
    /// them the same are the reason this returns an `Option`.
    ///
    /// The fold is [`join_custody`], the existing max-restrictiveness join,
    /// not a second one.
    pub fn evidence_custody(&self) -> Option<Custody> {
        if self.citations.is_empty() {
            return None;
        }
        let classes: Vec<Custody> = self.citations.iter().map(|c| c.custody).collect();
        Some(join_custody(&classes))
    }
}

/// An answer cleared to leave this machine for a named peer.
///
/// The mesh reply path takes THIS, not an [`Answer`]. Its only door is
/// [`PeerAnswer::bound_for_peer`], which returns a `Result` — so a caller
/// cannot obtain one without either passing the custody sweep or handling the
/// refusal. "We forgot to check custody before replying to a peer" has no
/// spelling.
///
/// This is the row `nc-thesis` calls *a non-shareable Evidence in a
/// peer-bound reply*, and it is un-guarded on this tree today: `grep -rn
/// Custody commonwealth/` finds nothing at all.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PeerAnswer(Answer);

impl PeerAnswer {
    /// Clear an answer for a peer at a release floor, or refuse and say what
    /// was withheld.
    ///
    /// The decider is [`Custody::released_by`] — the same one
    /// `egress::ConsentGrant::covers` asks about a third-party provider. One
    /// ordering, two boundaries.
    ///
    /// An answer that cites nothing releases: there is no evidence to
    /// withhold. That is deliberately NOT a claim that the text is
    /// public-web — see [`Answer::evidence_custody`] — and it is why this
    /// door sweeps citations rather than reading a single custody field off
    /// the answer.
    pub fn bound_for_peer(answer: Answer, floor: Custody) -> Result<PeerAnswer, Refused> {
        match answer.evidence_custody() {
            None => Ok(PeerAnswer(answer)),
            Some(custody) if custody.released_by(floor) => Ok(PeerAnswer(answer)),
            Some(custody) => {
                // Name the citation that carries the blocking class. The join
                // is max-restrictiveness, so at least one citation matches it;
                // taking the first makes the refusal deterministic.
                let quote = answer
                    .citations
                    .iter()
                    .find(|c| c.custody == custody)
                    .map(|c| c.quote.clone())
                    .unwrap_or_default();
                Err(Refused::Custody {
                    quote,
                    custody,
                    floor,
                })
            }
        }
    }

    /// The cleared answer.
    pub fn answer(&self) -> &Answer {
        &self.0
    }

    /// Take the cleared answer back out.
    pub fn into_answer(self) -> Answer {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::ContentHash;
    use crate::ids::CorpusId;
    use crate::origin::{Locator, Server, Source};

    /// A minimal seal, standing in for `corpus_engine::EvidenceSet`. The
    /// kernel cannot name the real one, so the trait is exercised through the
    /// shape a real implementor has: a list of (text, origin, custody).
    struct FakeSeal(Vec<(String, Origin, Custody)>);

    impl Seal for FakeSeal {
        fn locate(&self, quote: &str) -> Option<(&Origin, Custody)> {
            self.0
                .iter()
                .find(|(text, _, _)| text.contains(quote))
                .map(|(_, o, c)| (o, *c))
        }
        fn sealed_len(&self) -> usize {
            self.0.len()
        }
    }

    fn origin(grain: Grain) -> Origin {
        Origin {
            source: Source::Corpus {
                corpus: CorpusId::new("wikipedia").unwrap(),
                document: ContentHash::of(b"the text"),
                locator: Locator::new("chunk:42").unwrap(),
            },
            served_by: Server::Local,
            grain,
        }
    }

    fn seal(custody: Custody, grain: Grain) -> FakeSeal {
        FakeSeal(vec![(
            "the whale is a mammal and it breathes air".to_string(),
            origin(grain),
            custody,
        )])
    }

    fn attribution() -> Attribution {
        Attribution {
            model: "qwen3-30b".into(),
            build: "b4321".into(),
            quantization: Some("Q4_K_M".into()),
            host: Server::Local,
        }
    }

    fn r(s: &'static str) -> Reason {
        Reason::literal(s)
    }

    #[test]
    fn a_citation_the_seal_does_not_hold_is_refused_and_says_so() {
        let s = seal(Custody::PublicWeb, Grain::Leaf);
        let err = Citation::pointing_into(&s, "the whale is a fish").unwrap_err();
        assert_eq!(
            err,
            Refused::NotInSeal {
                quote: "the whale is a fish".into(),
                sealed_len: 1
            }
        );
        // The refusal renders, and it is a Reason — so an abstention can
        // carry the exact sentence that caused it.
        let reason: Reason = err.into();
        assert!(reason.as_str().contains("does not contain"));
    }

    #[test]
    fn an_empty_seal_cites_nothing_and_reports_the_zero() {
        let s = FakeSeal(vec![]);
        let err = Citation::pointing_into(&s, "anything").unwrap_err();
        let Refused::NotInSeal { sealed_len, .. } = err else {
            panic!("an empty seal refuses by absence");
        };
        assert_eq!(sealed_len, 0);
    }

    #[test]
    fn an_empty_quote_cites_nothing_even_though_every_string_contains_it() {
        // `"anything".contains("")` is true, so a naive seal lookup would hand
        // back the first member and mint a citation pointing at it.
        let s = seal(Custody::PublicWeb, Grain::Leaf);
        for empty in ["", "   ", "\t\n"] {
            assert!(
                Citation::pointing_into(&s, empty).is_err(),
                "{empty:?} must not mint a citation"
            );
        }
    }

    #[test]
    fn a_summary_may_orient_retrieval_but_may_not_be_quoted() {
        // Reuses `Grain::may_be_quoted` — the same decider the grounding gate
        // currently spells `metadata.get("source") == Some("raptor")`.
        let s = seal(Custody::PublicWeb, Grain::Summary);
        let err = Citation::pointing_into(&s, "the whale is a mammal").unwrap_err();
        assert!(matches!(
            err,
            Refused::NotQuotable {
                grain: Grain::Summary,
                ..
            }
        ));
    }

    #[test]
    fn a_citation_carries_the_custody_of_what_it_quotes() {
        let s = seal(Custody::Personal, Grain::Leaf);
        let c = Citation::pointing_into(&s, "the whale is a mammal").unwrap();
        assert_eq!(c.quote(), "the whale is a mammal");
        assert_eq!(c.custody(), Custody::Personal);
        assert_eq!(c.source().grain, Grain::Leaf);
    }

    #[test]
    fn release_folds_by_the_one_policy_and_worst_wins() {
        let s = seal(Custody::PublicWeb, Grain::Leaf);
        let c = Citation::pointing_into(&s, "the whale is a mammal").unwrap();
        let draft = Draft::composed("Whales are mammals.", vec![c]);

        let judgements = [
            Judgement::passed("support", r("every claim quoted")),
            Judgement::could_not_judge("numeric", r("no numbers in the answer")),
        ];
        let a = draft.release(attribution(), &judgements);
        // Worst-wins, straight out of `Judgement::roll_up` — a pass beside a
        // could-not-judge is a could-not-judge, never green.
        assert_eq!(
            a.judgement().verdict(),
            crate::judgement::Verdict::CouldNotJudge
        );
        assert_eq!(a.judgement().subject(), TURN_SUBJECT);
        assert_eq!(a.text(), "Whales are mammals.");
    }

    #[test]
    fn a_release_with_no_judgements_is_could_not_judge_never_a_pass() {
        let draft = Draft::composed("Whales are mammals.", vec![]);
        let a = draft.release(attribution(), &[]);
        assert_eq!(
            a.judgement().verdict(),
            crate::judgement::Verdict::CouldNotJudge
        );
        assert!(a.judgement().reason().as_str().contains("empty set"));
    }

    #[test]
    fn an_ungated_turn_is_never_ran_and_not_a_pass() {
        // The defect this closes: before this door, an un-gated turn was
        // indistinguishable at the type level from a gated-and-passed one.
        let draft = Draft::composed("Whales are mammals.", vec![]);
        let a = draft.release_ungated(attribution(), r("grounding gate disarmed for this turn"));
        assert_eq!(a.judgement().verdict(), crate::judgement::Verdict::NeverRan);
        assert!(a.judgement().needs_attention());
        assert_ne!(a.judgement().verdict(), crate::judgement::Verdict::Passed);
    }

    #[test]
    fn an_abstention_is_an_answer_with_a_failed_verdict_and_no_citations() {
        let a = Answer::abstained(
            "I could not find anything in your corpora about this.",
            attribution(),
            r("retrieval returned nothing above the relevance floor"),
        );
        assert_eq!(a.judgement().verdict(), crate::judgement::Verdict::Failed);
        assert!(a.citations().is_empty());
        assert!(a.text().starts_with("I could not find"));
    }

    #[test]
    fn an_uncited_answer_reports_absence_rather_than_defaulting_to_public() {
        let a = Answer::abstained("no.", attribution(), r("nothing retrieved"));
        assert_eq!(a.evidence_custody(), None);
    }

    #[test]
    fn evidence_custody_is_the_max_restrictiveness_join() {
        let public = seal(Custody::PublicWeb, Grain::Leaf);
        let personal = seal(Custody::Personal, Grain::Leaf);
        let a = Draft::composed(
            "Whales are mammals.",
            vec![
                Citation::pointing_into(&public, "the whale is a mammal").unwrap(),
                Citation::pointing_into(&personal, "breathes air").unwrap(),
            ],
        )
        .release(attribution(), &[Judgement::passed("support", r("quoted"))]);
        assert_eq!(a.evidence_custody(), Some(Custody::Personal));
    }

    #[test]
    fn a_peer_reply_refuses_estate_material_and_names_what_it_withheld() {
        let s = seal(Custody::Personal, Grain::Leaf);
        let a = Draft::composed(
            "Whales are mammals.",
            vec![Citation::pointing_into(&s, "the whale is a mammal").unwrap()],
        )
        .release(attribution(), &[Judgement::passed("support", r("quoted"))]);

        let err = PeerAnswer::bound_for_peer(a, Custody::PublicWeb).unwrap_err();
        assert_eq!(
            err,
            Refused::Custody {
                quote: "the whale is a mammal".into(),
                custody: Custody::Personal,
                floor: Custody::PublicWeb,
            }
        );
        assert!(err.to_string().contains("does not release"));
    }

    #[test]
    fn a_peer_reply_releases_web_material_and_an_uncited_answer() {
        let s = seal(Custody::PublicWeb, Grain::Leaf);
        let a = Draft::composed(
            "Whales are mammals.",
            vec![Citation::pointing_into(&s, "the whale is a mammal").unwrap()],
        )
        .release(attribution(), &[Judgement::passed("support", r("quoted"))]);
        assert!(PeerAnswer::bound_for_peer(a, Custody::PublicWeb).is_ok());

        // Nothing cited => nothing to withhold. NOT a claim that the text is
        // public-web.
        let uncited = Answer::abstained("no.", attribution(), r("nothing retrieved"));
        assert!(PeerAnswer::bound_for_peer(uncited, Custody::PublicWeb).is_ok());
    }

    #[test]
    fn unknown_custody_never_reaches_a_peer_at_any_floor() {
        let s = seal(Custody::Unknown, Grain::Leaf);
        let a = Draft::composed(
            "Whales are mammals.",
            vec![Citation::pointing_into(&s, "the whale is a mammal").unwrap()],
        )
        .release(attribution(), &[Judgement::passed("support", r("quoted"))]);

        for floor in [Custody::PublicWeb, Custody::Peer, Custody::Personal] {
            let a = a.clone();
            assert!(
                PeerAnswer::bound_for_peer(a, floor).is_err(),
                "unknown custody must refuse at the {floor} floor"
            );
        }
    }

    #[test]
    fn an_answer_serializes_but_cannot_be_deserialized_back_in() {
        // The `Serialize` half is asserted here; the missing `Deserialize` is
        // asserted by the compile-fail suite, which is the only place it can
        // be. A round-trip test would not compile, which IS the point.
        let a = Answer::abstained("no.", attribution(), r("nothing retrieved"));
        let wire = serde_json::to_string(&a).unwrap();
        assert!(wire.contains("\"failed\""));
        assert!(wire.contains("nothing retrieved"));
    }

    #[test]
    fn a_refusal_renders_a_long_quote_elided() {
        let long = "x".repeat(200);
        let s = FakeSeal(vec![]);
        let err = Citation::pointing_into(&s, long).unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains('…'));
        assert!(rendered.len() < 200);
    }
}
