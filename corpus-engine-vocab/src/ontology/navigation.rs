// SPDX-License-Identifier: AGPL-3.0-or-later
//! The map's third role — navigation policy (`EPISTEMIC_INDEX.md` §2.2).
//!
//! `shape` says what exists and `derivation` what was inferred; this section
//! says how a reader WALKS the atlas for each kind of question: which atom
//! kinds to seed on, which edge kinds to follow, how many hops, and how much
//! evidence to keep. It is data with a schema. Its one consumer is the walk
//! (`corpus-engine::enrichment::atlas::ground`, ei-4-walk 2026-09-04), which
//! reads a row per question kind and executes it; what this file fixes is the
//! shape and the pre-registered defaults — the spec's table, verbatim — so
//! that a pipeline or recipe that declares no navigation gets exactly those
//! rows, and a tuned row is a reviewable diff against them.
//!
//! A row also carries the [`WalkPolicy::exemplars`] that classify a question
//! ONTO its kind, so the map answers both "which row" and "how to walk it"
//! from one declaration.
//!
//! Closed sets are enums (ARCH §2): the question kinds are [`QuestionKind`],
//! the seed kinds reuse [`AtomType`] / [`EntityType`], the edges reuse
//! [`EdgeType`]. No spelling is minted here — a row is written in the
//! on-disk tags the atoms and edges already carry.

use serde::{Deserialize, Serialize};

use crate::atoms::AtomType;
use crate::edges::EdgeType;
use crate::taxonomy::EntityType;

/// What a READER asks — the five question kinds the navigation table is keyed
/// by. Not to be confused with `taxonomy::QuestionType`, which classifies a
/// Question ATOM the text itself raises; `QuestionKind::Thematic` is "what is
/// this about?" asked of the atlas, `QuestionType::Thematic` is a question
/// the work poses. Classification of open text onto this set is a centroid
/// per kind (ARCH §2.4), seeded from these names — the walker's concern, not
/// this file's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// "What is this about?", "what are the themes?"
    Thematic,
    /// "How does X change?"
    Trajectory,
    /// "Where does it disagree with itself?"
    Tension,
    /// "Which X are there?" — a declared type and its subtypes, listed.
    Enumeration,
    /// "Who is X?" — one entity by name.
    Lookup,
}

impl QuestionKind {
    /// Every kind, in table order. The closed set as data, so a display or a
    /// walker iterates it rather than hand-listing five arms.
    pub const ALL: [QuestionKind; 5] = [
        QuestionKind::Thematic,
        QuestionKind::Trajectory,
        QuestionKind::Tension,
        QuestionKind::Enumeration,
        QuestionKind::Lookup,
    ];

    /// The snake_case wire spelling, read back through serde so it can never
    /// disagree with what the parser accepts.
    pub fn as_str(&self) -> &'static str {
        match self {
            QuestionKind::Thematic => "thematic",
            QuestionKind::Trajectory => "trajectory",
            QuestionKind::Tension => "tension",
            QuestionKind::Enumeration => "enumeration",
            QuestionKind::Lookup => "lookup",
        }
    }
}

/// Where a walk starts. `kinds` are on-disk `atom_type` tags (`Entity`,
/// `State`, `Claim`, `Position`, `Configuration`, …); `entity_types` narrows
/// an `Entity` seed to the listed `entity_type` values (`concept`, `person`,
/// …) and is ignored when empty; `declared` seeds on the declared types under
/// `shape.types` and their subtypes — the enumeration row. A pipeline that
/// does not produce a kind simply does not list it; the walker skips absent
/// kinds and says so in its ledger.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeedPolicy {
    /// Atom kinds to seed on — the `atom_type` tag as written on disk.
    #[serde(default)]
    pub kinds: Vec<AtomType>,
    /// When `kinds` includes `Entity`, only entities of these types
    /// (`concept`, `person`, `work`, …). Empty means any entity.
    #[serde(default)]
    pub entity_types: Vec<EntityType>,
    /// Also seed on the declared types (`shape.types`) and their subtypes.
    #[serde(default)]
    pub declared: bool,
}

/// One row of the navigation table: how to walk for one question kind.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WalkPolicy {
    /// Where the walk starts.
    #[serde(default)]
    pub seed: SeedPolicy,
    /// Edge kinds to follow, in order of preference — the `edge_type` tag as
    /// written on disk (`Involves`, `Tension`, `Grounds`, `Transition`, …).
    /// Empty means no walk: seeds are the answer (the enumeration row).
    #[serde(default)]
    pub walk: Vec<EdgeType>,
    /// How many edge hops from a seed. `0` enumerates the seeds only.
    #[serde(default)]
    pub hops: u8,
    /// How many atoms the walk keeps as evidence requests.
    #[serde(default)]
    pub budget: u32,
    /// What a question of this kind SOUNDS like — the exemplar phrases the
    /// walker embeds into one centroid per kind to classify open text onto
    /// the closed set (ARCH §2.4, principle 9).
    ///
    /// They live HERE, in the map, and not in the walker's `.rs`, because a
    /// corpus whose readers phrase a kind differently ("what does this
    /// catalogue cover" for a numismatic atlas) retunes its classifier by
    /// editing its own declaration — the same way it retunes seeds and edges.
    /// The defaults below are the spec's own glosses of the five kinds
    /// (`EPISTEMIC_INDEX.md` §2.2, column 1), so the pre-registered table and
    /// the pre-registered exemplars are one artifact.
    ///
    /// An EMPTY list is load-bearing: that kind gets no centroid and can
    /// never be classified, which is how a corpus switches a row off without
    /// deleting it. Every row empty means the classifier cannot be built at
    /// all, and the walker says so rather than guessing a kind.
    #[serde(default)]
    pub exemplars: Vec<String>,
}

/// The evidence budget every default row carries.
///
/// TWELVE, read off the live call site rather than off prose: atlas grounding
/// keeps `((KQ_PER_CORPUS_LIMIT as f32) * 0.6).ceil()` with
/// `KQ_PER_CORPUS_LIMIT = 20` (`sovereign-core/src/runtime/prompts.rs`), which
/// is 12. `EPISTEMIC_INDEX.md` §1's Walk row said "budget 6" and this constant
/// was minted from that sentence; the sentence was wrong about the code, and a
/// default pre-registered from a mis-citation would have HALVED the evidence
/// budget of the SEP lane the same campaign is holding flat (principle 4 —
/// cite, don't recall). The spec row is corrected in the same commit.
///
/// The two cannot silently diverge again:
/// `sovereign-core::runtime::retrieval::atlas_grounding`'s
/// `the_default_budget_is_the_live_fetch_budget` test asserts the formula
/// against this constant, and fails if either side moves alone.
///
/// One number, one home; the rows below all cite it.
pub const DEFAULT_BUDGET: u32 = 12;

/// The pre-registered exemplars for one kind — `EPISTEMIC_INDEX.md` §2.2's own
/// gloss of the kind, plus the two or three phrasings a reader actually types.
///
/// Deliberately SHORT and few: a centroid of four phrases is what the router's
/// axis classifiers use (`scope_classifier.rs`), and a long list drifts toward
/// a keyword bag, which is the thing §2.4 says not to build.
fn exemplars(of: &[&str]) -> Vec<String> {
    of.iter().map(|s| s.to_string()).collect()
}

impl WalkPolicy {
    /// thematic: seed on Configuration and concept Entity; walk Involves →
    /// Tension → Grounds; 2 hops.
    pub fn thematic() -> Self {
        Self {
            seed: SeedPolicy {
                kinds: vec![AtomType::Configuration, AtomType::Entity],
                entity_types: vec![EntityType::Concept],
                declared: false,
            },
            walk: vec![EdgeType::Involves, EdgeType::Tension, EdgeType::Grounds],
            hops: 2,
            budget: DEFAULT_BUDGET,
            exemplars: exemplars(&[
                "what is this about",
                "what are the themes",
                "what does this work say as a whole",
                "summarise what this is concerned with",
            ]),
        }
    }

    /// trajectory: seed on Entity and State; walk Transition, Causes; 2 hops.
    pub fn trajectory() -> Self {
        Self {
            seed: SeedPolicy {
                kinds: vec![AtomType::Entity, AtomType::State],
                entity_types: Vec::new(),
                declared: false,
            },
            walk: vec![EdgeType::Transition, EdgeType::Causes],
            hops: 2,
            budget: DEFAULT_BUDGET,
            exemplars: exemplars(&[
                "how does this change",
                "how did it develop over time",
                "what happens to them over the course of it",
                "trace the arc of this",
            ]),
        }
    }

    /// tension: seed on Claim and Position; walk Tension, OpposesIn; 1 hop.
    /// The spec table writes the second edge as "Opposition"; the edge kind
    /// that frames a concept into an Opposition atom is spelled `OpposesIn`
    /// on disk, and that is the spelling recorded here.
    pub fn tension() -> Self {
        Self {
            seed: SeedPolicy {
                kinds: vec![AtomType::Claim, AtomType::Position],
                entity_types: Vec::new(),
                declared: false,
            },
            walk: vec![EdgeType::Tension, EdgeType::OpposesIn],
            hops: 1,
            budget: DEFAULT_BUDGET,
            exemplars: exemplars(&[
                "where does it disagree",
                "what tensions does it raise",
                "which claims contradict each other",
                "what are the objections to this position",
            ]),
        }
    }

    /// enumeration: seed on the declared types and their subtypes; no walk.
    pub fn enumeration() -> Self {
        Self {
            seed: SeedPolicy {
                kinds: Vec::new(),
                entity_types: Vec::new(),
                declared: true,
            },
            walk: Vec::new(),
            hops: 0,
            budget: DEFAULT_BUDGET,
            exemplars: exemplars(&[
                "which ones are there",
                "list all of them",
                "how many of these does it record",
                "enumerate the items",
            ]),
        }
    }

    /// lookup: seed on Entity (by name); walk Involves; 1 hop.
    pub fn lookup() -> Self {
        Self {
            seed: SeedPolicy {
                kinds: vec![AtomType::Entity],
                entity_types: Vec::new(),
                declared: false,
            },
            walk: vec![EdgeType::Involves],
            hops: 1,
            budget: DEFAULT_BUDGET,
            exemplars: exemplars(&[
                "who is this person",
                "tell me about this one",
                "what is this thing",
                "give me the entry for it",
            ]),
        }
    }

    /// The row a walk uses when NO kind was established — the classifier
    /// abstained, could not be built, or the corpus switched every row off.
    ///
    /// It is not a sixth question kind and it is not a guess. It is the
    /// STATUS QUO ANTE, written down: no seed-kind filter, no edge-kind
    /// filter, two hops, [`DEFAULT_BUDGET`] — exactly what
    /// `apply_atlas_grounding` did before it read a map at all. Naming it as
    /// data means "unclassified" runs one code path with the classified
    /// cases instead of a second branch, and means the walk cannot silently
    /// become a different walk when classification fails (ARCH §18.3: the
    /// substitution is named, in the ledger and in `ask`'s result text).
    ///
    /// Empty `seed.kinds` and empty `walk` both mean UNFILTERED here rather
    /// than "nothing", which is why `hops` carries the distinction: a row
    /// with `hops: 0` enumerates its seeds (the enumeration row) and a row
    /// with `hops > 0` and no edge list follows every edge kind.
    pub fn unfiltered() -> Self {
        Self {
            seed: SeedPolicy::default(),
            walk: Vec::new(),
            hops: 2,
            budget: DEFAULT_BUDGET,
            // Never classified onto, so it needs no exemplars — it is the
            // row you get when classification did not happen.
            exemplars: Vec::new(),
        }
    }
}

/// `[enrichment.ontology.navigation]` — one [`WalkPolicy`] per
/// [`QuestionKind`]. Every row defaults to the spec's pre-registered value
/// (`EPISTEMIC_INDEX.md` §2.2), so a block that omits the section, or a row,
/// gets that row. A row you write replaces the default row whole: set every
/// key you mean, because an omitted `seed` is an empty seed, not the default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NavigationPolicy {
    /// "What is this about?" — Configuration + concept Entity; Involves →
    /// Tension → Grounds; 2 hops.
    #[serde(default = "WalkPolicy::thematic")]
    pub thematic: WalkPolicy,
    /// "How does X change?" — Entity + State; Transition, Causes; 2 hops.
    #[serde(default = "WalkPolicy::trajectory")]
    pub trajectory: WalkPolicy,
    /// "Where does it disagree?" — Claim + Position; Tension, OpposesIn; 1 hop.
    #[serde(default = "WalkPolicy::tension")]
    pub tension: WalkPolicy,
    /// "Which X?" — the declared types and subtypes; no walk.
    #[serde(default = "WalkPolicy::enumeration")]
    pub enumeration: WalkPolicy,
    /// "Who is X?" — Entity by name; Involves; 1 hop.
    #[serde(default = "WalkPolicy::lookup")]
    pub lookup: WalkPolicy,
}

impl Default for NavigationPolicy {
    fn default() -> Self {
        Self {
            thematic: WalkPolicy::thematic(),
            trajectory: WalkPolicy::trajectory(),
            tension: WalkPolicy::tension(),
            enumeration: WalkPolicy::enumeration(),
            lookup: WalkPolicy::lookup(),
        }
    }
}

impl NavigationPolicy {
    /// The row for one question kind. The ONE accessor: a walker or a display
    /// asks by kind rather than naming a field.
    pub fn walk(&self, kind: QuestionKind) -> &WalkPolicy {
        match kind {
            QuestionKind::Thematic => &self.thematic,
            QuestionKind::Trajectory => &self.trajectory,
            QuestionKind::Tension => &self.tension,
            QuestionKind::Enumeration => &self.enumeration,
            QuestionKind::Lookup => &self.lookup,
        }
    }

    /// Every row in table order.
    pub fn rows(&self) -> impl Iterator<Item = (QuestionKind, &WalkPolicy)> {
        QuestionKind::ALL.iter().map(move |k| (*k, self.walk(*k)))
    }

    /// The kinds this table can actually classify onto — every row that
    /// carries at least one exemplar, in table order.
    ///
    /// A row with no exemplars gets no centroid and is unreachable by
    /// classification (it can still be selected by a caller that already
    /// knows the kind). An EMPTY result means this map declares no
    /// classifier at all, and the walker reports that rather than guessing:
    /// absence is reported, never defaulted (principle 6).
    pub fn classifiable(&self) -> Vec<(QuestionKind, &[String])> {
        self.rows()
            .filter(|(_, w)| !w.exemplars.is_empty())
            .map(|(k, w)| (k, w.exemplars.as_slice()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pre-registered table, row by row (`EPISTEMIC_INDEX.md` §2.2).
    /// Failing input: change any seed, edge, hop count or budget below.
    #[test]
    fn defaults_are_the_spec_table() {
        let n = NavigationPolicy::default();
        let t = n.walk(QuestionKind::Thematic);
        assert_eq!(
            t.seed.kinds,
            vec![AtomType::Configuration, AtomType::Entity]
        );
        assert_eq!(t.seed.entity_types, vec![EntityType::Concept]);
        assert_eq!(
            t.walk,
            vec![EdgeType::Involves, EdgeType::Tension, EdgeType::Grounds]
        );
        assert_eq!((t.hops, t.budget), (2, DEFAULT_BUDGET));

        let t = n.walk(QuestionKind::Trajectory);
        assert_eq!(t.seed.kinds, vec![AtomType::Entity, AtomType::State]);
        assert_eq!(t.walk, vec![EdgeType::Transition, EdgeType::Causes]);
        assert_eq!(t.hops, 2);

        let t = n.walk(QuestionKind::Tension);
        assert_eq!(t.seed.kinds, vec![AtomType::Claim, AtomType::Position]);
        assert_eq!(t.walk, vec![EdgeType::Tension, EdgeType::OpposesIn]);
        assert_eq!(t.hops, 1);

        let t = n.walk(QuestionKind::Enumeration);
        assert!(t.seed.declared && t.seed.kinds.is_empty() && t.walk.is_empty());
        assert_eq!(t.hops, 0);

        let t = n.walk(QuestionKind::Lookup);
        assert_eq!(t.seed.kinds, vec![AtomType::Entity]);
        assert_eq!(t.walk, vec![EdgeType::Involves]);
        assert_eq!(t.hops, 1);

        assert_eq!(n.rows().count(), QuestionKind::ALL.len());
    }

    /// A JSON document with no `navigation` key — every `ontology.json`
    /// written before this section existed — reads as the default table, and
    /// a document that carries one row keeps that row and defaults the rest.
    /// Failing input: drop `#[serde(default = …)]` from any field.
    #[test]
    fn json_round_trips_with_defaults() {
        let absent: NavigationPolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(absent, NavigationPolicy::default());

        let one_row = serde_json::json!({
            "tension": { "seed": { "kinds": ["Claim"] }, "walk": ["Tension"], "hops": 2, "budget": 3 }
        });
        let n: NavigationPolicy = serde_json::from_value(one_row).unwrap();
        assert_eq!(n.tension.seed.kinds, vec![AtomType::Claim]);
        assert_eq!((n.tension.hops, n.tension.budget), (2, 3));
        assert_eq!(n.thematic, WalkPolicy::thematic());
        assert_eq!(n.lookup, WalkPolicy::lookup());

        let text = serde_json::to_string(&n).unwrap();
        let back: NavigationPolicy = serde_json::from_str(&text).unwrap();
        assert_eq!(back, n);
    }

    /// The wire spellings are the ones the atoms and edges already carry —
    /// PascalCase `atom_type` / `edge_type` tags, snake_case entity types and
    /// question kinds. Failing input: a `rename_all` on any of the four enums.
    #[test]
    fn rows_are_written_in_the_on_disk_tags() {
        let text = serde_json::to_string(&NavigationPolicy::default()).unwrap();
        for tag in [
            "\"Configuration\"",
            "\"Entity\"",
            "\"concept\"",
            "\"Involves\"",
            "\"OpposesIn\"",
            "\"thematic\"",
            "\"enumeration\"",
        ] {
            assert!(text.contains(tag), "{tag} missing from {text}");
        }
        for k in QuestionKind::ALL {
            assert_eq!(
                serde_json::to_string(&k).unwrap().trim_matches('"'),
                k.as_str()
            );
        }
    }
}
