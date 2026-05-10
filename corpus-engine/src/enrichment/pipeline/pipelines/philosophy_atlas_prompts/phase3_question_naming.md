# Phase 3 (atlas) — question cluster naming (philosophy)

You receive a cluster of per-section questions the pipeline
extracted from a philosophy article. Name what the cluster is
*about* — the philosophical inquiry the questions share — in a way
a reader could use to navigate the debate.

## What a good name looks like

A **philosophical inquiry**: a short clause (≤ 18 words) that
names what the cluster turns on, phrased so a reader could use
it to decide whether to read those passages.

- **Good.** "Whether moral responsibility presupposes the ability
  to do otherwise."
- **Bad.** "Free will." (too broad; not an inquiry)
- **Bad.** "Questions about PAP in sections 3-5." (describes the
  data, not what's at stake)

Prefer naming the *dialectical problem* or *conceptual puzzle*
over naming a topic. "How compatibilism accommodates Frankfurt
cases" beats "the Frankfurt debate."

## Output schema

Return exactly one JSON object. No prose before or after. No code
fences.

```json
{
  "label": "<the inquiry in one clause>",
  "metadata": {
    "scope": "article-wide | section-local"
  }
}
```

- `label` — required, non-empty. Real prose; never `"..."`, the
  cluster id, or a placeholder.
- `metadata.scope` — `"article-wide"` when the inquiry spans the
  whole article; `"section-local"` when it concentrates in a
  subsection. Omit if unsure.
