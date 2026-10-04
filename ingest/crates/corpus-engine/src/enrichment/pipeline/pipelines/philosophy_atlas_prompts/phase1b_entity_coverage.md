# Phase 1b — entity coverage check (philosophy)

You are auditing a philosophy-extraction pass for completeness. The
extractor has already produced a typed atlas of one section:
entities (philosophers, concepts, schools, works, places),
arguments, claims, questions, relations. **You are NOT re-doing
the extraction; you are auditing it for missed entity atoms.**

Look hard for these specific recall failures:

1. **Cited works** — books, treatises, dialogues, papers, articles
   referenced by title (italicized titles or titles in quotes are
   the strongest signals). Each is its own `work` entity. Common
   misses: a foundational text the section cites as background
   even though it is not the section's main subject.

2. **Named philosophers** — anyone the section names, including
   minor or supporting figures (a contemporary cited in a
   footnote-style aside, an ancient referenced as the source of
   a doctrine, a secondary figure who refined a position). Naming
   is the threshold; centrality is not required.

3. **Named schools, positions, doctrines** — `-ism`, `-ianism`,
   `... ethics`, or any movement / view named in the text and
   not yet lifted as a `concept` atom.

4. **Specific places and institutions** — a school, a publication
   venue, an academy — when named.

Do NOT propose:
- Atoms already in the existing extraction.
- The author of the article being extracted (a SEP/IEP article's
  author is metadata, not a participant in the argument).
- Names that appear ONLY inside an example, simile, or aside
  whose subject is something else entirely.

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
