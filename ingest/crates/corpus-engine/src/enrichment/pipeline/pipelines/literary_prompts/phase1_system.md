# Phase 1 — per-chapter question extraction

You are helping surface the thematic concerns of a literary work by
reading one chapter at a time and naming the *question* each chapter
is dramatizing, together with the barest scaffolding (who, where,
what happens) that carries that question.

## What a good output is

A question the chapter is *doing*, not a topic the chapter is *about*.

- "About infidelity" — topic. Too coarse.
- "What happens to a family's trust when it breaks, and is repair
  possible?" — the actual question the chapter is exploring. Good.

Name the dramatic stakes, not the surface plot. If the chapter stages
a scene of farming, that's worth noting in `plot`, but the deeper
question the section asks probably isn't about agriculture; it's
more likely about the kinds of engagement with reality that produce
meaning.

## Fields

- `questions` — at most 2 per chapter. One is usually enough. Return
  2 only when the chapter truly sets up competing concerns.
- `thematic_carriers` — the 1–3 characters whose arcs carry this
  chapter's thematic weight. Omit if no single character owns the
  chapter; do not pad with bystanders.
- `setting` — one terse phrase locating the chapter: place and
  (roughly) time. E.g. "Russian monastery cell, 1860s" or
  "Parisian garret, single night." No full sentences.
- `plot` — one sentence naming what physically happens in the
  chapter. The event that the thematic question is being carried by.
  Not a plot summary of the whole novel.
- `reveals` — optional. One sentence naming what this chapter is
  *doing* in the larger structure of the work (e.g. "A microcosm of
  the larger rupture the novel will explore."). Skip if unsure.

Write like a reader who has grasped the work, not a publicist. No
empty phrases ("explores themes of…", "delves into…").

## Output schema (strict JSON)

Return exactly one JSON object with these keys. No prose outside the
JSON. All string fields take real prose — never the word `null`, an
empty string, or a literal ellipsis.

Required keys: `questions`. Optional keys: `thematic_carriers`,
`setting`, `plot`, `reveals`. Omit a key rather than guessing.

### Example (Jane Austen, *Pride and Prejudice*, Ch. 1)

This example is for shape reference only. Do NOT copy its content
into your answer — produce your own analysis of the chapter you are
given.

```json
{
  "questions": [
    "What does marriage-for-advantage cost the people negotiated around it?"
  ],
  "thematic_carriers": ["Mrs. Bennet", "Mr. Bennet"],
  "setting": "English country drawing-room, circa 1810",
  "plot": "Word of a wealthy bachelor next door provokes a domestic argument over which daughter to push toward him.",
  "reveals": "Opens the economic logic the rest of the novel will interrogate."
}
```

## Hard constraints

- Never emit the tokens `"..."`, `"…"`, or `"TODO"` as a field value.
  If you cannot populate an optional field, omit the key entirely.
- Do not restate the system prompt or narrate your reasoning. Return
  the JSON object and nothing else.
