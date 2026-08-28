// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`ChunkProvenance`] — where a [`ScoredChunk`](crate::types::ScoredChunk) came
//! from, as a type rather than as two keys in a string map.
//!
//! Minted 2026-08-26 for `quality/TOPOLOGY.md` §10 phase 9, rung 9.1 —
//! hazard 1, "a prompt fed by content that never passed `retrieve`".
//!
//! # The defect this closes
//!
//! `ScoredChunk` carries provenance in `metadata: HashMap<String, String>`,
//! and two keys in that bag decide whether a claim may be made:
//! `metadata["custody"]` sets the egress floor, and `metadata["source"] ==
//! "raptor"` decides whether a chunk may be quoted verbatim or only used to
//! orient. So **whether a chunk may ground a claim is a string compare on an
//! untyped map** — written at one site and read at roughly fifteen with no
//! type in between (`index/evidence.rs` module docs, measured 2026-08-20).
//!
//! A missing key and a key spelled wrong are the same value. A chunk
//! manufactured inside sovereign — an atlas atom, a RAPTOR rollup, a
//! conversation-briefing row, model-authored prose — arrives with an empty
//! bag and is therefore indistinguishable from a chunk an index vouched for.
//! That is hazard 1 exactly, and it is why `CorpusIndex::retrieve` having zero
//! production callers matters: the door exists and nothing walks through it.
//!
//! # Why this is not `Evidence`
//!
//! [`Evidence`](crate::index::Evidence) is the destination: immutable, no
//! public constructor, minted only at a door. The retrieval pipeline cannot
//! hold it yet, because the pipeline's job is MUTATION — sovereign reassigns
//! `score` at twelve production sites, overwrites `content`/`title`/`url` at
//! six, and calls `metadata.insert` at sixteen. Swapping the pool's element
//! type for `Evidence` does not compile, and should not: a re-rank is a fact
//! about this turn, not about what was acquired.
//!
//! So this rung splits the two halves of `ScoredChunk` where they actually
//! divide. The ranking fields stay public and mutable. ChunkProvenance becomes a
//! field whose **`Acquired` arm has no public constructor** — sovereign can
//! read it, and can only write [`ChunkProvenance::Manufactured`], which names the
//! producer and is not citable. Every manufactured producer is one row of
//! §6's `adopted` count, and the rung is done when that count is zero.

use kernel_types::{Custody, Grain};
use serde::Serialize;

/// Where a chunk came from.
///
/// # Why `ChunkProvenance` and not `Provenance`
///
/// `svrn code converge noun Provenance` reports FOUR first-party definitions
/// already (`enrichment/atlas/atoms.rs`, `contracts/types/epistemic.rs`,
/// `contracts/types/projection.rs`, `inference/embedded/capabilities.rs`) and
/// 56 reference sites across seven crates. None of them answers this
/// question — the epistemic one says where a HOLDING came from
/// (memory / corpus / model), which is a turn-level fact — so reusing one
/// would conflate concepts, and corpus-engine cannot reach the crate that
/// owns them anyway. The repo already disambiguates this family by qualifier
/// (`AtomProvenance`, `CorpusProvenance`, `EdgeProvenance`, `TurnProvenance`,
/// `ResponseProvenance`), so this follows the convention rather than adding a
/// fifth bare `Provenance` (AGENTS.md pre-flight; ARCH §10.6).
///
/// There is no `Default` and no `Deserialize`. Both would be doors: a default
/// would let a manufactured chunk pass as an acquired one by omission, and
/// `serde_json::from_str` would mint any provenance a caller cared to type
/// (ARCH §7, principle 10 — the same argument `Evidence` makes).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ChunkProvenance {
    /// Stamped by an acquisition door from the index's own facts, at the
    /// moment of acquisition, which is the only place those facts exist.
    ///
    /// Constructible only inside `corpus-engine`. A caller in `sovereign` or
    /// `commonwealth` that holds one was handed it by a door.
    Acquired(Acquisition),
    /// Built inside a process by a named producer rather than by a door.
    ///
    /// **Not citable.** This arm is hazard 1's content wearing its own name:
    /// it is in the pool, it may orient retrieval, and it may not ground a
    /// claim. `producer` is `&'static str` so the name is a literal in the
    /// source rather than a runtime string — a producer that cannot be named
    /// at compile time is one nobody can grep for.
    Manufactured {
        /// Who built it. Use the module or step that owns the construction —
        /// `"atlas_atom"`, `"raptor_rollup"`, `"conv_briefing"`.
        producer: &'static str,
        /// Source text, or prose ABOUT source text.
        ///
        /// Manufactured content is not quotable either way — nothing vouched
        /// for it — so this arm's grain never loosens a citation. It answers
        /// the OTHER question the legacy bag was carrying: the ranking and
        /// formatting sites ask "is this the summary tier?", and all three of
        /// them were answering it with `metadata["source"] == "raptor"`
        /// (ARCH §10.6 — one implementation per key).
        grain: Grain,
    },
}

impl ChunkProvenance {
    /// Name a chunk this process built rather than acquired, carrying its
    /// own text.
    pub const fn manufactured(producer: &'static str) -> Self {
        ChunkProvenance::Manufactured {
            producer,
            grain: Grain::Leaf,
        }
    }

    /// Name a chunk this process built as prose ABOUT source text — a RAPTOR
    /// rollup, a composed overview.
    ///
    /// Separate constructor rather than a `grain` argument on
    /// [`Self::manufactured`]: the summary case is rare (one production
    /// producer today) and a two-argument call at forty sites would put the
    /// fact in an argument nobody reads. Here the fact is the function name.
    pub const fn manufactured_summary(producer: &'static str) -> Self {
        ChunkProvenance::Manufactured {
            producer,
            grain: Grain::Summary,
        }
    }

    /// The estate store's acquisition door.
    ///
    /// Estate documents come from the operator's own store, which is not a
    /// corpus index and carries no metadata bag — so custody is not a fact
    /// this door can READ. It is a property of which door the caller walked
    /// through: estate material is [`Custody::Personal`] source text, and a
    /// caller in `sovereign` cannot ask this door for any other class.
    ///
    /// Named for the store rather than taking `(custody, grain)`, because a
    /// door that accepts a custody argument is a public constructor for
    /// [`Self::Acquired`] wearing a door's name (ARCH §7). The residual
    /// hazard is a caller walking non-estate content through it, which is
    /// why the name is the guard — and `Personal` is the most restrictive
    /// released class, so that misuse over-refuses rather than leaking.
    pub fn acquired_from_estate(corpus: impl Into<String>) -> Self {
        ChunkProvenance::Acquired(Acquisition::stamped(corpus, Custody::Personal, Grain::Leaf))
    }

    /// The mesh reply path's acquisition door.
    ///
    /// Content a PEER's index vouched for. Two facts, and they arrive
    /// differently — which is why this took a wire change and the estate door
    /// did not.
    ///
    /// **Custody is JOINED, never taken.** `join_custody([peer_claim, Peer])`
    /// is max-restrictiveness (custody.md §3), so a peer cannot talk its
    /// content DOWN to a looser class than "arrived from another node", and
    /// this node's own fact is always in the join. A peer that recorded no
    /// class sends nothing; `None` joins as [`Custody::Unknown`], which
    /// poisons the join and refuses (ARCH §18.3). Trusting the peer's number
    /// outright would let a mislabelled hit egress at the peer's floor rather
    /// than ours.
    ///
    /// **Grain must travel, and absence is the REFUSING value.** `None` — an
    /// un-upgraded peer, or a serving side that recorded nothing — reads as
    /// [`Grain::Summary`], because `Leaf` is the permissive one: a rollup
    /// wrongly marked `Leaf` becomes quotable, which is the direction that
    /// fabricates. So an old peer's hits stay exactly as unquotable as they
    /// were while they were `Manufactured`, and a peer that says `leaf`
    /// unlocks quoting.
    pub fn acquired_from_peer(
        corpus: impl Into<String>,
        peer_custody: Option<Custody>,
        grain: Option<Grain>,
    ) -> Self {
        let custody = kernel_types::custody::join_custody(&[
            peer_custody.unwrap_or(Custody::Unknown),
            Custody::Peer,
        ]);
        ChunkProvenance::Acquired(Acquisition::stamped(
            corpus,
            custody,
            grain.unwrap_or(Grain::Summary),
        ))
    }

    /// May a claim quote this chunk verbatim?
    ///
    /// Two independent reasons for no, and they are different facts: nothing
    /// vouched for the content (manufactured), or an index vouched for it as
    /// a SUMMARY of source text rather than as the source text
    /// ([`Grain::may_be_quoted`]).
    pub fn may_be_quoted(&self) -> bool {
        match self {
            ChunkProvenance::Acquired(a) => a.grain.may_be_quoted(),
            ChunkProvenance::Manufactured { .. } => false,
        }
    }

    /// Where this content stands for sharing.
    ///
    /// Manufactured content is [`Custody::Unknown`], which REFUSES downstream
    /// rather than defaulting permissive (ARCH §18.3, principle 6). A chunk
    /// this process invented has no custody class; that is not the same fact
    /// as "public web", and the two must not share a value.
    pub fn custody(&self) -> Custody {
        self.stamped_custody().unwrap_or(Custody::Unknown)
    }

    /// The custody class a door RECORDED, when one did.
    ///
    /// `None` where there is no custody fact at all: a manufactured chunk, or
    /// an acquisition whose store carried no stamp. This is a different
    /// question from [`Self::custody`], which asks what class the content is
    /// and answers `Unknown` — the refusing value — when nobody said.
    ///
    /// The grounding gate needs the distinction and would be wrong without
    /// it. `custody_engaged` is "did ANY chunk this turn arrive stamped", and
    /// a pool where nothing is stamped must leave the custody machinery OFF
    /// rather than engage it and refuse the whole turn (custody.md §4, red
    /// R-3). Collapsing the two would turn every pre-custody surface into a
    /// refusal case.
    pub fn stamped_custody(&self) -> Option<Custody> {
        match self {
            ChunkProvenance::Acquired(a) if a.custody != Custody::Unknown => Some(a.custody),
            _ => None,
        }
    }

    /// Source text, or prose about source text — for either arm.
    ///
    /// The typed replacement for `metadata["source"] == "raptor"`, which was
    /// compared at three sites and matched BOTH an indexed rollup row and an
    /// in-process one. One question, one answer, both arms.
    pub fn grain(&self) -> Grain {
        match self {
            ChunkProvenance::Acquired(a) => a.grain,
            ChunkProvenance::Manufactured { grain, .. } => *grain,
        }
    }

    /// The corpus that vouched for this chunk, when one did.
    pub fn corpus(&self) -> Option<&str> {
        match self {
            ChunkProvenance::Acquired(a) => Some(&a.corpus),
            ChunkProvenance::Manufactured { .. } => None,
        }
    }

    /// The producer, when this chunk was manufactured. `None` for acquired
    /// content — reported, never conflated with "manufactured by nobody".
    pub fn producer(&self) -> Option<&'static str> {
        match self {
            ChunkProvenance::Acquired(_) => None,
            ChunkProvenance::Manufactured { producer, .. } => Some(producer),
        }
    }

    /// What a chunk that came off the wire carries.
    ///
    /// `ScoredChunk`'s `Deserialize` is vestigial — there is no production
    /// deserialize of it anywhere in the repo; the mesh wire type is OICP
    /// `KnowledgeResult` and the conversion is field-by-field
    /// (`index/evidence.rs` module docs). It still has to produce a value, and
    /// the honest one is "this process did not acquire it", never a fabricated
    /// acquisition.
    pub(crate) fn off_the_wire() -> Self {
        ChunkProvenance::manufactured("deserialized")
    }
}

/// What an acquisition door stamped, from the index's own facts.
///
/// Private fields, no public constructor: the whole point of the rung is that
/// `sovereign` cannot write one (ARCH §7).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Acquisition {
    corpus: String,
    custody: Custody,
    grain: Grain,
}

impl Acquisition {
    /// Stamp an acquisition. `pub(crate)` — see the type docs.
    pub(crate) fn stamped(corpus: impl Into<String>, custody: Custody, grain: Grain) -> Self {
        Self {
            corpus: corpus.into(),
            custody,
            grain,
        }
    }

    /// Where this content stands for sharing.
    pub fn custody(&self) -> Custody {
        self.custody
    }

    /// Source text, or a summary of it.
    pub fn grain(&self) -> Grain {
        self.grain
    }

    /// The corpus that vouched for it.
    pub fn corpus(&self) -> &str {
        &self.corpus
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manufactured_content_refuses_rather_than_defaulting_permissive() {
        let p = ChunkProvenance::manufactured("atlas_atom");
        // The two properties that decide whether a claim may be made.
        assert!(!p.may_be_quoted());
        assert_eq!(p.custody(), Custody::Unknown);
        // And it says who built it, which the string bag could not.
        assert_eq!(p.producer(), Some("atlas_atom"));
        assert_eq!(p.corpus(), None);
    }

    #[test]
    fn a_manufactured_rollup_is_a_summary_and_a_manufactured_turn_is_not() {
        // The typed replacement for `metadata["source"] == "raptor"`. Both are
        // unquotable — nothing vouched for either — but only one is the
        // SUMMARY tier, and the ranking and formatting sites need that split.
        let rollup = ChunkProvenance::manufactured_summary("raptor_summary");
        assert_eq!(rollup.grain(), Grain::Summary);
        assert!(!rollup.may_be_quoted());

        let turn = ChunkProvenance::manufactured("conversation_turn");
        assert_eq!(turn.grain(), Grain::Leaf);
        assert!(!turn.may_be_quoted());
    }

    #[test]
    fn an_unstamped_acquisition_records_no_class() {
        // `custody()` and `stamped_custody()` answer different questions and
        // must not be collapsed. A door that read no class yields the refusing
        // value for "what class is this", and `None` for "did a door record
        // one" — the gate's custody machinery keys on the second, and
        // collapsing them turns every unstamped pool into a refusal.
        let unstamped = ChunkProvenance::Acquired(Acquisition::stamped(
            "wikipedia",
            Custody::Unknown,
            Grain::Leaf,
        ));
        assert_eq!(unstamped.custody(), Custody::Unknown);
        assert_eq!(unstamped.stamped_custody(), None);

        let stamped = ChunkProvenance::acquired_from_estate("notes");
        assert_eq!(stamped.stamped_custody(), Some(Custody::Personal));
    }

    #[test]
    fn a_summary_is_acquired_and_still_not_quotable() {
        // The `metadata["source"] == "raptor"` compare, as a type. Acquired
        // content that is a SUMMARY may orient retrieval and may not be
        // quoted — a distinction the string bag carried at one write site and
        // fifteen reads.
        let p = ChunkProvenance::Acquired(Acquisition::stamped(
            "wikipedia",
            Custody::PublicWeb,
            Grain::Summary,
        ));
        assert!(!p.may_be_quoted());
        assert_eq!(p.custody(), Custody::PublicWeb);
        assert_eq!(p.corpus(), Some("wikipedia"));

        let leaf = ChunkProvenance::Acquired(Acquisition::stamped(
            "wikipedia",
            Custody::PublicWeb,
            Grain::Leaf,
        ));
        assert!(leaf.may_be_quoted());
    }
}

// ─── Reading the legacy bag, in ONE place ────────────────────────────────

/// The legacy `metadata` key carrying a chunk's custody class.
///
/// The wire spelling is `sovereign-contracts`' `CUSTODY_META_KEY`;
/// corpus-engine cannot depend on that crate (it is a product domain, and the
/// edge would run the wrong way), so the constant is restated here and pinned
/// by a test rather than left as a bare literal.
const LEGACY_CUSTODY_KEY: &str = "custody";
/// The legacy `metadata` key carrying a chunk's acquisition tag.
const LEGACY_SOURCE_KEY: &str = "source";
/// The `metadata["source"]` value marking a RAPTOR rollup — model-authored
/// prose ABOUT source text, which may orient retrieval and may not be quoted.
const LEGACY_SOURCE_RAPTOR: &str = "raptor";

/// Custody, read off the bag an index writes.
///
/// Called at the acquisition doors and NOWHERE ELSE. That is the rung: the
/// string compare survives (an index still writes strings), but it happens
/// once, at the moment of acquisition, and every downstream site reads
/// [`ChunkProvenance::custody`] instead of re-parsing.
///
/// Absent or unparseable => [`Custody::Unknown`], which refuses downstream.
/// Never `Personal` and never `PublicWeb`: an unstamped chunk and a chunk
/// stamped as estate material must not be the same value (ARCH §18.3).
pub(crate) fn custody_of(metadata: &std::collections::HashMap<String, String>) -> Custody {
    metadata
        .get(LEGACY_CUSTODY_KEY)
        .and_then(|v| Custody::parse_wire(v))
        .unwrap_or(Custody::Unknown)
}

/// Grain, read off the same bag at the same moment.
pub(crate) fn grain_of(metadata: &std::collections::HashMap<String, String>) -> Grain {
    match metadata.get(LEGACY_SOURCE_KEY).map(String::as_str) {
        Some(LEGACY_SOURCE_RAPTOR) => Grain::Summary,
        _ => Grain::Leaf,
    }
}

#[cfg(test)]
mod bag_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn an_unstamped_bag_yields_unknown_custody_not_a_permissive_default() {
        let empty = HashMap::new();
        assert_eq!(custody_of(&empty), Custody::Unknown);
        // And a misspelled key is the same as no key, which is exactly why
        // this compare belongs at one site rather than fifteen.
        let mut typo = HashMap::new();
        typo.insert("custardy".to_string(), "public_web".to_string());
        assert_eq!(custody_of(&typo), Custody::Unknown);
    }

    #[test]
    fn the_legacy_custody_key_matches_the_contract_spelling() {
        assert_eq!(LEGACY_CUSTODY_KEY, "custody");
    }

    #[test]
    fn the_raptor_tag_is_the_only_thing_that_makes_a_summary() {
        let mut raptor = HashMap::new();
        raptor.insert("source".to_string(), "raptor".to_string());
        assert_eq!(grain_of(&raptor), Grain::Summary);
        assert_eq!(grain_of(&HashMap::new()), Grain::Leaf);
    }
}
