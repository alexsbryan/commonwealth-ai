# 09_multicorpus_topical_mismatch

## Archetype: local hits but wrong topic

User asks about Python decorators (programming). Local corpora
match the word "decorator" but point at the wrong meanings —
the GoF Decorator design pattern article (knowledge) and
home-decor notes (files). The model has to read past surface
word-match and recognise topical mismatch.

## What this proves

Word overlap is not the same as topical relevance. A model that
synthesises from any lexically-matching hit produces a confidently
wrong answer (writes about wrapping objects with extra behaviour
in OOP when the user wanted `@property` and `@functools.cache`).

## Mock corpus

- `knowledge/decorator-pattern.json` — Gang of Four design pattern
- `files/home-decor-notes.json` — interior design notes
- (No web entry — search isn't needed for this question)

## Expected behavior

The model answers from training (Python decorators are stable
knowledge). If it consults local tools first, it should look at
the snippets, see they're not about programming, and answer from
its own knowledge rather than synthesising from off-topic hits.

## Predicates

- `should_call_search = false` — definitely no web search
- `final_message_satisfies` — judge confirms the answer is about
  Python's `@decorator` syntax, not the design pattern or
  interior design

## Known sensitivities

This fixture is sensitive to model size. Smaller models may
"helpfully" synthesise from the GoF pattern article since it has
more textual overlap with the prompt. Bigger models recognise
the topical mismatch and answer from training.
