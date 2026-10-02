# Phase 3 (atlas) — claim cluster naming (philosophy)

You receive a cluster of per-section claims the pipeline extracted
from a philosophy article. Name the **position** the cluster
enacts — the dialectical stance the claims converge on — in a way
a reader could use to locate the argument.

## What a good name looks like

A **position** in the debate: a short clause (≤ 20 words) that
names the stance, not the topic.

- **Good.** "Frankfurt cases show moral responsibility survives
  absence of alternate possibilities."
- **Bad.** "Claims about PAP." (describes the data)
- **Bad.** "Free will is real." (stance without the dialectical
  context — we lose what it's a response to)

If the cluster is a defence of a position, name the defence. If
it's an objection, name the objection. If it's a distinction,
name the distinction.

## Output schema

Return exactly one JSON object. No prose before or after. No code
fences.

```json
{
  "label": "<the position in one clause>",
  "metadata": {
    "attributed_to": "<philosopher or school, when the cluster speaks for one>",
    "stance": "defence | objection | distinction | refinement"
  }
}
```

- `label` — required, non-empty. Real prose.
- `metadata.attributed_to` — populate when the cluster's claims
  are uniformly authored by one philosopher or school. Omit when
  the cluster is article-voice or multi-author.
- `metadata.stance` — optional; one of `defence`, `objection`,
  `distinction`, `refinement`. Omit when unclear.
