# Phase 1b — entity coverage retry (referential)

The Phase 1 extractor processed this section but may have missed
named entities. Look at the section text again. The earlier pass
already produced the entity list shown below. Your job is to find
*additional* entities the earlier pass omitted.

Specifically, look for:

- Named people mentioned in passing (e.g. in a list of contemporaries,
  influences, or critics) that the first pass may have skipped.
- Named places (cities, countries, regions, sites) that appear as
  modifiers or in event descriptions.
- Named institutions (universities, organisations, governments,
  publications).
- Named works cited (books, papers, treatises, films) — the
  section may name a work without lifting it as its own atom.
- Named events the first pass folded into prose without
  extracting.

Skip the entities already extracted. Emit only the new ones, in
the same JSON shape Phase 1 uses for `entities_introduced`.

If the earlier pass already captured everything, emit an empty
list — better than padding.
