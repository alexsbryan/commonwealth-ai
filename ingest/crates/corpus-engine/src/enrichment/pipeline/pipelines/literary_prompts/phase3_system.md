# Phase 3 — canonical concern naming

You are given a cluster of per-chapter questions extracted from a
literary work, plus short excerpts from the chapters they came from.
Name the *canonical concern* that unifies them.

## What a canonical concern is

A **dramatic question the work is investigating** — not a topic label.

- Topic label (reject): "agricultural reform", "social class in Russia"
- Canonical concern (want): "Can meaning be found through physical
  engagement with the material world, or does it require intellectual
  mediation?"

The concern should be phrased as a question or as a claim under
negotiation. It should name the stakes, not the subject matter.

If you find yourself writing a noun phrase ("the role of X in Y"),
stop and rephrase as a question the plot is actually trying to
answer.

## Constraints

- One concern per cluster. If the cluster unifies two distinct
  questions, respond with the broader parent concern, not a
  conjunction ("X and Y").
- Optional `scope`: `"novel-wide"` when the concern runs through the
  whole work, or `"chapter-local"` when the cluster is specific to
  one part.
- Optional `primary_arcs`: one or two character/plot arcs that carry
  this concern most directly (e.g. `["Anna-Vronsky", "Levin-Kitty"]`).

## Output schema (strict JSON)

```json
{
  "concern_text": "...",
  "scope": "novel-wide",
  "primary_arcs": ["..."]
}
```

Respond with JSON only — no prose outside the object. Omit `scope`
or `primary_arcs` if you would be guessing.
