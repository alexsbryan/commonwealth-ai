// SPDX-License-Identifier: AGPL-3.0-or-later
//! Capability bands — the tier floor of `SCHEDULER_QUALITY.md` §4.1.
//!
//! §4.1 replaces the scoring product with a predicted time-to-answer,
//! and the arm that priced it found the thing that would have broken
//! had it shipped: **ranking on time alone prefers whichever node
//! answers soonest, which on every fleet is a small fast model.**
//! `Arm::PredictedTime` sent 37 of 38 household offloads to 4B laptops
//! and in `twin-hubs` never chose a hub at all.
//!
//! The product objective was not *better* at this — it merely had
//! `claim_affinity` multiplied into its score, and a self-reported
//! multiplier that happens to correlate with model size is an
//! accidental brake of exactly the family this arc keeps finding
//! (`cold_start_weight`'s 0.7 floor; a mis-rated fleet's benchmark).
//! Remove the product and the brake goes with it, because the brake
//! was never the point of the term.
//!
//! So capability has to be a **filter, not a term** — §4.1's own
//! words, "capability filters, predicted cost ranks". This module is
//! the filter. Nothing here ever adjusts a score.
//!
//! # Two hazards, and only one of them is a regression
//!
//! Both show up as "the turn was served by a small model", and
//! conflating them mis-reads the §4.1 result:
//!
//! - **Downgrade** — served by a model materially weaker than the
//!   origin's *own* local option. A real quality regression: the user
//!   would have got a better answer by not offloading at all.
//! - **Declined upgrade** — a materially stronger node was feasible
//!   and time-ranking passed it over. Not a regression against
//!   staying home, but it forfeits the reason a household bought the
//!   hub in the first place.
//!
//! In `household_evening_12` most origins are 4B laptops, so most of
//! predicted-time's laptop offloads are the *second* kind. The
//! scoreboard counts them separately for that reason.
//!
//! # Why bands are computed per decision
//!
//! An absolute threshold ("primary tier starts at 12GB") and a table
//! of model names are both stale the moment the fleet changes what it
//! runs, and neither is anything the decider measured. So:
//!
//! - the capability signal is `size_gb` off the manifest the decider
//!   is **currently holding** — probed and TTL'd, never a config
//!   table keyed on model name;
//! - the band boundary is **relative** — a ratio between candidates
//!   in this decision — so it does not rot as models get bigger, and
//!   a fleet of three 4B laptops and a fleet of three 70B servers are
//!   banded the same way;
//! - the partition is recomputed for every decision from the
//!   candidates actually visible then, so it tracks the fleet rather
//!   than describing the fleet someone had when they wrote the code.
//!
//! What this does *not* fix: `size_gb` is peer-advertised, so a peer
//! can be wrong or stale about it. That is priced, not assumed away —
//! `SimConfig::advertised_size_error` mis-states it per node, exactly
//! as `advertised_rate_error` prices the rate card.

use sovereign_core::oicp::{InferenceRequirements, LatencyClass};

/// Advertised-weight ratio at which two models stop being treated as
/// interchangeable.
///
/// **This is a classification boundary, not a weight.** §4.1's
/// no-tunable-constant rule governs the *ranking* — a coefficient in
/// a product of dimensionless multipliers is unfalsifiable fudge. A
/// band edge is falsifiable: point at a fleet and say "that put the
/// 9B in the wrong band". The existing hard gates are thresholds of
/// the same kind (`max_context`, `max_output`).
///
/// Why 2.0. The ladder the fleet actually runs is 4B → 9B → 35B, or
/// 2.8 / 6.0 / 21.0 GB at Q4 — successive ratios of 2.1× and 3.5×.
/// Below a doubling you are comparing quantizations and neighbouring
/// sizes; above it you are comparing model classes. It is relative,
/// so it stays true when the ladder moves.
///
/// It is a declared number and therefore owes a sensitivity result
/// rather than an assertion. [`bands_with_ratio`] overrides it, and
/// `the_band_edge_is_where_the_ratio_puts_it` walks the real fleet
/// sizes across 1.5× → 4× so the reader can see exactly where the
/// choice starts to matter. It matters in one place only: a
/// [`TierFloor::TopBand`] cares about band 0 and nothing else, so the
/// ratio's whole effect is **how many near-largest models join the
/// hub in the top band.**
pub const BAND_RATIO: f32 = 2.0;

/// What capability a request requires of whatever serves it.
///
/// Derived from the OICP envelope's latency class, which is where
/// `slot_policy::Workload` put it: `Route` / `Housekeep` /
/// `EnrichBulk` declare `Fast`, and `Judge` / `Synthesize` /
/// `ExtractDurable` / `Passthrough` declare `Normal`. Locally,
/// `latency_to_speed` already resolves those to the fast and primary
/// slots — so this is not a new policy, it is **the policy the local
/// slot picker has always enforced, finally applied to peers.** That
/// asymmetry is the gap: a node will not answer its own synthesis
/// turn from its 4B, then happily ship it to someone else's.
///
/// **Known seam.** `LatencyClass` conflates "how soon do I need this"
/// with "how capable must it be" — which is precisely why
/// `latency_match_score`'s `abs_diff` cannot tell a downgrade from an
/// upgrade (`oicp-types/src/scoring.rs:82`). Deriving the floor from
/// it inherits the conflation. Today the two coincide, because
/// SLOT_POLICY §3 assigns `Normal`/`Extended` to exactly the classes
/// that want the primary slot. The day a workload wants "quickly, but
/// from a big model" the floor needs its own declared field on the
/// envelope, and this derivation becomes its fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TierFloor {
    /// No capability requirement — rank on predicted cost alone. What
    /// every arm before this one did, and what production still does.
    #[default]
    None,
    /// Must be served from the most capable band currently visible.
    TopBand,
}

impl TierFloor {
    /// The floor a request declares, read off its OICP envelope.
    pub fn from_requirements(req: &InferenceRequirements) -> Self {
        match req.effective_latency_class() {
            LatencyClass::Fast => TierFloor::None,
            LatencyClass::Normal | LatencyClass::Extended => TierFloor::TopBand,
        }
    }

    /// Whether a candidate in `band` may serve this request.
    ///
    /// A candidate whose size is unknown (`None`) does **not** satisfy
    /// a floor. This is deliberately the opposite of the choice
    /// `LoadDebt` makes for an unadvertised load estimate, and the
    /// asymmetry is the point: under-charging load makes the objective
    /// optimistic about latency, which the scoreboard measures and
    /// punishes. Admitting an unmeasurable candidate to a *quality*
    /// gate creates an incentive to omit the field, and nothing
    /// downstream can detect that it happened.
    pub fn admits(self, band: Option<u32>) -> bool {
        match self {
            TierFloor::None => true,
            TierFloor::TopBand => band == Some(0),
        }
    }

    /// Whether this floor filters anything at all.
    pub fn is_binding(self) -> bool {
        !matches!(self, TierFloor::None)
    }
}

/// Partition candidate sizes into capability bands, most capable
/// first: band `0` holds the strongest models visible in this
/// decision, band `1` the next class down, and so on. `None` in,
/// `None` out — a candidate that advertises no size cannot be banded.
///
/// A band holds every model within [`BAND_RATIO`] of **that band's
/// most capable member**, and the walk is greedy from the top.
/// Comparing against the band's maximum rather than a running minimum
/// is what stops single-linkage chaining: sizes 21 / 12 / 7 / 4 are
/// each within 2× of their predecessor, so a running-minimum rule
/// would collapse a 5× spread into one band and the floor would never
/// bind. Against the band max they split `{21, 12}` / `{7, 4}`.
///
/// Returned index-parallel to `sizes`, because every caller has the
/// candidate list in hand and wants to keep its own pairing.
pub fn bands(sizes: &[Option<f32>]) -> Vec<Option<u32>> {
    // Descending by size. Ties keep input order, which only affects
    // which of two equal sizes is called the band leader — not the
    // partition.
    let mut order: Vec<usize> = (0..sizes.len()).filter(|&i| sizes[i].is_some()).collect();
    order.sort_by(|&a, &b| {
        let (sa, sb) = (sizes[a].unwrap_or(0.0), sizes[b].unwrap_or(0.0));
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut out = vec![None; sizes.len()];
    let mut band = 0u32;
    let mut band_top: Option<f32> = None;
    for idx in order {
        let size = sizes[idx].unwrap_or(0.0);
        match band_top {
            // A non-positive size carries no information; treat it as
            // unbanded rather than letting it divide by zero or lead a
            // band it cannot justify.
            _ if size <= 0.0 => continue,
            None => {
                band_top = Some(size);
            }
            Some(top) if top / size >= BAND_RATIO => {
                band += 1;
                band_top = Some(size);
            }
            Some(_) => {}
        }
        out[idx] = Some(band);
    }
    out
}

/// [`bands`] with a caller-supplied ratio, so the sensitivity of
/// [`BAND_RATIO`] is a measurement rather than a claim.
pub fn bands_with_ratio(sizes: &[Option<f32>], ratio: f32) -> Vec<Option<u32>> {
    if (ratio - BAND_RATIO).abs() < f32::EPSILON {
        return bands(sizes);
    }
    let mut order: Vec<usize> = (0..sizes.len()).filter(|&i| sizes[i].is_some()).collect();
    order.sort_by(|&a, &b| {
        let (sa, sb) = (sizes[a].unwrap_or(0.0), sizes[b].unwrap_or(0.0));
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut out = vec![None; sizes.len()];
    let mut band = 0u32;
    let mut band_top: Option<f32> = None;
    for idx in order {
        let size = sizes[idx].unwrap_or(0.0);
        match band_top {
            _ if size <= 0.0 => continue,
            None => band_top = Some(size),
            Some(top) if top / size >= ratio => {
                band += 1;
                band_top = Some(size);
            }
            Some(_) => {}
        }
        out[idx] = Some(band);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::oicp::CapabilityHint;

    /// The fleet `scenario.rs` actually runs: one 35B hub, three 9B
    /// desktops, eight 4B laptops. If this partition ever changes, the
    /// §4.1 tier-floor numbers stop being comparable.
    #[test]
    fn the_household_fleet_splits_into_hub_desktop_and_laptop_bands() {
        let sizes = vec![
            Some(21.0),
            Some(6.0),
            Some(6.0),
            Some(6.0),
            Some(2.8),
            Some(2.8),
        ];
        assert_eq!(
            bands(&sizes),
            vec![Some(0), Some(1), Some(1), Some(1), Some(2), Some(2)]
        );
    }

    /// The reason a band is measured against its own maximum and not
    /// against a running minimum. Each of these is within 2× of its
    /// predecessor, so single-linkage would call a 5.25× spread one
    /// band and the floor would admit everything.
    #[test]
    fn a_size_continuum_does_not_chain_into_one_band() {
        let sizes = vec![Some(21.0), Some(12.0), Some(7.0), Some(4.0)];
        assert_eq!(bands(&sizes), vec![Some(0), Some(0), Some(1), Some(1)]);
    }

    /// `twin_hubs`: the top band is not a singleton, which is the
    /// whole point of that fleet — the floor leaves three candidates
    /// for predicted time to rank between.
    #[test]
    fn identical_hubs_share_the_top_band() {
        let sizes = vec![Some(21.0), Some(21.0), Some(21.0), Some(2.8), Some(2.8)];
        assert_eq!(
            bands(&sizes),
            vec![Some(0), Some(0), Some(0), Some(1), Some(1)]
        );
    }

    /// Relative, not absolute: a fleet of small models bands exactly
    /// like a fleet of large ones. This is what an absolute GB
    /// threshold cannot do, and why the boundary is a ratio.
    #[test]
    fn banding_is_scale_free() {
        let small = bands(&[Some(2.8), Some(1.2), Some(1.1)]);
        let large = bands(&[Some(280.0), Some(120.0), Some(110.0)]);
        assert_eq!(small, large);
    }

    #[test]
    fn a_candidate_with_no_advertised_size_is_unbanded_and_fails_a_floor() {
        let sizes = vec![Some(21.0), None, Some(2.8)];
        assert_eq!(bands(&sizes), vec![Some(0), None, Some(1)]);
        assert!(!TierFloor::TopBand.admits(None));
        assert!(TierFloor::None.admits(None));
    }

    #[test]
    fn a_single_candidate_is_its_own_top_band() {
        assert_eq!(bands(&[Some(2.8)]), vec![Some(0)]);
        assert!(TierFloor::TopBand.admits(Some(0)));
    }

    #[test]
    fn an_empty_or_sizeless_candidate_set_bands_to_nothing() {
        assert!(bands(&[]).is_empty());
        assert_eq!(bands(&[None, None]), vec![None, None]);
    }

    /// A zero or negative advertised size is not a capability claim of
    /// any kind, and must not become a band leader.
    #[test]
    fn a_nonpositive_size_is_unbanded() {
        assert_eq!(bands(&[Some(21.0), Some(0.0)]), vec![Some(0), None]);
    }

    /// Exactly at the ratio the split happens — stated so the
    /// boundary's direction is a test rather than a reading of the
    /// comparison operator.
    #[test]
    fn the_band_edge_is_inclusive_at_the_ratio() {
        assert_eq!(bands(&[Some(4.0), Some(2.0)]), vec![Some(0), Some(1)]);
        assert_eq!(bands(&[Some(4.0), Some(2.01)]), vec![Some(0), Some(0)]);
    }

    /// The knob has to actually move the partition, or the
    /// sensitivity result it exists to produce would be a no-op.
    #[test]
    fn the_ratio_knob_moves_the_partition() {
        let sizes = vec![Some(21.0), Some(12.0)];
        assert_eq!(bands_with_ratio(&sizes, BAND_RATIO), vec![Some(0), Some(0)]);
        assert_eq!(bands_with_ratio(&sizes, 1.5), vec![Some(0), Some(1)]);
        assert_eq!(bands_with_ratio(&sizes, 3.0), vec![Some(0), Some(0)]);
    }

    /// [`BAND_RATIO`]'s sensitivity, stated on the sizes the Tier-1
    /// fleets actually run (35B / 9B / 4B at Q4 = 21.0 / 6.0 / 2.8 GB)
    /// rather than argued from the constant.
    ///
    /// A [`TierFloor::TopBand`] only ever asks "is this band 0?", so
    /// the ratio's entire influence is the size of the top band:
    ///
    /// | ratio | top band | what the floor then requires |
    /// |---|---|---|
    /// | 1.5× | hub | the 35B |
    /// | 2.0× | hub | the 35B — the shipped choice |
    /// | 3.0× | hub | the 35B |
    /// | 4.0× | hub + desktops | 35B **or** 9B |
    ///
    /// So the floor's behaviour is flat across 1.5×–3.0× and only
    /// changes past 3.5× (= 21.0/6.0), where 9B desktops become
    /// acceptable for synthesis. The shipped 2.0× sits in the middle
    /// of the flat region, which is the useful thing to know about it:
    /// the household result is not balanced on the constant.
    #[test]
    fn the_band_edge_is_where_the_ratio_puts_it() {
        let household = vec![Some(21.0), Some(6.0), Some(6.0), Some(2.8), Some(2.8)];
        let top_band = |ratio: f32| {
            bands_with_ratio(&household, ratio)
                .iter()
                .filter(|b| **b == Some(0))
                .count()
        };
        assert_eq!(top_band(1.5), 1, "1.5x: hub alone");
        assert_eq!(top_band(2.0), 1, "2.0x (shipped): hub alone");
        assert_eq!(top_band(3.0), 1, "3.0x: hub alone");
        assert_eq!(top_band(4.0), 3, "4.0x: desktops join the hub");
    }

    /// SLOT_POLICY §3, read through the envelope: the classes that
    /// resolve to the primary slot locally are the ones that carry a
    /// floor on the mesh.
    #[test]
    fn fast_class_requests_carry_no_floor_and_normal_ones_do() {
        let fast = InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Fast);
        let normal = InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Normal);
        let extended = InferenceRequirements::new()
            .with_hint(CapabilityHint::general())
            .with_latency_class(LatencyClass::Extended);

        assert_eq!(TierFloor::from_requirements(&fast), TierFloor::None);
        assert_eq!(TierFloor::from_requirements(&normal), TierFloor::TopBand);
        assert_eq!(TierFloor::from_requirements(&extended), TierFloor::TopBand);
        assert!(!TierFloor::from_requirements(&fast).is_binding());
        assert!(TierFloor::from_requirements(&normal).is_binding());
    }

    /// An absent latency class is `Normal` per OICP §8, so an envelope
    /// that declares nothing still gets the floor. The alternative —
    /// silence meaning "anything may serve this" — is the failure mode
    /// the whole module exists to close.
    #[test]
    fn an_envelope_declaring_no_latency_class_still_carries_the_floor() {
        let bare = InferenceRequirements::new();
        assert_eq!(TierFloor::from_requirements(&bare), TierFloor::TopBand);
    }
}
