# Transcript — recipe for *The Saltgrass Ledger*

The partner was not available, so each question below is answered from `DESCRIPTION.md`
where it speaks, and otherwise from general knowledge of how a mystery novelist works
with their own manuscript. Sources: `description` (quoted) or `assumption`.

Before answering, I looked at the templates (`sovereign recipe new --ontology list`) and
scaffolded the closest one, `literary`, into `scaffold-literary.toml` for reference
(the first try, writing it to `/tmp`, was denied for needing approval). The recipe below
starts from its shape: one markdown file, narrative clock, narrator not an entity. It
then replaces the template's critic-facing `reading` claim with the claims this book runs on.

## 0. Framing: what is the material, whose is it, what is the source shape?

**Answer.** It is a finished or near-finished mystery novel in sixteen chapters. The
owner is its author. They want to use the corpus to check the book's machinery: who
claims what, how they could know it, and whether that matches the tide ledger. The
source is one manuscript file, exported to markdown with one heading per chapter in
reading order. So `acquire.type = "local_file"` with `path = "SOURCE_PATH"`, the
`markdown` extractor (one document per heading-bounded section, which here means per
chapter), and `paragraph` chunks.

**Source.** description: "Sixteen chapters of salt, tar and quiet lies"; "*A Tale of
Glasswater Stave*" (a novel, owned by its author). assumption: that the owner is the
author and not a reader or critic; that the manuscript is markdown with chapter
headings. A novelist more often works in .docx or Scrivener, and the schema has no
extractor for those, so a markdown export is the practical route. `plaintext` would
lose the chapter boundaries.

## 1. Shape: "What is this material about, and what would you want to know about each kind of thing? Is any a kind of another, or a part something plays? Any of it in a table?"

**Answer.**
- **character**: the people of Glasswater Stave. This is a part a `person` plays
  (`role_of = "person"`). Want to know: occupation in the port.
- **vessel**: boats through the gates and in the basin. Want to know: what sort of
  craft and who owns it.
- **object**: physical things the story dwells on (the ledger itself, the painted gauge,
  rope, lantern, key). Want to know: whose it is.
- **incident** (event): Pellow's death, plus any other happening the village talks about.
  Participants: victim, finder, place.
- **gate_opening** (event): one opening of the lock gates on one tide. Participants:
  vessel, keeper.
- **inner_state** (state of character): Maren's doubt and growing suspicion, others'
  fear and guilt, in book order.
- Places (the lock basin, the gates, the cobbles) use the base `place` kind and are
  not declared.
- No tables exist. Everything is prose.

**Source.** description: "a salvage port on the Hethering coast, the lock basin … behind
heavy gates"; "Corwin Pellow, harbormaster for twenty-one years … found floating in the
basin at dawn"; "Maren Ashquill keeps the tide ledger: every opening of the gates, every
vessel through them, every height of water against the painted gauge"; "Now she starts
to wonder". assumption: that objects and an inner-state trajectory matter to the author
(standard for a mystery: clues and the detective's arc), and that there is no table.

## 2. Assertion: "What do the sources say about those things? About what? What does each statement carry? How do you tell strong evidence from weak? Who speaks? What must it never do?"

**Answer.** Everything is said inside the story (`scope = "in_work"`), and all of it is
assertive. Six claim kinds:
- **account** (about a character): someone places a person somewhere at some hour.
  Carries speaker, place, hour, what they were doing.
- **sighting** (about a vessel): the same, for a boat. Carries speaker, place, hour, who
  was aboard.
- **ledger_entry** (about a gate_opening): what the tide ledger records. Carries hour,
  vessel, direction (closed set: inward / outward / no vessel), gauge height in feet,
  and the hand that wrote it.
- **verdict** (about an incident): what someone holds about how it came about. Carries
  speaker, cause (accident / murder / self-inflicted / undecided), whom they blame, and
  whether it was said openly, whispered, or kept to themselves.
- **motive** (about the character who has it): a reason to want harm done, or a secret.
  Carries against whom, the reason (debt / salvage / smuggling / family / love / office /
  secret), and speaker.
- **notice** (about an object): where it was, in what state, who held it.

Evidence grades (strongest first): accounts and sightings `saw it > heard it > inferred >
hearsay`. Ledger `written at the time > written after > altered`. Verdicts
`from evidence > from reasoning > village talk`. Motives `shown > told > suspected`.
Notices `shown > reported`.

Who speaks: the characters. The narrator is not an entity. Must never: treat the story
as real-world fact, or present a villager's certainty about the killer as the answer.

**Source.** description: "the whole village has decided by breakfast that it was no
accident. Nobody will say so above a whisper" (verdict, cause, the `aloud` set);
"why the people she has known all her life are so sure of things they cannot have
seen" (the grades separating seen from heard/inferred/hearsay); "what the ledger saw"
and "every height of water against the painted gauge" (ledger_entry and gauge);
"everyone has a reason" (motive); "quiet lies" (accounts that can be false).
assumption: the gauge unit (feet), the reason and cause value sets, the ledger grades
(that the ledger might be altered is my guess at the plot), notice as a claim kind.

## 3. Identity: "How do you know two mentions are the same thing? Is there an ID?"

**Answer.** There are no IDs. Every type is resolved by an `identity_criterion`:
- character: name, surname, nickname or office. "Pellow", "Corwin" and "the harbormaster"
  are one man while he holds the office. After his death the title names whoever holds it
  next. People who share a surname are different people unless the text joins them.
- vessel: its name, ignoring "the" and case. A described but unnamed boat ("the black
  lighter") is matched to a named one only when the text ties them. A renamed vessel is
  still one vessel.
- object: the same thing as the text treats it ("the ledger" = "Maren's ledger"). Two of a
  kind stay two.
- incident: the same happening to the same person or place. Every mention of Pellow's
  drowning is one incident.
- gate_opening: the same tide and hour. A ledger line and a remembered "gates at dawn"
  are one opening when they name the same tide.

**Source.** description: "Corwin Pellow, harbormaster for twenty-one years" (the office
is an alias of the man); "every opening of the gates" (openings are countable per tide).
assumption: all the rules themselves, and the successor-harbormaster case.

## 4. Change: "When does a later statement replace an earlier one? Does anything move through stages? Which document fields give date and thread?"

**Answer.** Nothing replaces anything. In a mystery, a story that changes is a lie being
exposed, and the author wants both versions kept and set against each other. So no
`supersedes`. The tension pass handles them. Time is the order of the book:
`clock = "narrative"`. No protocol fold: the only thing that moves through stages is a
character's inner state, which is the `inner_state` trajectory. Chapters have no dates
or threads, so there is no `change.document`. I have no material to run
`validate --corpus` against, and I did not want to guess field names.

**Source.** description: "Sixteen chapters", "quiet lies", "the truth comes in with the
tide" (truth arrives late, and the earlier lies matter). assumption: no supersession;
no document metadata.

## 5. Derivation: "What should it notice that no single document says? What looks like a contradiction but isn't?"

**Answer.** It should notice contradictions between accounts, sightings, ledger entries
and verdicts, across subjects: a villager placing Pellow on the cobbles against a ledger
that shows the gates open with no one logged; a sighting of a boat at an hour the ledger
has the gates shut. So `tension.between = [account, sighting, ledger_entry, verdict]`,
`same = []` (the default `same` would compare only claims about the same subject, which
rules out exactly the person-vs-ledger pairs), and `label = "contradiction"`. Also
interpretive configurations (`derive.configurations = true`), meaning groupings of
characters in scenes.

Not contradictions: two accounts of one person at different hours of the same morning; a
ledger entry and an account about different tides or days; one rumour repeated by many
(that is spreading, not corroboration); a verdict that changes as the speaker learns more
and says why.

**Source.** description: "what the ledger saw, and why the people … are so sure of things
they cannot have seen"; "everyone has a reason". assumption: the `not_conflicts` list and
turning configurations on.

## 6. Questions: "Once it is read, what will you ask of it?"

**Answer.** Each question, and where its answer lands:
1. *Where does each character say they were at dawn, and who else places them there?*
   → `account` (subject character, `speaker`, `place`, `at`), aggregated by speaker / place.
2. *Which accounts contradict the ledger?* → tension between `account` / `sighting` and
   `ledger_entry`.
3. *Which vessels went through the gates the night Pellow died, and who wrote each
   entry?* → `ledger_entry` (`vessel`, `direction`, `gauge`, `hand`) on `gate_opening`;
   `sighting` by `aboard`.
4. *Who believes it was murder, whom do they blame, and who says so aloud?* → `verdict`
   (`cause`, `blamed`, `aloud`), aggregated by speaker / blamed.
5. *Who claims to know things they cannot have seen?* → grades `hearsay` / `inferred` on
   `account`, `sighting` and `village talk` on `verdict`.
6. *What reason did each person have, and against whom?* → `motive` (`against`, `reason`).
7. *Where is each object first shown, and who has it later?* → `notice` (`place`,
   `holder`) on `object`, in narrative order.
8. *How does Maren's doubt grow chapter by chapter?* → `inner_state` of character,
   narrative clock.

Every question lands in a declared type. Before the first validation run, nothing would
have answered "where was a boat at an hour" outside the ledger. That gap became the
`sighting` claim (see run 1).

**Source.** assumption (the description poses no questions). Questions 2 and 5
paraphrase the description's "what the ledger saw" and "so sure of things they cannot
have seen".

## Validation runs

### Run 1: `sovereign recipe validate recipe.toml`

```
✓ Validation passed
Derived from your declarations:
  clock: narrative — supersession folds on order within the work
  tension selector: embedding top-k (k = 10, floor = 0.5) over account, ledger_entry, verdict — …
  identity: character / vessel / object / incident / gate_opening → RESOLVE under its identity_criterion …
  question shapes: enumerate [character, vessel, object]; relations [(none)]; events [incident, gate_opening]; aggregate […]
  fill: character: occupation ← Point on each account, motive statement, not the record
  fill: vessel: craft ← nothing; owner ← nothing
  fill: object: owner ← Pick on each notice statement, not the record (candidates: Mention)
  …
  ⚠  RESOLVE decides `vessel` by its identity_criterion, but no claim kind names it as its `subject`: `enrich extract` refuses to build this, since no statement of it would be read. Declare a claim kind about it.
  ⚠  ontology type `vessel`: nothing fills `craft` …, `owner` …. Each stays empty on every record; give it a source or a derivation, or drop it.
```

Validation passed, but the build would refuse: no claim kind was about a vessel. The
right fix is a declaration, the one question 3 already needed: **sighting** (assertive,
in_work, subject `vessel`, with speaker / place / at / aboard and the same grades as
`account`), added to `tension.between`. The facets also showed that `occupation` stays
only on statements and never reaches the character record. A person's trade is stable,
so I added `by = "most"` to `character.occupation` and to `vessel.craft` / `vessel.owner`.
`object.owner` stays per-statement because objects change hands in a mystery.

### Run 2: `sovereign recipe validate recipe.toml`

```
✓ Validation passed

Derived from your declarations:
  clock: narrative — supersession folds on order within the work
  tension selector: embedding top-k (k = 10, floor = 0.5) over account, sighting, ledger_entry, verdict — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: character → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: vessel → RESOLVE under its identity_criterion, …
  identity: object → RESOLVE under its identity_criterion, …
  identity: incident → RESOLVE under its identity_criterion, …
  identity: gate_opening → RESOLVE under its identity_criterion, …
  question shapes: enumerate [character, vessel, object]; relations [(none)]; events [incident, gate_opening]; aggregate [account by speaker, account by place, ledger_entry by vessel, ledger_entry by hand, sighting by speaker, sighting by place, sighting by aboard, verdict by speaker, verdict by blamed, motive by against, motive by speaker, notice by place, notice by holder, notice by speaker]
  fill: character: occupation ← Point on each account, motive statement, the record by most
  fill: vessel: craft ← Point on each sighting statement, the record by most; owner ← Pick on each sighting statement, the record by most (candidates: Mention)
  fill: object: owner ← Pick on each notice statement, not the record (candidates: Mention)
  fill: account: subject `character` ← RESOLVE; speaker ← Pick; place ← Pick; at ← Point; doing ← Point
  fill: ledger_entry: subject `gate_opening` ← RESOLVE; at ← Point; vessel ← Pick; direction ← Choose; gauge ← Point; hand ← Pick
  fill: sighting: subject `vessel` ← RESOLVE; speaker ← Pick; place ← Pick; at ← Point; aboard ← Pick
  fill: verdict: subject `incident` ← RESOLVE; speaker ← Pick; cause ← Choose; blamed ← Pick; aloud ← Choose
  fill: motive: subject `character` ← RESOLVE; against ← Pick; reason ← Choose; speaker ← Pick
  fill: notice: subject `object` ← RESOLVE; place ← Pick; holder ← Pick; condition ← Point; speaker ← Pick
```

No warnings.

### Reading the derived facets against the answers

- **clock: narrative.** Matches answer 4. Nothing declares `supersedes`, so the clock only
  orders claims and states. Correct.
- **tension selector over account, sighting, ledger_entry, verdict, by embedding net.**
  Matches answer 5, and `same = []` lets person-vs-ledger pairs through. One limit, not a
  wrong inference: an embedding net may not pair "I saw him at six" with a terse ledger
  line ("0600 — gates — *Kittiwake* out — 9ft 2"), so some ledger contradictions could be
  missed. No declaration changes that.
- **identity: every type → RESOLVE under its criterion.** Matches answer 3 (no IDs).
- **question shapes.** `relations [(none)]` is intended: motives and grievances are
  claims, not relations, because the document-reading plan reads claims, and a motive is
  something *said*, with a speaker and a grade. Events and aggregates cover questions 1–7.
- **fill.** Every attribute now has a filler. Stable facts fold onto the record by `most`,
  and per-statement facts (object holder, every claim field) stay cited on each statement,
  as intended.
- One thing the facets do not show: `incident`'s participants (victim, finder, place) and
  `gate_opening`'s (vessel, keeper) have no `fill:` line. They are event participants,
  not attributes, so I cannot tell from validate whether the document plan reads them.
  The facts behind them are still captured through claims: the finder's presence through
  `account`, and the vessel and keeper through `ledger_entry.vessel` / `hand`. So no
  declaration change.
