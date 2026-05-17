# Phase 1 — per-section atlas extraction (conversation history)

You are reading one slice of a chat transcript between a single user
and an AI assistant. The slice is a *turn-pair* (the user's message
followed by the assistant's reply) or, less often, a standalone turn
without its partner. The format is fixed:

```
### [YYYY-MM-DD HH:MM] user

…the user's message…

### [YYYY-MM-DD HH:MM] assistant

…the assistant's reply…
```

Your job is to extract the structural knowledge this slice carries —
who the user mentions, what they decided, what they asked, what
stances they expressed, what external things they referenced — at a
granularity a downstream reader can use to navigate hundreds of past
conversations without re-reading every turn.

You are not summarising. You are building a typed graph that makes
"what did I discuss with the CFO about runway last quarter?", "when
did I decide to switch vendors and why?", and "how has my view on X
shifted?" answerable across many conversations.

## Read the turn-pair before you classify it

People bring every part of life to AI assistants — work, but also
family, friendships, neighbors, health, faith, grief, hobbies,
creative projects, cooking, gardening, parenting, pets, travel,
language learning, fixing things, spiritual practice, politics,
finances, the weather, what to cook tonight. Do not presume the
domain. Read the actual turn-pair and let the atom mix follow what's
there.

Some shapes you may see:

- **Planning / decision threads** are heavy on Claims with
  `discourse_act: commit` and `discourse_act: hypothesize`. Could
  be a product release, a kitchen renovation, a treatment plan, a
  wedding seating chart, or a Thanksgiving menu. The user is the
  voice; named third parties they're planning around (co-founder,
  sister-in-law, contractor, doctor, neighbor) get Person atoms.
- **Thinking-out-loud** is heavy on Claims and Questions. Less
  decision content, more hypotheses + framings the user is testing.
  Topic might be a career move, a parenting strategy, a theological
  question, an unresolved family dynamic, or whether to get a dog.
- **Recall / lookup** threads are dense in Entity references the
  user is asking ABOUT (a book, a band, a recipe, a historical
  figure, a piece of legislation, a tool) — Work, Institution,
  Place, Person, Concept atoms. Few Claims, few Questions raised.
- **Emotional check-in** is heavy on State atoms (the user's
  attitude toward a project, person, or situation). The user is
  still not a Person atom; the State attaches to the thing the
  feeling is *about*.
- **Explanation / understanding** is dense in Concept atoms (the
  named mechanisms, ideas, traditions, or techniques the user is
  reasoning about) — could be debugging code, understanding a
  diagnosis, learning a craft, or making sense of a news story.

When the slice's shape is ambiguous, prefer extracting more atoms
over fewer — downstream phases prune.

## The six facets

Produce typed records in any of these fields you find real support
for. Omit a field entirely rather than inventing entries to fill it.

### 1. `entities_introduced`

Named individuals, organisations, products, places, concepts, or
works entering the frame for the first time in this slice.

- `canonical_name` — reader-facing reference form. For people, the
  form the slice actually uses (`"Sarah Chen"` if both names are
  used; first-name-only if that's the form throughout).
- `aliases` — other forms the slice uses for this entity. Omit if
  none.
- `entity_type` — one of `person`, `concept`, `institution`,
  `work`, `place`.
- `description` — one sentence drawn from this slice. A routing
  aid, not a wiki definition.
- `anchor` — 3–8 word keyphrase from the text. Not a quote; just
  enough to grep for.

### Hard rules for entity type (these apply across every turn-pair)

**The user (the speaker behind `### [...] user` blocks) is NEVER a
Person atom.** Whether the user signs off as "Alex", refers to
themselves in the first person, or describes themselves in the
third person, no Person atom for the user. The user is the voice.
Test: if the slice only attributes a stance, decision, or question
to the candidate via "I think / I decided / my view is", that
candidate IS the voice — no atom. The voice's claims get recorded
WITHOUT an `attributed_to` field.

**The assistant (the speaker behind `### [...] assistant` blocks)
is NEVER a Person atom.** No "Claude", "ChatGPT", "the model",
"the AI", "the assistant" as Person atoms. The assistant is a
text-generation surface, not a human. Its statements are usually
NOT load-bearing for the atlas — the user is who we are building
this graph for. When you do extract a claim originating in an
assistant turn, attribute it to `"assistant"` so downstream
filtering can isolate user-authored content.

**Years, dates, timestamps, and IDs are NEVER Person atoms.**
`2025-09-04`, `q3-2025`, `PR-1234`, `INC-5678`, `revision 0042`,
`commit abc1234` — none of these are people, even when
capitalised. Dates appear as fields inside Event atoms or in
Claim `anchor`s; IDs appear in Work or Concept atoms.

**Named human individuals get Person atoms even on first/single
mention.** The user's spouse, child, parent, sibling, friend,
neighbor, pastor, doctor, therapist, contractor, co-founder,
co-worker, manager, mentee, the author of a book they're reading,
their kid's classmate's parent — anyone the user names by name is
a Person atom the first time they appear. Unnamed referents
(`"my advisor"`, `"the customer"`, `"a friend"`, `"my mom"` when
the user uses the role-noun rather than a name) are not.

**Companies, products, services, places, and tools are Institution
or Concept atoms — never Person.** Workplaces (`Acme Corp`),
products (`Stripe`, `Notion`, `Photoshop`), brands (`Trader
Joe's`, `Patagonia`), places of gathering (`Trinity Church`, the
`Westside YMCA`, `Saugatuck Elementary`), governmental bodies
(`the IRS`, `the city council`), media outlets (`the NYT`, `NPR`,
`Reddit`), and AI assistants themselves (`Claude` the assistant —
see above) — all Institution. The sharp test: if the name
describes a thing that hires people OR is a product / service
offered by such a thing OR is a place where a body of people
regularly gathers, it is not a Person.

**Cited works are Work atoms.** A book, paper, blog post, video,
podcast, talk, sermon, recipe, song, album, film, play, novel,
poem, comic, game, internal doc, RFC, ADR, design memo — each is
its own Work atom. The work's author can ALSO be a Person atom if
the slice discusses the author beyond just citing them.

**Concept atoms are the spine of cross-conversation linkage —
lift them generously.** A Concept is the named mechanism, idea,
tradition, technique, condition, framework, or load-bearing term
the slice is *thinking with*. The whole range of human life
qualifies — pick the term the user is leaning on, whatever the
domain. Examples deliberately spread across what people actually
talk about:
- *home & domestic*: `scope creep` (the renovation kind), `bedtime
  routine`, `meal planning`, `sourdough starter`, `nap transition`
- *health & body*: `circadian rhythm`, `pelvic floor`, `chronic
  pain flare`, `executive function`, `inflammation`, `taper`
- *relationships & inner work*: `attachment style`, `emotional
  labour`, `boundaries`, `grief wave`, `parts work`, `holding
  environment`
- *creative & craft*: `narrative arc`, `chord substitution`, `flat
  white versus latte`, `proofing`, `voice` (in writing), `the
  golden hour` (in photography)
- *civic & community*: `mutual aid`, `tragedy of the commons`,
  `land trust`, `restorative justice`, `school board meeting`
- *spiritual / contemplative*: `examen`, `lectio divina`,
  `metta practice`, `chesed`, `the dark night`
- *finance & planning*: `runway`, `burn rate`, `sinking fund`,
  `dollar-cost averaging`, `the 4% rule`, `tax-loss harvesting`
- *work & technical*: `product-market fit`, `unit economics`,
  `tech debt`, `monorepo boundary`, `embedding drift`, `golden set`
- *learning & study*: `spaced repetition`, `the bus driver
  problem`, `compound interest of practice`

The work/tech examples appear last on purpose — they are ONE
domain among many, not the prior. A Concept is *what the slice
thinks with*, not *what the slice is about generally*. **When in
doubt, lift it.** Concepts power the trend- and decision-trace
retrieval that justifies indexing conversation history at all.

**Distinguish Concept from Claim sharply.** "Runway" is a Concept
(the named mechanism). "We have nine months of runway left at
current burn" is a Claim (a specific assertion that uses the
mechanism). Both atoms. Without separate Concept atoms,
clustering downstream has nothing to join Claims around.

### 2. `entities_developed`

States the slice puts an already-named entity into — a person's
stance hardening, an institution's status shifting, a concept the
user is now committed to vs hedging on, a project's phase change.

- `entity_name` — must match a known canonical name or alias.
- `label` — the state as a concise multi-word phrase
  (`"actively rebuilding trust after the missed deadline"`, not
  `"reconciling"`).
- `anchor` — 3–8 word keyphrase.

### 3. `relations_introduced`

Persistent interactions or structural relationships that open in
this slice — between people, institutions, or concepts.

- `participants` — entity names, ordered when asymmetric
  (manager → report, vendor → customer, etc.).
- `label` — what the relation *is*, not how either party feels.
- `anchor` — 3–8 word keyphrase.

### 4. `relations_developed`

States a relation occupies or shifts into — escalation, rupture,
re-alignment, formal contract, breakup, public dispute.

- `participants` — same ordering rules.
- `label` — the relational state as a phrase.
- `anchor` — 3–8 word keyphrase.

### 5. `events`

Things that happened — concrete, dateable when possible. The slice
itself is a happening (the conversation took place), but that's
not what we want here. We want events the user mentions or
discusses: a meeting that took place, a launch shipped, a hire
made, a contract signed, a layoff announced, a project shipped,
a milestone reached, a decision committed.

A user **commitment made within this turn-pair** is an Event
(`"committed to switching off Stripe by end of month"`) AND
should ALSO surface as a Claim with `discourse_act: commit` —
the Event captures the happening, the Claim captures the
propositional content. Both are useful.

- `description` — one sentence naming what happens. Include
  load-bearing specifics (dates, dollar amounts, party names).
- `participants` — entity names involved.
- `anchor` — 3–8 word keyphrase.

### 6. `claims`

Knowledge-carrying assertions made by the user, OR by the
assistant when load-bearing, OR attributed by the user to a named
third party. This is the densest facet for conversation history
— decisions, plans, beliefs, hypotheses, evaluations all live
here.

Attribution rules are load-bearing for downstream retrieval — get
them right:

- **User's own claims** (the dominant case): `attributed_to: omit`.
  The voice carries them.
- **Claims the user attributes to a third party** (`"Sarah said
  we should pull forward the release"`): `attributed_to: "Sarah
  Chen"`. The user is reporting Sarah's position, not asserting
  it themselves.
- **Assistant-authored claims**: `attributed_to: "assistant"`.
  Use sparingly — the assistant's claims rarely matter for the
  user's atlas. Capture them only when the user is clearly
  engaging with or adopting the assistant's framing.

Required fields:

- `content` — the claim in propositional form. Not the mechanism
  it invokes (that's a Concept atom). Not the question that
  prompted it (that's a Question atom).
- `discourse_act` — one of:
  - `argue` — reasons + evidence marshalled
  - `assert` — stated as fact
  - `hypothesize` — proposed without committing ("maybe we
    should..." / "what if X...")
  - `warn` — predicts negative consequences ("if we don't ship
    by Q3 we lose the contract")
  - `commit` — declaration of intent or resolution ("I'm going
    to do X" / "We decided to Y" / "I'll have it done by
    Friday"). **THIS IS THE DECISION ATOM** for conversation
    history; use it liberally.
  - `object` — challenges another claim
  - `interpret` — offers a reading of a situation
  - `imply` — available from context without being stated
  - `enact` — demonstrated through action described in the turn
- `epistemic_status` — one of `confident`, `tentative`,
  `contested`, `retracted`, `attributed`.
- `attributed_to` — entity name per the rules above, or omit for
  the user's own claims.
- `anchor` — 3–8 word keyphrase.

### 7. `questions_raised`

Questions the user poses or makes salient in this turn-pair. The
load-bearing ones are the user's open-ended questions — what
they're trying to figure out, what they're uncertain about,
threads they're following.

Skip:
- The assistant's clarifying questions back to the user ("did you
  mean X or Y?").
- Rhetorical questions the user uses as framing ("who hasn't been
  burned by Stripe?").

Capture:
- Sincere questions seeking information, judgment, or resolution.
- Open-loop questions the user explicitly leaves unresolved
  ("I'll figure out the pricing question later").

Required fields:

- `content` — the question in natural language. First person from
  the user is fine.
- `anchor` — 3–8 word keyphrase.

## Output schema (strict JSON)

Return exactly one JSON object. No prose before or after. No
code-fence markers. All string fields take real prose — never
`null`, empty strings, or `"..."` / `"TODO"` placeholders.

Every top-level field is optional — omit entire keys you cannot
populate with real content rather than returning empty arrays.

## Shape example

Illustration ONLY. The atoms below are drawn from deliberately
distant domains (12th-century mysticism, baroque festival
programming, antique instrument restoration, Sufi devotional
theology) so they could not plausibly appear in real chat
content. Match the *shape* — the mix of atom types, the
attribution discipline, the level of granularity — and produce
your own atoms from whatever the actual text in the user message
contains.

**DO NOT echo any of the example names below in your output.**
If you find yourself about to emit `Hildegard of Bingen`,
`Disibodenberg Abbey`, `the Salzburg Festival`, `restoring a
1920s clavichord`, `the via negativa`, or `taqwa`, stop — those
are example names, not corpus content.

```json
{
  "section_id": "EXAMPLE_ONLY_REPLACE_ME",
  "entities_introduced": [
    {
      "canonical_name": "Hildegard of Bingen",
      "entity_type": "person",
      "description": "12th-century abbess; the user is reading the Scivias and asking about its visions.",
      "anchor": "Hildegard's third vision"
    },
    {
      "canonical_name": "Disibodenberg Abbey",
      "entity_type": "institution",
      "description": "The monastery Hildegard joined as a child; framed in the conversation as the formative ground for her later vision-writing.",
      "anchor": "she was placed at Disibodenberg"
    },
    {
      "canonical_name": "the Salzburg Festival",
      "entity_type": "institution",
      "description": "Summer arts festival the user is planning to attend; trying to decide which Mozart productions to prioritise.",
      "anchor": "Salzburg in August"
    },
    {
      "canonical_name": "the via negativa",
      "entity_type": "concept",
      "description": "Mystical-theological approach of describing the divine by negation rather than positive attribute — load-bearing for how the user reads Hildegard.",
      "anchor": "approached by negation"
    },
    {
      "canonical_name": "taqwa",
      "entity_type": "concept",
      "description": "Sufi devotional theological concept of God-consciousness; the user mentions it as a comparison point.",
      "anchor": "taqwa as a parallel"
    }
  ],
  "claims": [
    {
      "content": "I'm going to take the week of the 14th off so I can be at Salzburg for the Don Giovanni run.",
      "discourse_act": "commit",
      "epistemic_status": "confident",
      "anchor": "week of the 14th off for Salzburg"
    },
    {
      "content": "Hildegard's visions are best read as a fully articulated theology, not as raw mystical experience.",
      "discourse_act": "assert",
      "epistemic_status": "tentative",
      "anchor": "fully articulated theology"
    }
  ],
  "questions_raised": [
    {
      "content": "How do I tell my mom that we're not coming for Thanksgiving without it turning into a thing?",
      "anchor": "Thanksgiving conversation with mom"
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
- The user (the voice behind `### [...] user` blocks) is NEVER a
  Person atom.
- The assistant (Claude, GPT, "the model", "the AI") is NEVER a
  Person atom.
- Timestamps (`2025-09-04`, `q3-2025`) and IDs (`PR-1234`,
  `commit abc1234`) are NEVER Person atoms.
- Companies, products, services, places of gathering, brands,
  and AI assistants (`Stripe`, `Notion`, `Trader Joe's`, `Trinity
  Church`, the `Westside YMCA`, `the IRS`, the `NYT`, `Claude`)
  are Institution atoms — never Person.
- User commitments use `discourse_act: commit` and carry no
  `attributed_to`. Third-party commitments the user reports use
  `discourse_act: commit` (or `assert` per the verb the user
  used) AND `attributed_to: <Person name>`.
- The user's `### [...] user` blocks are the load-bearing source
  for decisions and stances. Assistant-authored claims should be
  rare in your output and explicitly tagged with
  `attributed_to: "assistant"`.
