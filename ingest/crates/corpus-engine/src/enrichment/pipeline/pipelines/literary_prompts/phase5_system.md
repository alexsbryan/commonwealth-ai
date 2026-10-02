# Phase 5 — grounded position extraction

You are given one canonical concern and a cluster of paragraph-level
passages from the work. Extract the **argument-through-narrative**
the work makes about that concern using this cluster as evidence.

## What a position is

A **claim the novel makes through its plot**, stated as something
that could be debated. Not a plot summary.

- Plot summary (reject): "Anna has an affair with Vronsky and their
  relationship deteriorates."
- Position (want): "Anna's trajectory argues that authentic passion
  which defies the social order consumes itself — not because society
  punishes it, but because the defiance itself becomes the
  relationship's identity, displacing the original feeling."

Ground every position in the supplied passages. A position the
passages cannot evidence is a hallucination.

## Constraints

- One position per call.
- `grounding`: 2–5 entries, each citing a specific chunk by its
  supplied `chunk_id` + a short `summary` of what that passage
  contributes. Every `chunk_id` you cite MUST appear in the supplied
  cluster — do not invent ids.
- Optional `extensions.character_voice`: which character's arc
  carries this argument (e.g. `"Anna"`, `"Levin"`). Skip if the
  concern runs structural/narrator-wide.

## Output schema (strict JSON)

```json
{
  "position_text": "...",
  "grounding": [
    {"chunk_id": 1234, "section_id": "sec_0012", "summary": "..."}
  ],
  "extensions": {"character_voice": "Anna"}
}
```

Respond with JSON only.
