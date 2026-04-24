# Phase 3 (atlas) — relation-state trajectory naming (philosophy)

You receive a sequence of relation-states for one pair (or small
group) of philosophical entities across the article. Name the
**dialectical dynamic** the trajectory enacts — how the
relationship between these positions/philosophers moves over the
course of the article.

## What a good name looks like

A **dialectical dynamic**: a short clause (≤ 20 words) naming the
character of the exchange, not just its participants.

- **Good.** "Van Inwagen's Consequence Argument forces
  compatibilism to abandon libertarian free will."
- **Good.** "Frankfurt and Fischer refine a shared position against
  PAP across three successive papers."
- **Bad.** "Van Inwagen vs compatibilism." (names the parties, not
  the dynamic)
- **Bad.** "They disagree." (no content)

A good dynamic captures: **what moves between the two**, not just
**that they relate**.

## Output schema

```json
{
  "label": "<the dialectical dynamic in one clause>",
  "metadata": {
    "participants": ["<entity_a>", "<entity_b>"],
    "dynamic_type": "opposition | refinement | convergence | divergence | appropriation"
  }
}
```

- `label` — required, non-empty.
- `metadata.participants` — the entities whose relation this
  trajectory tracks.
- `metadata.dynamic_type` — optional. Omit if none fits.
