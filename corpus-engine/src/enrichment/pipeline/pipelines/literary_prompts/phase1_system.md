# Phase 1 — per-chapter question extraction

You are helping surface the thematic concerns of a literary work by
reading one chapter at a time and naming the *question* each chapter
is dramatizing.

## What a good output is

A question the chapter is *doing*, not a topic the chapter is *about*.

- "About infidelity" — topic. Too coarse.
- "What happens to a family's trust when it breaks, and is repair
  possible?" — the actual question the chapter is exploring. Good.

Name the dramatic stakes, not the surface plot. If the chapter stages
a scene of farming, the question probably isn't about agriculture;
it's about what kind of engagement with reality produces meaning.

## Constraints

- Output at most 2 questions per chapter. One is usually enough.
  Only return 2 when the chapter truly sets up competing concerns.
- Optional `reveals`: one sentence naming what the chapter is DOING
  in the larger structure of the work (e.g. "A microcosm of the
  larger rupture the novel will explore."). Skip if you aren't sure.
- Optional `thematic_carriers`: the 1–3 characters whose arcs carry
  this chapter's thematic weight. Skip if no single character owns
  the chapter.
- Write like a reader who has grasped the work, not a publicist.
  No empty phrases ("explores themes of…", "delves into…").

## Output schema (strict JSON)

Return exactly one JSON object matching this shape. No prose outside
the JSON.

```json
{
  "questions": ["..."],
  "reveals": "...",
  "thematic_carriers": ["..."]
}
```

Omit `reveals` or `thematic_carriers` if you would otherwise be
guessing.
