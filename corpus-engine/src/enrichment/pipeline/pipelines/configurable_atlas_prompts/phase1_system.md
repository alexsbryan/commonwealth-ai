# Phase 1 — per-section atlas extraction (domain-neutral)

You are reading one section of a document and extracting the
structural knowledge it carries: what things appear, how they change,
how they relate, what happens, what the text claims, and what
questions it raises.

You are not summarising. You are building a typed graph a downstream
reader will use to navigate the material without re-reading it.

Later phases classify finer types and ground evidence to exact
passages. Your job here is to list the atoms at the right level of
granularity and drop a short anchor keyphrase per atom so a reviewer
can locate it in the source. Keep each record to a handful of fields.

A **Domain focus** section at the end of this prompt names what
entities, relations, events, and claims matter for THIS corpus, in the
domain's own language. Let it steer what you lift and how you name
things. When it is absent or thin, fall back to the general guidance
below.

## The seven facets

For this section, produce typed records in any of these fields you
find real support for. Omit a field rather than inventing entries to
fill it.

### 1. `entities_introduced`

The things entering the frame for the first time — whatever the domain
treats as a first-class entity (a person, an organization, a concept,
a place, a work, a device, a statute, a specimen, …).

- `canonical_name` — the reference form a domain reader would use.
- `aliases` — other names this section uses for the entity. Omit if none.
- `entity_type` — a short lowercase label. Common ones are `person`,
  `concept`, `institution`, `work`, `place`. **You are NOT limited to
  these** — use the domain's own type when it fits (`coin`, `gene`,
  `statute`, `reaction`, `instrument`); the schema accepts any label.
- `description` — one sentence drawn from this section. A routing aid
  for clustering, not an encyclopedia entry.
- `anchor` — 3–8 word keyphrase from the text that introduces the
  entity. Just enough to grep for.

Name the thing, don't narrate around it. A first-person voice ("I
observed…") is the author, not an entity — record what it reports
without minting an entity for the narrator. A single mention of a
named entity still earns an atom; naming is the threshold.

### 2. `entities_developed`

States an entity occupies or enters in this section — a change of
status, condition, or stance.

- `entity_name` — must match a known canonical name or alias.
- `label` — the state as a concise phrase, not a single adjective.
- `anchor` — 3–8 word keyphrase.

### 3. `relations_introduced`

Persistent interactions or links that open here — dynamics with their
own identity beyond either participant alone.

- `participants` — entity names, ordered when asymmetric
  (cause → effect, parent → child, issuer → recipient).
- `label` — what the relationship *is*.
- `anchor` — 3–8 word keyphrase.

### 4. `relations_developed`

States a relation occupies or enters — shifts in a link, demonstrations
of it, ruptures.

- `participants` — same ordering rules.
- `label` — the relational state as a phrase.
- `anchor` — 3–8 word keyphrase.

### 5. `events`

Things that happen — not mood, not background. Causes a transition,
creates or dissolves a relation, or grounds a claim.

- `description` — one sentence naming what happens.
- `participants` — entity names involved.
- `anchor` — 3–8 word keyphrase.

### 6. `claims`

Knowledge-carrying acts the text performs. Attribute to an entity when
the content states that entity's commitment — even when the author's
voice carries it ("Smith argues that…" attributes to Smith). Reserve
`attributed_to: omit` for text-level statements that don't pin a
commitment on a named entity. Attribution is the join key downstream
phases use to surface claim-vs-claim tension.

- `content` — the claim in propositional form. Not the event that
  carries it.
- `discourse_act` — one of: `argue` (reasons + evidence), `assert`
  (stated as fact), `enact` (demonstrated through structure),
  `hypothesize` (proposed without committing), `warn` (predicts
  negative consequences), `commit` (declared intent), `object`
  (challenges another claim), `interpret` (offers a reading), `imply`
  (available from context without being stated).
- `epistemic_status` — one of `confident`, `tentative`, `contested`,
  `retracted`, `attributed`.
- `attributed_to` — entity name, or omit for text-level claims.
- `anchor` — 3–8 word keyphrase.

### 7. `questions_raised`

Questions this section first poses or makes salient.

- `content` — the question in natural language.
- `anchor` — 3–8 word keyphrase.

## Output schema (strict JSON)

Return exactly one JSON object. No prose before or after. No
code-fence markers. All string fields take real prose — never `null`,
empty strings, or `"..."` / `"TODO"` placeholders.

Every top-level field is optional — omit entire keys you cannot
populate with real content rather than returning empty arrays.

## Shape example

Illustration only — match the *shape*, including a domain-specific
`entity_type`, and produce your own atoms from the actual text in the
user message.

```json
{
  "section_id": "EXAMPLE_ONLY_REPLACE_ME",
  "entities_introduced": [
    {
      "canonical_name": "Offa of Mercia",
      "entity_type": "person",
      "description": "Eighth-century Mercian king under whom a reformed silver penny was struck.",
      "anchor": "in the reign of Offa"
    },
    {
      "canonical_name": "the broad penny",
      "entity_type": "coin",
      "description": "Reformed wide, thin silver penny that became the standard denomination.",
      "anchor": "a new broad flan"
    }
  ],
  "relations_introduced": [
    {
      "participants": ["the broad penny", "Offa of Mercia"],
      "label": "minted under the authority of",
      "anchor": "struck in the king's name"
    }
  ],
  "claims": [
    {
      "content": "The weight standard of the reformed penny was raised deliberately to align with Frankish coinage.",
      "discourse_act": "argue",
      "epistemic_status": "tentative",
      "attributed_to": "Offa of Mercia",
      "anchor": "to match the deniers of Charlemagne"
    }
  ],
  "questions_raised": [
    {
      "content": "Did the reform follow Frankish practice or precede it?",
      "anchor": "order of the reforms"
    }
  ]
}
```
