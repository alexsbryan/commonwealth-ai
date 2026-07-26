// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fleets and arrival streams — the *world* a Tier-1 run happens in.
//!
//! A scenario is fully determined by its spec plus a seed: same pair,
//! same arrival stream, same answer. Nothing here depends on wall
//! time, thread scheduling, or the host's hardware.

use sovereign_core::oicp::{
    BenchmarkResult, CapabilityClaim, CapabilityHint, InferenceRequirements, LatencyClass,
    ModelStatus, ProviderManifest, ProviderModel, ShardingPrivacy, OICP_VERSION,
};

use super::rng::Rng;

/// What a node can do, in the two currencies that matter: how fast it
/// turns prompt into state, and how fast it emits tokens.
///
/// These are the numbers the sim's service-time model consumes. They
/// are also exactly the fields `BenchmarkResult` carries, which is
/// what the calibration contract (§5) means by "the service-time
/// model is fit from data the fleet already collects".
#[derive(Debug, Clone, Copy)]
pub struct Hardware {
    pub pp_tok_s: f32,
    pub tg_tok_s: f32,
}

/// One node in the simulated mesh.
#[derive(Debug, Clone)]
pub struct NodeSpec {
    pub name: String,
    /// Model this node advertises and serves from.
    pub model_id: String,
    pub size_gb: f32,
    /// Claim affinity — the capability axis. A bigger model claims a
    /// higher affinity for general work, which is what makes the hub
    /// the *capability* winner independently of how loaded it is.
    pub affinity: f32,
    pub hardware: Hardware,
    /// Turns this node's own user starts, expressed as a mean gap in
    /// seconds. `None` = this node hosts no user (a pure server).
    pub mean_arrival_gap_s: Option<f64>,
    /// Gossiped `inference_availability`. `None` = never gossiped one.
    /// F2 lives here: in production this is set from *human keyboard
    /// activity*, not from load.
    pub availability: Option<f32>,
    /// Whether this node publishes a `BenchmarkResult`.
    pub advertises_benchmark: bool,
}

impl NodeSpec {
    pub fn manifest(&self) -> ProviderManifest {
        ProviderManifest {
            oicp_version: OICP_VERSION.into(),
            provider: None,
            models: vec![ProviderModel {
                id: self.model_id.clone(),
                base_model: None,
                quantization: None,
                context_tokens: 32_768,
                status: ModelStatus {
                    available: true,
                    loaded: true,
                    estimated_tokens_per_sec: None,
                    estimated_ttft_ms: None,
                    estimated_load_time_sec: None,
                },
                size_gb: Some(self.size_gb),
                claims: vec![CapabilityClaim::new(
                    CapabilityHint::general(),
                    LatencyClass::Extended,
                    32_768,
                    4_000,
                    self.affinity,
                )],
                fingerprint: None,
            }],
            knowledge: None,
            federation: None,
            features: vec![],
        }
    }

    pub fn benchmark(&self) -> Option<BenchmarkResult> {
        self.advertises_benchmark.then(|| BenchmarkResult {
            baseline_model_id: self.model_id.clone(),
            baseline_size_gb: self.size_gb,
            pp_tok_s: self.hardware.pp_tok_s,
            tg_tok_s: self.hardware.tg_tok_s,
            measured_at: 0,
        })
    }
}

/// The shape of one request. Classes differ in what they demand and
/// in what the protocol permits doing with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RequestClass {
    /// A knowledge turn: long prompt, real answer, offloadable.
    Knowledge,
    /// Latency-`Fast` housekeeping (routing, titling). Must never
    /// offload — SLOT_POLICY §5.
    Fast,
    /// `LocalOnly` privacy posture. Must never cross the wire.
    Private,
}

impl RequestClass {
    pub fn requirements(&self) -> InferenceRequirements {
        match self {
            RequestClass::Knowledge => InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Extended)
                .with_sharding(ShardingPrivacy::MeshAllowed),
            RequestClass::Fast => InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Fast)
                .with_sharding(ShardingPrivacy::MeshAllowed),
            RequestClass::Private => InferenceRequirements::new()
                .with_hint(CapabilityHint::general())
                .with_latency_class(LatencyClass::Extended)
                .with_sharding(ShardingPrivacy::LocalOnly),
        }
    }
}

/// One arrival: who asked, when, and for what.
#[derive(Debug, Clone)]
pub struct Arrival {
    pub at_ms: u64,
    pub origin: usize,
    pub class: RequestClass,
    pub context_tokens: u32,
    pub output_tokens: u32,
}

/// A complete Tier-1 world: a fleet, the links between its nodes, and
/// the stream of work that hits it.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: String,
    pub nodes: Vec<NodeSpec>,
    /// `rtt_ms[i][j]` — round-trip time from i to j. The diagonal is
    /// unused. Feeds both the locality bonus (via `classify_rtt_ms`)
    /// and the sim's wire cost.
    pub rtt_ms: Vec<Vec<u32>>,
    pub duration_ms: u64,
    pub arrivals: Vec<Arrival>,
}

/// Fraction of arrivals that are `Fast` / `Private` rather than
/// knowledge turns. Small but non-zero: they exist so the hard
/// invariants (§5) have something to bite on in every run, not so
/// they dominate the latency statistics.
const FAST_SHARE: f64 = 0.10;
const PRIVATE_SHARE: f64 = 0.05;

/// Generate arrivals for every node with a user, up front, from the
/// seed. Generating the whole stream before the run starts is what
/// lets two arms be compared on *identical* work.
fn generate_arrivals(nodes: &[NodeSpec], duration_ms: u64, seed: u64) -> Vec<Arrival> {
    let mut out = Vec::new();
    for (idx, node) in nodes.iter().enumerate() {
        let Some(gap_s) = node.mean_arrival_gap_s else {
            continue;
        };
        // Per-node stream, so adding a node doesn't reshuffle the
        // others' arrivals — a property you want the first time an
        // arm regression turns out to be a scenario change.
        let mut rng = Rng::new(seed ^ ((idx as u64 + 1).wrapping_mul(0x0F1E_2D3C_4B5A_6978)));
        let mut t = rng.exponential(gap_s * 1000.0);
        while (t as u64) < duration_ms {
            let roll = rng.next_f64();
            let class = if roll < FAST_SHARE {
                RequestClass::Fast
            } else if roll < FAST_SHARE + PRIVATE_SHARE {
                RequestClass::Private
            } else {
                RequestClass::Knowledge
            };
            let (ctx, out_toks) = match class {
                RequestClass::Fast => (rng.range_u32(200, 900), rng.range_u32(16, 64)),
                _ => (rng.range_u32(3_000, 14_000), rng.range_u32(250, 900)),
            };
            out.push(Arrival {
                at_ms: t as u64,
                origin: idx,
                class,
                context_tokens: ctx,
                output_tokens: out_toks,
            });
            t += rng.exponential(gap_s * 1000.0);
        }
    }
    // Stable order: time, then origin. Two arrivals at the same
    // millisecond must resolve the same way on every run.
    out.sort_by_key(|a| (a.at_ms, a.origin));
    out
}

fn hub(name: &str) -> NodeSpec {
    NodeSpec {
        name: name.into(),
        model_id: "qwen3.5-35b-q4".into(),
        size_gb: 21.0,
        // The big model is the capability winner. This is the whole
        // reason the hub attracts traffic — and why F5's herding is
        // not a bug in anyone's arithmetic but a consequence of a
        // shared, deterministic argmax over a shared signal.
        affinity: 0.95,
        hardware: Hardware {
            pp_tok_s: 2_000.0,
            tg_tok_s: 25.0,
        },
        mean_arrival_gap_s: Some(600.0),
        availability: Some(1.00),
        advertises_benchmark: true,
    }
}

fn desktop(name: &str, gap_s: f64) -> NodeSpec {
    NodeSpec {
        name: name.into(),
        model_id: "qwen3.5-9b-q4".into(),
        size_gb: 6.0,
        affinity: 0.80,
        hardware: Hardware {
            pp_tok_s: 1_600.0,
            tg_tok_s: 60.0,
        },
        mean_arrival_gap_s: Some(gap_s),
        availability: Some(0.85),
        advertises_benchmark: true,
    }
}

fn laptop(name: &str, gap_s: f64) -> NodeSpec {
    NodeSpec {
        name: name.into(),
        model_id: "qwen3.5-4b-q4".into(),
        size_gb: 2.8,
        affinity: 0.60,
        hardware: Hardware {
            pp_tok_s: 1_200.0,
            // 120 tok/s — six times the hub's rate, and utterly
            // invisible to the scorer: `throughput_factor` clamps
            // everything above 20 tok/s to 1.0 (F3).
            tg_tok_s: 120.0,
        },
        mean_arrival_gap_s: Some(gap_s),
        availability: Some(0.65),
        advertises_benchmark: true,
    }
}

/// Uniform LAN: everyone 8ms from everyone.
fn lan_rtts(n: usize) -> Vec<Vec<u32>> {
    (0..n)
        .map(|i| (0..n).map(|j| if i == j { 0 } else { 8 }).collect())
        .collect()
}

/// The scenario §3's probe was reaching for: one hub, three
/// desktops, eight laptops, all deciding for themselves, all
/// gossiping into the same 10-second window.
///
/// N=12 is the number that matters. At N=2 a decider's self-observed
/// in-flight count *is* the peer's true load, so F1 cannot appear;
/// every existing test has one decider.
pub fn household_evening_12(seed: u64) -> Scenario {
    let mut nodes = vec![hub("hub")];
    for i in 0..3 {
        nodes.push(desktop(&format!("desk-{i}"), 150.0));
    }
    for i in 0..8 {
        nodes.push(laptop(&format!("lap-{i}"), 180.0));
    }
    let duration_ms = 30 * 60 * 1000;
    let arrivals = generate_arrivals(&nodes, duration_ms, seed);
    Scenario {
        name: "household-evening-12".into(),
        rtt_ms: lan_rtts(nodes.len()),
        nodes,
        duration_ms,
        arrivals,
    }
}

/// Two nodes, one decider's-eye view — the configuration every
/// existing test runs in. Included so a claim of the form "this only
/// shows up at scale" can be checked rather than asserted.
pub fn pair(seed: u64) -> Scenario {
    let nodes = vec![hub("hub"), laptop("lap-0", 120.0)];
    let duration_ms = 30 * 60 * 1000;
    let arrivals = generate_arrivals(&nodes, duration_ms, seed);
    Scenario {
        name: "pair".into(),
        rtt_ms: lan_rtts(nodes.len()),
        nodes,
        duration_ms,
        arrivals,
    }
}

/// A deliberately heterogeneous fleet: one slow-but-capable hub and
/// four fast-but-small laptops, all above the 20 tok/s reference. The
/// scorer's heterogeneity term is constant across every node here.
pub fn heterogeneous_fleet(seed: u64) -> Scenario {
    let mut nodes = vec![hub("hub")];
    for i in 0..4 {
        nodes.push(laptop(&format!("lap-{i}"), 90.0));
    }
    let duration_ms = 20 * 60 * 1000;
    let arrivals = generate_arrivals(&nodes, duration_ms, seed);
    Scenario {
        name: "heterogeneous-fleet".into(),
        rtt_ms: lan_rtts(nodes.len()),
        nodes,
        duration_ms,
        arrivals,
    }
}

/// Three *identical* hubs and eight laptops.
///
/// The point of this fleet is that the capability winner is not
/// unique: a laptop's eligible set holds three candidates whose
/// scores are equal to the last bit. That is the exact condition F5
/// describes — "deterministic argmax over a shared signal" — and the
/// only condition under which a sampling remedy has anything to
/// sample. `household_evening_12` cannot test it, because there the
/// eligible set is a singleton and every sampling policy is a no-op.
pub fn twin_hubs(seed: u64) -> Scenario {
    let mut nodes = Vec::new();
    for i in 0..3 {
        nodes.push(hub(&format!("hub-{i}")));
    }
    for i in 0..8 {
        nodes.push(laptop(&format!("lap-{i}"), 180.0));
    }
    let duration_ms = 30 * 60 * 1000;
    let arrivals = generate_arrivals(&nodes, duration_ms, seed);
    Scenario {
        name: "twin-hubs".into(),
        rtt_ms: lan_rtts(nodes.len()),
        nodes,
        duration_ms,
        arrivals,
    }
}

/// One interactive actor and one background actor sharing a fleet.
/// The isolation metric asks what the interactive actor's p95 does
/// when the background actor starts.
pub fn isolation(seed: u64) -> Scenario {
    let mut nodes = vec![hub("hub")];
    // The interactive actor: frequent, small-ish turns.
    nodes.push(desktop("interactive", 45.0));
    // The background actor: an enrichment run, hammering constantly.
    nodes.push(desktop("background", 8.0));
    for i in 0..2 {
        nodes.push(laptop(&format!("lap-{i}"), 240.0));
    }
    let duration_ms = 20 * 60 * 1000;
    let arrivals = generate_arrivals(&nodes, duration_ms, seed);
    Scenario {
        name: "isolation".into(),
        rtt_ms: lan_rtts(nodes.len()),
        nodes,
        duration_ms,
        arrivals,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_produces_the_same_arrival_stream() {
        let a = household_evening_12(7);
        let b = household_evening_12(7);
        assert_eq!(a.arrivals.len(), b.arrivals.len());
        for (x, y) in a.arrivals.iter().zip(b.arrivals.iter()) {
            assert_eq!(x.at_ms, y.at_ms);
            assert_eq!(x.origin, y.origin);
            assert_eq!(x.context_tokens, y.context_tokens);
        }
    }

    #[test]
    fn arrivals_are_ordered_and_inside_the_window() {
        let s = household_evening_12(3);
        assert!(!s.arrivals.is_empty());
        for w in s.arrivals.windows(2) {
            assert!(w[0].at_ms <= w[1].at_ms);
        }
        assert!(s.arrivals.iter().all(|a| a.at_ms < s.duration_ms));
    }

    #[test]
    fn every_class_appears_so_the_hard_invariants_have_something_to_bite_on() {
        let s = household_evening_12(11);
        for class in [
            RequestClass::Knowledge,
            RequestClass::Fast,
            RequestClass::Private,
        ] {
            assert!(
                s.arrivals.iter().any(|a| a.class == class),
                "no {class:?} arrivals generated"
            );
        }
    }

    #[test]
    fn a_pure_server_generates_no_arrivals_of_its_own() {
        let mut nodes = vec![hub("hub")];
        nodes[0].mean_arrival_gap_s = None;
        nodes.push(laptop("lap", 60.0));
        let arrivals = generate_arrivals(&nodes, 600_000, 5);
        assert!(arrivals.iter().all(|a| a.origin == 1));
    }
}
