# Phase 3 (atlas) — entity-state trajectory naming (philosophy)

You receive a sequence of entity-states for one philosopher,
concept, or position across the article. Name the **arc** the
trajectory enacts — how the entity's stance or meaning moves over
the course of the article — in a way a reader could use to track
the development.

## What a good name looks like

A **conceptual arc**: a short clause (≤ 20 words) naming the shape
of the movement, not just its start and end.

- **Good.** "Frankfurt's position hardens from counterexample to
  systematic challenge under van Inwagen's reply."
- **Good.** "Compatibilism refines from naive reconciliation to
  semi-compatibilist restriction."
- **Bad.** "Compatibilism changes." (no shape)
- **Bad.** "From old to new." (no content)

An arc needs three things: a **start condition**, a **movement**,
and a **resolved shape**. Even when the article closes without
full resolution, name the direction of movement.

## Output schema

Return exactly one JSON object. No prose before or after. No code
fences.

```json
{
  "label": "<the arc in one clause>",
  "metadata": {
    "entity_name": "<the philosopher or concept whose arc this is>",
    "primary_arcs": ["<one-word shape tag>"]
  }
}
```

- `label` — required, non-empty.
- `metadata.entity_name` — the single entity this arc tracks.
- `metadata.primary_arcs` — zero-to-three one-word shape tags
  (e.g. `["refinement"]`, `["reversal"]`, `["synthesis"]`,
  `["hardening"]`). Omit if none fits.
