# Phase 1a — seed entity extraction (referential)

You are reading the opening section of a referential text — the
lead paragraph(s) of a Wikipedia article, the header of a wiki
page, or the introduction of an encyclopedia entry. You are
producing a seed list of canonical entities the rest of the
article will refer to repeatedly.

Why this exists: downstream Phase-1 extraction works one section
at a time and needs to know "this entity is the SAME as the one
introduced in section 1, not a new one." A canonical seed list
keeps later phases from minting duplicate atoms with slight
spelling variations.

## What to extract

For each entity that the article will likely refer back to, emit:

- `canonical_name` — the form used as the article title or the
  most prominent form in the lead.
- `aliases` — every other form the lead uses for this entity.
  Include natural-language variants a reader might query for
  (e.g. *Albert Einstein* + *Einstein*; *atomic bombings of
  Hiroshima and Nagasaki* + *Hiroshima bombing* + *Nagasaki
  bombing*).
- `entity_type` — `person` | `place` | `concept` | `institution` |
  `work` | `event`.
- `salience` — how central this entity is to the article: `primary`
  (the article's main subject), `central` (recurs throughout),
  or `mention` (named in the lead but unlikely to drive structure).

## Generosity

Lead sections are dense. Extract every named entity the lead
mentions, even briefly — Phase 1 of each later section will
reference back to this list, and a missing seed costs more than
a redundant one. Concepts especially: lead sections often invoke
multiple technical terms a reader would query for.

## Output

Single JSON object per the schema in the runtime. Entities only.
No prose, no think block.
