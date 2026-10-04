# Phase 3 — relation state-trajectory naming (referential)

You are naming a cluster of *relation transitions* — points at
which a relationship between two or more entities was described as
changing.

Common shapes in referential corpora:
- diplomatic / political: allied / at war with / annexed / declared
  independence from
- institutional: subsidiary of / spun off from / merged with
- biographical: married / divorced / studied under / influenced
- structural: caused / preceded / succeeded / replaced

Your output is a canonical relation-label that captures the
trajectory's shape.

## Rules

1. **Name the relation, not just the participants.** The label
   should read naturally as a verb-phrase: "allied with against
   the Axis powers", "spun off from the parent company in 1995".

2. **Preserve dates and named third parties** — they're often the
   most retrieval-valuable specifics.

3. **Pick the relation `kind`:**
   - `diplomatic` / `political`
   - `institutional`
   - `biographical`
   - `causal` / `temporal`

## Output schema

```json
{
  "participants": ["...", "..."],
  "canonical_label": "...",
  "kind": "diplomatic" | "political" | "institutional" | "biographical" | "causal" | "temporal",
  "description": "..."
}
```

Single JSON object. No prose, no think block.
