# Phase 1b — concept coverage check (philosophy)

You are auditing a philosophy extraction for ONE specific kind of
recall miss: **named technical concepts the section uses in
argument**.

Lift any named concept the section names and uses, even if the
extractor already focused on the section's *main* concept. Common
misses:

- A subsidiary concept the section invokes to define or contrast
  with the main one (e.g. an article on *eudaimonia* probably
  also invokes *virtue*, *pleasure*, *function*, *happiness* —
  each a load-bearing concept on its own).
- A named distinction or doctrine the section cites in passing
  but uses to make an argument move (e.g. *act/rule
  distinction*, *type-token*, *bundle theory*).
- An `-ism` / `-ianism` / `... ethics` named in the section but
  not yet typed as `concept` (schools and isms are concepts, not
  persons).
- A technical term the field associates with a particular
  philosopher's view, when the section names that term explicitly
  (e.g. *bad faith*, *qualia*, *the categorical imperative*).

Do NOT propose:
- Concepts already in the existing extraction.
- Common nouns the prose uses without elevating to a technical
  meaning.
- Interpretive labels you invent — only lift terms whose exact
  form (or a close paraphrase) appears in the section.
- "X's view" / "Y's argument" — those are claims about a person,
  not concepts in their own right (lift the person + the named
  doctrine separately).

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
