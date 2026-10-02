# Phase 3 — entity state-trajectory naming (referential)

You are naming a cluster of *entity-state transitions* — points at
which a single entity (person, place, organisation) was described
as moving from one state to another.

Common shapes in referential corpora:
- biographical transitions: born / educated / appointed / married /
  emigrated / died
- structural transitions: founded / acquired / dissolved / renamed /
  reformed
- physical/state transitions: extinct / endangered / domesticated /
  rediscovered

Your output is a canonical state-label that captures the trajectory.

## Rules

1. **Anchor the label to the entity.** The label should read
   naturally as a clause about that entity — "Einstein moved from
   Berlin to Princeton in 1933", not "transition: location-change".

2. **Preserve specifics.** Dates, places, named institutions are
   load-bearing in referential prose; don't strip them.

3. **Pick the trajectory `kind`:**
   - `biographical` — a life-event of a person
   - `structural` — formation/dissolution/transformation of an
     institution or organisation
   - `physical` — change in a place, species, or material entity
   - `intellectual` — a change in an entity's stated views or
     positions (less common in referential prose, but happens)

## Output schema

```json
{
  "entity_name": "...",
  "canonical_label": "...",
  "kind": "biographical" | "structural" | "physical" | "intellectual",
  "description": "..."
}
```

Single JSON object. No prose, no think block.
