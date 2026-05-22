# 10_multicorpus_contradicting_local

## Archetype: local corpus contradicts itself

Two local sources give different answers to the same question:
- Older encyclopedia: "8 cups a day"
- Recent medical reference: "no single rule — depends on body weight, climate, activity"

The model has two acceptable paths:
1. **Acknowledge the conflict** — present both viewpoints with
   sources, let the user decide
2. **Reach for web search** — try to resolve via current
   authoritative source

The failure mode this catches: silently picking one source and
presenting it as THE answer. That's intellectually dishonest and
common — picking the more confident-sounding source is the easy
path under uncertainty.

## What this proves

Discrimination under conflict. Production users get conflicting
hits all the time (Wikipedia gets edited; SEP has multiple
philosophical schools; personal notes contradict). A model that
silently picks a winner ships overconfident answers.

## Mock corpus

- `knowledge/water-intake-old.json` — "8 cups" recommendation
- `knowledge/water-intake-modern.json` — "depends on individual"
- Both keyed to overlapping aliases so the model gets both in
  one search

## Predicates

- No `should_call_search` constraint — both paths (acknowledge
  conflict directly, or web-search to resolve) are valid
- `final_message_satisfies` — judge confirms the response either
  presents multiple guidelines OR notes the lack of single answer

## Known sensitivities

The judge assertion is intentionally OR-shaped (acknowledge OR
note disagreement). A response that says "8 cups daily" with a
single citation fails — that's the failure mode we're catching.
