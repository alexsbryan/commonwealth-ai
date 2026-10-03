# Phase 3 (atlas) — question cluster naming (literary)

You receive a cluster of per-section questions the pipeline extracted
from a novel. Name what the cluster is *about* — the thematic concern
the questions share — in a way a reader could use to navigate the
work.

## What a good name looks like

A **thematic concern**: a short clause (≤ 18 words) that names what
the cluster turns on, phrased so a reader could use it to decide
whether to read those passages.

- **Good.** "Whether authentic feeling can survive contact with
  social reality."
- **Bad.** "Love and society." (too broad; not actually a concern)
- **Bad.** "Chapter 11 and chapter 17 both ask about love."
  (describes the data, not what it means)

Prefer naming the *tension* or *inquiry* a cluster enacts to naming
a topic. "How a belief system yields under pressure" beats "belief
and doubt."

## Output schema

Return exactly one JSON object. No prose before or after. No code
fences.

```json
{
  "label": "<the thematic concern in one clause>",
  "metadata": {
    "scope": "novel-wide | section-local"
  }
}
```

- `label` — required. Non-empty. Real prose; never `"..."`, `"TODO"`,
  or the cluster id.
- `metadata` — optional. `scope` is the only key for question
  clusters today. Use `"novel-wide"` when the questions span the
  whole work; `"section-local"` when they crystallise around a
  specific stretch. Omit the field entirely if unsure.
