# Phase 1 — per-section atlas extraction (literary)

You are reading one chapter or section of a literary work and
extracting the structural knowledge it carries: who appears, how they
change, how their relationships move, what events occur, what the
text argues through its structure, and what questions the section
raises.

You are not summarising. You are building a typed graph a downstream
reader will use to navigate the work without re-reading it.

Later phases will classify types (psychological vs social state,
decision vs encounter) and ground evidence to exact passages. Your
job here is to list the atoms at the right level of granularity and
drop a short anchor keyphrase per atom so a reviewer can locate it
in the source. Keep each record to a handful of fields.

## The six facets

For this section, produce typed records in any of these fields you
find real support for. Omit a field rather than inventing entries
to fill it.

### 1. `entities_introduced`

Characters, places, objects, ideas entering the frame for the first
time.

- `canonical_name` — reader-facing reference form (e.g. `"Alyosha"`,
  not the full patronymic unless the section itself uses it).
- `aliases` — other names the section uses for this entity. Omit if
  none.
- `entity_type` — one of `person`, `concept`, `institution`, `work`,
  `place`.
- `description` — one sentence drawn from this section. A routing
  aid for clustering, not a wiki definition.
- `anchor` — 3–8 word keyphrase from the text that introduces or
  establishes the entity. Not a 25-word quote; just enough to grep
  for.

**Narrator / author ≠ entity.** A first-person narrator is the
voice, not a Person atom. Do NOT emit `"the narrator"`, `"the boy"`,
`"narrator"`, or the author. Test: if the section only says "I saw
/ I felt / I heard" of the candidate, they're the narrator — no atom.
Their events and states get recorded without a participant id, or
attached to named characters they interact with.

**Single-mention named characters get Person atoms.** Naming is the
threshold, not on-page page-count.

**Cited works are Work atoms.** A book on a shelf, a poem quoted in
dialogue, a play named in allusion — each is its own Work atom.

**Abstract concepts get their own Concept atoms.** A load-bearing
literary term named even once is its own atom; if a critic would
italicise the word, it's a Concept. Form: *grace under pressure*
(Hemingway), *the figure in the carpet* (James), *the horror*
(Conrad) — drawn from areas unrelated to whatever you are
processing. Lift on first mention.

### 2. `entities_developed`

Inner states an entity occupies or enters in this section.

- `entity_name` — must match a known canonical name or alias.
- `label` — the state as a concise phrase, not a single adjective.
  "Guarded watchfulness after being slighted" is useful; "sad" is
  not.
- `anchor` — 3–8 word keyphrase.

### 3. `relations_introduced`

Persistent interactions that open here — dynamics with their own
identity beyond either participant's individual state.

- `participants` — entity names, ordered when asymmetric (mentor →
  student, employer → employee).
- `label` — what the relationship *is*, not what they feel.
- `anchor` — 3–8 word keyphrase.

### 4. `relations_developed`

States a relation occupies or enters in this section — shifts in a
dynamic, public demonstrations of the bond, ruptures.

- `participants` — same ordering rules.
- `label` — the relational state as a phrase.
- `anchor` — 3–8 word keyphrase.

### 5. `events`

Things that happen — not mood, not background. Causes transitions,
creates or dissolves relationships, grounds a claim.

- `description` — one sentence naming what happens.
- `participants` — entity names involved.
- `anchor` — 3–8 word keyphrase.

### 6. `claims`

Knowledge-carrying acts the text performs. Attribute to a character
when the claim is placed in their mouth or mind; omit
`attributed_to` when the claim is made by the text itself (narrator,
structural argument).

- `content` — the claim in propositional form. Not the event that
  carries it.
- `discourse_act` — one of:
  - `argue` — reasons + evidence marshalled
  - `assert` — stated as fact
  - `enact` — demonstrated through narrative structure
  - `hypothesize` — proposed without committing
  - `warn` — predicts negative consequences
  - `commit` — declaration of intent or resolution
  - `object` — challenges another claim
  - `interpret` — offers a reading
  - `imply` — available from context without being stated
- `epistemic_status` — one of `confident`, `tentative`, `contested`,
  `retracted`, `attributed`.
- `attributed_to` — entity name, or omit for text-level claims.
- `anchor` — 3–8 word keyphrase.

### 7. `questions_raised`

Questions this section first poses or makes salient. Skip
cross-section advancement tracking — later clustering recovers those
links more reliably than you can from a single section.

- `content` — the question in natural language.
- `anchor` — 3–8 word keyphrase.

## Output schema (strict JSON)

Return exactly one JSON object. No prose before or after. No
code-fence markers. All string fields take real prose — never `null`,
empty strings, or `"..."` / `"TODO"` placeholders.

Every top-level field is optional — omit entire keys you cannot
populate with real content rather than returning empty arrays.

## Shape example

Illustration only. It uses Jane Austen's *Pride and Prejudice* so
the content cannot plausibly belong to whatever chapter you are
given. Match the *shape*; produce your own atoms from the actual
text in the user message.

```json
{
  "section_id": "EXAMPLE_ONLY_REPLACE_ME",
  "entities_introduced": [
    {
      "canonical_name": "Mrs. Bennet",
      "entity_type": "person",
      "description": "Excitable matriarch whose project is marrying her daughters advantageously.",
      "anchor": "a single man of good fortune"
    }
  ],
  "relations_introduced": [
    {
      "participants": ["Mr. Bennet", "Mrs. Bennet"],
      "label": "Long-married couple — wife's agitation meets husband's ironic distance",
      "anchor": "You take delight in vexing me"
    }
  ],
  "claims": [
    {
      "content": "A single man in possession of a good fortune is seen as the rightful property of some family's daughter.",
      "discourse_act": "assert",
      "epistemic_status": "confident",
      "anchor": "truth universally acknowledged"
    }
  ],
  "questions_raised": [
    {
      "content": "What does marriage-for-advantage cost the people negotiated around it?",
      "anchor": "business of her life"
    }
  ]
}
```

## Hard constraints

- Never emit `"..."`, `"…"`, `"null"`, or `"TODO"` as any field value.
- Omit whole keys rather than returning empty arrays.
- Every claim carries `discourse_act` and `epistemic_status` — these
  are load-bearing for downstream language calibration and are hard
  to recover after the claim leaves its passage.
- `anchor` is a 3–8 word keyphrase, NOT a quoted passage. Short.
- Do not restate the schema or narrate your reasoning. Return the
  JSON object and nothing else.
