# Transcript — The Saltgrass Ledger recipe

Author stance: a high-school English teacher building a unit around the novel. The partner was
not available, so each question below is answered from `DESCRIPTION.md` where it speaks, and
otherwise from general knowledge of how a mystery novel is taught and read.

Template consulted: `sovereign recipe new --ontology literary --id saltgrass-ledger` (printed to
stdout, not written). It gave the shape — markdown, narrative clock, `character` + `inner_state` —
but not the claim types. Its `reading` (criticism, `about_work`) does not apply, because the
material is the novel alone.

---

## 0. Source shape (Turn-1 framing)

**Q:** What are the files, and how are they laid out?

**A:** One novel of sixteen chapters. I assume it is a manuscript in markdown (one file, or one per
chapter) with a heading for each chapter in reading order, so I use `local_file` with
`path = "SOURCE_PATH"`, the `markdown` extractor (one section per heading, so every statement can
be cited to a chapter) and `paragraph` chunks.

**Source:** description: "Sixteen chapters of salt, tar and quiet lies". The file format and the
heading layout are an **assumption**. If the material is a `.txt` with no headings, the extractor
should be `plaintext` instead.

## 1. Shape

**Q:** What is this material about, and what would you want to know about each kind of thing? Is
any of them a kind of another, or a part something plays? Do you already have any of it in a table?

**A:** It is a mystery about a death in a small port. The kinds of thing:
- **character**: a role of `person`. For each one: occupation, and their habits (routine).
  Description: "Corwin Pellow, harbormaster for twenty-one years", "Maren Ashquill keeps the tide
  ledger", "she has watched the harbormaster cross the cobbles at the same hour every morning".
- **location**: specializes `place`. The basin, the gates, the gauge, the cobbles. Description:
  "the lock basin holds its water … behind heavy gates", "the painted gauge", "the cobbles".
- **vessel**: description: "every vessel through them".
- **incident** (event): a particular thing that happened, with participants (who, vessel,
  where), when it happened and the gauge height. The death is one, and so is each gate opening.
  Description: "found floating in the basin at dawn", "every opening of the gates".
- **object**: the ledger itself, plus any physical item the plot turns on. The ledger is from the
  description ("the tide ledger"). The other items are an **assumption** based on the genre.
- **tie** (relation, character→character): kin, debt, rivalry and so on. Description: "a village
  where everyone has a reason". The particular kinds of tie are an **assumption**.
- **inner_state** (state of character): what Maren suspects or fears, and how that changes.
  Description: "Now she starts to wonder".

No tables; it is prose only. **Source:** description (quoted above), plus assumption where marked.

## 2. Assertion

**Q:** What do the sources say about those things — stating, requiring, deciding, asking? About
what? What does each statement carry? How do you tell strong evidence from weak? Who is speaking,
and are they part of the subject? What must it never do?

**A:**
- **account** (assertive, `in_work`, about an incident): a character tells what happened or where
  someone was. It carries the speaker, who it was told to, and the manner (openly / whispered /
  in private / under questioning / overheard). Description: "Nobody will say so above a whisper",
  "the people she has known all her life are so sure of things they cannot have seen."
- **ledger_record** (assertive, `in_work`, about an incident): what the ledger shows. It carries
  who entered it, the vessel, and its condition (as written / corrected / in another hand / blank
  / missing). Description: "every opening of the gates, every vessel through them, every height of
  water against the painted gauge"; "what the ledger saw". The condition values are an
  **assumption**: in a mystery about a record-keeper, a tampered ledger is the expected device.
- **suspicion** (assertive, `in_work`, about a character): someone holds or hints that a character
  had a hand in the death, or a motive for it. It carries who holds it, the motive, and the manner.
  Description: "everyone has a reason", "the whole village has decided by breakfast that it was no
  accident".
- **finding** (assertive, `in_work`, about an object): someone finds or describes a physical
  thing. It carries who found it. The object's owner and where it was found are read on each
  finding. This was added after validation (see run 1). **Assumption.**
- **pressure** (directive: require / forbid / request, about an incident): someone tells another
  person to keep quiet, look away or stop asking. Description: "Nobody will say so above a
  whisper", "quiet lies". The directive reading is an **assumption**.
- **Evidence grades**, strongest first: eyewitness, documented (the ledger), inference, hearsay,
  rumor. Description: "so sure of things they cannot have seen". Distinguishing these grades is
  the core reading skill the unit teaches. The order is an **assumption**.
- **Voices:** the narrator is not an entity. Characters speak and are also the subject matter, so
  the speaker is a `ref` attribute.
- **must_not:** never state the killer (or that it was murder) unless the narration establishes
  it; never present a claim as what happened; never count repeated gossip as corroboration; never
  invent quotations or chapter references. Source: **assumption**, from a teacher's standard. The
  first rule rests on the description: "decided … that it was no accident. Nobody will say so".

## 3. Identity

**Q:** How do you know two mentions are the same thing? Is there an ID?

**A:** There are no IDs, so every entity is decided by `identity_criterion` in prose:
- character: the same person under full name, surname, first name, nickname or office ("the
  harbormaster" = Pellow while he holds the post). Description: "Corwin Pellow, harbormaster".
- vessel: the same boat name, or the same unnamed craft identified by master, look or berth.
- location: the same physical spot ("the basin" = "the lock basin").
- object: the same item followed through the story ("the ledger" = "Maren's book").
- incident: the same occurrence, meaning the same place, night or tide, and people, even when
  witnesses tell it differently. A daily habit is not one incident.
- character.occupation is folded `by = "agree"`, so a disagreement about what someone does shows
  up as a conflict.

**Source:** "harbormaster" as both an office and a name is from the description. The rest is an
**assumption**.

## 4. Change

**Q:** When does a later statement replace an earlier one? Does anything move through stages?
Which document fields give date and thread?

**A:** The clock is **narrative** (chapter order). Nothing supersedes anything: in a mystery, a
later account does not cancel an earlier one, and the gap between them is what students must find
(description: "quiet lies", "the truth comes in with the tide"). So I set no `supersedes`. There is
no state protocol, because nothing moves through formal stages. There is no `change.document`,
because a novel's chapters carry no dates or threads, and I could not run `--corpus` against the
material to see what fields exist. Maren's changing suspicion is an `inner_state` trajectory, not
a fold. **Source:** assumption, guided by the description.

## 5. Derivation

**Q:** What should it notice that no single document says? What looks like a contradiction but
isn't?

**A:** Tensions (labelled "discrepancy") between **accounts and ledger records about the same
incident** (`same = ["subject"]`, so that statements from different chapters are compared). These
are the core of the plot: "what the ledger saw" versus what people "are so sure of". Configurations
(groups of characters in scenes) stay on. Argument reconstruction is off.

Not conflicts:
- an added detail
- the ledger's silence about something that never needed the gates opened
- the same rumour repeated by two people
- a time given by the tide versus the same time given by the clock

**Source:** description for the ledger-versus-testimony axis. The `not_conflicts` list is an
**assumption**.

## 6. Questions

**Q:** Once it is read, what will you ask of it?

**A** (walked to declarations):
1. *Where does each character say they were the night Pellow died, and who heard them say it?*
   → `account` on the death `incident`, by `speaker` / `told_to`.
2. *Which statements about the death come from someone who saw it, and which are hearsay or
   rumour?* → `account.grades`.
3. *Does the ledger agree with what the village says about the gates that night?* → tension
   between `account` and `ledger_record` on the same `incident`.
4. *Who has a reason, and who says so, and how loudly?* → `suspicion` (`held_by`, `motive`,
   `manner`), plus `tie`.
5. *How does Maren's understanding change from chapter to chapter?* → `inner_state` of
   `character`, narrative clock.
6. *Who tells whom to keep quiet?* → `pressure` (`from_whom` / `to_whom`).
7. *What physical evidence is there, where was it, and who found it?* → nothing in the first
   draft held this, because `object` had no claim reading it. That pointed to a missing kind of
   statement, so I declared `finding` with `subject = "object"` (see run 1).
8. *What was Pellow's routine, and where did it break?* → `character.routine`, plus `incident`.

**Source:** assumption. These are questions a teacher would set for this novel.

---

## Validation runs

### Run 1 — `sovereign recipe validate recipe.toml`

```
✓ Validation passed
Derived from your declarations:
  clock: narrative — supersession folds on order within the work
  tension selector: embedding top-k (k = 10, floor = 0.5) over account, ledger_record — ...
  identity: character|vessel|location|object|incident → RESOLVE under its identity_criterion, ...
  question shapes: enumerate [character, vessel, location, object]; relations [tie]; events [incident];
    aggregate [account by speaker, account by told_to, ledger_record by entered_by, ledger_record by vessel,
    suspicion by held_by, pressure by from_whom, pressure by to_whom]
  fill: character: occupation ← Point on each inner_state, suspicion, tie statement, not the record; routine ← ...
  fill: vessel: master ← nothing; rig ← nothing
  fill: object: owner ← nothing; found_at ← nothing
  fill: incident: occurred ← Point on each account, ledger_record, pressure statement, not the record; gauge_height ← (same)
  fill: account: ... claimed_time ← Point on account
  fill: ledger_record: ... gate_time ← Point on ledger_record; gauge_height ← Point on ledger_record; ...
  ⚠  `voices` (attributed_to) are applied by the general extractor only; the document reader attributes no statement to a speaker.
  ⚠  ontology type `vessel`: nothing fills `master`, `rig` ...
  ⚠  ontology type `object`: nothing fills `owner`, `found_at` ...
```

**Reading the facets against my answers, and the fixes:**
- **object** was never anyone's subject, so nothing would ever read it. Question 7 had no home.
  Fix: declared the claim `finding` (subject `object`, `found_by`). Its owner and found_at are now
  read on each finding.
- **vessel.master / rig**: nothing could fill them. I dropped them. A vessel's master can still be
  read through `incident` participants and `tie`.
- **incident.occurred / gauge_height** are already read on every account and ledger record about
  that incident, so `account.claimed_time`, `ledger_record.gate_time` and
  `ledger_record.gauge_height` asked the same thing twice. I removed them. Each statement keeps
  its own reading of the time and the water height, which is exactly what makes discrepancies
  visible.
- **voices.attributed_to** does nothing under the document reader, and the speaker is already
  carried by `account.speaker`, `suspicion.held_by` and so on. Removed.
- **character.occupation** is held per statement only. Set `by = "agree"` so the record holds
  one occupation, or flags a conflict.

### Run 2 — `sovereign recipe validate recipe.toml`

```
✓ Validation passed
Derived from your declarations:
  clock: narrative — supersession folds on order within the work
  tension selector: embedding top-k (k = 10, floor = 0.5) over account, ledger_record — cross-document declared corpora select the embedding net; the classifier judges each pair
  identity: character → RESOLVE under its identity_criterion, over the claims declaring it their subject; atoms Phase 1 named are retired
  identity: vessel → RESOLVE under its identity_criterion, ...
  identity: location → RESOLVE under its identity_criterion, ...
  identity: object → RESOLVE under its identity_criterion, ...
  identity: incident → RESOLVE under its identity_criterion, ...
  question shapes: enumerate [character, vessel, location, object]; relations [tie]; events [incident]; aggregate [account by speaker, account by told_to, ledger_record by entered_by, ledger_record by vessel, suspicion by held_by, finding by found_by, pressure by from_whom, pressure by to_whom]
  fill: character: occupation ← Point on each inner_state, suspicion, tie statement, the record by agree; routine ← Point on each inner_state, suspicion, tie statement, not the record
  fill: object: owner ← Pick on each finding statement, not the record (candidates: Mention); found_at ← Pick on each finding statement, not the record (candidates: Mention)
  fill: incident: occurred ← Point on each account, ledger_record, pressure statement, not the record; gauge_height ← Point on each account, ledger_record, pressure statement, not the record
  fill: tie: from `character` ← RESOLVE; nature ← Choose on tie; to ← Pick on tie (candidates: Mention)
  fill: inner_state: of `character` ← RESOLVE; inner_state ← Point on inner_state
  fill: account: subject `incident` ← RESOLVE; speaker ← Pick on account (candidates: Mention); told_to ← Pick on account (candidates: Mention); manner ← Choose on account
  fill: ledger_record: subject `incident` ← RESOLVE; entered_by ← Pick on ledger_record (candidates: Mention); vessel ← Pick on ledger_record (candidates: Mention); condition ← Choose on ledger_record
  fill: suspicion: subject `character` ← RESOLVE; held_by ← Pick on suspicion (candidates: Mention); motive ← Point on suspicion; manner ← Choose on suspicion
  fill: finding: subject `object` ← RESOLVE; found_by ← Pick on finding (candidates: Mention)
  fill: pressure: subject `incident` ← RESOLVE; from_whom ← Pick on pressure (candidates: Mention); to_whom ← Pick on pressure (candidates: Mention)
```

**Reading the facets:**
- **clock:** narrative, as answered.
- **tension:** account against ledger_record, as answered.
- **Question shapes:** these cover questions 1–8.
- **Time and gauge readings:** "not the record" is right for `incident.occurred` and
  `gauge_height`. Witnesses must keep their own times, or the discrepancy disappears.
- **Remaining doubt:** `vessel` and `location` are subjects of no claim. They are reached only as
  Picks (ledger_record.vessel, object.found_at) and as incident participants. Validate raises no
  warning, so I leave them, but they are the least-exercised types (see summary).
- No further changes.
