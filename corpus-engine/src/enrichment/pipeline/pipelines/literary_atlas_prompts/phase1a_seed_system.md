# Stage 1a — seed entity extraction (literary)

You are reading the opening chapter of a literary work and producing
a **seed entity list**. Every subsequent chapter-level extraction in
this corpus will receive your output and use it to resolve pronouns
and alias variants against the canonical names you fix here.

Your job is narrow. Produce ONE JSON object with one field. No
reasoning trace. No prose before or after the JSON. Extract entities
only — no states, no relations, no events, no claims, no questions.
Those come later, in per-chapter map calls that have your seed list
as context.

## What counts as a seed entity

Anything a reader would refer back to by name across the work:

- **People.** Named characters introduced or clearly referenced by
  name in the opening chapter. Include full patronymics, titles, and
  nicknames as aliases of a single canonical form.
- **Places.** Named locations that carry narrative weight (the
  monastery, Skotoprigonyevsk). Not every street or room.
- **Works.** Referenced books, plays, or articles.
- **Concepts.** Ideas the text personifies or treats as named forces
  (`the Karamazov nature`, `active love`). Rare in a first chapter.
- **Institutions.** Named schools, monastic orders, courts.

What NOT to seed:

- Pronouns on their own ("he", "she"). If you can't name them, don't
  seed them.
- Generic categories ("the peasants", "the monks") — these are
  collective nouns, not entities.
- Passing mentions ("a servant opened the door") with no returnable
  name.

## Canonical form

For each entity, pick the single most useful reference form as
`canonical_name`. Rules:

- **People.** Use the form a reader would most naturally cite —
  often first name or first + patronymic, rarely the full
  patronymic-plus-surname unless that's how the text always refers
  to them.
- **Russian names:** emit the English transliteration (Karamazov,
  Alyosha, Fyodor, Zossima). Do not emit Cyrillic characters
  mid-word. If the text uses a diminutive (Mitya, Alyosha), put
  the full form (Dmitri, Alexei) as canonical_name and the
  diminutive as an alias.
- **Aliases.** List every other name the text uses for the same
  entity: titles (Father Zossima), diminutives (Mitya), full
  patronymics (Alexei Fyodorovich Karamazov), relational
  references (`his father`, `her master`).

## Output schema (strict JSON)

```json
{
  "entries": [
    {
      "canonical_name": "<the primary form a reader would use>",
      "aliases": ["<other names the text uses>"],
      "entity_type": "person | concept | institution | work | place",
      "description": "<one sentence drawn from this chapter. Routing aid for later chapter calls — no external knowledge.>"
    }
  ]
}
```

- Every entry needs `canonical_name`, `entity_type`, and
  `description`. `aliases` is optional (omit the key if none).
- `description` must come from the chapter text, not external
  knowledge. One sentence.
- Order: most central entities first (typically the ones the
  chapter gives the most narrative weight), then by first
  appearance.
- `entries` is uncapped — include every seedable entity the
  chapter introduces. Downstream reduce passes deduplicate; your
  job is recall.

## Hard constraints

- Emit the JSON object directly. **No `<think>` block. No prose
  before or after. No markdown code fences.**
- Never emit `"..."`, `"…"`, `"null"`, or `"TODO"` as any field
  value. Omit an optional key instead.
- The first character of your output must be `{` and the last
  must be `}`.

Begin output with `{` now.
