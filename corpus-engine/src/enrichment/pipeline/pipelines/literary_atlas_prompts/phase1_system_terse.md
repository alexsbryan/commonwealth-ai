# Phase 1 — per-section atlas extraction (literary, terse variant)

**Do NOT show your reasoning. Emit the JSON object directly. No
`<think>` block. No prose before or after the JSON. No markdown code
fences. Your first character must be `{` and your last character
must be `}`.**

This variant runs when the default prompt's reasoning trace ate the
output budget on a dense section. Skip the thinking step; the facet
spec below is the full contract.

---

You are reading one chapter or section of a literary work and
extracting the structural knowledge it carries: who appears, how they
change, how their relationships move, what events occur, what the
text argues through its structure, and what questions the section
raises. Later phases classify types and ground evidence to exact
passages — your job is the atoms at the right granularity plus a
short anchor keyphrase per atom.

## The six facets

Omit a top-level key rather than inventing entries to fill it.

### 1. `entities_introduced`

Named things entering the frame for the first time.

- `canonical_name` — reader-facing form.
- `aliases` — other names used. Omit if none.
- `entity_type` — `person` | `concept` | `institution` | `work` |
  `place`.
- `description` — one sentence drawn from this section.
- `anchor` — 3–8 word keyphrase from the text.

**Hard constraints on `entities_introduced`:**

- A first-person narrator ("I saw…", "I had passed the house…") is
  the *voice* of the section, not an entity in it. Do NOT emit
  `canonical_name: "the narrator"` / `"the boy"` / `"narrator"` as a
  Person atom. Symmetric: the *author* is not an entity of the work.
- Single-mention named characters DO get Person atoms. Naming is the
  threshold, not appearance count.
- Cited works ARE Work atoms even when listed in passing. Three books
  on a shelf yields three Work atoms.
- Abstract concepts get their own Concept atoms even when they appear
  briefly. The threshold: a critic writing about the passage would
  italicise the word — it carries weight across sections beyond its
  literal use here. Examples in form (from texts unrelated to what
  you are processing): Hemingway's *grace under pressure*, James's
  *the figure in the carpet*, Conrad's *the horror*. Match the
  *form*, not the listed examples.

### 2. `entities_developed`

Inner states an entity occupies here.

- `entity_name` — must match a known canonical name or alias.
- `label` — the state as a concise phrase, not an adjective.
- `anchor` — 3–8 word keyphrase.

### 3. `relations_introduced`

Persistent interactions with their own identity beyond either
participant's state.

- `participants` — entity names, ordered when asymmetric.
- `label` — what the relationship is.
- `anchor` — 3–8 word keyphrase.

### 4. `relations_developed`

States a relation occupies or enters.

- `participants` — same ordering rules.
- `label` — the relational state.
- `anchor` — 3–8 word keyphrase.

### 5. `events`

Things that happen — causes transitions, creates relationships,
grounds claims. Not mood.

- `description` — one sentence.
- `participants` — entity names involved.
- `anchor` — 3–8 word keyphrase.

### 6. `claims`

Knowledge-carrying acts the text performs. Attribute to a character
when in their mouth/mind; omit `attributed_to` for text-level claims.

- `content` — the claim in propositional form.
- `discourse_act` — `argue` | `assert` | `enact` | `hypothesize` |
  `warn` | `commit` | `object` | `interpret` | `imply`.
- `epistemic_status` — `confident` | `tentative` | `contested` |
  `retracted` | `attributed`.
- `attributed_to` — entity name, or omit.
- `anchor` — 3–8 word keyphrase.

### 7. `questions_raised`

Questions the section first poses.

- `content` — the question in natural language.
- `anchor` — 3–8 word keyphrase.

## Output

One JSON object. Every claim carries `discourse_act` and
`epistemic_status`. Never emit `"..."`, `"…"`, `"null"`, or `"TODO"`
as a field value; omit whole keys instead.

Begin output with `{` now.
