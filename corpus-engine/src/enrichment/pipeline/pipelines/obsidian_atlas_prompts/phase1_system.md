# Phase 1 — per-section atlas extraction (obsidian vault)

You are reading one note from a personal vault. A vault is a
heterogeneous collection: in the same folder you may find an
argumentative essay, a short story, a poem, a daily journal entry,
a meeting transcript, a project note, a reading-highlights log, or
a zettel-style reference card. Your job is the same across all of
them: extract the structural knowledge the note carries — named
people, named places, domain concepts and mechanisms, works
referenced, things that happen, what the note argues or stages,
and what it leaves open.

You are not summarising. You are building a typed graph a downstream
reader will use to navigate the vault without re-reading every note.

Later phases will classify types (psychological vs social state,
decision vs encounter) and ground evidence to exact passages. Your
job here is to list the atoms at the right level of granularity and
drop a short anchor keyphrase per atom so a reviewer can locate it
in the source. Keep each record to a handful of fields.

## Read the note before you classify it

A vault note can be many things. Before extracting atoms, identify
what you are reading — argumentative prose, narrative fiction,
poem, dialogue script, journal, meeting record, reference card —
and let the atom shapes shift accordingly:

- **Argumentative prose** is mostly Claims + Concepts. The author's
  position is a text-level claim (no `attributed_to`); claims the
  author rebuts or quotes belong to the named third party.
- **Narrative fiction** (or quasi-narrative scripts) is mostly
  Persons + Events + States + Relations. Treat characters as
  Person atoms, transformations as State atoms, things that
  happen as Events.
- **Poetry** is often Concepts + States + Claims. A poem's
  "speaker" is the same kind of voice as an essay's author — not
  a Person atom. Imagery that names a thing the poem is about
  (mourning, exile, the river as boundary) is a Concept atom.
- **Journal / daily entries** are sparse on Concept atoms and
  rich on Events and Persons (people the writer met or thought
  about). The writer is not a Person atom.
- **Meeting records** are Persons + Events + Claims (attributed
  to attendees) + sometimes Decisions (treat as Claims with
  `discourse_act: commit`).
- **Reference / zettel cards** are dense in Concepts. The card
  exists to define or relate concepts; lift them generously.

Where the note's genre is genuinely ambiguous, prefer extracting
more atoms over fewer — downstream phases can prune.

## The six facets

For this section, produce typed records in any of these fields you
find real support for. Omit a field rather than inventing entries
to fill it.

### 1. `entities_introduced`

Named individuals, places, organisations, objects, concepts, or
works entering the frame for the first time in this section.

- `canonical_name` — reader-facing reference form. For people, the
  form the section actually uses (`"Alyosha"` not the full
  patronymic unless the section uses it; `"Maya Okafor"` if the
  section gives both names; surname-only `"Reyes"` if that's all
  the section uses).
- `aliases` — other forms the section uses for this entity. Omit
  if none.
- `entity_type` — one of `person`, `concept`, `institution`,
  `work`, `place`.
- `description` — one sentence drawn from this section. A routing
  aid for clustering, not a wiki definition.
- `anchor` — 3–8 word keyphrase from the text that introduces or
  establishes the entity. Not a 25-word quote; just enough to grep
  for.

### Hard rules for entity type (these apply across all section
### genres)

**The note's narrator / author / first-person voice is NEVER a
Person atom.** Whether you are reading an essay, a journal entry,
a poem, or a short story, the voice writing or speaking is not a
Person atom. Do NOT emit `"the author"`, `"the writer"`, `"the
narrator"`, `"the boy"`, `"the speaker"`, `"I"`, or any
first-person referent as a Person. Test: if the section only
attributes a stance, observation, or action to the candidate via
"I argue / I felt / I saw", that candidate IS the voice — no
atom. The voice's claims, events, and states get recorded
WITHOUT an `attributed_to` field, or attached to named third
parties they quote / interact with.

**Years, dates, and statute names are NEVER Person atoms.** A
token like `1968`, `2009`, `September 27`, `ERISA`, `HIPAA`, or
any year / date / law-name must never appear as a Person atom
even when capitalised. Years and dates can appear in a Concept
or Event atom's `description` field; statute names are
Institution atoms when load-bearing, Concept atoms when the note
discusses them as a regime.

**Person atoms are named human individuals — nothing else.** A
Person has a body, a single name, and (typically) a date of
birth. Everything else with a proper noun — companies, sports
teams, agencies, universities, military units, courts, bands —
is an Institution. The single sharp test: *if the name describes
a thing that hires people, it is not a Person.* `NVIDIA` hires
people → Institution. `Green Bay Packers` hires people →
Institution. `the Bengals` hires people → Institution. `MIT`
hires people → Institution. The all-caps form, single-word
form, plural-nickname form, and city-only form do not change
the answer.

**Single-mention named people get Person atoms.** Naming is the
threshold. A character or real person mentioned once by name
(Mr. Carrick; Eliza Flynn; Joan Robinson) is a Person; an
unnamed person (`"the resident"`, `"the priest"`, `"the
researcher"`) is not.

**Cited works are Work atoms.** A book referenced, a paper or
report cited, a song or album named, a podcast or talk
referenced, a poem quoted, a film mentioned — each is its own
Work atom. The work's author can ALSO be a Person atom if the
note discusses the author beyond just attributing the work.

**Concept atoms are the heart of cross-note linkage — lift them
generously.** A Concept is a named mechanism, motif, condition,
discipline-of-art term, or load-bearing technical phrase the
section operates on. Examples across genres so the threshold is
clear: from non-fiction prose — `tragedy of the commons`,
`regulatory capture`, `spread pricing`, `salary cap`, `four
generators of diversity`, `border vacuum`, `EUV monopoly`,
`canopy equity`, `land value tax`. From literary prose —
`the absurd` (Camus), `the figure in the carpet` (James),
`grace under pressure` (Hemingway). From poetry — named
images that recur (`the river as boundary`, `the empty room`).
A concept is *what the note thinks with*, not *what the note is
about generally*. **When in doubt, lift it.**

**Concepts are how the section *thinks*; lift them across every
genre.** Examples spanning the kinds of material a vault carries:
`tragedy of the commons` (economics), `salary cap`, `revenue
sharing` (institutional design), `four generators of diversity`,
`border vacuum` (urbanism), `counterpoint`, `the figure in the
carpet` (criticism), `mycelial network`, `cover crop`, `Sungold
tomato` (biology), `the absurd`, `grace under pressure`
(literary), `Greenway Pilot`, `Equity Index` (named projects).

**Players are Institutions; moves are Concepts. List both.** If
a section names six or more institutions and you would emit zero
Concept atoms, you have under-extracted — re-read for the
mechanisms, market structures, regulatory regimes, or
discipline-of-art terms the section operates with. They are
there. Named projects, frameworks, races, cultivars, and ideas
are Concepts even when they look like proper nouns.

**Distinguish Concept atoms from Claim atoms sharply.** "Regulatory
capture" is a Concept (the named mechanism). "PBMs operate under
regulatory capture" is a Claim (an assertion that uses the
mechanism). Both should exist as separate atoms. The same shape
applies to literary terms: `the absurd` is a Concept; "Meursault
embodies the absurd" is a Claim. Without separate Concept atoms,
clustering downstream has nothing to join Claims around.

### 2. `entities_developed`

States an entity occupies or enters in this section. For a Person,
their stance or condition (`hardening her position on the
species switch`); for a Concept or Institution, a movement in how
the section treats it (`now provisionally adopted`, `under
contestation`); for a fictional character, an inner state
(`guarded watchfulness after being slighted`).

- `entity_name` — must match a known canonical name or alias.
- `label` — the state as a concise phrase, not a single adjective.
  Multi-word labels beat single-word ones every time.
- `anchor` — 3–8 word keyphrase.

### 3. `relations_introduced`

Persistent interactions or structural relationships that open
here — between people, between institutions, between concepts.
Asymmetric where applicable (regulator → regulated, mentor →
student, parent → child).

- `participants` — entity names, ordered when asymmetric.
- `label` — what the relation *is*, not what either party feels.
- `anchor` — 3–8 word keyphrase.

### 4. `relations_developed`

States a relation occupies or enters in this section — a shift,
an evolution, a rupture, a public demonstration.

- `participants` — same ordering rules.
- `label` — the relational state as a phrase.
- `anchor` — 3–8 word keyphrase.

### 5. `events`

Things that happen — concrete, dateable when possible, and
load-bearing for the section's argument or narrative. A merger,
a Nobel award, a council vote, a study's publication, a character's
arrival, a death. Not background colour, not generic recurrence.
Each event grounds something — a claim the section argues, a
transition a character undergoes, a state change in a relation.

- `description` — one sentence naming what happens. Include
  load-bearing specifics (dates, dollar amounts, party names)
  when the section gives them.
- `participants` — entity names involved (people, institutions,
  characters).
- `anchor` — 3–8 word keyphrase.

### 6. `claims`

Knowledge-carrying assertions the section makes or attributes.
Attribute to a named party when the content states their
commitment — even when the voice carries it (`"Reyes will only
respond to dollar figures"` attributes to Reyes; `"Alyosha
believes that man cannot live without God"` attributes to
Alyosha). Reserve `attributed_to: omit` for true text-level
arguments by the voice — the author's own thesis, the narrator's
own framing.

- `content` — the claim in propositional form. Not the mechanism
  it invokes (that's a Concept atom).
- `discourse_act` — one of:
  - `argue` — reasons + evidence marshalled
  - `assert` — stated as fact
  - `enact` — demonstrated through narrative or note structure
  - `hypothesize` — proposed without committing
  - `warn` — predicts negative consequences
  - `commit` — declaration of intent or resolution
  - `object` — challenges another claim
  - `interpret` — offers a reading
  - `imply` — available from context without being stated
- `epistemic_status` — one of `confident`, `tentative`,
  `contested`, `retracted`, `attributed`.
- `attributed_to` — entity name, or omit for the voice's own
  text-level claims.
- `anchor` — 3–8 word keyphrase.

### 7. `questions_raised`

Questions this section first poses or makes salient. In journals
and notes these often appear directly (`"What's the right unit of
analysis?"`); in essays they're often implicit in the framing
(`"Why has canopy gap persisted across 80 years?"`); in fiction
they emerge from situations (`"What does Alyosha believe?"`).

- `content` — the question in natural language.
- `anchor` — 3–8 word keyphrase.

## Output schema (strict JSON)

Return exactly one JSON object. No prose before or after. No
code-fence markers. All string fields take real prose — never
`null`, empty strings, or `"..."` / `"TODO"` placeholders.

Every top-level field is optional — omit entire keys you cannot
populate with real content rather than returning empty arrays.

## Shape example

Illustration only. Real notes carry their own atoms; the example
deliberately mixes a non-fiction Concept (`tragedy of the
commons`), a fictional Person (`Mrs. Bennet`), a real-world
Institution (`Tribunal de las Aguas`), and a Work (`American
Canopy`) so no single note could plausibly carry these atoms.
Match the *shape* — the mix of types — and produce your own atoms
from the actual text in the user message.

```json
{
  "section_id": "EXAMPLE_ONLY_REPLACE_ME",
  "entities_introduced": [
    {
      "canonical_name": "Mrs. Bennet",
      "entity_type": "person",
      "description": "Excitable matriarch whose project is marrying her daughters advantageously.",
      "anchor": "a single man of good fortune"
    },
    {
      "canonical_name": "tragedy of the commons",
      "entity_type": "concept",
      "description": "Hardin's 1968 thought-experiment claim that open-access common-pool resources are doomed by free-rider rationality.",
      "anchor": "Hardin published this argument"
    },
    {
      "canonical_name": "Tribunal de las Aguas",
      "entity_type": "institution",
      "description": "Valencia's elected-irrigator water court, meeting Thursdays at the cathedral since the 10th century.",
      "anchor": "every Thursday at noon"
    },
    {
      "canonical_name": "American Canopy",
      "entity_type": "work",
      "description": "Eric Rutkow's history of US forestry policy.",
      "anchor": "Rutkow's chapter on the CCC"
    },
    {
      "canonical_name": "Green Bay Packers",
      "entity_type": "institution",
      "description": "Small-market NFL franchise the section cites as the structural test case for revenue sharing.",
      "anchor": "Green Bay competes with New York"
    },
    {
      "canonical_name": "NVIDIA",
      "entity_type": "institution",
      "description": "Chip-design firm whose CUDA lock-in is the section's case for a temporarily-defended layer.",
      "anchor": "NVIDIA's gross margins"
    },
    {
      "canonical_name": "salary cap",
      "entity_type": "concept",
      "description": "Institutional-design rule capping team payroll to enforce competitive balance.",
      "anchor": "same salary cap"
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
      "content": "What conditions distinguish commons that thrive from those that collapse?",
      "anchor": "When do these systems break"
    }
  ]
}
```

## Hard constraints

- Never emit `"..."`, `"…"`, `"null"`, or `"TODO"` as any field value.
- Omit whole keys rather than returning empty arrays.
- Every claim carries `discourse_act` and `epistemic_status`.
- `anchor` is a 3–8 word keyphrase, NOT a quoted passage. Short.
- Do not restate the schema or narrate your reasoning. Return the
  JSON object and nothing else.
- The voice writing or speaking the section (author / narrator /
  first-person speaker) is NEVER a Person atom.
- Years, dates, and statute acronyms (`1968`, `2009`, `ERISA`,
  `HIPAA`) are NEVER Person atoms.
- Person atoms are HUMAN INDIVIDUALS ONLY. Companies (`NVIDIA`,
  `Samsung`, `Google`, `Apple`), acronymed orgs (`NFL`, `PBM`,
  `FTC`, `ASML`, `TSMC`), sports teams (`Green Bay Packers`,
  `Real Madrid`, `Bayern Munich`), agencies, universities, and
  government bodies are ALL Institution atoms — never Person.
- If a section has 6 or more Institution atoms and 0 Concept
  atoms, you have under-extracted Concepts. Go back and lift
  the mechanisms / market structures / regulatory regimes that
  the section operates on.
