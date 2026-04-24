# Stage 1a seed entity list (philosophy)

You are reading the first section of a philosophy article and
producing a canonical list of the **named entities** the article
will reference throughout — so every downstream section's Phase 1
extraction threads the same spellings and aliases.

For a SEP article on (say) Compatibilism, the seed should include
the canonical name "Compatibilism" (plus aliases like "soft
determinism"), the philosophers who introduced or shaped the
debate (Hume, Frankfurt, van Inwagen, …), the core concepts that
carry the argument (principle of alternate possibilities, causal
determinism, free will), and any landmark works named
(Frankfurt's "Alternate Possibilities and Moral Responsibility").

Entities only. No claims, no events, no questions. The downstream
Phase 1 extractor builds those on top of this canonical vocabulary.

## Output schema (strict JSON)

```json
{
  "entries": [
    {
      "canonical_name": "<reader-facing reference form>",
      "aliases": ["<alt-form>", "<abbreviation>"],
      "entity_type": "person|concept|institution|work|place",
      "description": "<one sentence naming what the entity is>"
    }
  ]
}
```

Rules:

- **No `<think>` block, no prose before or after the JSON, no code
  fences.** Emit the JSON directly.
- `canonical_name` is the form the article uses most consistently.
  When an article uses "compatibilism" (lowercase) throughout,
  match that; don't force "Compatibilism" just for uniformity.
- `aliases` covers genuine alternate names the article uses.
  Include abbreviations ("PAP" for "principle of alternate
  possibilities"). Omit when empty.
- `entity_type` is one of `person`, `concept`, `institution`,
  `work`, `place`. Philosophy leans heavily on `concept` and
  `person`; `place` is rare (reserve for genuine topographical
  references like "the Academy" or "Königsberg").
- `description` is one sentence drawn from the section itself.
- Cap the list at the entities that will appear in ≥ 2 sections.
  A name mentioned exactly once is noise, not a seed.

## Transliteration + diacritics

When a philosopher's name has a conventional English spelling, use
it ("Heidegger", not "Heideggér"). For untransliterated Greek
("Diogenes Laertius") keep the English form; for Latin titles
retain the Latin ("De Cive"). Never mix scripts mid-word.
