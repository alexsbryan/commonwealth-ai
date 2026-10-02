# Phase 1b — concept coverage check (literary)

You are auditing a literary extraction for ONE specific kind of
recall miss: **foregrounded thematic concepts**.

A foregrounded concept is a word or short phrase the *work itself*
treats as load-bearing — not a regular noun the prose uses
incidentally. Signals:

- The narrator stops to examine it ("I said softly to myself the
  word X"), define it, or muse on its sound or meaning.
- It is italicized or set apart in a list (catechistic enumeration,
  a triplet of terms set off for attention).
- It is a recognised critical or thematic term the field associates
  with the work or its author.
- It is repeated across the section in a way that signals motif
  rather than incidental usage.
- It is capitalised as an imperative or exclamation in the prose.

Single appearances of a word the prose explicitly examines ARE
exactly what makes a concept thematic. The fact that a term appears
"only once" or "only in a simile" does NOT disqualify it.

Do NOT propose:
- Concepts already in the existing extraction.
- Common nouns the prose uses incidentally.
- Personal-state descriptions ("Eveline's terror") — those are
  state atoms, not concepts.
- Interpretive labels you invent (e.g. "the absurd / the
  uncanny"); only lift terms whose exact form appears in the
  section.

Quality over quantity — return an empty list rather than padding.

## Output schema (strict JSON)

Return exactly one JSON object. No prose before or after. No
code-fence markers.

```json
{
  "missed_concepts": [
    {
      "canonical_name": "...",
      "description": "one sentence on the concept as the section uses it",
      "anchor": "3-8 word keyphrase from the section text"
    }
  ]
}
```

Omit the key entirely (rather than returning an empty array) if
nothing was missed.
