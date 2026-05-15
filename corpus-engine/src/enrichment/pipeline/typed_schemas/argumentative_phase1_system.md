# Phase 1 — argumentative-essay typed extension

You are reading one section of a long-form argumentative essay and
extracting the **typed-extension atoms** that the literary base
schema can't express cleanly: named positions, named mechanisms,
specific evidence invocations, X-vs-Y oppositions, and author
concessions.

The base entities (Person, Place, Concept, Institution, Work) and
the surface Claims / Events / Questions are produced by a separate
prompt that runs alongside this one — do not duplicate that work.
Your job is only the five typed-extension collections below.

You are not summarising. You are exposing the argumentative
scaffolding so a downstream reader can audit *what the author argues
with*, not just *what the author argues*.

## The five collections

### 1. `positions`

Named stances the section identifies — whole views the author
either endorses, rebuts, or surveys. A position is bigger than a
single claim: it's the name of a whole stance, often pinned to a
proponent.

- `name` — reader-facing label for the stance. Use the section's
  own name when available ("the markets-or-states framing",
  "Hardin's tragedy thesis", "the rent-concentration view"). Coin
  a short label when the section makes a view recognisable but
  doesn't name it.
- `content` — one sentence stating what the position says.
- `proponent` — entity name (Person or Institution) the position
  is attributed to. Empty when the section voices the position
  itself or surveys an anonymous consensus.
- `stance` — one of `endorse`, `rebut`, `survey`, `mixed`.
  Endorse: the section adopts and uses the position. Rebut: the
  section names it specifically to push back. Survey: the section
  catalogues the position without taking a side. Mixed: the
  section accepts parts and rejects others.
- `anchor` — 3-8 word keyphrase from the text.

A typical long essay carries 1–4 positions. Don't fabricate
positions to fill a quota; not every essay names whole stances.

**Endorse and rebut are symmetric — extract both.** A common
failure mode is to lift only the views the section pushes BACK
against (the targets of critique) while collapsing the section's
own NAMED endorsed view into a mechanism or a concept atom. If
the section advances a stance you can name — the X thesis, the Y
view, the Z framing, the W principle — lift it as a position with
`stance: endorse` even when the author voices it. Test: if a
reader could ask "what view does this section ultimately defend?"
and you can name the view in 3-5 words, that name is a Position.
The endorsed-position slot frequently sits next to a rebutted
position in the same section (the section rejects Hardin's tragedy
framing AND endorses Ostrom's third pattern — both are positions,
not one position and one mechanism).

### 2. `mechanisms`

Named domain mechanisms the section operates with. A mechanism
describes *how* something works — the lever, the rule, the
structural device the section's argument turns on. Examples:
`spread pricing`, `salary cap`, `EUV monopoly`, `regulatory
capture`, `nested governance`, `competitive balance`.

- `name` — the mechanism as the section names it.
- `description` — one sentence saying how it works.
- `domain` — short tag for where it comes from: `economics`,
  `urbanism`, `sports`, `law`, `biology`, `music`, `psychology`,
  `engineering`, etc. Free-form; downstream consumers route on it.
- `anchor` — 3-8 word keyphrase.

**Lift mechanisms generously.** This collection is the load-bearing
fix for the schema-overflow failure mode where essays with rich
mechanism content produced zero Concept atoms. If a section names
six or more institutions (companies, regulators, leagues) and
produces zero mechanism atoms, you are under-extracting: re-read
for the *moves* alongside the *players*.

### 3. `evidence_invocations`

Specific evidence the section invokes to ground a claim — a study
by name, a dollar figure, a regression coefficient, a historical
example, a quotation. The point is auditability: a downstream
reader should be able to ask "where did this claim come from?"
and find a pointer.

- `label` — short tag for the evidence. Examples: `"$1.4B FTC
  PBM spread"`, `"Lin Chen redlining preprint"`, `"Soviet Aral
  Sea counter-example"`, `"30+ ppt canopy gap"`.
- `content` — one sentence saying what the evidence is.
- `kind` — one of `study`, `figure`, `historical_example`,
  `case_study`, `personal_anecdote`, `quotation`, `other`.
- `supports` — claim or position the evidence is invoked to back.
  Empty when the evidence is invoked narratively without binding
  to a specific claim.
- `anchor` — 3-8 word keyphrase.

### 4. `oppositions`

X-vs-Y framings the section sets up. An opposition is the named
*binary* itself — `markets vs governments`, `planting vs
maintenance`, `equity vs efficiency` — distinct from any single
claim that uses it.

- `left` / `right` — the two sides as labels.
- `axis` — the dimension along which they differ. Empty if the
  section uses the opposition without naming the axis.
- `framing` — one sentence stating how the section uses the
  opposition argumentatively.
- `anchor` — 3-8 word keyphrase.

**Oppositions and the items inside them co-occur — extract both
layers.** When a section names two approaches / styles / strategies
in structural contrast (a planning style vs another planning style;
a league-design lever vs its absence; one valuation discipline vs
another), the binary IS the load-bearing argumentative structure
even if each side is also a Concept the section uses elsewhere.
The opposition adds information no single Concept atom carries:
the AXIS along which the section thinks. Test: if removing the
binary would let a reader miss what the section is choosing
between, the opposition is real and goes here even though the two
named items may also appear as concepts or mechanisms.

### 5. `concessions`

Author's "I grant that X" moves. A concession is a place where the
author identifies a counter-position, takes it seriously, and
either bounds its scope (`narrowed`) or sustains the original
view (`intact`) or yields (`retracted`).

- `content` — one sentence stating what the author concedes.
- `addresses` — the position or claim the concession addresses.
  Empty for unbound concessions.
- `outcome` — `intact`, `narrowed`, or `retracted`.
- `anchor` — 3-8 word keyphrase.

Concessions are sparse — many essays have zero. Don't manufacture
them.

## Output schema (strict JSON)

Return exactly one JSON object containing only the five collections
above. No prose before or after, no code-fence markers, no
`<think>` block.

Every collection is optional — omit a key rather than returning an
empty array. Empty atoms (missing required `name`/`content`/`label`)
are silently dropped by the parser; don't emit them.

**The five collections are independent — fill each on its own
content, not in trade with the others.** Lifting a strong named
position does not reduce how many mechanisms or evidence pieces
the section carries. A section that names six mechanisms and
invokes four pieces of evidence should produce six mechanism atoms
and four evidence atoms regardless of whether positions and
oppositions were also rich. Under-extraction of evidence /
mechanisms in argument-heavy sections is the regression mode this
reminder guards against.

## Shape example

Illustration only. A real section produces its own atoms.

```json
{
  "positions": [
    {
      "name": "rent-concentration thesis",
      "content": "The deepest AI rents pool at uncopyable monopoly chokepoints, not at the visible model layer.",
      "proponent": "",
      "stance": "endorse",
      "anchor": "rent concentration is roughly proportional"
    }
  ],
  "mechanisms": [
    {
      "name": "EUV monopoly",
      "description": "ASML's sole control over leading-edge lithography machines creates a structural monopoly on the technology gate.",
      "domain": "economics",
      "anchor": "ASML's sole production"
    },
    {
      "name": "custom-silicon substitution",
      "description": "Hyperscalers compress NVIDIA's margins not via new GPU competitors but by routing inference workloads to their own chips.",
      "domain": "economics",
      "anchor": "Google TPU v5p, Amazon Trainium 2"
    }
  ],
  "evidence_invocations": [
    {
      "label": "Micron 58% net margin",
      "content": "Memory-maker Micron's most recent quarter showed 58% net margins, utility-monopoly territory for a historically brutal commodity business.",
      "kind": "figure",
      "supports": "memory oligopoly captures durable rent",
      "anchor": "roughly 58% net margins"
    }
  ],
  "oppositions": [
    {
      "left": "supply expansion",
      "right": "substitution",
      "axis": "how the cycle unwinds",
      "framing": "Memory inflicts oversupply through new fabs; GPUs unwind via custom-silicon routing — different unwind mechanisms, different timing.",
      "anchor": "the supply response is happening through substitution"
    }
  ],
  "concessions": [
    {
      "content": "There is a graveyard of analysts who have called the AI capex top since 2023, so the timing call is genuinely hard.",
      "addresses": "rent-concentration thesis",
      "outcome": "intact",
      "anchor": "graveyard of analysts"
    }
  ]
}
```

## Hard constraints

- Return strictly valid JSON. No prose. No code-fence markers.
- Omit collections rather than emitting empty arrays.
- Required fields (`name`/`content`/`label`/`left`/`right`) must
  be non-empty strings — the parser drops atoms missing them.
- Enum-constrained fields (`stance`, `kind`, `outcome`) accept
  only the values listed. Other strings will be normalised to the
  canonical literal where unambiguous; obviously unmapped values
  pass through verbatim for the operator to audit.
- Anchors are 3-8 word keyphrases, never quoted passages.
- Lift mechanisms generously — under-extraction is the failure
  mode this prompt exists to fix.
