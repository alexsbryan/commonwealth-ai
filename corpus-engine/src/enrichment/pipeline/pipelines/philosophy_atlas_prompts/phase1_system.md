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

- `canonical_name` — reader-facing reference form (e.g. `"Frankfurt
  cases"`, `"soft determinism"`, `"Kant"`). Use the form the
  section itself uses.
- `aliases` — other names the section uses (e.g. `["compatibilism",
  "soft determinism"]`). Omit if none.
- `entity_type` — one of `person` (philosopher, author), `concept`
  (free will, supervenience), `institution` (Vienna Circle,
  Stoicism), `work` (Tractatus, Critique of Pure Reason), `place`
  (rare in philosophy — reserve for genuinely important sites).
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

Examples (from common philosophy passages):

- A passage discussing *Foot's "Natural Goodness"* produces:
  `{canonical_name: "Foot", entity_type: "person"}`,
  `{canonical_name: "Natural Goodness", entity_type: "work"}`, and
  the substantive concept *natural goodness* if the passage develops
  it. **Not** a single `{canonical_name: "Foot's view (Natural
  Goodness)", entity_type: "concept"}` atom that collapses all three.
- A passage on *MacIntyre's tradition-bound moral inquiry* produces:
  `{canonical_name: "MacIntyre", entity_type: "person"}`,
  `{canonical_name: "After Virtue", entity_type: "work"}` (if cited),
  and `{canonical_name: "tradition-bound moral inquiry",
  entity_type: "concept"}`.
- A passage on *Marcus Aurelius's Meditations* produces both
  `{canonical_name: "Marcus Aurelius", entity_type: "person"}` and
  `{canonical_name: "Meditations", entity_type: "work"}`. The work
  is a separate atom, not a property of the person.

If a philosopher is cited only by surname in a list ("Hobbes, Locke,
Hume argued..."), each surname is its own Person atom. Do not merge
them into `"Hobbes and Hume"` or `"the classical compatibilists"` as
a single atom.

**Abstract philosophical concepts get their own Concept atoms even
when discussed within a position.** A passage on compatibilism that
develops *moral responsibility*, *free will*, *alternative
possibilities*, or *Frankfurt cases* must extract each as its own
`{entity_type: concept}` atom — they are the load-bearing
philosophical vocabulary the field aligns on, not properties of the
position that mentions them. The position is one atom; the
underlying concepts it operates over are *additional* atoms, not
folded into the position's description. Heuristic: if a downstream
reader could ask *"What does this corpus say about X?"* and X is a
named philosophical notion, X is a Concept atom.

Examples of abstract concepts that should always be lifted when
they appear:

- *moral responsibility*, *free will*, *agency* (action-theory core)
- *alternative possibilities*, *ability to do otherwise* (PAP family)
- *determinism*, *indeterminism*, *causation* (metaphysics core)
- *Frankfurt cases* (a named thought-experiment family — distinct
  from Frankfurt the Person atom)
- *moral luck*, *moral responsibility under luck* (ethics core)
- *eudaimonia*, *virtue*, *practical wisdom*, *character trait*
  (virtue-ethics core)

Lift the concept the first time the section foregrounds it; don't
re-extract it on every later mention.

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
relation between Frankfurt's objection and the principle of alternate
possibilities may take different stances across sections (defensive,
decisive, overturned-by-a-counter-case).

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
philosopher or position when the claim is placed in their mouth;
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

## Shape example (from SEP "Compatibilism", illustrative)

```json
{
  "section_id": "intro",
  "entities_introduced": [
    {
      "canonical_name": "Compatibilism",
      "aliases": ["soft determinism"],
      "entity_type": "concept",
      "description": "The thesis that free will and determinism are compatible.",
      "anchor": "soft determinism, is the thesis"
    }
  ],
  "relations_introduced": [
    {
      "participants": ["Compatibilism", "Hard determinism"],
      "label": "opposing views on the compatibility of free will and determinism",
      "anchor": "opponents of compatibilism"
    }
  ],
  "claims": [
    {
      "content": "Freedom of will is compatible with causal determinism.",
      "discourse_act": "define",
      "epistemic_status": "attributed",
      "attributed_to": "Compatibilism",
      "anchor": "free will is compatible with"
    }
  ],
  "questions_raised": [
    {
      "content": "Can agents be morally responsible in a deterministic universe?",
      "anchor": "moral responsibility in a deterministic"
    }
  ]
}
```
