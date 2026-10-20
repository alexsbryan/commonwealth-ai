# Transcript — The Saltgrass Ledger recipe

The partner was not available. I answered each question as the editor, from
`DESCRIPTION.md` where it speaks and from general knowledge of how mystery
manuscripts are edited where it doesn't. Sources: `description` (with a quote) or
`assumption`.

## 0. Source shape (before the interview)

**Q. What files is this, and how should they be read?**
A. One novel manuscript in sixteen chapters. I assume it's exported as markdown,
with one heading per chapter in reading order. So: `local_file` at `SOURCE_PATH`,
the `markdown` extractor (one document per heading-bounded section, so one per
chapter), and paragraph chunks of 2000 characters with 200 overlap. Because the
manuscript is unpublished, it's `license = "private"`, `mesh_sharing = false` and
`query_sharing = false`.
Source: description, "Sixteen chapters of salt, tar and quiet lies". The file
format, markdown, and the privacy settings are an **assumption**.

## 1. Shape

**Q. What is this material about, and what would you want to know about each kind
of thing? Is any of them a kind of another, or a part something plays? Do you
already have any of it in a table?**

A. It's a village mystery. As an editor I see five kinds of thing:
- **character**: the villagers, the dead harbormaster and Maren. For each I want
  their occupation and household. Source: description, "Corwin Pellow,
  harbormaster for twenty-one years", "Maren Ashquill keeps the tide ledger", "the
  people she has known all her life".
- **object**: the physical things a mystery turns on: the tide ledger, the
  painted gauge, the gates, vessels, letters. For each I want its sort and its
  owner. Source: description, "every opening of the gates, every vessel through
  them, every height of water against the painted gauge". Making vessels a sort
  of object rather than their own type is an **assumption**.
- **incident** (event): what happened, with when, where, what, and the actor,
  witness and victim. Each opening of the gates is an incident. Source:
  description, "found floating in the basin at dawn", "cross the cobbles at the
  same hour every morning".
- **tie** (relation, character → character): kin, debt, rivalry, courtship and
  so on. Source: **assumption**. In a village where "everyone has a reason", the
  reasons usually run through these bonds.
- **frame_of_mind** (a state of a character): afraid, guilty, suspicious, and how
  that changes chapter to chapter. Source: **assumption**, standard
  character-arc tracking.

Nothing is a specialisation of anything else. "Harbormaster" is a title, and I
handle it in the identity criterion rather than as a `role_of` (see Q3). There
are no tables. Source: **assumption**.

## 2. Assertion

**Q. What do the sources say about those things? About what? What does each
statement carry? How do you tell strong evidence from weak? Who is speaking, and
are they part of the subject? What must it never do?**

A. In a mystery the important thing is *who* is telling the reader something.
That gives six claim kinds, all assertive and all `scope = "in_work"`:
- **account** (about an incident): what a character *says* happened, with
  speaker, told_to and the source they name. It's graded by how the speaker could
  know it: `saw it > heard it > was told > supposes`. Source: description, "why
  the people she has known all her life are so sure of things they cannot have
  seen", "Nobody will say so above a whisper". The grades and the source_named
  field are the editor's translation of that sentence.
- **narrated** (about an incident): what the narration itself shows. This is the
  reference the accounts are measured against. Source: **assumption**, standard
  for a third-person mystery.
- **record** (about an incident): what a written record inside the story shows.
  It carries the document, written_by, whether the entry was altered (as written,
  corrected, missing, torn out, forged), and for ledger entries the vessel, the
  water height in feet and which way the gates opened. Source: description, "she
  starts to wonder what the ledger saw", "a man who wrote everything down". The
  `altered` values and the unit `ft` are an **assumption**. The unit is a guess
  for an English-coast tide gauge.
- **knowledge** (about a character): what that character knows, suspects,
  believes, doubts, conceals, denies or learns about an incident, and how
  (saw it, heard it, was told, read it, worked it out, unexplained). Source:
  description, "the whole village has decided by breakfast", "what each person
  knows" (my brief as editor). "unexplained" is there to flag knowledge a
  character shouldn't have.
- **motive** (about a character): a reason they have, or are said to have, to act
  against someone, with who it's against, the reason and who alleges it. Source:
  description, "a village where everyone has a reason", "could not be bought".
- **custody** (about an object): who holds an object and where it is, with who
  says so. Source: **assumption**. Missing or moved objects are standard
  mystery machinery, and "the ledger" is the book's title object.

Voices: the narrator speaks but is not an entity. Source: **assumption**.

Must never:
- treat a character's say-so as what happened
- invent incidents, motives or a solution, or guess the killer
- rewrite the author's prose

Source: **assumption**. These are an editor's ethics toward a manuscript and its
spoilers.

## 3. Identity

**Q. How do you know two mentions are the same thing? Is there an ID?**

A. There are no IDs. It's fiction, so every type is decided by RESOLVE under a
criterion written in words:
- **character**: any name, surname, nickname, kin term or title the scene ties to
  the same person. A title names whoever holds it at that point, because "the
  harbormaster" after Pellow's death may be his successor. A shared surname does
  not make two people one.
- **object**: the same physical thing by name, owner or description. A vessel is
  known by its name. A ledger page is part of the ledger unless it is torn out or
  copied.
- **incident**: the same occurrence, meaning the same people, act, place and tide,
  even when speakers date it differently. This matters most: two villagers who
  put Pellow's crossing at different hours must land on *one* incident, or the
  contradiction is never seen.

Source: the title point comes from description, "harbormaster for twenty-one
years" (so someone else may hold the post later). The rest is **assumption**.

## 4. Change

**Q. When does a later statement replace an earlier one, and from when? Does
anything move through stages? Which fields give each document's date and
thread?**

A. The clock is narrative: chapter order. The documents are chapters with no
calendar dates, so there's no `change.document`.

I deliberately declared **no supersession**. In a mystery an earlier account is
not retired by a later one; the gap between them is the point. Supersession also
works per subject, so a later knowledge claim about Maren would retire an earlier
one about a *different* incident. Changes of mind are read in order through
`knowledge.stance` (including "learns") and `frame_of_mind`.

There's no staged protocol. "The case" moving from accident to murder is
something the village believes, not a formal state.

Source: **assumption**, from general knowledge of the form. The description
supports the order mattering with "the truth comes in with the tide".

## 5. Derivation

**Q. What should it notice that no single document says, such as contradictions
and patterns? What looks like a contradiction but isn't?**

A. It should find contradictions between account, narrated, record, custody and
motive claims about the same subject, across chapters. That covers:
- an account set against the ledger
- an account set against the narration
- two villagers' versions of one incident
- an object in two places at once

So I set `same = ["subject"]` and dropped the default `clock`, which would only
compare claims within the same chapter. I called these "contradictions" and
turned on the configurations rollup, to show scenes as groupings of characters.

Things that are not contradictions:
- a speaker's story before and after they learned something
- one rumour repeated many times, which is one story and not corroboration
- water heights at different hours of the same tide
- a title naming different holders
- a lie the narration later exposes, which is plot and not a slip

Source: description, "quiet lies", "the truth comes in with the tide", "so sure
of things they cannot have seen". The specific exclusions are **assumption**.

## 6. Questions

**Q. Once it is read, what will you ask of it?**

A. Each question below, with the declared type that holds its answer:
1. *Who claims to know how Pellow died, and how could they know?* Account grades
   and source_named, plus knowledge `how` (an "unexplained" value flags it).
2. *Which accounts of that dawn disagree with the tide ledger?* A contradiction
   between account and record on the same incident.
3. *Which ledger entries were altered or are missing, and who wrote them?*
   `record.altered`, `record.written_by` and `record.document`.
4. *What does Maren know or suspect by chapter N, and when did she learn it?*
   Knowledge claims about Maren in narrative order.
5. *Who has a reason to want Pellow gone, and who says so?* Motive, with
   `against` and `alleged_by`.
6. *Where is the ledger, or any object, at each point, and who has it?* Custody.
7. *Is the tide continuity consistent: heights, gate openings, hours?* Record's
   `water_height` and `gates`, and incident `when`, checked through
   contradictions.
8. *How do X and Y stand toward each other, and how does X change?* Tie and
   frame_of_mind.

Every question lands on something declared, so no kind of thing is missing.
Source: **assumption**. These are an editor's questions; the description
doesn't list any.

## Validation runs

### Run 1, `sovereign recipe validate recipe.toml`

```
✓ Validation passed

Derived from your declarations:
  clock: narrative — supersession folds on order within the work
  tension selector: embedding top-k (k = 10, floor = 0.5) over account, narrated, record, custody, motive — ...
  identity: character → RESOLVE under its identity_criterion, ...
  identity: object → RESOLVE under its identity_criterion, ...
  identity: incident → RESOLVE under its identity_criterion, ...
  identity: passage → RESOLVE under its identity_criterion, ...
  question shapes: enumerate [character, object]; relations [tie]; events [incident, passage]; aggregate [...]
  fill: character: occupation ← Point on each frame_of_mind, knowledge, motive, tie statement, not the record; ...
  fill: object: sort ← Choose on each custody statement, not the record; ...
  fill: incident: when ← Point on each account, narrated, record statement, not the record; where ← nothing; ...
  fill: passage: water_height ← nothing; direction ← nothing
  fill: tie_state: tie_state ← nothing
  ...
  ⚠  the document reader ... makes no record of `tie_state` (state): no statement kind is about them ...
  ⚠  `voices` (attributed_to) are applied by the general extractor only; the document reader attributes no statement to a speaker.
  ⚠  ontology type `incident`: nothing fills `where` ...
  ⚠  ontology type `passage`: nothing fills `water_height`, `direction` ...
  ⚠  ontology type `tie_state`: nothing fills `tie_state` ...
  ⚠  ontology type `custody`: nothing fills `where` ...
```

It passed, but reading the facets against my answers showed five things wrong:
- **passage** (an event specialising incident, holding height and direction) was
  never filled, because no claim names `passage` as its subject. The ledger's
  heights would have been lost. **Fix:** I dropped `passage` and moved `vessel`,
  `water_height` (ft) and `gates` onto the `record` claim. A ledger entry is a
  record about the incident of a gate opening, and it stays comparable with
  accounts of that same incident.
- **tie_state** had no reader. **Fix:** I dropped it. The bond itself (`tie`) is
  still read.
- **`where` as `ref of place`** had no candidates, because `place` is a base kind
  with no source. **Fix:** I made it text, pointed at as written, on both
  `incident` and `custody`.
- **`voices.attributed_to`** is ignored by the document reader. **Fix:** I
  removed it. Speakers are already explicit fields (`speaker`, `alleged_by`,
  `said_by`, `written_by`).
- **character occupation/household** and **object sort** stayed on each
  statement, with nothing held on the record itself. **Fix:** `by = "most"` for
  occupation and household, and `by = "agree"` for sort. Owner stays
  per-statement, because owners change in this kind of story.

### Run 2, `sovereign recipe validate recipe.toml`

```
✓ Validation passed

Derived from your declarations:
  clock: narrative — supersession folds on order within the work
  tension selector: embedding top-k (k = 10, floor = 0.5) over account, narrated, record, custody, motive — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: character → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: object → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: incident → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  question shapes: enumerate [character, object]; relations [tie]; events [incident]; aggregate [account by speaker, account by told_to, account by source_named, record by document, record by written_by, record by vessel, knowledge by about, motive by against, motive by alleged_by, custody by held_by, custody by said_by]
  fill: character: occupation ← Point on each frame_of_mind, knowledge, motive, tie statement, the record by most; household ← ... the record by most
  fill: object: sort ← Choose on each custody statement, the record by agree; owner ← Pick on each custody statement, not the record (candidates: Mention)
  fill: incident: when ← Point on each account, narrated, record statement, not the record; where ← Point on ...; what ← Point on ...
  fill: tie: from `character` ← RESOLVE; nature ← Choose on tie; to ← Pick on tie (candidates: Mention)
  fill: frame_of_mind: of `character` ← RESOLVE; frame_of_mind ← Point on frame_of_mind
  fill: account: subject `incident` ← RESOLVE; speaker ← Pick on account (candidates: Mention); told_to ← ...; source_named ← ...
  fill: narrated: subject `incident` ← RESOLVE
  fill: record: subject `incident` ← RESOLVE; document ← Pick; written_by ← Pick; altered ← Choose; vessel ← Pick; water_height ← Point; gates ← Choose
  fill: knowledge: subject `character` ← RESOLVE; about ← Pick; stance ← Choose; how ← Choose
  fill: motive: subject `character` ← RESOLVE; against ← Pick; reason ← Point; alleged_by ← Pick
  fill: custody: subject `object` ← RESOLVE; held_by ← Pick; where ← Point; said_by ← Pick
```

No warnings. Checking the facets against my answers:
- **Clock = narrative.** Correct.
- **Tension selector across documents, over the five claim kinds.** Correct. A
  lie in chapter 3 can meet its exposure in chapter 12.
- **Identity.** All three entity and event types go through RESOLVE under their
  criteria. Correct.
- **Incident `when`/`where`/`what` are kept per statement, not on the record.**
  This is correct and intended. Two speakers' different hours for one incident
  must each keep their own reading, because that disagreement is the evidence.
- **Question shapes.** They cover questions 1 to 8 above.

No further fixes.
