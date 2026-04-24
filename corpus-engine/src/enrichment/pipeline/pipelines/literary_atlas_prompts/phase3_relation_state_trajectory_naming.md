# Phase 3 (atlas) — relation-state trajectory naming (literary)

You receive a cluster of relational-state descriptions about a pair
(or small group) of characters. Name the **relational dynamic** the
cluster traces — a movement in the relationship itself, not in either
participant's solo state.

## What a good name looks like

A **relational arc**: the trajectory of the interaction. The
participants appear in the label; movement words anchor the clause.

- **Good.** "Jane–Rochester's power inversion from employer /
  dependent to equal partners."
- **Good.** "Anna–Vronsky's slide from mutual passion into
  asymmetric dependency and mutual claustrophobia."
- **Bad.** "Jane and Rochester." (no dynamic)
- **Bad.** "Rochester's attachment." (an entity state, not a
  relation)

Emphasise the *emergent* quality — properties of the bond that don't
live in either participant's individual trajectory (power, mutual
deception, claustrophobia, shared self-construction). These are the
properties spec §2.6 is the atom type for.

## Output schema

```json
{
  "label": "<relational arc in one clause>",
  "metadata": {
    "participants": "name1 × name2 [× name3 …]"
  }
}
```

- `label` — required.
- `metadata.participants` — required. Canonical participant names
  joined by ` × ` (space-times-space). Ordered when the relationship
  is asymmetric (mentor first, employer first); otherwise
  alphabetical. Downstream Phase 5 uses this to tie the trajectory to
  the canonical Relation atom.
