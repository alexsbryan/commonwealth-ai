# Phase 6 — holistic fault-line classifier (philosophy atlas)

You are reading a small philosophical corpus, broken down into named
positions and the claims attributed to each. Identify the load-bearing
fault lines between positions.

A *fault line* is a place where two distinct named positions take
incompatible stances on the same underlying question — the kind of
disagreement the corpus is *navigating*, not just two positions
co-existing.

Distinguish fault lines from these neighbours (do NOT emit them):

- *Internal coherence* — claims that elaborate or refine one
  position. Same position; not a tension.
- *Convergence* — two positions that broadly agree on a question.
  Aligned, not in tension.
- *Intra-school refinement* — two figures within the same broader
  school disagreeing on a subordinate point. At the level the corpus
  is *navigating*, both belong to one position.

Name each side using a label from the lexicon you'll be shown.
**When both a school/doctrine label (e.g. an `-ism`, an `-ian`
school, a `... ethics`) and a proponent name pick out the same
position, prefer the school/doctrine label** — that names the
*position*, while a proponent is one of its bearers.

## How to answer

First, in 2-4 short paragraphs, work through how the positions in
this corpus relate. Tell me what you notice — which contrasts are
between distinct positions, which look like internal elaboration,
which converge, which are refinements within one school. Use the
corpus's own vocabulary.

Then, on a new line, output the structured JSON. Begin the JSON with
`{` and emit nothing after the closing `}`.

```
{
  "fault_lines": [
    {
      "position_a": "<a label from the lexicon>",
      "position_b": "<a label from the lexicon>",
      "crux": "<one-sentence question the disagreement turns on>"
    }
  ]
}
```

If you find no genuine between-position fault lines, end with
`{"fault_lines": []}`.
