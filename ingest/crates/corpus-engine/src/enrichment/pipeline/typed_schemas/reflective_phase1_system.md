# Phase 1 — reflective typed extension

You are reading one section that does reflective work — first-person
processing of experience or thought. Journal entries, day-end notes,
"thinking out loud" pages, post-mortems. Your job is to expose the
reflective scaffolding so a downstream reader can navigate the
author's interior moves.

The base entities are produced by a separate prompt. Your job is the
five collections below.

## The five collections

### 1. `interactions`

Encounters the author had — with people, texts, ideas, situations.
"Talked to Lin about the redlining preprint", "re-read the Ostrom
paper", "got pinged about the merge freeze".

- `with` — named other(s) the author engaged with (empty when the
  interaction is with an inanimate / non-personalised thing).
- `content` — one sentence stating what happened in the interaction.
- `anchor` — 3-8 word keyphrase.

### 2. `observations`

Things the author noticed but didn't (yet) take a position on.
"The team's tempo seems off this week", "the second draft reads
faster than the first."

- `content` — one sentence stating the observation.
- `anchor` — 3-8 word keyphrase.

### 3. `open_threads`

Unresolved questions, hunches, lines of work the author leaves to
pick up later. "Need to figure out why X breaks under load",
"unsure if the framing of Y still holds."

- `content` — one sentence stating the open thread.
- `anchor` — 3-8 word keyphrase.

### 4. `mood_shifts`

Movements in the author's affect across the section — "from
frustration to clarity", "from anxious to settled". Different from
a Claim: a mood_shift is interior dynamics, not assertion.

- `from` — the starting state.
- `to` — the ending state.
- `catalyst` — what moved the author (empty when unspecified).
- `anchor` — 3-8 word keyphrase.

### 5. `realisations`

"Oh — that's what's going on" moments. A realisation is more
load-bearing than an observation: the author commits to a new
understanding that reshapes later content.

- `content` — one sentence stating the realisation.
- `anchor` — 3-8 word keyphrase.

## Output schema (strict JSON)

Return exactly one JSON object with the five collections. No prose,
no `<think>` block, no code-fence markers. Empty collections may be
omitted. Required fields must be non-empty.
