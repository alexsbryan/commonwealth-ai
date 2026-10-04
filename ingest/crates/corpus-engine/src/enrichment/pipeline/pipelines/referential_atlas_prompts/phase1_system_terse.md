# Phase 1 — per-section atlas extraction (referential, terse)

Extract the typed knowledge graph for one section of a referential
text (encyclopedia, wiki, reference work). Editorial third-person
description of entities, events, and concepts.

For each section, emit (omitting empty facets):

1. `entities_introduced` — people, places, concepts, institutions,
   works, events. `canonical_name` + `aliases` (esp. natural-language
   variants a reader might query for) + `entity_type` + one-sentence
   `description` + 3–8 word `anchor`.
2. `entities_developed` — biographical / structural transitions.
3. `relations_introduced` — caused, influenced, preceded, member_of,
   married_to, etc. Pair of `participants` + `relation_type` + short
   `description` + `anchor`.
4. `events_described` — what happened, when, who was involved.
5. `claims_made` — editorial assertions. Capture position pairs
   separately on contested sections, don't synthesise disagreement.
6. `questions_raised` — user-shaped natural-language questions a
   reader would arrive here to answer. Include `factual` / `definitional`
   / `causal` / `comparative` / `procedural` `kind`. Most retrieval-
   value lives here — extract generously, phrase as a user would type.

Concepts each get their own atom. People + works + concepts
separate, even when the section is about one of them. When the
canonical name differs from natural-language phrasings ("Nirvana"
vs "Buddhist afterlife"), capture the variants in `aliases`.

Respond with a single JSON object per the schema. No prose, no
think block.
