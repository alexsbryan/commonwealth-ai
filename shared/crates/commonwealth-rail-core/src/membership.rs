// SPDX-License-Identifier: AGPL-3.0-or-later
//! Membership — the seed plus two act kinds, in one function (ARCH §10.6).
//!
//! `RING_APPLICATIONS.md` "Amendment 2026-09-18" is the design; this is its
//! one membership function, the piece `admit` calls and "the piece most likely
//! to be replaced", with the permutation property as its contract. Nothing
//! else on the journal changes membership. `p2panda-auth` was read first (its
//! strong-removal rule 4 — invalidation is transitive over dependents — is
//! what the walk below gets for free; its absence of a `Remove{key,
//! through_seq}` cut is what makes ours novel).
//!
//! The rule, in two layers:
//!
//! 1. **The void set, computed first and commutatively** (today's rule,
//!    carried forward): the targets of [`RailAct::Correct`]s whose signer is
//!    seed-bound or named by a non-voided [`RailAct::Admit`] — a monotone
//!    fixpoint, so voiding an `Admit` unbinds its key and that key's own
//!    corrections stop voiding. Known limit of the replaceable rule, stated
//!    rather than discovered: a key whose `Admit` is voided in the SAME batch
//!    as its corrections may still land those voids (one pass of the fixpoint
//!    cannot see the other's removal). The same layer binds structurally, so
//!    a key that a `Remove` cut (rather than an un-admit) can still land
//!    voids — a candidate future leg.
//! 2. **One walk in the rail's order** `(ts_unix, actor, seq, id)` over the
//!    survivors: an act counts iff its signer holds standing at the act's
//!    position — the seed's keys to start, plus keys admitted by counting
//!    `Admit`s, minus keys cut by counting `Remove`s. This one sentence is
//!    the cascade (void one `Admit` and everything that key admitted falls —
//!    their admissions were signed by a key that no longer holds), the
//!    no-cuts-back rule (a removed key's later `Remove` does not count), and
//!    the mutual-removal resolver (the rail's order picks one — the
//!    replaceable place).
//!
//! The permutation contract: the walk sorts, so every permutation of the op
//! set produces a byte-identical [`Membership`], gaps included.
//! `ra-membership-is-order-free`'s five legs pin exactly that.

use std::collections::{BTreeMap, BTreeSet};

use oplog_types::Op;

use crate::admit::derived_id;
use crate::{OpId, Person, RailAct, Roster, SignedOp};

/// What one fold of the membership acts concludes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Membership {
    /// key → person, from the seed and every counting `Admit`. CUMULATIVE: a
    /// cut key keeps its binding — names stay stable and every past act keeps
    /// the person it always had (leg 4) — it only leaves [`Self::standing`].
    pub bindings: BTreeMap<String, Person>,
    /// Who holds standing NOW: the seed plus counting `Admit`s, minus
    /// counting `Remove`s.
    pub standing: BTreeSet<String>,
    /// Derived ids of the acts that count: the signer held standing at the
    /// act's position in the rail's order. Everything else is held and
    /// reported, never counted.
    pub counted: BTreeSet<OpId>,
    /// The void set the walk applied — the targets of counting corrections.
    pub voided: BTreeSet<OpId>,
}

impl Membership {
    /// Whether this act counts, by derived id.
    pub fn counts(&self, op: &Op<SignedOp>) -> bool {
        self.counted.contains(&derived_id(op))
    }

    /// The person an act's actor resolves to, through the record.
    pub fn person_for(&self, actor: &str) -> Option<&Person> {
        self.bindings.get(actor)
    }
}

/// The one membership function. `ops` must already be authenticated (the
/// caller's verifier judged the signatures); everything else is decided
/// here, and decided the same way for every arrival order.
pub fn membership<'a>(
    ops: impl IntoIterator<Item = &'a Op<SignedOp>>,
    seed: &Roster,
) -> Membership {
    let ops: Vec<&Op<SignedOp>> = ops.into_iter().collect();

    // ── layer 1: the void set (commutative; today's rule, carried) ────
    //
    // Targets of corrections whose signer is bound by the seed or by a
    // non-voided Admit. Monotone: voids only grow, so the loop terminates,
    // and "undoing an undo is a new act" keeps the original voided.
    let mut voided: BTreeSet<OpId> = BTreeSet::new();
    loop {
        let mut bound: BTreeSet<&str> = seed
            .members
            .values()
            .flatten()
            .map(String::as_str)
            .collect();
        for op in &ops {
            if let RailAct::Admit { key, .. } = &op.kind.act {
                if !voided.contains(&derived_id(op)) {
                    bound.insert(key);
                }
            }
        }
        let mut grew = false;
        for op in &ops {
            if let RailAct::Correct { corrects, .. } = &op.kind.act {
                if bound.contains(op.actor.as_str()) && voided.insert(corrects.clone()) {
                    grew = true;
                }
            }
        }
        if !grew {
            break;
        }
    }

    // ── layer 2: one walk in the rail's order over the survivors ──────
    let mut order: Vec<&Op<SignedOp>> = ops
        .iter()
        .copied()
        .filter(|op| !voided.contains(&derived_id(op)))
        .collect();
    order.sort_by_key(|op| (op.ts_unix, op.actor.clone(), op.kind.seq, derived_id(op)));

    let mut bindings: BTreeMap<String, Person> = seed
        .members
        .iter()
        .flat_map(|(person, keys)| {
            keys.iter()
                .map(move |k| (k.clone(), person.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    let mut live: BTreeMap<String, Person> = bindings.clone();
    let mut counted: BTreeSet<OpId> = BTreeSet::new();
    for op in order {
        if !live.contains_key(&op.actor) {
            continue;
        }
        counted.insert(derived_id(op));
        match &op.kind.act {
            RailAct::Admit { person, key } => {
                tracing::debug!(
                    target: "rail:membership",
                    key = %key,
                    person = %person,
                    "membership: standing gained"
                );
                bindings.insert(key.clone(), person.clone());
                live.insert(key.clone(), person.clone());
            }
            RailAct::Remove { key, .. } => {
                tracing::debug!(target: "rail:membership", key = %key, "membership: standing lost");
                live.remove(key);
            }
            _ => {}
        }
    }

    Membership {
        bindings,
        standing: live.into_keys().collect(),
        counted,
        voided,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_support::{key, record, signed};
    use crate::{actor_of, Ed25519Verifier};

    fn seed_of(seeds: &[(u8, &str)]) -> Roster {
        let mut m = BTreeMap::new();
        for (k, name) in seeds {
            m.insert(Person::from(*name), vec![actor_of(&key(*k))]);
        }
        Roster::new(m)
    }

    fn admit_act(by: u8, ts: i64, seq: u64, person: &str, joining: u8) -> Op<SignedOp> {
        signed(
            &key(by),
            ts,
            seq,
            RailAct::Admit {
                person: Person::from(person),
                key: actor_of(&key(joining)),
            },
        )
    }

    fn remove_act(by: u8, ts: i64, seq: u64, leaving: u8, through_seq: u64) -> Op<SignedOp> {
        signed(
            &key(by),
            ts,
            seq,
            RailAct::Remove {
                key: actor_of(&key(leaving)),
                through_seq,
            },
        )
    }

    fn void(by: u8, ts: i64, seq: u64, target: &Op<SignedOp>) -> Op<SignedOp> {
        signed(
            &key(by),
            ts,
            seq,
            RailAct::Correct {
                corrects: derived_id(target),
                replacement: None,
            },
        )
    }

    /// **Leg 1 — the permutation property, at membership level.** The rail's
    /// order is content-derived, so arrival order must not reach the answer:
    /// every permutation of a set with a mutual removal and an Admit racing
    /// its own void produces the identical Membership. Watched red against a
    /// fold that walks arrival order (the pre-registration's named defect).
    #[test]
    fn every_permutation_of_a_membership_set_agrees() {
        let seed = seed_of(&[(1, "alex")]);
        let admit_dee = admit_act(1, 104, 2, "dee", 4);
        let ops = vec![
            admit_act(1, 100, 0, "bo", 2),
            admit_act(1, 101, 1, "cy", 3),
            // A mutual removal: bo and cy cut each other.
            remove_act(2, 102, 0, 3, 1),
            remove_act(3, 103, 0, 2, 0),
            // An Admit racing its own void.
            admit_dee.clone(),
            signed(
                &key(1),
                105,
                3,
                RailAct::Correct {
                    corrects: derived_id(&admit_dee),
                    replacement: None,
                },
            ),
            signed(&key(2), 106, 1, record("x")),
        ];
        let base = membership(ops.iter(), &seed);
        // Every arrival order is the same answer.
        for rotation in 1..ops.len() {
            let mut rotated: Vec<&Op<SignedOp>> = ops.iter().collect();
            rotated.rotate_left(rotation);
            assert_eq!(
                membership(rotated, &seed),
                base,
                "rotation {rotation} changed the answer"
            );
        }
        // And the two named conflict cases resolved by the rail's order: the
        // mutual removal's later cut does not count (its signer had fallen),
        // and the voided Admit never admitted dee.
        assert!(
            base.counts(&ops[0]),
            "bo's Admit-of-cy counts first in order"
        );
        assert!(
            !base.counted.contains(&derived_id(&ops[3])),
            "cy's counter-cut does not"
        );
        assert!(
            !base.bindings.contains_key(&actor_of(&key(4))),
            "dee fell with the void"
        );
    }

    /// **Leg 2 — void one Admit, and every key it transitively admitted
    /// falls** (p2panda strong-removal rule 4). Watched red against a fold
    /// that voids only the direct Admit (the pre-registration's named
    /// defect).
    #[test]
    fn voiding_an_admit_drops_what_it_transitively_admitted() {
        let seed = seed_of(&[(1, "alex")]);
        let stranger = admit_act(1, 100, 0, "stranger", 4);
        let stranger_admits_p = admit_act(4, 101, 0, "p", 5);
        let undo = signed(
            &key(1),
            102,
            1,
            RailAct::Correct {
                corrects: derived_id(&stranger),
                replacement: None,
            },
        );
        let m = membership([&stranger, &stranger_admits_p, &undo], &seed);
        assert!(
            !m.bindings.contains_key(&actor_of(&key(4))),
            "the stranger fell with their Admit"
        );
        assert!(
            !m.bindings.contains_key(&actor_of(&key(5))),
            "and every key they admitted fell with them"
        );
    }

    /// **Leg 3 — voiding a Remove restores the member and admits what they
    /// wrote while removed.** The void takes the Remove out of the walk, so
    /// the removed window never existed. Watched red against a restore that
    /// skips the in-between writes.
    #[test]
    fn voiding_a_remove_restores_the_member_and_their_in_between_writes() {
        let seed = seed_of(&[(1, "alex"), (2, "bo")]);
        let cut = remove_act(1, 100, 2, 2, 1);
        let while_out = signed(&key(2), 101, 2, record("written-while-out"));
        let undo = signed(
            &key(1),
            102,
            3,
            RailAct::Correct {
                corrects: derived_id(&cut),
                replacement: None,
            },
        );
        let m = membership([&cut, &while_out, &undo], &seed);
        assert!(
            m.standing.contains(&actor_of(&key(2))),
            "voiding the Remove restores the member"
        );
        assert!(
            m.counts(&while_out),
            "and what they wrote while removed counts"
        );
    }

    /// The goodhart leg of the pre-registration, at this level: a function
    /// that ignores Admit and Remove passes the permutation test perfectly.
    /// These assertions are what prove the acts do anything at all.
    #[test]
    fn the_membership_acts_do_something() {
        let seed = seed_of(&[(1, "alex")]);
        let add = admit_act(1, 100, 0, "bo", 2);
        let write = signed(&key(2), 101, 0, record("bo-writes"));
        let cut = remove_act(1, 102, 1, 2, 0);
        let after_cut = signed(&key(2), 103, 1, record("after-the-cut"));
        let m = membership([&add, &write, &cut, &after_cut], &seed);
        assert!(m.counts(&write), "bo counts while admitted");
        assert!(
            !m.counts(&after_cut),
            "and not after the cut — position-scoped standing"
        );
        let _ = Ed25519Verifier;
    }

    /// **The permutation contract at breadth** (leg 1's battery): sixty-four
    /// seeded shuffles of a set carrying every interesting pair — a mutual
    /// removal, an Admit racing its own void, a re-Admit after a cut — and
    /// every shuffle folds identically. The seed is fixed so the battery is
    /// deterministic; the assertion is the CONTRACT (arrival never reaches
    /// the answer), and the shuffles are the arrival orders a live room
    /// produces.
    #[test]
    fn every_interleaving_of_a_membership_set_folds_identically() {
        let seed = seed_of(&[(1, "alex")]);
        let raced = admit_act(1, 104, 2, "dee", 4);
        let ops = vec![
            admit_act(1, 100, 0, "bo", 2),
            admit_act(1, 101, 1, "cy", 3),
            remove_act(2, 102, 0, 3, 1),
            remove_act(3, 103, 0, 2, 0),
            raced.clone(),
            void(1, 105, 3, &raced),
            admit_act(1, 106, 3, "bo", 2),
            signed(&key(2), 107, 1, record("bo-writes")),
            remove_act(1, 108, 4, 2, 1),
        ];
        let base = membership(ops.iter(), &seed);
        let mut state: u64 = 0x5eed;
        for round in 0..64u64 {
            let mut order: Vec<&Op<SignedOp>> = ops.iter().collect();
            // A fixed-seed LCG shuffle — deterministic, no new dependency.
            for i in (1..order.len()).rev() {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let j = (state >> 33) as usize % (i + 1);
                order.swap(i, j);
            }
            assert_eq!(
                membership(order.iter().copied(), &seed),
                base,
                "interleaving {round} changed the answer — arrival reached the fold"
            );
        }
    }
}
