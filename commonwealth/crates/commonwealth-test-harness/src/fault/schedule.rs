// SPDX-License-Identifier: AGPL-3.0-or-later
//! Seeded fault schedules. The harness is NOT bit-deterministic (real tokio +
//! TCP), but the *schedule* — which links partition when, which nodes crash,
//! which edges get wire faults — is fully reproducible from a `seed`. A failing
//! seed replays the exact scenario.

use commonwealth_core::ids::NodeId;

use super::policy::{SharedPolicy, WireFault};

/// Tiny deterministic PRNG (splitmix64) — for picking schedule events, not
/// cryptography or statistics. Matches the existing `soak.mjs` mulberry32
/// precedent and avoids a `rand` dependency in the harness crate.
#[derive(Debug, Clone)]
pub struct DetRng {
    state: u64,
}

impl DetRng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, n)`. Returns 0 if `n == 0`.
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }
}

/// A scheduled fault, applied at the start of a specific round.
#[derive(Debug, Clone)]
pub enum FaultEvent {
    Partition { a: NodeId, b: NodeId },
    HealPartition { a: NodeId, b: NodeId },
    NodeDown { node: NodeId },
    NodeUp { node: NodeId },
    SetWire {
        observer: NodeId,
        target: NodeId,
        fault: WireFault,
    },
    ClearWire {
        observer: NodeId,
        target: NodeId,
    },
}

/// A reproducible timeline of fault events over a bounded number of rounds.
#[derive(Debug, Clone)]
pub struct FaultSchedule {
    pub seed: u64,
    pub rounds: usize,
    pub events: Vec<(usize, FaultEvent)>,
}

impl FaultSchedule {
    /// An empty schedule (no faults) over `rounds` rounds.
    pub fn empty(rounds: usize) -> Self {
        Self {
            seed: 0,
            rounds,
            events: Vec::new(),
        }
    }

    /// Apply every event scheduled for `round` to the shared policy. Node
    /// crash/restart only flip the policy's reachability — the harness pairs
    /// them with the real `SimulatedNode` shutdown/restart.
    pub fn apply_round(&self, round: usize, policy: &SharedPolicy) {
        let mut pol = policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (r, ev) in &self.events {
            if *r != round {
                continue;
            }
            match ev {
                FaultEvent::Partition { a, b } => pol.partition(*a, *b),
                FaultEvent::HealPartition { a, b } => pol.heal(*a, *b),
                FaultEvent::NodeDown { node } => pol.mark_down(*node),
                FaultEvent::NodeUp { node } => pol.mark_up(*node),
                FaultEvent::SetWire {
                    observer,
                    target,
                    fault,
                } => pol.set_wire(*observer, *target, fault.clone()),
                FaultEvent::ClearWire { observer, target } => pol.clear_wire(*observer, *target),
            }
        }
    }

    /// Generate a mixed partition / crash / wire-fault schedule from `seed`.
    /// The same `(seed, nodes, rounds)` always produces the same schedule, so
    /// a `seed=…` line from a failed run replays the exact scenario.
    pub fn generate(seed: u64, nodes: &[NodeId], rounds: usize) -> Self {
        let mut rng = DetRng::new(seed);
        let mut events = Vec::new();
        if nodes.len() >= 2 && rounds >= 4 {
            for round in 0..rounds {
                // ~1-in-4 rounds gets a fault event.
                if rng.below(4) != 0 {
                    continue;
                }
                let a = nodes[rng.below(nodes.len())];
                let mut b = nodes[rng.below(nodes.len())];
                if a == b {
                    b = nodes[(rng.below(nodes.len()) + 1) % nodes.len()];
                }
                if a == b {
                    continue;
                }
                let last = rounds.saturating_sub(1);
                match rng.below(3) {
                    0 => {
                        events.push((round, FaultEvent::Partition { a, b }));
                        let heal = (round + 1 + rng.below(3)).min(last);
                        events.push((heal, FaultEvent::HealPartition { a, b }));
                    }
                    1 => {
                        events.push((round, FaultEvent::NodeDown { node: a }));
                        let up = (round + 1 + rng.below(3)).min(last);
                        events.push((up, FaultEvent::NodeUp { node: a }));
                    }
                    _ => {
                        events.push((
                            round,
                            FaultEvent::SetWire {
                                observer: a,
                                target: b,
                                fault: WireFault {
                                    drop_conn: true,
                                    ..Default::default()
                                },
                            },
                        ));
                        let clear = (round + 1 + rng.below(2)).min(last);
                        events.push((clear, FaultEvent::ClearWire { observer: a, target: b }));
                    }
                }
            }
        }
        Self {
            seed,
            rounds,
            events,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_schedule() {
        let nodes: Vec<NodeId> = (1..=5).map(NodeId::from_u128).collect();
        let a = FaultSchedule::generate(42, &nodes, 20);
        let b = FaultSchedule::generate(42, &nodes, 20);
        assert_eq!(a.events.len(), b.events.len());
        // Same rounds + same variant ordering => reproducible.
        for ((ra, _), (rb, _)) in a.events.iter().zip(b.events.iter()) {
            assert_eq!(ra, rb);
        }
        // Different seed => (almost surely) a different timeline.
        let c = FaultSchedule::generate(43, &nodes, 20);
        assert!(a.events.len() != c.events.len() || {
            a.events
                .iter()
                .zip(c.events.iter())
                .any(|((ra, _), (rc, _))| ra != rc)
        });
    }

    #[test]
    fn detrng_is_deterministic() {
        let mut x = DetRng::new(7);
        let mut y = DetRng::new(7);
        assert_eq!(x.next_u64(), y.next_u64());
        assert!((0..10).all(|_| x.below(3) < 3));
    }
}
