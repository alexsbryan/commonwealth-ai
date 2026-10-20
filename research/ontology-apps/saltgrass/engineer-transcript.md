# Transcript — The Saltgrass Ledger recipe

The partner was not available, so I answered each interview question myself. The source of each answer is
either `description` (quoting DESCRIPTION.md) or `assumption` (my own reading, based on general knowledge
of the genre).

Reference consulted: `sovereign recipe new --ontology literary` was scaffolded once to see the closest
template (character / inner_state / reading, narrative clock). I did not copy it. Its `reading` claim is
criticism *about* the work, while this material needs statements made *inside* the work. The scratch
file `literary-template.toml` is still in this directory. Deleting it needed approval, which this
session could not grant.

---

## Source shape (pre-interview)

**Q. What files are these, and what extractor fits?**
A. One manuscript of a novel, sixteen chapters. Acquire it as `local_file` at `SOURCE_PATH`, extract it
with `markdown` (one document per `## ` chapter heading, in reading order), and chunk by `paragraph`.
Source: `description` for the form ("Sixteen chapters"), `assumption` for the file format: the
description does not say whether the manuscript is markdown, plain text or a word-processor file. I chose
markdown because chapter headings give chapter boundaries for free.

## 1. Shape

**Q. What is this material about, and what would you want to know about each kind of thing? Is any of
them a kind of another, or a part something plays? Do you already have any of it in a table?**

A. It is a mystery set in a small salvage port. The things that matter, as tables:

- **villager**: a person in the story. This is a role of the base `person` kind. Columns: occupation and
  years in post. Source: `description`: "Corwin Pellow, harbormaster for twenty-one years"; "Maren
  Ashquill keeps the tide ledger"; "the people she has known all her life".
- **vessel**: a boat through the gates. Source: `description`: "every vessel through them".
- **harbor_place**: a fixed place. It specializes the base `place` kind. Source: `description`: "the
  lock basin", "heavy gates", "the painted gauge", "the cobbles".
- **occurrence**: the central event table. A gate opening, a vessel passage, the harbormaster's morning
  walk, a meeting, the death, the finding of the body. Columns: kind (a closed set), actor, vessel, time
  and place. Source: `description` for the kinds ("every opening of the gates", "cross the cobbles at the
  same hour every morning", "found floating in the basin at dawn"). Making it one table that every
  statement points at is an `assumption`. It is my modelling choice.
- **tie**: a relation from one villager to another (kin, debt, grudge, bribe offered, …). Source:
  `description`: "everyone has a reason", "could not be bought". The specific tie kinds are an
  `assumption`.
- **standing**: a state of a villager: trusted, suspected, cleared, exposed. Source: `assumption`,
  following from "the whole village has decided … that it was no accident".

No existing tables. Source: `assumption`. The description mentions none, and the ledger exists only
inside the story.

## 2. Assertion

**Q. What do the sources say about those things — stating, requiring, deciding, asking? About what? What
does each statement carry? How do you tell strong evidence from weak? Who is speaking? What must it
never do?**

A. Every statement that matters is an assertion made *inside* the story (`scope = "in_work"`). There are
three claim types:

- **account**: a villager says something happened. Its subject is an `occurrence`. It carries the
  speaker and the basis (saw it / heard it said / supposes). These are also its grades, strongest first.
  Source: `description`: "why the people she has known all her life are so sure of things they cannot
  have seen"; "Nobody will say so above a whisper".
- **ledger_entry**: a line of the tide ledger. Its subject is an `occurrence`. It carries who entered it,
  the gate action (opened / closed) and the water height on the gauge. Source: `description`: "every
  opening of the gates, every vessel through them, every height of water against the painted gauge";
  "what the ledger saw". The unit (feet) is an `assumption`.
- **suspicion**: one villager suspecting or whispering about another. Its subject is the suspected
  villager. It carries who holds the suspicion, the motive, and how openly it is said (accusation /
  whisper / unspoken). These are also its grades. Source: `description`: "decided by breakfast that it
  was no accident. Nobody will say so above a whisper"; "everyone has a reason".

Who speaks: villagers, and the ledger as a record. The narrator is not an entity (`voices.not_entities`).
Source: `assumption`. This is standard for fiction.

Must never: state as fact who killed Pellow, or that he was killed, unless the text establishes it; and
never present a rumour or suspicion as something a character saw. Source: `assumption`. It follows from
"quiet lies" and "the truth comes in with the tide": in a mystery, the gap between belief and fact is the
material.

## 3. Identity

**Q. How do you know two mentions are the same thing? Is there an ID?**

A. There are no external IDs. Everything resolves by judgement under a written criterion:

- villager: the same individual whether named in full, by surname, by first name, by title ("the
  harbormaster") or by nickname. Kin who share a surname are different people. Fallback key: `name`.
- vessel: the same named boat. "Pellow's launch" counts as the named boat when the text makes that
  clear. Fallback key: `name`.
- harbor_place: the same physical place under any name ("the basin" / "the lock"). Fallback key: `name`.
- occurrence: the same kind, at the same place, on the same night or tide. Two accounts that give
  different hours for one happening are still one occurrence. The difference between them is the
  contradiction, not a second row.

Source: `assumption` for all four. A novel carries no keys. The one fact taken from the description is
that titles and names refer to the same person ("Corwin Pellow, harbormaster").

## 4. Change

**Q. When does a later statement replace an earlier one, and from when? Does anything move through
stages? Which document fields give date and thread?**

A. The clock is narrative, meaning chapter order (`clock = "narrative"`). Nothing supersedes. A later
account does not retire an earlier one, because a lie exposed in chapter 12 is still evidence of what
was said in chapter 3. Source: `assumption`, from "Sixteen chapters of … quiet lies" and "the truth
comes in with the tide".

Stages: a villager's `standing` moves (trusted → suspected → cleared/exposed). I declared it as a plain
state and not a `by = "protocol"` fold. A protocol fold needs typed move claims with source-supported
identity fields, which a novel's prose won't carry. Source: `assumption`.

Document fields: none declared (`change.document` omitted). Chapters carry no dates or threads, and with
no material to hand I cannot run `validate --corpus` to read the extractor's field names. Source:
`assumption`.

## 5. Derivation

**Q. What should it notice that no single document says? What looks like a contradiction but isn't?**

A. The main thing to notice is contradictions between what villagers say happened and what the ledger
recorded, and between villagers. These are `tension.between = ["account", "ledger_entry"]`, compared
only when they share a subject (`same = ["subject"]`). In other words, the join key is the occurrence.
Source: `description`: "she starts to wonder what the ledger saw, and why the people … are so sure of
things they cannot have seen".

Not conflicts: (a) a firsthand and a secondhand account that agree in substance; (b) times that differ
only by rounding to the tide or the hour. Source: `assumption`.

Interpretive configurations stay on (`derive.configurations = true`). That covers groups of villagers
who close ranks. Source: `assumption`.

## 6. Questions (checks the ontology)

**Q. Once it is read, what will you ask of it?** Source: `assumption` for all. The description implies
them but does not list them.

1. *Where was Corwin Pellow the night before he was found, and by whose account?* → `occurrence` with
   `actor` = Pellow; `account.speaker`, `account.basis`. ✓
2. *Does the ledger contradict anyone?* → tension between `ledger_entry` and `account` on the same
   `occurrence`. ✓
3. *When were the gates opened that night, which vessels came through, and how high was the water?* →
   `occurrence` (kind, at, vessel) + `ledger_entry.gate_action`, `water_height`. ✓
4. *Who suspects whom, and why?* → `suspicion` (subject, held_by, motive, spoken). ✓
5. *What reason did each villager have?* → `tie` (tie_kind) + `suspicion.motive`. ✓
6. *Who is under suspicion at chapter N, and who has been cleared?* → `standing` on the narrative clock. ✓

Every question lands on a declared type, attribute or claim kind. None needed a new type.

---

## Validation runs

### Run 1 — `sovereign recipe validate recipe.toml`

```
✓ Validation passed

Derived from your declarations:
  clock: narrative — supersession folds on order within the work
  tension selector: embedding top-k (k = 10, floor = 0.5) over account, ledger_entry — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: villager → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: vessel → RESOLVE under its identity_criterion, ...
  identity: harbor_place → RESOLVE under its identity_criterion, ...
  identity: occurrence → RESOLVE under its identity_criterion, ...
  question shapes: enumerate [villager, vessel, harbor_place]; relations [tie]; events [occurrence]; aggregate [account by speaker, account by claimed_place, ledger_entry by entered_by, ledger_entry by vessel, suspicion by held_by]
  fill: villager: occupation ← Point on each standing, suspicion, tie statement, the record by agree; years_in_post ← ... not the record
  fill: vessel: vessel_kind ← nothing; master ← nothing
  fill: occurrence: occurrence_kind ← Choose on each account, ledger_entry statement, not the record; at ← Point ...; place ← Pick ...
  ...
  ⚠  `voices` (attributed_to) are applied by the general extractor only; the document reader attributes no statement to a speaker.
  ⚠  ontology type `vessel`: nothing fills `vessel_kind` ..., `master` .... Each stays empty on every record; give it a source or a derivation, or drop it.
```

Reading it against the answers:
- `vessel.vessel_kind` / `master`: no claim asks for them, so these columns would always be empty.
  **Fix:** dropped both. The vessel is identified by name and linked from occurrences.
- `voices.attributed_to`: has no effect under the document reader. Speakers are already the `speaker` /
  `entered_by` / `held_by` refs. **Fix:** removed it.
- `occurrence` had no column for *who* took part. Question 1 ("where was Pellow") had nowhere to land,
  because event `participants` are not filled under this plan. **Fix:** added `actor` (ref villager) and
  `vessel` (ref vessel) attributes.
- `occurrence_kind` was "not the record". The kind of happening should sit on the row. **Fix:**
  `by = "most"`. Time and place stay per-statement on purpose, because their disagreement is the
  contradiction.

### Run 2 — `sovereign recipe validate recipe.toml`

```
✓ Validation passed
  ... (no warnings)
  fill: occurrence: occurrence_kind ← Choose on each account, ledger_entry statement, the record by most; actor ← Pick ...; vessel ← Pick ...; at ← Point ...; place ← Pick ...
  fill: account: ... speaker ← Pick; basis ← Choose; claimed_time ← Point; claimed_place ← Pick
  fill: ledger_entry: ... entered_by ← Pick; recorded_time ← Point; gate_action ← Choose; vessel ← Pick; water_height ← Point
```

Reading it: `account.claimed_time` / `claimed_place` and `ledger_entry.recorded_time` / `vessel`
duplicate `occurrence.at` / `place` / `vessel`. Those subject fields are already read on each statement
and kept with a citation, so the same fact was stored twice. **Fix:** dropped the four duplicates.
Time, place and vessel now live on `occurrence` only.

### Run 3 — `sovereign recipe validate recipe.toml`

```
Validating recipe: recipe.toml
✓ Validation passed

Derived from your declarations:
  clock: narrative — supersession folds on order within the work
  tension selector: embedding top-k (k = 10, floor = 0.5) over account, ledger_entry — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: villager → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: vessel → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: harbor_place → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: occurrence → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [villager, vessel, harbor_place]; relations [tie]; events [occurrence]; aggregate [account by speaker, ledger_entry by entered_by, suspicion by held_by]
  fill: villager: occupation ← Point on each standing, suspicion, tie statement, the record by agree; years_in_post ← Point on each standing, suspicion, tie statement, not the record
  fill: occurrence: occurrence_kind ← Choose on each account, ledger_entry statement, the record by most; actor ← Pick on each account, ledger_entry statement, not the record (candidates: Mention); vessel ← Pick on each account, ledger_entry statement, not the record (candidates: Mention); at ← Point on each account, ledger_entry statement, not the record; place ← Pick on each account, ledger_entry statement, not the record (candidates: Mention)
  fill: tie: from `villager` ← RESOLVE; tie_kind ← Choose on tie; to ← Pick on tie (candidates: Mention)
  fill: standing: of `villager` ← RESOLVE; standing ← Point on standing
  fill: account: subject `occurrence` ← RESOLVE; speaker ← Pick on account (candidates: Mention); basis ← Choose on account
  fill: ledger_entry: subject `occurrence` ← RESOLVE; entered_by ← Pick on ledger_entry (candidates: Mention); gate_action ← Choose on ledger_entry; water_height ← Point on ledger_entry
  fill: suspicion: subject `villager` ← RESOLVE; held_by ← Pick on suspicion (candidates: Mention); motive ← Point on suspicion; spoken ← Choose on suspicion
```

Reading the facets against the answers:
- clock = narrative ✓ (Change).
- tension over account + ledger_entry ✓ (Derivation). The selector is an embedding net, and the
  classifier judges each pair. `same = ["subject"]` restricts pairs to the same occurrence.
- identity: all four entity/event types resolve under their written criteria ✓ (Identity).
  `vessel` and `harbor_place` are the subject of no claim, so RESOLVE has no statements of their own to
  work from. They are reached only as `Mention` candidates for refs. That is the intended shape:
  vessels and places are lookup dimensions, not things anyone makes claims about. I left it as is.
- `villager.years_in_post` is per-statement, not on the record. That is acceptable: it is descriptive,
  and Pellow's "twenty-one years" will sit on the statements that mention it.
- question shapes cover questions 1–6 ✓.

---

## Summary

**Types declared and their identity**

| type | kind | identity |
|---|---|---|
| villager (role of person) | entity | criterion: same individual across full name / surname / title / nickname; kin are distinct; fallback `name` |
| vessel | entity | criterion: same named boat (incl. "X's launch"); fallback `name` |
| harbor_place (specializes place) | entity | criterion: same physical place under any name; fallback `name` |
| occurrence | event | criterion: same kind + place + night/tide; differing hours ≠ new row |
| tie | relation villager→villager | by its endpoints |
| standing | state of villager | by its villager |
| account | claim (assertive, in_work), subject occurrence | — |
| ledger_entry | claim (assertive, in_work), subject occurrence | — |
| suspicion | claim (assertive, in_work), subject villager | — |

Tension: account × ledger_entry on the same occurrence. Clock: narrative. Nothing supersedes.

**Least-sure assumptions**
1. The manuscript is markdown with one `## ` heading per chapter. If it is a .docx or plain text, the
   extractor changes, and chapter order (the narrative clock) depends on it.
2. That nothing supersedes. If the owner wants "what does the village believe *now*", `suspicion` would
   need supersession, keyed per holder and suspect pair, which the subject key alone cannot express.
3. The `occurrence` identity criterion. Merging "the same happening at different reported hours" into
   one row is what makes contradictions show up, but it could also merge two genuinely separate gate
   openings on the same night.
4. That the owner wants the novel read as an in-world fact base (who did what, and when), not as
   criticism or themes. The description is a jacket blurb and does not say why they want it read.
5. Water height in feet, and the closed sets for tie kinds and occurrence kinds.
