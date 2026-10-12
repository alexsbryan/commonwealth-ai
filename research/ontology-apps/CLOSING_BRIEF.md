# The ontology layer at close: a brief for review (2026-10-10)

Written for a reader who has not seen this work, at the close of the campaign. It says what we set out to build,
how, what eight days produced (2026-10-03 to 2026-10-10), what the final run shows, why the campaign closes with
its objective not met, and the questions we would put to a reviewer. It keeps the shape of the 2026-10-07 brief
(`REVIEW_BRIEF_2026-10-07.md`); the 2026-10-10 status brief (`REVIEW_BRIEF.md`) describes the design in more
detail. Numbers come from the final run (`runs/converge-s2`, local to this host; its readings are committed under
`converge-s2/` and `blind-author/converge-s2/`), its frozen baseline
(`~/.svrnmesh/bench-corpora/baseline-fullreader-20261010`), the measurement rows in
`~/.sovereign/comaintainer/bar-measurements.jsonl` and the commit log. Where something was not measured it says so.

**Corrections (2026-10-11, from the review in note 49fa8428; the campaign is reopened).** What below was wrong:
1. The closure's cited authority does not exist: the 2026-10-10 handoff says demonstrate the objective and finish
   one coherent path (CONVERGE.md:49-59), not "then close".
2. Served ref -> text (Q1) was an introduced type-binding regression, not a found limit: Pick answers identity keys
   as strings and the fold wrote them. Repaired in d3b12ba9c (a ref holds a record of its type or nothing; the
   mention kept cited and unresolved); "about six matched deals" was a scorer rescore, not served correctness.
3. "PLACE, so no reader boundary" is unsupported: the first-loss bucket is not a cause, Pick makes wrong company
   choices, and RESOLVE asks the model too. The blind ladders' largest losses are READ (Ward 44/47, uv 65/182,
   GVC 606/977).
4. The uv authors read different inputs (blind: ours' 554 documents plus 402), so masking the evaluation does not
   remove input effects; the common row (blind-author/converge-s2/agreement.json:151-172: 59/132 vs 58/132)
   belongs beside every two-author uv number.
5. Bars: three met, two failed, three unjudged, and the GVC failure is the older RESOLVE-alone run; current failure,
   stale evidence and missing measurement are three things.
6. Q2's candidate cited-line comparison already exists (resolve_records/select.rs calls answer::describe, which
   shows up to four cited quotes per candidate); its limits are to be measured, not proposed as new.
7. 17/17 fixture tests and 469 cited lists do not show a ref targets its declared type; the control that does is
   layer_contract_tests reader `a_served_reference_is_a_record_of_its_type_and_a_mention_stays_unresolved`.
   Ward's cost in wall seconds is 23.98 s/doc for extract alone; 25.93 summed model-seconds is another unit.

## The verdict

The campaign closes with its objective not met. What holds: one production path (publish a recipe, then
`enrich extract`, `--finalize` and `atlas-resolve` with no flags) runs on mail, issues and news; all six final
legs, our recipe and a blind author's on each system, replay byte for byte without the model (seven record files
each, no refusal); each of the 469 values our three runs' folds decided cites the documents it came from; the six
contracts pass as tests on fixtures of all three systems (17 of 17 contract and reader tests); and no recipe
carries a measured number or a mechanism switch.

What does not hold is what a user would see. The records are sparse, and often wrong at the concept they claim.
On Ward, 7 of 47 current deals are served at the right stage. On uv, 14 of 44 cases serve the right current state
and 309 of 488 tune documents sit in no case. On GVC, 40 of 977 event mentions sit on a record that serves their
gold type. Two authors' records agree (B³ .86 to 1.0) only over the units both placed, which are 12% (Ward),
23% (uv) and 35% (GVC) of all units, and they serve different fields. A Ward message costs 41 model calls and 26
model-seconds, against a 1.7 s target.

## What we are building

A layer that turns documents plus a declared ontology into typed, cited records: the deals in a sales mailbox
with their counterparty and stage, the cases in an issue tracker with their current state, the events in a news
archive. The user writes one declarative recipe (the types, what makes two mentions one thing, how state changes,
which statements to look for), runs the CLI's default commands, and every value and link cites the passage behind
it. The objective, as the operator set it on 2026-10-09 (`quality/campaigns/closed/ontology-layer.toml`, which
superseded the Ward-first `crm-proof` campaign), has four parts: the same code serves unlike domains, with no domain word in
code; two authors who understand the domain and the syntax get very similar records; a recipe carries no number
measured on data and no mechanism switch; and everything runs on a local model, recorded, replaying byte for byte
without it.

## The examples

- **Ward**: about 1,000 messages from one Enron gas trader's mailbox. Gold, reviewed by the operator, marks deals
  with counterparty, stage over time and the messages about them. The final run reads the tune split's 81 sections
  (99 documents): 47 current deals, 39 transactions and 8 master agreements.
- **uv**: issues, comments and timeline events from astral-sh/uv, with hand-labelled support cases and their state
  over time. Runs read a fixed third of the tune fold: for our recipe 61 sections, 554 documents and 132 gold
  states. The blind author's recipe sections the same population into 55 sections over 956 documents (182 states),
  so the two ladders read different item sets; agreement compares them over the 488 tune gold documents.
- **GVC**: the Gun Violence Corpus, a public cross-document event-coreference benchmark: 977 event mentions in 78
  dev documents. Gold chains are sub-events (firing, hitting, injury, death) of incidents.

## Method

Declared structure settles what it can (fields, keys, threads, declared references, order, state folds). The model
answers small closed questions about one document at a time: Locate the lines stating each claim kind, Choose each
closed value, Point at each open value, Mention referenced types, Pick each reference among candidates code
proposes. Identity is decided in one place, RESOLVE, which weighs every source (declared fields, the model's
choice, similarity) by weights estimated on the corpus; the model never decides identity alone. Records fold their
statements into state by the recipe's declared protocol and, since Session 1, hold read fields by a declared fold
(`by = agree | most | latest | ...`). Six contracts are tests: questions only where structure leaves something open,
one document per reading turn, every value with its source and that source's precision, order independence,
replay without the model, and rename invariance.

Measurement follows each gold item to where it is lost (READ: the claim exists on the right document; PLACE: it
sits on its gold particular's record; FOLD: the record's state is right), with membership coverage and recovery B³
reported beside the conditional identity score, and model calls per document beside both. Blind authors are fresh
sessions given only the domain description, the authoring guide and the validator; their records are compared with
ours over the units both placed, and each is scored against gold at the concept it declared.

## What was done, in order

- **10-03 to 10-06** (the 2026-10-07 brief has the detail). Ward baseline through the old pipeline; research
  prototypes retired; uv gold and a shared partition scorer; header-derived people and companies (Ward holdout
  people .584 → .801); RESOLVE built in rings with bars before data, where the adopted design equalled the
  zero-model floor on Ward and uv; RESOLVE and derived attributes in the production build (Ward holdout deals .232).
- **10-07.** Three ingest defects fixed before the first uv production build. Declared document reading with
  evidence refusals and a protocol fold (`791b328f8`).
- **10-08.** Reading becomes a plan of small closed questions (`441873ca0`, `fce128f84`). crm-stage credits only a
  deal's served current stage, with its message (`9c7417d3b`).
- **10-09.** The ontology-layer campaign opens with its bars pre-registered (`5b9cc28e7`) and a frozen baseline
  row. Blind round 1 (`627554920`). RESOLVE weighs every source and holds what it cannot settle (`d91b76ad8`), with
  weights estimated on the corpus (`1a2978828`). The bare `atlas-resolve` runs the whole layer (`84fa10f17`); the
  reader is chosen by the declaration and the one-shot reader is deleted (`79a640c63`). The six contracts become
  tests (`984427884`); stage ladders per system (`23b6c45fc`).
- **10-09 night to 10-10 midday.** Blind rounds 2 and 3, each scored through an evaluator map frozen before its run
  (`400253034`, `c5239f7d6`, `f984835a1`). No label-free precision estimator came within .1 of the truth on every
  run (`ecb58efcc`). Locate shows each kind's declared values: uv hits 32 → 67 of 132 (`ee6c0735c`). The validator
  names what fills each attribute (`f4e4df330`). Point, Mention and Pick land (`7e4e1b28a`); held statements are
  settled after the last document (`d8458602b`). A status brief for review (`02456e75f`).
- **10-10, the operator's handoff** (`41b686513`, `CONVERGE.md`): two final sessions. Session 1 made the existing
  path keep its evidence: a frozen full-reader baseline; a line counts as a quote only on structural evidence
  (`cf33c151d`); ladders show coverage, recovery and served state (`f3108fd6f`); a claim keeps its subject's
  readings through RESOLVE (`7e043fce8`); one locator decides a claim's document (`3f7148016`); a record holds its
  statements' read fields by the recipe's declared fold (`d256c8d72`); our recipes declare those folds
  (`666637f34`, `9945afc92`). Session 2 ran the result on all three systems with both authors (`runs/converge-s2`,
  binary at `d1fee1c15`, 4 h 53 min), read it, and took the campaign's last measurement rows (`41de44e13`).

## Results

Each system's final run against the frozen baseline, with the blind author's recipe beside it on the same binary.
Cost is model calls and model-seconds per document read, over every phase. Model-seconds move by about a fifth
between runs at the same call count (GVC ours: 21.5 and 17.3 s at 23.2 and 23.5 calls), so calls are the stable
figure.

**Ward** (47 current deals)

| | Baseline | Final, ours | Final, blind |
|---|---|---|---|
| deals served at the right stage | 2 | 7 | 2 |
| deals matched to a record (of 39) | 8 | 5 | 11 |
| membership coverage | .245 | .245 | .408 |
| recovery B³ | .233 | .233 | .327 |
| conditional B³ (units placed) | .957 (12) | .957 (12) | .806 (20) |
| calls / model-s per document | 22.8 / 15.4 | 41.2 / 25.9 | 37.2 / 30.8 |

**uv** (the fixed third)

| | Baseline | Final, ours | Final, blind |
|---|---|---|---|
| state claims right on the matched case | 54 of 132 | 58 of 132 | 74 of 182 |
| cases serving the right current state | could not judge: no case served a state | 14 of 44 | 0 of 54: no record serves a mapped state |
| membership coverage | .309 | .367 | .334 |
| recovery B³ | .188 | .248 | .221 |
| conditional B³ (units placed) | .895 (151) | .898 (179) | .889 (283) |
| calls / model-s per document | 4.0 / 2.5 | 4.4 / 2.5 | 7.4 / 6.0 |

**GVC** (977 mentions)

| | Baseline | Final, ours | Final, blind |
|---|---|---|---|
| mentions on a record serving their gold type | 0 (322 could not judge) | 40 (42 could not judge) | 0 (159 could not judge) |
| membership coverage | .799 | .800 | .375 |
| recovery B³ | .396 | .397 | .232 |
| conditional B³ (units placed), sub-event chains | .457 (781) | .458 (782) | .543 (366) |
| conditional B³ at the incident | not read | .607 (782) | .700 (366) |
| calls / model-s per document | 23.2 / 21.5 | 23.5 / 17.3 | 37.8 / 29.8 |

**Two authors, one corpus.** B³ between the two partitions over the units both placed; the reference is our own
recipe against itself across two builds.

| System | Unit | Both placed (joint coverage) | Blind vs ours | Ours vs ours |
|---|---|---|---|---|
| GVC | event mention | 344 of 977 (.352) | .864 | .720 |
| Ward | tune gold message in one deal | 6 of 49 (.122) | 1.0 | 1.0 |
| uv | tune gold document | 113 of 488 (.232) | .975 | .975 |

**The campaign's bars**, last rows (2026-10-11T06:13Z): layer-default-path met (3 of 3, this run);
layer-invariants met (6 of 6 contracts at `41de44e13`); layer-no-tuning met (0); layer-identity-ward failed (.806
against .875, the blind Ward leg); layer-identity-gvc failed (.515 against .599: it reads RESOLVE alone, and the
newest such run is blind round 3's, at `8c88f2634`; Session 2 ran none); layer-identity-uv could not judge (the
bar is defined on the whole tune fold, which no run read; the third is a slice); layer-author-gap could not judge
(no RESOLVE-alone leg; its three end-to-end ratios are 1.20, 1.0 and 1.0); layer-estimator could not judge (no
estimate for Ward or uv).

Where a frozen bar measures a different concept than the objective. The identity bars read B³ conditional on the
units a recipe placed, and the 2026-10-09 baseline they compare against was Ward's all-singletons floor; they
cannot see that our Ward recipe places a quarter of the units. Ward's scorer matches a deal record by its
counterparty: a company record that matches the gold company by domain or written form, or text that matches a
written form; a domain string held as text (`ci.mesa.us`) matches neither. uv's ladder hit counts a state claim on
the matched case, not the state the case serves, which is why served state is reported beside it. GVC's hit asks
for the gold sub-event type, so the blind author, who declared incidents, reads 0 by construction; at the incident
its records score .700. The blind uv map, frozen before any run, names no attribute for the blind recipe's
`problem.status`; read raw, 4 of its 98 records serve one.

## Why it closes here

**The binding loss is placing, on every system.** PLACE loses 28 of 47 Ward items, 32 of 132 uv items and 461 of
977 GVC items, more than READ (14, 27, 194) or FOLD (0, 15, 240). Inspecting the records says what that loss is
made of.

**Ward's deal records are statements.** 84 stage updates opened 78 deal records, most named by the sentence they
came from (68 of the 78 names run six words or more); one RFP message (smurfit/20) produced four. Of the 40 current
deals with no matched record, 9 have no record citing their messages; 6 have a record naming their party that
one-to-one matching gave to a sibling deal (one message, smurfit/23, lists eight executed deals and produced three
records); and the other 25 have records whose counterparty is another party (15), the right one as a domain string
(7), or none (3).

**A declared reference became text.** Session 1 moved Ward's counterparty from a header-derived link to a value
Pick reads from each statement (`9945afc92`), held by `most` (`d256c8d72`). The attribute is declared `ref` of
company, but the fold stores the picked string: at the baseline 31 of 78 deal records linked a company record, in
the final run none does (39 hold a name, 23 a domain string, 16 nothing). Pick's choices are mixed: Summit Energy,
the broker that sent Smurfit's RFPs, on five records; PG&E, the utility, on five; Enron itself on four, which the
recipe excludes as `ours` by domain but a text mention escapes; EPNG, a pipeline, for Tucson Electric's bid; once
the span "BHP CONTACT TO VERIFY THAT BHP BILLITON LTD". Between the baseline and the final run served stages went
from 2 to 7 and matched deals from 8 to 5, while the blind recipe, which kept header links, matches 11. A side
reading that lets a domain string resolve by domain gives ours 11 deals and 8 served stages, so about six deals are
the lost link rather than the read. The fix is in the fold for references (keep the candidate's record; every
header-named company is one) and was found at close, not made (`converge-s2/ward-residual.txt`).

**uv's states are half right where a case is found.** Our 58 case records serve a state on 54 (the protocol fold,
`666637f34`). Of 44 gold cases with a state in the third, 16 match no record, 14 serve the right state, 11 a wrong
one (declined and reported swap five times) and 3 none (`converge-s2/uv-served.txt`). 309 of the 488 tune
documents sit in no case, so recovery B³ is .248 while conditional B³ is .898. The best uv reading of the
campaign, 67 of 132 hits (E4, before the full reader), was not regained: of the 13 the full reader's build lost,
Session 1 recovered 4, and the other 9 were not traced.

**GVC's records are incidents more than sub-events.** 101 of 145 happening records decide a kind; 55 of those
decide "the incident as a whole", against gold chains that are firing, hitting, injury and death. 41 records hold
conflicting kinds under `agree` and serve none. 175 statements span several gold chains before RESOLVE sees them.

**The authors diverge in what records serve.** The round-3 blind recipes were written before read fields could be
folded onto records, and their status folds serve little: the blind uv problem serves a status on 4 of 98 records
(25 pending, 69 with nothing to fold), blind Ward a status on 19 of 158 deals, and blind GVC declares no record
field. From the pre-repair binary to the final one the blind ladders moved by a unit or less (Ward served 1 → 1, uv
hits 73 → 74, GVC 0 → 0); ours moved through the folds our recipes declared. Where both authors place a unit they
agree; they place few in common. On GVC the authors declared different concepts, and the blind author's incident B³
fell from .777 on the pre-repair binary to .700 on the final one (one run each, so not yet a measurement).

**Cost went up.** Ward's model calls per document went from 22.8 to 41.2 and model-seconds from 15.4 to 25.9:
Mention (12.1) and Pick (0.8) are new with the counterparty read, Point rose from 3.0 to 5.7 with product and
delivery point, and Locate from 16.8 to 19.7 with the lines no longer suppressed. crm-cost's target is 1.7 s per
message. uv costs 4.4 calls and 2.5 s, GVC 23.5 and 17.3.

**No stronger-model comparison was run.** The handoff made it conditional on a reader boundary that binds. The
loss that binds is placing, and its parts above are modelling and structure (a record per statement, a reference
held as text, incident against sub-event) rather than one question the reader gets wrong. The daemon's strongest
local model is the reader itself (Qwen3.6-35B-A3B at 4 bits); a stronger one would be a download.

## Open questions

1. **References from text.** Should Pick return the candidate's record (a header-named company is one already) and
   a mention join or open a record of the referenced type under that type's declared keys, so a `ref` can never
   hold text? Or should references stay structural, with the body only proposing?
2. **Records per statement.** Where the identifying fields are in free text, statement-level RESOLVE opens a record
   per statement. Is the next step a record-level read (the model reads a candidate's cited lines with the new
   statement, as one weighed source), or declared keys (product, delivery point, term) applied before RESOLVE?
3. **What "similar records whoever writes the recipe" should mean.** Two authors chose different concepts (incident
   against sub-event) and different folds (state on the record or only on its claims). Is the right test each
   author's records against gold at the concept declared, plus a check that every declared field is served,
   rather than agreement over the units both placed?
4. **Cost.** Is a per-document plan of small closed questions compatible with about 2 s per message on local
   hardware, or must structure settle most fields first and the model see only what is left?
5. **The statement unit.** A line, a sentence or a mention: how should a general system choose the unit of a
   statement in event-dense text without a domain rule?
6. **Frozen instruments.** Ward's scorer resolves raw text by written form but not a domain string by domain; uv's
   frozen blind map names no state attribute. Should instruments frozen before data be revised when they measure
   a concept the objective does not, and how without fitting them to results?

## Where to look

`CONVERGE.md` (the two final sessions, step by step, with commits); `converge-s2/` (the final run's manifest,
ladders, measurement rows and the two closing readings, `ward_residual.py` and `uv_served.py`; the run directory
itself, with its run log and every leg's replay, is `runs/converge-s2`, local); `blind-author/converge-s2/` (author
agreement and the incident reading); `ladder.py` and `bars.py` (the instruments), `ward/score.py` and `support/score.py` (the scorers);
`svrn/docs/specs/ONTOLOGY_METHOD.md` and `ONTOLOGY_PRIMITIVES.md` (the method);
`quality/campaigns/closed/ontology-layer.toml` (the bars and the closeout); `REVIEW_BRIEF.md` (the 2026-10-10
status) and `REVIEW_BRIEF_2026-10-07.md`.
