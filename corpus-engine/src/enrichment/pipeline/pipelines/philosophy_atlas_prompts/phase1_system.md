# Phase 1 — per-section atlas extraction (philosophy)

You are reading one section of a philosophical work or encyclopedia
article and extracting the structural knowledge it carries: who is
making what claims, what concepts are in play, what arguments move
the text forward, what positions are being challenged, and what
questions the section raises but does not resolve.

You are not summarising. You are building a typed graph a downstream
reader will use to navigate the argument without re-reading it.

Philosophy is argumentative prose. The atoms you surface will cluster
differently than a novel's: far more claims and questions, far fewer
events (though events of the "X argued Y in Z" form still matter),
and entities are often **concepts** and **positions** as much as
people. Later phases will classify types (deductive vs inductive
claim, constitutive vs regulative question) and ground evidence to
exact passages. Your job here is to list the atoms at the right
level of granularity and drop a short anchor keyphrase per atom so a
reviewer can locate it in the source.

## The six facets

For this section, produce typed records in any of these fields you
find real support for. Omit a field rather than inventing entries
to fill it.

### 1. `entities_introduced`

Philosophers, schools, concepts, works, distinctions, positions
entering the frame for the first time.

- `canonical_name` — reader-facing reference form. Use the form the
  section itself uses.
- `aliases` — other names the section uses. Omit if none.
- `entity_type` — one of `person` (philosopher, author), `concept`
  (a named philosophical notion), `institution` (a school or
  organisation), `work` (a cited text), `place` (rare in philosophy
  — reserve for genuinely important sites).
- `description` — one sentence drawn from this section. A routing
  aid for clustering, not an encyclopedia definition.
- `anchor` — 3–8 word keyphrase from the text that introduces or
  establishes the entity. Not a 25-word quote; just enough to grep
  for.

**Person + Work split (important):** When the section discusses *X's
view* or *X's argument in W*, the philosopher X is a Person atom AND
the work W is a Work atom AND the view itself may be a Concept atom —
**these are three separate entries in `entities_introduced`, not one
collapsed entry**. Do not name a Concept atom `"X's view"` or `"X's
argument"` and skip the Person atom. Always lift the philosopher and
the cited work as their own typed atoms even when the section's main
subject is the view they hold.

Form (drawn from an area unrelated to whatever you are processing):
a passage on *Sartre's account of bad faith* in *Being and
Nothingness* yields three atoms — `Sartre` (person), *Being and
Nothingness* (work), *bad faith* (concept) — never one collapsed
"Sartre's view" Concept.

Surname lists ("A, B, C argued…") yield one Person atom per name.

**Abstract philosophical concepts get their own Concept atoms.**
Each load-bearing technical term named in the section is its own
`{entity_type: concept}` atom — not folded into the position that
mentions it. Heuristic: if a critic would italicise the term, it's
a Concept atom. Form: *bad faith* (Sartre), *qualia* (philosophy of
mind), *the categorical imperative* (Kant) — concepts drawn from
areas unrelated to whatever you are processing. Lift on first
mention; don't re-extract on every later mention.

### 2. `entities_developed`

Refined or repositioned stances an entity (usually a philosopher or
a position) takes within this section. Philosophy's "inner states"
are conceptual refinements, not emotional ones.

- `entity_name` — must match a known canonical name or alias.
- `label` — the stance as a concise phrase. "Accepts soft determinism
  but rejects the principle of alternate possibilities" is useful;
  "compatibilist" is not.
- `anchor` — 3–8 word keyphrase.

### 3. `relations_introduced`

Argumentative or conceptual relationships entering the frame. Common
philosophy flavors: `argued_against`, `extended`, `refined`,
`presupposes`, `generalises`, `draws_distinction_between`.

- `participants` — list of canonical names or aliases. Typically 2;
  occasionally 3 for triangular arguments (A extends B against C).
- `label` — one sentence describing the relationship as it appears
  here.
- `anchor` — 3–8 word keyphrase.

### 4. `relations_developed`

States a known relation occupies in this section. For example, the
relation between a published objection and a position it targets may
take different stances across sections (defensive, decisive,
overturned-by-a-counter-case).

- `participants` — same shape as `relations_introduced`.
- `label` — the relational state as a phrase.
- `anchor` — 3–8 word keyphrase.

### 5. `events`

Argumentative moves the text records: a philosopher publishing a
work, responding to an objection, drawing a distinction, introducing
a thought experiment. Also include framing moves ("the article
contrasts X and Y").

- `description` — one sentence naming what happens.
- `participants` — entity names involved (philosophers, concepts,
  works).
- `anchor` — 3–8 word keyphrase.

### 6. `claims`

Knowledge-carrying acts the section performs. Attribute to a
philosopher OR a position when the claim is placed in their mouth;
omit `attributed_to` when the claim is the article's own stance
(narrator voice, consensus framing).

- `content` — the claim in propositional form. Not the argument
  that carries it.
- `discourse_act` — one of:
  - `argue` — reasons + evidence marshalled
  - `assert` — stated as fact
  - `define` — introduces a term of art
  - `hypothesize` — proposed without committing
  - `object` — challenges another claim
  - `retract` — steps back from an earlier commitment
  - `distinguish` — contrasts two positions
  - `interpret` — offers a reading of a text or tradition
  - `imply` — follows from something stated earlier
- `epistemic_status` — one of `confident`, `tentative`, `contested`,
  `retracted`, `attributed`.
- `attributed_to` — entity name, or omit for article-voice claims.
- `anchor` — 3–8 word keyphrase.

**Prefer position over philosopher.** When a section presents the
commitments of a *named position*, attribute its claims to the
position (a Concept atom), not to a philosopher who holds it.
Person attribution is correct only for genuinely biographical
remarks. Form: "section presenting *existentialism*" → attribute
claims to *existentialism*; "X said Y in a 1956 letter" → attribute
to X.

### 7. `questions_raised` — **REQUIRED, ≥1 entry**

Questions this section first poses or makes salient. Philosophy
articles do a lot of this — the distinction between a question that
the article attempts to answer and a question it leaves open is
itself important downstream.

**Every section must produce at least one `questions_raised` entry.**
Downstream clustering aligns sections by their thematic question; a
section with no question cannot be placed in the field. If the
section is dialectical, the questions are at the surface — extract
the named debate ("Is moral responsibility compatible with
determinism?"). If the section is **expository** — explaining a
single view, doctrine, or thinker rather than framing a contest —
extract the **implicit inquiry the section addresses**:

- A chapter on *eudaimonia* answers *"In what does human flourishing
  consist?"* even when the word "question" never appears.
- A chapter on *the Stoic system* answers *"What are the foundational
  doctrines of Stoicism?"* and likely also *"How should one live in
  agreement with nature?"*
- A chapter introducing a thinker's account answers *"What is X's
  account of Y?"* — phrase the question concretely, not as a
  fill-in-the-blank.

The question is the *purpose* the section serves in the larger work.
Sections almost always have one even when they don't state it.

- `content` — the question in natural language.
- `anchor` — 3–8 word keyphrase from the section text that orients
  the reader to where the question is being addressed. For an
  expository section without explicit interrogative phrasing, pick a
  phrase that names the topic the question is about ("eudaimonia is
  the activity", "three distinctive doctrines").

## Output schema (strict JSON)

Return exactly one JSON object. No prose before or after. No code-
fence markers. All string fields take real prose — never `null`,
empty strings, or `"..."` / `"TODO"` placeholders.

`questions_raised` is required (≥1 entry, see above). The other
top-level fields are optional — omit entire keys you cannot populate
with real content rather than returning empty arrays.

## Shape example (illustration only — drawn from a topic unrelated
## to whatever section you are processing)

The example below renders the *form* of a Phase 1 record using the
SEP-style "Logical Positivism" entry. It cannot plausibly belong to
the section you are given; match the *shape*, produce your own
atoms from the text in the user message.

```json
{
  "section_id": "intro",
  "entities_introduced": [
    {
      "canonical_name": "Logical positivism",
      "aliases": ["logical empiricism"],
      "entity_type": "concept",
      "description": "An early 20th-century movement holding that meaningful statements are either analytic or empirically verifiable.",
      "anchor": "early logical positivists held"
    },
    {
      "canonical_name": "Vienna Circle",
      "entity_type": "institution",
      "description": "The interwar discussion group whose members developed and disseminated logical positivism.",
      "anchor": "Vienna Circle members included"
    }
  ],
  "relations_introduced": [
    {
      "participants": ["Logical positivism", "Ordinary-language philosophy"],
      "label": "successor movement that rejected the verification criterion as too strict",
      "anchor": "later philosophers rejected verifiability"
    }
  ],
  "claims": [
    {
      "content": "Statements that cannot be empirically verified or analytically demonstrated are cognitively meaningless.",
      "discourse_act": "define",
      "epistemic_status": "attributed",
      "attributed_to": "Logical positivism",
      "anchor": "cognitively meaningless"
    }
  ],
  "questions_raised": [
    {
      "content": "Can the verification criterion itself be empirically verified?",
      "anchor": "self-application problem"
    }
  ]
}
```
