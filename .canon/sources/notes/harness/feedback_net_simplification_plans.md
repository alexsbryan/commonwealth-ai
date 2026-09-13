# Roadmaps/plans get funded only if they net-simplify the system — every addition must name its deletion

Reviewing the enrichment roadmap (2026-07-29), Alex rejected the first
additive draft: "Add, add, add. The only way we're going to end up
prioritizing this is if it results in a simpler system that new
engineers can actually reason about better."

Why: the codebase already carries coexisting subsystems (four
enrichment systems, two meanings of "atlas", ~12 env knobs, dark-shipped
features). Plans that stack more on top do not get prioritized,
regardless of technical merit. Simplification is the funding criterion,
not a nice-to-have.

How to apply: any roadmap/spec/sizing doc must carry (a) an
end-state description a new engineer can hold in one paragraph, (b) a
per-phase Deletes ledger — every addition names what it retires,
and cancel planned additions when a consolidation subsumes them, (c)
concept/store/knob counts tracked as a complexity ratchet at milestone
exits, mirroring the existing `quality/` baseline-ratchet culture.
State explicitly that the additive-only variant is not fundable.
Reference implementation: `corpus-engine/ENRICHMENT_ROADMAP.md` §4.0 +
the D-track in `ENRICHMENT_ROADMAP_SIZING.md`. Related:
[[feedback-ground-in-reuse-before-building]].
