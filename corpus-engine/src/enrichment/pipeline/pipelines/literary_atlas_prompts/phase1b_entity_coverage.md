# Phase 1b — entity coverage check (literary)

You are auditing a literary-extraction pass for completeness. The
extractor has already produced a typed atlas of one section:
entities (persons, concepts, works, institutions, places), events,
states, relations, claims, questions. **You are NOT re-doing the
extraction; you are auditing it for missed entity atoms.**

Look hard for these specific recall failures:

1. **Cited works** — books, poems, plays, periodicals, articles
   referenced by title (italicized titles are the strongest
   signal). Each one is its own `work` entity.

2. **Named characters** — anyone the prose names, including by
   relation form (a possessive like "X's sister" or a
   role-with-modifier like "the Y's housekeeper"), even when they
   appear only briefly. Naming is the threshold, not page count.
   Skip the unnamed first-person narrator.

3. **Specific places and institutions** — named churches,
   businesses, towns, schools, journals — anything specifically
   identified by proper name.

Do NOT propose:
- Atoms already in the existing extraction.
- The unnamed narrator/protagonist.
- The author of the work being extracted.
- Generic places ("the city", "the room") that aren't proper-named.
- Names that appear ONLY inside a simile, metaphor, allusion, or
  comparison. If a famous historical, literary, or mythological
  figure is invoked as a comparison ("she resembled X", "like Y in
  Z's play"), that figure is decoration — the prose is using them
  as a measuring stick, not introducing them as a participant.
  Lift only entities whose actions, statements, or relationships
  are recorded in the section's actual events.

Quality over quantity — return an empty list rather than padding.

## Output schema (strict JSON)

Return exactly one JSON object. No prose before or after. No
code-fence markers.

```json
{
  "missed_entities": [
    {
      "canonical_name": "...",
      "entity_type": "person | concept | work | institution | place",
      "description": "one sentence drawn from the section",
      "anchor": "3-8 word keyphrase from the text"
    }
  ]
}
```

Omit the key entirely (rather than returning an empty array) if
nothing was missed.
