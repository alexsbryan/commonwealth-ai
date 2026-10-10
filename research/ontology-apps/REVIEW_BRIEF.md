# Ontology layer: status brief for outside review (2026-10-10)

Written for reviewers who have not followed the work. Numbers come from runs on this repo; every figure has a
commit or run directory behind it, and the campaign log (`.sovereign/features/ontology-layer/campaign.md`,
local) holds the dated decisions.

## 1. What we are trying to build

A general layer that turns a pile of documents into typed, cited records:
- the deals in a sales mailbox;
- the cases in an issue tracker;
- the events in a news archive.

The user does not train a model or write extraction rules. They write one declarative **recipe**:
- the kinds of things their documents are about (entity and event types);
- what makes two mentions the same thing (an identity criterion, in words, plus declared keys);
- how a thing's state changes over time (a protocol);
- which statements to look for (claim kinds, with closed or open values).

Then they run the CLI's default commands. The records come out with every value and link citing the passage
and source behind it.

The objective has four parts:
1. **General:** the same code serves unlike domains. No domain word in code; domain knowledge lives only in the recipe.
2. **Author-independent:** two people who understand the domain and the syntax, writing separate recipes, get very similar records.
3. **No tuning in recipes:** a recipe carries no numbers measured on data and no mechanism switches.
4. **Local and auditable:** runs on a local model, every model answer is recorded, and a run replays byte for byte without the model.

## 2. The design (the method)

- **Declared structure first.** Code settles everything the recipe's structure can settle: fields, keys, threads, declared references, derived roles, order, state folds.
- **The model answers only small closed questions,** about one document at a time. Each answer is a distribution over single-token labels in one forward pass, or a pointer to words that code then verifies are in the text. The reader's plan of questions is a pure function of the recipe:
  - **Locate** which lines state each claim kind;
  - **Choose** each closed value;
  - **Point** at each open value;
  - **Mention** entity types that references target;
  - **Pick** each reference among candidates code proposes.
- **Identity is decided in one place (RESOLVE).**
  - Each statement is resolved to an existing record or a new one.
  - Candidates are proposed from declared structure and generic similarity.
  - Evidence from each source (declared fields, the model's choice, a similarity-based proposal) is weighed by a log-likelihood ratio. The weights are estimated on the corpus being read, without labels, so they are never numbers the author supplies.
  - A posterior above a bar links. Otherwise the statement opens a new record, or is held and settled after all documents are read.
  - The model never decides identity on its own: its verdict is one weighed source.
- **Six contracts, enforced as tests.**
  1. A question asks only what declared structure leaves open.
  2. A reading turn holds one document's text. A resolution turn may show a candidate's own cited lines, marked as quotes.
  3. Every value and link carries its source and that source's precision: declared, estimated, or unmeasured.
  4. Records do not depend on document order.
  5. A run replays without the model.
  6. Renaming every type and attribute changes only the names.

## 3. How we measure

Three gold-labelled corpora, used as instruments, not as training targets:

| System | Corpus | What gold marks |
|---|---|---|
| Ward | ~1,000 Enron emails from one gas trader's mailbox | 47 deals in the tune split, each with counterparty, stage over time and the messages about it |
| uv | GitHub issues, comments and timeline events from the `uv` project | Support cases and their state over time; we score a fixed third: 61 sections, 132 states |
| GVC | The Gun Violence Corpus | Cross-document event coreference, 977 event mentions in its 78 dev documents; gold splits one incident into sub-events (shooting, injury, death) |

For each system, a stage ladder shows where each gold item is lost:
- **read:** the right statement was found;
- **place:** it landed on the record of its gold particular;
- **fold:** the record's state is right.

GVC is also scored as **RESOLVE alone**: gold mentions are given as statements, which isolates identity from reading.

Author independence is tested with **blind authors**. A fresh LLM session gets only the domain description, the authoring guide and the validator, then writes a recipe that we run through the same default commands. We compare its records with ours.

## 4. Where we are

**Built and tested (unit tests plus the six contracts, all committed):**
- the default command path end to end;
- the answer recorder and byte-identical replay;
- the full reader plan above (Point, Mention and Pick landed last, on 2026-10-10);
- RESOLVE with corpus-estimated weights, candidates' own lines as quotes, and held statements settled after the last document;
- protocol folds;
- a validator that names, for every declared attribute, what fills it, and warns when nothing does.

One specified piece is unfinished: 13 older identity routines in the general extractor still decide types that have no identity criterion.

**Results on the instruments.** The newest run, with Point, Mention and Pick, is still in progress.

| Measure | Baseline (2026-10-09) | Best so far | Notes |
|---|---|---|---|
| Ward: deals whose current stage is served right | 2 of 47 | 3 of 47 | The motivating CRM case; essentially not working |
| Ward: deals matched to gold | 9 of 39 | 8 of 39 (ours), 10 of 39 (blind, round 3) | |
| uv: gold states hit (fixed third) | 30 of 132 | 67 of 132 | The latest build regressed to 54 through a defect (section 5) |
| GVC RESOLVE alone, CoNLL F1 | .599 | .607 | Floor with no model calls: .572 |
| GVC end to end, B3 | — | about .45 to .47 | 179 of 324 statements span more than one gold event |

**Blind authors, round 3.**

Agreement with our records, as B3 between the two partitions over items both placed. Our own recipe agrees with itself across two builds at GVC .720 and uv .975, which gives the scale.

| System | Agreement with ours |
|---|---|
| GVC end to end | .900 |
| uv | .976 |
| Ward | 1.0, over only 5 items |
| GVC RESOLVE alone | .445 |

Scored against gold at the concept each author declared:

| Measure | Blind author | Our recipe |
|---|---|---|
| GVC RESOLVE alone, against gold incidents (B3) | .814 | .328 |
| Ward deals matched (of 39) | 10 | 8 |
| uv states hit (of 132) | 56 | 67 |

The validator's warnings worked as feedback: the authors fixed declarations nothing would fill, and their recipes contain no tuned numbers.

## 5. What has gone wrong

**Process.**
- We spent much of two days measuring rather than building. Components the method specified (Point, Mention, Pick) were held back behind exploratory probes with pass bars we invented. The probes said "not worth building", but the method already called for these components. They were built only on the last day, so most results predate them.
- We chased each system's largest residual, which produced one-system levers and drift away from the general objective.
- We froze our own recipes to avoid fitting to gold. That kept a known modelling error in every "ours" row (below).
- Some green numbers were artifacts. Ward's deal-identity score of .875 is what you get when every message is its own deal: it scored well with zero correct links.

**Technical.**
- **Ward's identifying information is in free text.** A deal is a counterparty, product, delivery point and term. In this mailbox they sit in message bodies spread across threads, and much of the mail is internal discussion of an outside deal.
  - Our recipe derives the counterparty from email headers. For none of the 25 deals lost at placing is the counterparty in the latest message's headers, and 19 of those companies never appear in any header.
  - With no declared evidence to link two messages, and the model not allowed to decide identity alone, RESOLVE opens a new record for almost every statement: 84 of 92.
- **GVC's statement unit does not match gold's.** We make statements from whole lines, while gold marks individual mentions and splits incidents into sub-events. 179 of 324 statements already span several gold events before resolution.
- **Reader capability.** The local model is a 35B-parameter mixture-of-experts (about 3B active), quantised to 4 bits, answering one closed question at a time. Offline probes on our recorded data:
  - picking a deal's counterparty from text is right .44 of the time (.52 with a looser name match);
  - pointing at event mentions has .59 precision;
  - its "these are the same" verdict on candidate pairs has precision of about .45 to .54.
- **Label-free reliability estimation does not identify the text-reading sources.** The model's choice and the similarity proposal make false links that look like true ones on every feature we have (date, event kind, shared named entities). Every label-free estimator we tried leaves a gap of .4 to .7 between estimated and true precision on some run.
- **The newest build's line filter regressed uv.** It drops any line whose text appears in an earlier document. That removes the only line of formulaic tracker events ("closed", "labeled bug"), so 12 states went unread again. The fix is clear and structural; it is not applied yet.
- **Operations.** A shared host repeatedly ran low on disk, from other projects' builds and swap growth. That stopped one full test run and constrained builds. The full test suite at the latest commit has 4 failures and 5 timeouts, all outside this layer's code.

## 6. Open questions for reviewers

1. **Architecture vs ceiling.**
   - Does "declared structure decides, the model answers only closed questions, and never decides identity alone" cap quality on free-text-heavy domains such as deals in email?
   - Would letting the model reason over several documents, or generate and then verify, be worth the loss of auditability?
2. **Is the reader model the bottleneck?** The pipeline can run unchanged with a stronger model behind the same questions. Is that the right next experiment, and what would its result mean either way?
3. **Measuring "similar records whoever wrote the recipe."** Two reasonable authors chose different granularity (whole incident vs. sub-event) and so got different records. Should author independence be scored as:
   - each author's records against gold at the concept that author declared;
   - agreement only where the authors declared the same concept;
   - or something else?
4. **Label-free reliability of dependent, pre-selected evidence.** Candidate pairs are proposed by similarity, and two sources read the same text. Are there known estimators (beyond Fellegi–Sunter EM with independence) that work here, or should unmeasurable sources be refused rather than weighed?
5. **The statement unit.** A line, a sentence or a mention: how should a general system choose the unit of a statement in event-dense text without a domain rule?
6. **Gold as target.** These benchmarks encode particular concepts (GVC's sub-events, Ward's deal boundaries). How much should a general system be judged on matching them, rather than on being right at the author's own concept?
7. **Legacy identity routines.** Should the general extractor's older identity routines, used for types with no declared criterion, be folded into the single decider (which then needs a default criterion), or retired?

## 7. Formal objectives (the campaign's bars)

The campaign is met when each bar reads met on its newest measured row:

| Bar | What it measures |
|---|---|
| layer-identity-ward | A blind author's Ward deal identity, at or above the 2026-10-09 baseline |
| layer-identity-uv | A blind author's uv case identity, at or above the 2026-10-09 baseline |
| layer-identity-gvc | A blind author's GVC identity, at or above the 2026-10-09 baseline |
| layer-no-tuning | No measured numbers or switches in recipes: now 0, met |
| layer-default-path | All three systems run through the default commands: met |
| layer-invariants | The six contracts pass: met |
| layer-author-gap | Blind-vs-ours agreement at least our own cross-build agreement: now .595 against 1.0, bound by GVC RESOLVE alone |
| layer-estimator | Estimated vs true source precision within .1: not yet measurable on all systems |

The working judgement in this brief is that the bars and ladders are proxies. The objective is section 1.
