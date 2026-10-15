# Transcript — recipe for *The Saltgrass Ledger*

The partner was not available. Each question is answered from `DESCRIPTION.md`
(source `description`, words quoted) or from general knowledge of the domain
(source `assumption`). Starting point: `sovereign recipe new --ontology literary`
(scaffolded to `literary-template.toml`), the closest of the ten templates.

## Source shape (pre-interview)

**Q. What is the material, and in what format?**
A. One novel manuscript, a village murder mystery in sixteen chapters. I assume a
single markdown file with one heading per chapter, in reading order → `local_file`
+ `markdown` extractor + `paragraph` chunker.
Source: `description` — "**THE SALTGRASS LEDGER** *A Tale of Glasswater Stave*",
"Sixteen chapters of salt, tar and quiet lies" (the description is itself
markdown). The file format is an `assumption`.

## 1. Shape

**Q. What is this material about, and what would you want to know about each kind
of thing? Is any a kind of another, or a part something plays? Any of it already
in a table?**
A. A murder mystery in a salvage port. Kinds of thing:
- **character** — the villagers, the harbormaster, the ledger keeper; want their
  occupation/office and how long they have held it. Source: `description` —
  "Corwin Pellow, harbormaster for twenty-one years", "Maren Ashquill keeps the
  tide ledger … For eleven years".
- **vessel** — boats passing the lock gates. Source: `description` — "every
  vessel through them"; "a salvage port".
- **incident** (event) — what happened, when, where, who was involved: the
  death, gate openings, vessel passages, the harbormaster's morning crossing.
  Source: `description` — "found floating in the basin at dawn", "every opening
  of the gates", "cross the cobbles at the same hour every morning".
- **condition** (state of a character) — afraid, under suspicion, starting to
  doubt; changes across chapters. Source: `assumption` (genre: a mystery moves
  its characters through suspicion and doubt; "Now she starts to wonder" is
  the only direct hint in the description).
- Places (the lock basin, the gates, the cobbles) use the base `place` kind;
  no domain place type is needed. Source: `assumption`.
- No specialisations or roles needed; nothing is in a table. Source:
  `assumption` (a manuscript, no data files mentioned).

## 2. Assertion

**Q. What do the sources say about those things — stating, requiring, deciding,
asking? About what? What does each statement carry? How do you tell strong
evidence from weak? Who is speaking, and are they part of the subject? What must
it never do?**
A. Everything is assertive and inside the story (`scope = "in_work"`):
- **account** — what a character says about an incident (testimony, alibi,
  denial); carries the speaker and where they say they were; graded
  saw-it-themself > told-by-another > village-talk. Source: `description` —
  "why the people she has known all her life are so sure of things they cannot
  have seen", "Nobody will say so above a whisper", "quiet lies".
- **ledger_entry** — what the tide ledger records about a gate opening or
  vessel passage; carries the vessel, the water height on the gauge and the
  keeper. Source: `description` — "every opening of the gates, every vessel
  through them, every height of water against the painted gauge", "what the
  ledger saw".
- **suspicion** — who a character (or the village) thinks is responsible;
  carries holder and suspect; graded evidence-in-hand > motive-only > rumour.
  Source: `description` — "the whole village has decided by breakfast that it
  was no accident".
- **motive** — a reason a character had; carries the reason and whom it
  concerns; graded shown-in-narration > admitted > alleged. Source:
  `description` — "a village where everyone has a reason".
- **connection** — a standing tie between two characters (kin, debt, rivalry,
  secret …), closed set of natures. Source: `assumption` (village mystery
  genre; motives run through ties). Added after validation run 1 (see below)
  in place of a `tie` relation the reader never fills.
- Speakers: characters, the ledger, the village, the narrator; the narrator is
  not a character. Source: `assumption` (third-person narration inferred from
  the blurb's voice).
- must_not: never present an account, suspicion or village talk as what
  happened; never invent clues or a solution. Source: `assumption`, grounded in
  `description` — "the truth comes in with the tide".

**Follow-up (schema): every claim needs a subject the document reader can
resolve. What is each claim about?**
A. account, ledger_entry, suspicion → the incident; motive, connection → the
character who has it. Source: `assumption`.

## 3. Identity

**Q. How do you know two mentions are the same thing? Is there an ID?**
A. No external IDs.
- character: same individual by full name, surname, given name, nickname, or an
  office title pointing at them at that point in the story; a title passes to
  the next holder. Source: `assumption`; the title case comes from
  `description` — "Corwin Pellow, harbormaster".
- incident: same kind of happening, same place, same time or tide, same people
  or vessel; two gate openings on different tides are two incidents. Source:
  `assumption`.
- vessel: her name (canonical name). Source: `assumption`.

## 4. Change

**Q. When does a later statement replace an earlier one, and from when? Does
anything move through stages? Which document fields give date and thread?**
A. Nothing supersedes. The clock is narrative (chapter order), not the
calendar. A character changing their story is a discrepancy to keep, not a
correction that retires the first version. No stage protocol (no fold): a
suspect's status is not declared by anyone with authority in the story as far
as the description shows. No `change.document` fields: the manuscript's chapters
carry no dates, and the extractor's field names couldn't be checked without the
material (`--corpus` not available). Source: `assumption`; narrative order
from `description` — "Sixteen chapters".

## 5. Derivation

**Q. What should it notice that no single document says? What looks like a
contradiction but isn't?**
A. Discrepancies between accounts, ledger entries and suspicions about the same
incident, across chapters (`same = ["subject"]`, not the default
subject+clock, which would only compare claims in the same chapter). Also
scene configurations (who is together, in what grouping). Source:
`description` — "she starts to wonder what the ledger saw, and why the people
… are so sure of things they cannot have seen". Not contradictions: ledger
entries for different tides; rival suspicions with nothing contradicting them;
an office title moving to a new holder. Source: `assumption`.

## Validation runs

### Run 1 — `sovereign recipe validate recipe.toml`

```
Validating recipe: /Users/alexsbryan/blind-author-20261014/saltgrass/recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: narrative — supersession folds on order within the work
  tension selector: embedding top-k (k = 10, floor = 0.5) over account, ledger_entry, suspicion — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: character → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: vessel → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: incident → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [character, vessel]; relations [tie]; events [incident]; aggregate [account by speaker, ledger_entry by vessel, ledger_entry by keeper, suspicion by holder, suspicion by suspect, motive by toward]
  fill: character: occupation ← Point on each motive statement, not the record; tenure ← Point on each motive statement, not the record
  fill: vessel: master ← nothing; rig ← nothing
  fill: incident: at ← Point on each account, ledger_entry, suspicion statement, not the record; what ← Choose on each account, ledger_entry, suspicion statement, not the record
  fill: tie: nature ← nothing
  fill: account: subject `incident` ← RESOLVE; speaker ← Pick on account (candidates: Mention); where_they_were ← Point on account; at ← Point on account
  fill: ledger_entry: subject `incident` ← RESOLVE; vessel ← Pick on ledger_entry (candidates: Mention); water_height ← Point on ledger_entry; at ← Point on ledger_entry; keeper ← Pick on ledger_entry (candidates: Mention)
  fill: suspicion: subject `incident` ← RESOLVE; holder ← Pick on suspicion (candidates: Mention); suspect ← Pick on suspicion (candidates: Mention)
  fill: motive: subject `character` ← RESOLVE; reason ← Point on motive; toward ← Pick on motive (candidates: Mention)
  ⚠  RESOLVE decides `vessel` by its identity_criterion, but no claim kind names it as its `subject`: `enrich extract` refuses to build this, since no statement of it would be read. Declare a claim kind about it.
  ⚠  ontology type `vessel`: nothing fills `master` (no read claim kind, nor the subject of one, asks it), `rig` (no read claim kind, nor the subject of one, asks it). Each stays empty on every record; give it a source or a derivation, or drop it.
  ⚠  ontology type `tie`: nothing fills `nature` (no read claim kind, nor the subject of one, asks it). Each stays empty on every record; give it a source or a derivation, or drop it.
```

Passed, but the derived facets disagree with the answers:
- `vessel` would block `enrich extract` → dropped its `identity_criterion` so it
  resolves by canonical name (the answer to Q3 anyway), and dropped `master` /
  `rig`, which nothing reads.
- `tie.nature` never filled → replaced the `tie` relation with a `connection`
  claim about a character (`other`, `nature`), which the reader does read.
- `character.occupation` / `tenure` stayed on statements, not the record →
  added `by = "most"`. `incident.what` → `by = "agree"`. `incident.at` stays
  per-statement on purpose: differing times are what discrepancies are made of.
- `account.at` / `ledger_entry.at` duplicated the subject's `at` read on each
  statement → removed.

### Run 2 — `sovereign recipe validate recipe.toml`

```
Validating recipe: /Users/alexsbryan/blind-author-20261014/saltgrass/recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: narrative — supersession folds on order within the work
  tension selector: embedding top-k (k = 10, floor = 0.5) over account, ledger_entry, suspicion — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: character → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: vessel → canonical name (default; declare `identity = [...]` for an external key)
  identity: incident → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [character, vessel]; relations [(none)]; events [incident]; aggregate [account by speaker, ledger_entry by vessel, ledger_entry by keeper, suspicion by holder, suspicion by suspect, motive by toward, connection by other]
  fill: character: occupation ← Point on each connection, motive statement, the record by most; tenure ← Point on each connection, motive statement, the record by most
  fill: incident: at ← Point on each account, ledger_entry, suspicion statement, not the record; what ← Choose on each account, ledger_entry, suspicion statement, the record by agree
  fill: account: subject `incident` ← RESOLVE; speaker ← Pick on account (candidates: Mention); where_they_were ← Point on account
  fill: ledger_entry: subject `incident` ← RESOLVE; vessel ← Pick on ledger_entry (candidates: Mention); water_height ← Point on ledger_entry; keeper ← Pick on ledger_entry (candidates: Mention)
  fill: suspicion: subject `incident` ← RESOLVE; holder ← Pick on suspicion (candidates: Mention); suspect ← Pick on suspicion (candidates: Mention)
  fill: motive: subject `character` ← RESOLVE; reason ← Point on motive; toward ← Pick on motive (candidates: Mention)
  fill: connection: subject `character` ← RESOLVE; other ← Pick on connection (candidates: Mention); nature ← Choose on connection
```

Facets read against the answers: clock narrative ✓ (Q4); tension over account /
ledger_entry / suspicion ✓ (Q5); character and incident by their criteria,
vessel by name ✓ (Q3); every declared attribute has a filler, no warnings.
No further changes.
