# Phase 1 — per-section atlas extraction (referential)

You are reading one section of a referential text — an encyclopedia
article, wiki page, or reference work — and extracting the typed
knowledge it carries: who and what is being described, what claims
the section makes, what events it records, and what questions a
reader would arrive here to answer.

You are not summarising. You are building a typed graph a downstream
reader will use to retrieve and navigate the article without
re-reading it.

Referential prose is editorial third-person description. The atoms
you surface will cluster differently than a novel's or a
philosophical argument's: many entities (people, places, works,
concepts), many factual claims, many events (especially in
historical and biographical articles), and the section's
questions are *retrieval shapes* — the natural-language phrasings a
reader would use to seek out this content.

Your job is to list the atoms at the right level of granularity and
drop a short anchor keyphrase per atom so a reviewer can locate it
in the source.

## The six facets

For this section, produce typed records in any of these fields you
find real support for. Omit a field rather than inventing entries
to fill it.

### 1. `entities_introduced`

People, places, organisations, concepts, works, events entering
the frame for the first time.

- `canonical_name` — reader-facing reference form. Use the form the
  section itself uses (typically a Wikipedia-style title).
- `aliases` — other names the section uses for this entity. Common
  for people (full name + given name), places (city + country
  shorthand), and concepts that have multiple terms. Omit if none.
- `entity_type` — one of `person`, `place`, `concept`, `institution`,
  `work`, `event`. If a thing fits two types, pick the one that
  best fits how the section uses it.
- `description` — one sentence drawn from this section. A routing
  aid for clustering, not an encyclopedia definition.
- `anchor` — 3–8 word keyphrase from the text that introduces the
  entity. Just enough to grep for.

**Person split.** When the section names a person AND a work they
authored AND a concept they introduced — these are three separate
atoms, not one collapsed entry. Lift the person, the work, and the
concept as their own typed atoms even when the section's main
subject is one of them.

**Concepts get their own atoms.** Each load-bearing technical term
named in the section is its own `{entity_type: concept}` atom — not
folded into the entity that bears it. The bar is generous: lift any
named concept the section *uses* — named theories, named methods,
named goods, named periods, named movements. Don't re-extract on
every later mention; lift on first appearance. **When in doubt, lift it.**

**Cross-conceptual labels matter.** When an article describes a
concept whose canonical name in this section differs from how
readers might query for it (e.g. *Nirvana* is the Buddhist concept
a reader might query as "Buddhist afterlife" or "Buddhist
liberation"), capture the alternate phrasings in `aliases`. This is
the highest-value signal for retrieval — every alias becomes a
queryable surface.

### 2. `entities_developed`

Changes or transitions an entity undergoes within this section.
Common shapes for referential prose: biographical events
(*Einstein moved to Princeton*), state changes (*the empire was
divided*, *the species went extinct*), structural transitions
(*the company was acquired*).

- `entity_name` — must match a known canonical name or alias.
- `label` — the change as a concise phrase. "Moved from Berlin to
  Princeton in 1933" is useful; "relocated" is not.
- `anchor` — 3–8 word keyphrase.

### 3. `relations_introduced`

Structural relationships entering the frame. Common referential
flavors: `caused`, `influenced`, `preceded`, `succeeded`,
`includes`, `member_of`, `married_to`, `studied_under`,
`contemporaneous_with`, `derived_from`, `subspecies_of`,
`bordered_by`.

- `participants` — list of canonical names or aliases. Typically 2;
  occasionally 3.
- `relation_type` — short verb-phrase from the relation flavors
  above or a section-derived equivalent.
- `description` — one short sentence summarising the relation as the
  section frames it.
- `anchor` — 3–8 word keyphrase.

### 4. `events_described`

Events the section records. This is where referential corpora are
densest — historical articles, biographies, and timelines are
mostly events.

- `description` — one sentence, drawn from the section.
- `participants` — entities involved (canonical names or aliases).
- `time` — date, era, or period the section associates with the
  event. A bare year is fine.
- `anchor` — 3–8 word keyphrase.

### 5. `claims_made`

Editorial assertions of fact the section makes. Referential
corpora aim for neutrality, but every assertion is still a claim
— we capture what the article *says is the case*.

- `content` — the claim as a single declarative sentence.
- `subject` — what the claim is about (canonical name, alias, or
  short phrase).
- `attributed_to` — if the section attributes the claim to a
  source ("according to historian X"), record that source. If the
  section asserts the claim editorially without attribution, leave
  empty.
- `anchor` — 3–8 word keyphrase.

A note on contested sections: if the section presents *multiple*
positions on a question (e.g. a Debate or Criticism section),
extract each position as a separate claim with its own
`attributed_to`. Don't synthesise the disagreement away.

### 6. `questions_raised`

Questions a reader would arrive at this section to answer. This is
the most retrieval-shaped facet — write the questions in
natural-language form, the way a curious reader would type them.

- `content` — the question itself, in user-shaped phrasing. "What
  did Einstein contribute to physics?" — not "Einstein's
  contributions to physics".
- `kind` — one of `factual` (single-fact lookup),
  `definitional` (what is X?), `causal` (why did X happen?),
  `comparative` (how do X and Y differ?), `procedural` (how does
  X work?).
- `anchor` — 3–8 word keyphrase tying back to the source.

**Generosity here matters most.** A single Wikipedia section often
answers 3–10 distinct questions a reader might bring. Extract
generously. Phrase each question as a *user* would type it, not as
a librarian would index it. If the section has a "Debate" or
"Criticism" header, lift the comparative / contested phrasings
explicitly ("What are the major arguments for and against X?").

This facet is the highest retrieval-value output. Tags here become
the natural-language surface that closes the gap between a user's
phrasing and the article's editorial language.

## What to skip

- Don't extract atoms for navigational text (table of contents
  entries, "See also" headers without substantive content).
- Don't lift atoms from infobox-only data unless the body of the
  section also discusses them.
- Don't paraphrase claims into your own framing — quote the
  section's own language for `content`.

## Output

Respond with a single JSON object matching the schema in the
runtime. Omit any facet that has no entries rather than padding
with synthesised content.
