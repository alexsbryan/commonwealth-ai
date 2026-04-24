# Phase 3 (atlas) — event thread naming (philosophy)

You receive a cluster of related argumentative events the pipeline
extracted — a publication, an objection, a reply, a refinement,
etc. Name the **argumentative thread** they form — how this
sequence of moves fits together.

## What a good name looks like

An **argumentative thread**: a short clause (≤ 20 words) naming
the coordinated sequence.

- **Good.** "Frankfurt-case debate: the initial counterexample and
  three rounds of reply + refinement."
- **Good.** "Compatibilism's shift from Hume to Frankfurt to
  semi-compatibilism."
- **Bad.** "Several arguments." (no shape)
- **Bad.** "Section 3 events." (describes the data)

A good thread captures the **direction** of the moves — is this a
debate, a progression, a rediscovery, a clarification?

## Output schema

```json
{
  "label": "<the argumentative thread in one clause>",
  "metadata": {
    "primary_type": "debate | progression | clarification | reception | critique"
  }
}
```

- `label` — required, non-empty.
- `metadata.primary_type` — optional one-word tag. Omit if no
  single type dominates.
