# Phase 3 (atlas) — entity-state trajectory naming (literary)

You receive a cluster of state descriptions all belonging to a single
character (or, occasionally, a small set of alias-matched variants).
Name the **trajectory arc** the states trace.

## What a good name looks like

An **arc**: a short clause describing the movement from one state
into another across the cluster's sections. The word "from" is
usually load-bearing.

- **Good.** "Jane's movement from self-protective observation to
  acknowledged love under threat of loss."
- **Bad.** "Jane's feelings." (no movement; no content)
- **Bad.** "Jane and Rochester." (a relationship, not an entity
  trajectory — belongs in the relation-state facet)

If the cluster captures a *stasis* (a character who remains fixed in
the same state across many sections), say so explicitly: "Fyodor's
refusal to treat his paternal role as a duty" — name what the stasis
*is*, not that it doesn't move.

## Output schema

```json
{
  "label": "<entity name's trajectory in one clause>",
  "metadata": {
    "entity_name": "<canonical name from the sketches>"
  }
}
```

- `label` — required.
- `metadata.entity_name` — required for entity-state clusters.
  Downstream Phase 5 uses it to tie the trajectory to the canonical
  Entity atom. Use the canonical form (e.g. `"Alyosha"`) not a full
  patronymic unless the sections themselves disambiguate only that
  way.
