# Phase 1 — narrative typed extension

You are reading one section that does narrative work — sequencing
events through time with agents. Your job is to expose the
narrative scaffolding: who moves, what they do, what changes between
them, what arcs the section carries.

The base entities (Person, Place, Concept, Institution, Work) and
the surface Claims / Events / Questions are produced by a separate
prompt that runs alongside this one — do not duplicate that work.
Your job is the five collections below.

A "section that does narrative work" can be a whole short story, a
chapter, a journal entry that recounts a day's events, a vignette
inside a longer essay, a meeting recap that tells the story of who
said what. Most pure-argument sections have NO narrative work — the
prompt should return an empty object then.

## The five collections

### 1. `events`

Specific things that happen — a decision made, a confrontation, a
journey, a discovery. Different from a Claim: an event is something
that *happens* in time, not something the author asserts.

- `description` — one sentence stating what happens.
- `participants` — entity names involved in the event.
- `anchor` — 3-8 word keyphrase.

### 2. `entity_states`

A change in an entity's condition / mood / status that the event
sequence reveals. "Alyosha's faith shaken after the elder's death"
is an entity state; the elder's death is the event that caused it.

- `entity_name` — the entity whose state we're naming.
- `label` — one-clause name for the state.
- `anchor` — 3-8 word keyphrase.

### 3. `relations`

Inter-entity bonds the section introduces — friendships, rivalries,
employments, alliances. "Ostrom & Hardin" as a debate partnership is
a relation; their published work is not.

- `participants` — 2+ entity names.
- `label` — one-clause name for the bond.
- `anchor` — 3-8 word keyphrase.

### 4. `relation_states`

A shift in a relation already in play — "the rivalry sharpened
after the Lehman call" describes a relation_state on a relation
that was introduced earlier or assumed.

- `participants` — same shape as `relations`.
- `label` — one-clause name for the state of the relation.
- `anchor` — 3-8 word keyphrase.

### 5. `participant_arcs`

A through-line for a participant across multiple events — beyond a
single state change. "From outsider to insider", "from grief to
acceptance", "from skeptic to advocate". A short story typically
carries 1-3 arcs; an essay-with-vignette usually carries 0-1.

- `participant` — the entity whose arc this is.
- `label` — short phrase naming the arc shape.
- `anchor` — 3-8 word keyphrase.

## Output schema (strict JSON)

Return exactly one JSON object with the five collections. No prose,
no `<think>` block, no code-fence markers. Empty collections may be
omitted rather than emitted as empty arrays. Required fields on each
sketch must be non-empty strings — empty atoms are dropped.

## Hard constraints

- Strictly valid JSON, no prose, no code-fence markers.
- Required fields (`description`, `entity_name`, `participants`,
  `participant`, `label`) must be non-empty.
- Anchors are 3-8 word keyphrases.
- Don't manufacture arcs to fill a quota. A section with 0 narrative
  work returns an empty object.
