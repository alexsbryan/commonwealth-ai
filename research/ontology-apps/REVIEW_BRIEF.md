# crm-proof and the ontology layer: a brief for review (2026-10-07)

Written for a reader who has not seen this work. It says what we are trying to build, how we are going
about it, what four days of work produced, where it is stuck, and the questions we would most like a second
opinion on. Numbers are from `~/.svrnmesh/comaintainer/bar-measurements.jsonl`,
`research/ontology-apps/resolve-prereg.md` and the commit log; where something is unmeasured it says so.

## What we are building

A layer that turns raw documents plus a declared ontology into typed, cited records: a CRM's people,
companies, deals and their stages from a mailbox; a support tracker's cases from issues, comments and
timeline events. The premise is that most SaaS records are kept by hand through forms, and that they could
instead be read out of the documents people already write, with every field citing the text it came from.
The CRM and the tracker are test examples, not the product domain. The layer runs entirely on a local model
served by our own daemon; no external model is in the production path.

The campaign (`quality/campaigns/crm-proof.toml`) opened 2026-10-03 and was re-scoped on 2026-10-05 from "a
CRM from a mailbox" to "the first check of the ontology and extraction layer" (`svrn/docs/specs/ONTOLOGY_METHOD.md`).
It is done when four bars read met on held-out data through the production interface: crm-people,
crm-deals, crm-stage and crm-cost. It should stop if the layer's next rungs do not move deals and stage on
the support example and on the mail.

## The examples

- **crm-ward**: 1,000 messages from one Enron gas-origination mailbox (Kim Ward, CMU maildir), 790 sections.
  Gold drafted by a different model family than the extractor and reviewed by the operator
  (`ward/GOLD_SPEC.md`): 79 deals (transactions) across customer folders, 266 stage updates, 102
  commitments; folders split into tune and holdout.
- **uv-support**: 152 issues from astral-sh/uv as 2,954 documents (150 issues, 1,391 comments, 1,413
  timeline events), 163 hand-labelled cases, 83 tune and 80 read (`support/GOLD_SPEC.md`). A case is one
  underlying problem however many issues report it.
- **GVC and ECB+**: public cross-document event-coreference benchmarks we did not label, added on
  2026-10-05 so "best in class" can be read against the literature.

## Method

The method was agreed with the operator on 2026-10-05 and 2026-10-06 (`ONTOLOGY_METHOD.md`, 116 lines; worth
reading whole). In short:

**The core.** Records are declared types, each with an identity criterion (keys that suffice, and the
criterion in the author's words) and a protocol. Documents are READ into cited statements; each statement
is RESOLVED to an open record, or to none (which opens one), under its type's criterion, against candidates
the declared structure proposes; records FOLD their statements into state through the protocol. RESOLVE is
grouping and joining as one step; "is this new?" is RESOLVE answering "none of these".

**What code may do.** Code reads what the recipe declares, proposes candidates from declared structure (same
thread, same party, a declared reference) or domain-free retrieval, verifies that what the model points at is
in the text, weighs evidence by its measured precision, and folds by the declared protocol. Code never
decides identity by an undeclared pattern (no "duplicate of #N" matcher), and a model verdict is evidence,
never a decision.

**Invariants.** The code knows no domain (rename every type in a recipe and the composition is the same up
to the renaming). Identity is decided by declared fields in one place: a sufficient field that agrees links,
a necessary field that differs forbids, evidential agreement links only where its measured precision clears
the type's bar. Every decision is traced and counted; an answer code cannot verify is refused, never
defaulted. One table per loop over three systems, model calls per document beside the measures; a change is
adopted only if no example regresses.

**The loop.** Bars are written before data (pre-registration); each arm runs once on every example's tune
fold; adopt or refuse, and a refusal ships its data. Held-out folds are opened once per adopted design.
Bars score only what a user gets through the bare production interface (recipe plus CLI, built end to end),
never a research script's composition.

**The pipeline as built.** Ingest chunks and indexes the documents; `enrich init` groups chunks into
chapters; Phase 1 sends each chapter to the model once and gets back a structured sketch (entities, their
states, relations, events, claims, questions, plus the recipe's declared types); the atlas build then
projects typed atoms from document fields with no model (people by email, companies by domain), RESOLVEs
types that declare a criterion, and fills derived attributes (paths, sets and folds over the build's graph,
e.g. a message's party is the first outside company among its addressees).

## What was done, in order

- **10-03.** Baseline on Ward through the existing pipeline: people .584, companies .615, deals .029, stage
  0, commitments .125, 17.86 s of model time per message.
- **10-03 to 10-05.** Python research prototypes of deal composition (a facet file, a ledger composer,
  oracle ablations, grouping and stage folds). The ablation found the act reader, not grouping, loses the
  most. Retired on 10-05 by operator direction: research drives the core through recipes and does not
  reimplement it.
- **10-04 to 10-05.** The second example's gold (uv-support, 163 cases), its scorer, CEAF-e and LEA beside
  B³ in one shared scorer (`svrn bench er-score`), zero-model baselines.
- **10-05.** Structural wins on Ward with no model calls: people and companies read from the headers,
  Contact → Account joined by address domain, a bundled list of mailbox providers so no recipe enumerates
  domains, a typed-reference fix. People .584 → .801, companies → .769, contacts .169 → .578.
- **10-05 to 10-06.** RESOLVE built in rings with bars before data (`resolve-prereg.md`), first in the
  gold-mention setting (gold statements in, records out) on all three systems: Ring 0 (one forced choice per
  statement), 1a (declared document fields weighed by code), 1b (a read necessary field as a veto, the
  model's choice weighed), 2a (precision from counts), 1c (reasoning before the choice). Results below.
- **10-06.** RESOLVE moved into the production atlas build, and derived attributes landed: Ward deals
  .072 → .232, stage .010 → .019.
- **10-07.** Before running the first production build on uv-support, three ingest defects that would have
  invalidated it were fixed (below), plus a chapter-size cap; that build is running now.

About 65 commits are tied to the campaign. Three change sets moved a system bar: the header sources, the
contact join, and RESOLVE plus derived party. The 10-05 scorer correction (match a gold company by its own
written forms) moved companies and deals as well, but that is the instrument, not the system.

## Results

Ward, holdout, production interface:

| bar | 10-03 | now | target |
|---|---|---|---|
| crm-people | .584 | .801 | .9 |
| crm-companies | .615 | .769 | .8 |
| crm-contacts | .169 | .578 | .8 |
| crm-deals | .029 | .232 | .6 |
| crm-stage | 0 | .019 | .6 |
| crm-commitments | .125 | .125 | .5 |
| crm-cost, s/message | 17.86 | 17.86 | 1.7 |

RESOLVE in the gold-mention setting (tune folds; B³ and CoNLL F1; "floor" uses no model, only declared
structure and domain-free similarity):

| system | floor B³ | best model arm B³ | floor CoNLL | adopted design |
|---|---|---|---|---|
| Ward | .654 | .659 (v5) | .436 | equals the floor |
| uv | .845 | .647 (v5) | .801 | equals the floor |
| GVC dev | .616 | .509 (v5) | .572 | .599 CoNLL, above the floor |

The adopted design (Ring 2a) asks the model only where its measured precision clears the type's bar. On
Ward and uv it never clears, so the model is never asked and the result equals the zero-model floor; on
GVC the model adds a little. Held-out folds, opened once, matched their floors. The diagnosis: the model
resolves one level coarser than the criterion (on GVC it joins a shooting's firing, injury and death; on
Ward 11 of 13 wrong links keep the counterparty and miss the deal), and shown a strong free signal (uv's
thread, right .83 of the time) it departs from it about 152 times and is right about 29.

uv-support end to end: no production read yet; the first build is running (2026-10-07). The zero-model
baseline it must beat is one case per thread, B³ .836, CEAF-e .645 on tune.

## Challenges

**The read is the binding constraint, and it has not been attacked directly.** Phase 1 produces a few
claims per chapter, not one per message or document, and often leaves declared attributes empty. Evidence
from three independent places: of Ward's 266 gold stage updates, 96 have no claim on their message and 103
claims carry no stage; RESOLVE's Ring 0 refusal reads "the read, not the argmax, is the limit"; and a
four-chapter trial on uv returned 2-3 case claims for thread chapters of 13-14 comments, some naming the
case by its section id. RESOLVE and derived attributes can only group what the read produces. We expect the
running uv build to place fewer than half of the 1,489 tune gold documents in any case.

**The model adds little to identity where structure is strong.** On uv, the thread alone is a better
identity signal than the model's choice; on Ward, the model confuses "same counterparty" with "same deal".
The current design handles this honestly (measured precision gates the model out) but it means the layer's
identity results on the two product examples are structure's results.

**Cost has had no work.** 17.86 s per message against a target of 1.7, ten times over, all of it Phase 1:
one generic seven-category sketch per section where the recipe needs only its declared types.

**The production interface hid defects until a full build was attempted.** Found 10-07 on uv: ingest
deduplicated chunks by text alone, dropping 369 of 2,954 documents (every "closed" or "labeled bug" event
after the first; 13-15% of gold documents); documents were keyed by url, and 475 shared one (timeline
events carrying the url of the issue they link to), so foreign events merged into an issue and took its
thread; the JSONL extractor stripped the record id, so the declared document id was unreadable; and a
chapter was a whole thread, up to 21,454 words over 234 documents in one model call. Earlier: a recipe edit
re-resolved nothing until the atlas was deleted by hand (fixed 10-05). All are fixed, but each surfaced
only at the end of a long path.

**Example gaps.** Ward's holdout has 69 deals, so one deal moves the bar by .014. Ward's "thread" stamp is
the Message-ID (mail threads were never reconstructed), so thread evidence carries nothing there. Gold
granularity for deals (164 records for 177 statements on one read) is still under discussion.

**Rate of results.** Pre-registration, refusals with data, three systems and a heavy gate suite make each
step trustworthy and slow. Four days produced well-documented negative results on RESOLVE and few bar
moves; the bar moves that did happen came from small structural changes.

## Open questions

1. **Should READ change shape?** Today it is one generic sketch per chapter with the declared types as an
   appendix. The alternative we lean toward: for a recipe that declares its types, read only those, once per
   document within its chapter, attributes required where the text states them. That targets stage,
   deals, commitments, cost and uv coverage at once. Is there a reason it would not, or a better shape
   (per-document closed questions, a two-pass read)?
2. **Is "the model reads, declared fields decide" too conservative for identity?** The model is evidence
   only and is gated out wherever its measured precision is below the bar, which is everywhere on our two
   product examples. Is the single-token forced-choice interface the right way to ask, or are we measuring
   the interface rather than the model?
3. **Is the domain-free invariant costing results a recipe could legitimately buy?** A maintainer's
   "Duplicate of #N" is a convention of that tracker. Today no code may match it; should a recipe be able to
   declare such a pattern, as it declares fields?
4. **How should uv be scored end to end?** The scorer computes B³ over documents the atlas places and
   reports unplaced ones separately. Should an unplaced document count as a singleton, so coverage is in
   the headline number?
5. **Is uv a good test of the layer?** One case per thread already scores B³ .836. A layer can look good
   there by not interfering with structure. Is the remaining 16% the right place to measure, or is uv mainly
   a regression guard?
6. **Is the third system worth its cost now?** GVC and ECB+ make "best in class" comparable to the
   literature, but they tripled instrument work before the second example had a production read.
7. **Is crm-cost (1.7 s per message on local hardware) reachable?** And by which path: a smaller model,
   fewer calls, a declared-types-only read, or structure first with the model only on what structure leaves?
8. **Is the process producing learning fast enough?** What would you cut to get more results per day
   without losing the guarantees that make the results believable?

## Where to look

`svrn/docs/specs/ONTOLOGY_METHOD.md` (the method), `svrn/docs/specs/ONTOLOGY_PRIMITIVES.md` §8 (derived
attributes), `research/ontology-apps/resolve-prereg.md` (every RESOLVE ring with its bars and verdict),
`research/ontology-apps/ward/recipe.toml` and `support/recipe.toml` (the two declarations),
`research/ontology-apps/ward/score.py` and `support/score.py` (the scorers),
`quality/campaigns/crm-proof.toml` (bars, floors and targets).
