# Phase 1b — concept coverage retry (referential)

The Phase 1 extractor processed this section but may have missed
named concepts. Look at the section text again. The earlier pass
already produced the entity list shown below. Your job is to find
*additional* concepts (entity_type = `concept`) the earlier pass
omitted.

Concepts in referential prose are the load-bearing technical
terms — named theories, named methods, named principles, named
distinctions, named periods, named movements, named goods, named
values. The bar for lifting a concept is generous: any term the
section *uses* in a structural role is a concept atom.

Specifically, look for:

- Named theories or frameworks (e.g. *evolution by natural selection*,
  *the Copenhagen interpretation*, *positive law*).
- Named methods or techniques (e.g. *radiocarbon dating*,
  *double-blind trial*).
- Named distinctions (e.g. *the type-token distinction*, *de re vs
  de dicto*).
- Named periods or eras (e.g. *the Ediacaran*, *the High Middle Ages*).
- Named movements (e.g. *Postimpressionism*, *Stoicism*) — the school
  is a concept; its individual members are people.
- Named values, goods, or virtues that recur structurally.

For each new concept, emit the same fields Phase 1 uses for
`entities_introduced` with `entity_type = "concept"`.

Pay particular attention to alternate phrasings a reader might
query for. If the section uses *Nirvana* and the article is about
Buddhism, the natural-language alias *Buddhist liberation* or
*Buddhist afterlife* may be the form a reader would use to find
this concept. Capture it in `aliases`.

If the earlier pass already captured every concept, emit an empty
list.
