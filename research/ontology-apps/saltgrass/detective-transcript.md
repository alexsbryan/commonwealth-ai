# Transcript — The Saltgrass Ledger recipe

The partner was not available, so I answered each question myself. The source of each answer is
either `description` (quoting DESCRIPTION.md) or `assumption` (my reading of the domain). I came to
this as a retired detective inspector, so I read the book the way I would read a case file: through
timelines, statements and alibis, and with the record made at the time trusted over what people say
afterwards.

## Who the owner is and what they want

**Q (framing).** What is the material, and what does the owner want from it?

**A.** The material is the manuscript of a mystery novel in sixteen chapters. The owner is most
likely its author, and they want a way to question their own book as a case: who says they were
where and when, what the tide ledger recorded, and where the testimony and the record contradict
each other. An author needs this to keep the timeline and clues of a whodunit consistent, and to
check that what each character "knows" could actually have been seen.
*Source:* `description`: "Sixteen chapters of salt, tar and quiet lies"; "*A Tale of Glasswater
Stave*". The claim that the owner is the author, and that their aim is continuity and clue-checking,
is an `assumption`. The description is a jacket blurb and does not say what the owner wants.

**Q (follow-up: source shape).** What form are the files in?

**A.** I assumed a single manuscript file in markdown with one heading per chapter, in reading
order. That calls for `local_file` + `markdown` + `paragraph` chunking. If the manuscript turns out
to be a .docx or PDF, it would need exporting first, or the `custom` pdf extractor.
*Source:* `assumption` ("Sixteen chapters" from the `description` suggests chapter headings).

## 1. Shape

**Q.** What is this material about, and what would you want to know about each kind of thing? Is any
of them a kind of another, or a part something plays? Do you already have any of it in a table?

**A.** The material is about one death in a small port and everything said around it. The kinds of
thing are:
- **character**: name and trade. The harbormaster, the ledger-keeper and others are characters. I
  left offices and epithets as attributes and did not split them into roles, because "harbormaster"
  is how the village names one man.
- **vessel**: name, rig and master. The ledger records "every vessel through them".
- **item**: the physical objects: the ledger itself, the painted gauge, keys and lanterns.
- Events: **gate_opening** (time, water height, direction), **movement** (who was where, over what
  span; the unit an alibi is made of), **death** (where and when found) and **dealing** (money or
  goods changing hands, including an attempted bribe).
- Relations between people: **tie** (kin, debt, employment, grievance), **vouches_for** (an alibi
  given for someone else) and **crews** (a person to a vessel).
- One state: **standing**, how a character stands in the case at a point in the book.

Nothing is held in a table.
*Source:* `description`: "every opening of the gates, every vessel through them, every height of
water against the painted gauge"; "harbormaster for twenty-one years, a man who wrote everything
down and could not be bought"; "found floating in the basin at dawn"; "a salvage port". **dealing**
(from "could not be bought"), **tie** and **vouches_for** are `assumption`s from general knowledge
of the genre and of casework: in a village where "everyone has a reason", the reasons are ties, and
alibis come from the people tied to you.

## 2. Assertion

**Q.** What do the sources say about those things: stating, requiring, deciding, asking? About what?
What does each statement carry? How do you tell strong evidence from weak? Who is speaking, and are
they part of the subject? What must it never do?

**A.** Almost every statement is assertive and made inside the story (`scope = "in_work"`):
- **ledger_entry** (about a gate_opening): who recorded it, the hour written, the water height, the
  vessel, and the state of the page (clean, struck through, overwritten, added later, in another
  hand, missing). An altered entry matters as much as the entry itself.
- **account** (about a movement): speaker, who it was told to, the time claimed, the basis (saw it,
  heard it, was told, wrote it down, supposes), and whether it affirms or denies.
- **vessel_sighting** (about a vessel) and **trace** (about an item). I added these after the first
  validation run; see below.
- **suspicion** (about the death): speaker, suspect, cause, motive, basis, and whether it was spoken
  openly, in a whisper or only in thought.
- **dealing_report** (about a dealing): speaker, amount, basis, stance.
- **threat** (commissive, about a character): a threat, warning or promise.

Strength of evidence, strongest first: written at the time > seen first-hand > heard first-hand >
told by another > village talk > supposition. All speakers are characters, so speaker is a `ref` to
`character`. The narrator is not a witness.

`must_not`: never invent a time, tide height or whereabouts the text does not give; never treat a
suspicion or village opinion as fact; never merge two characters because they share a trade,
surname or boat.
*Source:* `description` for the ranking of the record over talk ("what the ledger saw" set against
people "so sure of things they cannot have seen"; "Nobody will say so above a whisper", which gives
the `spoken` attribute). The grade scale and the closed value sets are an `assumption`, drawn from
how I would weigh statements in a case file.

## 3. Identity

**Q.** How do you know two mentions are the same thing? Is there an ID?

**A.** There are no IDs; this is fiction. Each type gets a criterion in plain words, with name as
the descriptive fallback:
- Characters are one person when they are named the same way, or when an office or epithet
  ("the harbormaster", "old Pellow") plainly refers to a named person in that scene. A shared
  surname or trade does not make two people one.
- A vessel is the same vessel when its name is the same.
- An item is the same item when it has the same description and the same keeping.
- A gate_opening is the same opening when it falls on the same tide on the same day; one opening
  can pass several vessels.
- A movement is the same movement when it is the same person, in the same place, over overlapping
  time on the same day.
- There is one death per person.
- A dealing is the same dealing when the same parties exchange the same thing on the same occasion.

*Source:* `description` for the harbormaster/Corwin Pellow equivalence ("Corwin Pellow,
harbormaster for twenty-one years"). The criteria themselves are an `assumption`.

## 4. Change

**Q.** When does a later statement replace an earlier one, and from when? Does anything move through
stages over time? Which fields of each document give its date and the conversation it belongs to?

**A.** Nothing is replaced. When a witness changes their story later in the book, both versions stand,
and the change is itself evidence. So I set `clock = "narrative"` (chapter order) and declared no
`supersedes`. Story time (the hour, the tide) is a separate thing, carried as `time` attributes on
each event and statement. A character's **standing** moves across the book (trusted, watched,
suspected, accused, cleared). I declared it as a state of character rather than a protocol fold,
because the moves are made by the narrative, not by a named party under rules. The chapters carry
no date or thread metadata, so there is no `change.document`.
*Source:* `assumption`, based on the principle of trusting the contemporaneous record and keeping
every statement. The `description` supports narrative order ("Sixteen chapters").

## 5. Derivation

**Q.** What should it notice that no single document says: contradictions, patterns, larger
structures? What looks like a contradiction but isn't?

**A.**
- **Discrepancies** between the record and the talk, across ledger_entry, account,
  vessel_sighting, trace, suspicion and dealing_report. `same = []` is set so that a ledger entry
  about a gate opening can be set against an account of someone's movement, which is a different
  subject.
- **Mutual alibis**: a `circular_flow` pattern over vouches_for, with a minimum of 2.
- **Alibis from interested parties**: a `role_overlap` pattern between vouches_for and tie.
- **Configurations**: groups of characters seen together in scenes.

These are not conflicts:
- times a few minutes apart, or "first light" against "dawn"
- one person's movements at a different hour from another's
- characters suspecting different people
- a ledger entry and an account about different tides
- a later account that adds detail without changing the time, place or who was there

*Source:* `description` ("why the people she has known all her life are so sure of things they
cannot have seen"; "everyone has a reason"). The patterns and the not-conflicts list are an
`assumption`.

## 6. Questions

**Q.** Once it is read, what will you ask of it?

**A.** Each question below is followed by where its answer lands in the declaration.
1. *Where was each character between midnight and dawn, according to whom?* account → movement
   (where, during), speaker, basis.
2. *Which gate openings that night does the ledger record, and are any entries altered?*
   ledger_entry → gate_opening, page_state.
3. *Was a boat seen moving that the ledger does not show?* This question exposed a gap. Nothing
   could hold a sighting of a vessel, so I declared **vessel_sighting** (a claim about a vessel)
   and set it against ledger_entry in tension.
4. *Who claims to know something they could not have seen?* account/suspicion with basis "supposes"
   or "was told", set against movement records that place the speaker elsewhere (tension).
5. *Who vouches for whom, and are they tied?* vouches_for and tie, and both patterns.
6. *Who stood to gain from the harbormaster's death, and what had he refused?* dealing (outcome
   "refused"), dealing_report, suspicion.motive, tie.
7. *Where was his lantern / the missing page found, and by whom?* This also exposed a gap: no claim
   was about an item, so items had no fields filled. I declared **trace** (a claim about an item).
8. *How does Maren's standing (or anyone's) change chapter by chapter?* standing, on the narrative
   clock.

*Source:* `assumption`. These are the questions I would put to a case file. Questions 1–4 follow
from the `description`'s premise.

---

## Validation runs

### Run 1: first draft

```
$ sovereign recipe validate recipe.toml
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: narrative — supersession folds on order within the work
  tension selector: embedding top-k (k = 10, floor = 0.5) over ledger_entry, account, suspicion, dealing_report — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: character → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: vessel → RESOLVE under its identity_criterion, ...
  identity: item → RESOLVE under its identity_criterion, ...
  identity: gate_opening / movement / death / dealing → RESOLVE under its identity_criterion, ...
  question shapes: enumerate [character, vessel, item]; relations [tie, vouches_for, crews]; events [gate_opening, movement, death, dealing]; aggregate [ledger_entry by recorded_by, ledger_entry by vessel, account by speaker, account by told_to, suspicion by speaker, suspicion by suspect, dealing_report by speaker, threat by speaker]
  fill: vessel: name ← nothing; rig ← nothing; master ← nothing
  fill: item: name ← nothing; held_by ← nothing; found_at ← nothing
  fill: movement: where ← nothing; during ← Point on each account statement, not the record
  fill: death: where ← nothing; found_at ← Point on each suspicion statement, not the record
  fill: tie_state: tie_state ← nothing
  (other fill lines as in run 2)
  ⚠  the document reader ... makes no record of `tie_state` (state): no statement kind is about them, RESOLVE decides none of them and no source supplies them.
  ⚠  `voices` (attributed_to) are applied by the general extractor only; the document reader attributes no statement to a speaker.
  ⚠  ontology type `vessel`: nothing fills `name`, `rig`, `master` ...
  ⚠  ontology type `vessel`: identity is keyed on `name` (identity_fallback), which nothing fills ...
  ⚠  ontology type `item`: nothing fills `name`, `held_by`, `found_at` ...
  ⚠  ontology type `item`: identity is keyed on `name` (identity_fallback), which nothing fills ...
  ⚠  ontology type `movement`: nothing fills `where` ...
  ⚠  ontology type `death`: nothing fills `where` ...
  ⚠  ontology type `tie_state`: nothing fills `tie_state` ...
```

The recipe passed, but when I read the facets against my answers, several were wrong:
- **vessel and item had nothing filling them.** No statement kind was about either, so the
  vessel-versus-ledger question (Q3) and the evidence-object question (Q7) had nowhere to land. I
  added the claim kinds `vessel_sighting` (subject vessel) and `trace` (subject item), and added
  both to `tension.between`.
- **`where` (a `ref` to `place`) on movement, death and item was never asked.** A ref on a subject
  record is not read. I made `where` open text on movement and death and moved place onto the
  `trace` claim as text. `vessel.master` became text for the same reason.
- **`tie_state` is never read.** No statement kind is about a tie. I dropped it; how a tie changes
  over the book is not captured, and I note that as a limitation.
- **`voices.attributed_to` is ignored by the document reader.** I removed it. Attribution already
  lives in each claim's `speaker` and `basis`.

### Run 2: after adding vessel_sighting and trace, making places text, dropping tie_state and attributed_to

```
$ sovereign recipe validate recipe.toml
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: narrative — supersession folds on order within the work
  tension selector: embedding top-k (k = 10, floor = 0.5) over ledger_entry, account, vessel_sighting, trace, suspicion, dealing_report — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: character|vessel|item|gate_opening|movement|death|dealing → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [character, vessel, item]; relations [tie, vouches_for, crews]; events [gate_opening, movement, death, dealing]; aggregate [ledger_entry by recorded_by, ledger_entry by vessel, account by speaker, account by told_to, suspicion by speaker, suspicion by suspect, dealing_report by speaker, vessel_sighting by speaker, trace by speaker, trace by held_by, threat by speaker]
  fill: character: name ← Point on each crews, standing, threat, tie, vouches_for statement, not the record; trade ← (same)
  fill: vessel: name ← Point on each vessel_sighting statement, not the record; rig ← ...; master ← ...
  fill: item: name ← Point on each trace statement, not the record; owner ← ...
  fill: tie: from `character` ← RESOLVE; nature ← Choose on tie; to ← Pick on tie (candidates: Mention)
  fill: vouches_for: from `character` ← RESOLVE; to ← Pick on vouches_for (candidates: Mention)
  fill: crews: from `character` ← RESOLVE; to ← Pick on crews (candidates: Mention)
  fill: gate_opening: at, water_height ← Point on each ledger_entry statement; direction ← Choose on each ledger_entry statement, not the record
  fill: movement: where, during ← Point on each account statement, not the record
  fill: death: where, found_at ← Point on each suspicion statement, not the record
  fill: dealing: amount, goods, when ← Point; outcome ← Choose on each dealing_report statement, not the record
  fill: ledger_entry: subject `gate_opening` ← RESOLVE; recorded_by, vessel ← Pick (Mention); written_time, water_height ← Point; page_state ← Choose
  fill: account: subject `movement` ← RESOLVE; speaker, told_to ← Pick (Mention); claimed_time ← Point; basis, stance ← Choose
  fill: suspicion: subject `death` ← RESOLVE; speaker, suspect ← Pick (Mention); motive ← Point; cause, basis, spoken ← Choose
  fill: dealing_report: subject `dealing` ← RESOLVE; speaker ← Pick; amount ← Point; basis, stance ← Choose
  fill: vessel_sighting: subject `vessel` ← RESOLVE; speaker ← Pick; seen_at, where ← Point; basis ← Choose
  fill: trace: subject `item` ← RESOLVE; speaker, held_by ← Pick; where, when, condition ← Point
  fill: threat: subject `character` ← RESOLVE; speaker ← Pick; undertaking ← Point; spoken ← Choose
  fill: standing: of `character` ← RESOLVE; standing ← Point on standing
```

There were no warnings. Checking the facets against my answers:
- The clock is narrative with no supersession, which matches answer 4.
- The tension set covers all six evidential claim kinds and searches across documents, which
  matches answer 5.
- Every type is identified by RESOLVE under its own criterion, which matches answer 3.

One facet was still wrong. The names of characters, vessels and items were held only per statement
("not the record"), so a record would carry no name of its own, even though name is the identity
fallback. Within one case file, a statement's reading of a name should agree with the others, so I
gave `name` `by = "most"` on character, vessel and item. Times and places on events stay
per-statement on purpose: when witnesses give different hours for the same movement, each reading
must survive, because the disagreement is the evidence.

### Run 3: `by = "most"` on the name fields

```
$ sovereign recipe validate recipe.toml
Validating recipe: recipe.toml
✓ Validation passed
  ... (as run 2, except:)
  fill: character: name ← Point on each crews, standing, threat, tie, vouches_for statement, the record by most; trade ← ... not the record
  fill: vessel: name ← Point on each vessel_sighting statement, the record by most; rig ← ...; master ← ...
  fill: item: name ← Point on each trace statement, the record by most; owner ← ...
```

The recipe passes with no warnings, and the derived facets match the answers above.

Note: I could not run `validate --corpus`, because the manuscript is not here (`SOURCE_PATH` is a
placeholder). No `change.document` or `source.metadata` field is named, so nothing depends on
extractor field names.

Housekeeping: I scaffolded `scratch-literary.toml` from `sovereign recipe new --ontology literary`
to look at the template. My attempt to delete it was blocked by the permission settings, so it is
still in the directory and can be removed.
