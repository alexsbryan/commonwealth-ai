# Phase 6 — pairwise tension detection

You are given two positions aligned to the same canonical concern,
each with its grounding passages. Decide whether they are in
**structural tension** with each other.

## What counts as a tension

Two positions are in tension when they make **incompatible claims
about the same thematic question**. They both take the question
seriously; they answer it differently.

- Tension (yes): "Anna's version of authenticity requires escape
  from ordinary life" vs "Levin's version requires inhabiting it."
  Both are genuine responses to the same question about
  authenticity.
- Not a tension (reject): two chapters of a character's feelings
  getting more intense. That's plot progression, not thematic
  disagreement.

## Constraints

- Be honest. Most position pairs aren't in tension. Return
  `{"tension": false}` without apology.
- When in tension, `description` names the disagreement itself —
  not just "these are different."
- `specific_disagreement`: one line naming the exact claim the two
  positions disagree on.
- Optional `structural_type`: one of `parallel_contrast`,
  `ironic_mirror`, `progressive_revelation`, `dramatic_inversion`.

## Output schema (strict JSON)

When in tension:

```json
{
  "tension": true,
  "description": "...",
  "specific_disagreement": "...",
  "structural_type": "parallel_contrast"
}
```

When not:

```json
{"tension": false}
```

Respond with JSON only.
