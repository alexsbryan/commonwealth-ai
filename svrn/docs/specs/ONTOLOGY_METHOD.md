# Building the ontology and extraction layer: method

Agreed with the operator 2026-10-05. Companion to `ONTOLOGY_PRIMITIVES.md`, whose axes this applies.

## The job

We build the ontology and extraction layer: raw input plus a declared ontology plus a patterned method
yields typed, cited records. Most SaaS applications are such records (a CRM's deals, a tracker's cases, a
register of rules in force), kept today through forms by people who resent the work. CRM and support
cases are examples we tune with, not the domain.

## First principles first, then the measure

Start from what a record is, not from the bar. Every record unfolds to three readers and a few operations:

- readers: a **field** (a header, a column), a **mention** (a name in a passage), an **act** (a speaker
  asking, offering, promising, accepting, declining, informing);
- operations: GROUP, FOLD through a protocol from a registry, JOIN, SELECT, AGGREGATE, CLOSURE, COMPUTE.

Before building a lever, name the first-principles piece it is; a lever that is none fits the test.

## The core (agreed 2026-10-05)

Records are declared types, each with an identity criterion (keys that suffice, and the criterion in the
author's words) and a protocol. Documents are read into cited statements; each statement is RESOLVED to an
open record or to none (which opens one) under its type's criterion, against candidates the declared
structure proposes; records FOLD their statements into state through the protocol. A record holds its documents:
those its statements are in, and each document read that states nothing of its type when the statements of its
declared thread all sit on that record (`resolution_derived/membership.rs`, added 2026-10-11: replayed on the
converge-s2 uv run with no model, our recipe's coverage .367 → .953 at B³ .898 → .915, and the share of documents
both authors place .232 → .816).

RESOLVE is GROUP and JOIN as one step. Novelty is not a primitive: it is RESOLVE answering "none of these".
A question that gives the model neither the criterion nor the candidates ("is this comment a different
problem?") is not RESOLVE, and it fails (uv-support spin-offs: precision .206, 4f705dede).

## What code may do

Code READS what the recipe declares, PROPOSES candidates from declared structure, VERIFIES what a model
points at is in the text, WEIGHS evidence by its precision estimated on the corpus, and FOLDS by the declared protocol. The
model reads declared fields and answers closed questions as distributions. Neither decides identity any other
way: no undeclared pattern (no phrase matcher for "duplicate of", no owner by domain, no thread code infers from
a subject line: a thread is a field the recipe declares, and the mail extractor offers two, the reply chain's root
`thread_id` and the RFC 5256 base subject `base_subject`, the one a mailbox with no reply headers can declare;
`recipe validate --corpus` warns when a declared thread links no two documents), and no model verdict taken as a
decision. Domain knowledge lives
in the recipe. Best-in-class results come through this architecture, not through knowing a domain.

## Invariants

1. **The code knows no domain.** Rename every type and attribute in a recipe; the composition is the same up
   to the renaming.
2. **Identity is decided by declared fields, in one place** (§Identity): a sufficient field that agrees links,
   a necessary value a declared field supplies that differs forbids (a read one is weighed), and evidential
   agreement links only where its posterior, at weights estimated on the corpus, clears the type's bar; the
   rest is unsettled and counted.
3. **Candidates come from declared structure** (the same thread, the same party, a declared reference) **or
   from generic, domain-free retrieval** (similar names, similar descriptions); a proposer never decides, and
   its recall (how often the right record is among the candidates) is reported.
4. **Every decision is traced and counted;** an answer code cannot verify (a passage not in the text, a
   distribution that does not parse) is counted as refused, never defaulted.
5. **Every change is read on all three systems,** stage by stage, model calls per document beside each
   stage; a change that makes a stage worse on any system is tuned or reverted on its own, never kept on
   another system's gain.

Six contracts every component meets, each a test on fixtures of mail, issues and news (agreed
2026-10-09): **C1** a model question asks only what declared structure leaves open, never a value a
declared field, key, reference or derivation supplies; **C2** a READ turn holds one document's text and
no other stored document's, and a RESOLVE turn may add each candidate's own statements' cited lines, marked
as quotes from other documents, since its answer is a choice and every cite is checked against the asked
statement's own document (narrowed 2026-10-10, option B); **C3** every value and link carries its source and that source's precision,
saying whether the precision is declared or estimated on the corpus; **C4** records are the same in any
document order; **C5** a run replays without the model, every answer recorded; **C6** is invariant 1. The
tests: `sovereign-enrichment-build/src/layer_contract_tests/contracts.rs`, over the default path's own code on
`tests/fixtures/layer/{mail,issues,news}` with a scripted oracle that reads no declared name. Meeting C2, a RESOLVE
question shows a candidate record by its counts, declared values and the lines its statements cite, as marked quotes,
never the rest of its documents' text (`answer::describe`); meeting C3,
each link, read field and derived value carries `by` (`atlas/precision.rs`: declared, estimated or unmeasured).

## Identity (agreed 2026-10-06)

The model reads; declared fields decide. A type's criterion names the fields that decide it, each sufficient
(equal: one particular), necessary (unequal: different particulars) or evidential, and READ fills them from the
text by pointing at the words. Code links on a sufficient field, never across a necessary one, and otherwise
only where the agreeing evidence (declared structure, domain-free similarity, read fields, the model's own
choice) clears the type's precision bar; each link is a `same_as` claim carrying what made it. Every model
question is a small closed one, answered in one forward pass as a distribution over single-token labels
(`oicp_types::forced_choice`); where the one-pass choice cannot clear the bar,
the model may reason first, and its answer is still read as that distribution, after its own reasoning, and
weighed as its own source (`reasoned_choice`; amended 2026-10-06 on ward's measurement). Why: shown a strong free signal,
the model's partition departed from it far more often than it was right to (`research/ontology-apps/resolve-prereg.md`),
while its verdict was informative and stable across the three systems (+1.4 to +1.8 nats for "same").

Built in rings, innermost first, each with bars before data: **0** one forced choice per statement (built,
`--answer select`; the argmax decides only so the choice can be measured); **1** READ by pointing and per-field
comparison (1a built: declared document fields weighed by code against `identity_bar`; 1b
built for closed sets: `identity_necessary` attributes READ as one forced choice over their `values`; the
model's choice and the proposed answer weighed as evidence); **2** the decider: constraints, calibrated
evidence, the unsettled band (built; since E2, 2026-10-10, every source is weighed at Fellegi-Sunter agreement
weights fitted by EM on the corpus being read, with no labels, `resolve_records/estimate.rs`: the recipe
declares no precision, and a necessary value forbids outright only when a declared field supplies it on both
sides, one a model chose (the reader's Choose or RESOLVE's READ) being one more weighed source, which C3's test
checks on the default path; E3, 2026-10-10: a statement held in its document is settled after the last one,
weighed again against the records it was held between at the whole corpus's weights with the model asked once
more over those only, linking at the bar, opening its own record when nothing raises one, and otherwise taken by
the most probable, so nothing is left held, `resolve_records/settle.rs`; amended 2026-10-12 by the operator's
delivery review: the model's forced choice decides every statement it was shown a record for, in its document and
at settling, and the weighed posterior is kept beside it, deciding only what no model was shown, since on Ward the
weights overrode 36 of its 39 picks of an offered company; amended 2026-10-14 from gold-judged RESOLVE questions on
five generated worlds and Ward: the question shows each side by what its readers read it to be, their supported
values of the type's other declared attributes (`resolve_records::Described`, shown and never compared; since
2026-10-15 also the names a window read gave it, so a subject no field describes is not shown as nothing, and a
record is called by the name most of its statements' readers gave it, a tie to the first met in fold order, and
keeps its opening passage as its first passage, never as its name, when a reader named it; 2026-10-16), and the
default path asks the reasoned choice, so where one token over the offered records joined 18 of 28 statements whose
own record was not offered, reasoning joins 4 of 27; with both, the model's own pick decided 49 of 57 such
questions, and the veto of 2026-10-13, a pick overruled when the weighed sources put none at the bar, 44, so it is
withdrawn); **3** the layer's other identity deciders onto it (about fifteen today, with
three ways of combining keys; begun: an entity or event type with a criterion and no source is RESOLVE's alone in the
atlas build, over the claims whose subject it is, `resolution_records.rs`). A precision is measured on the rule as it runs, never under gold alone: a field
follows whatever decision put its value in a record (uv thread .905 under gold, .71 live). That is why it is
estimated on the run itself, document by document in clock order, and checked against the labelled ratios
(layer-estimator) rather than declared.

**What the window reader bound is RESOLVE's to decide (agreed 2026-10-17, "lean D").** A statement is placed
right when a handle unique to its particular comes with it, its own full description or a thread no sibling shares
(~1% wrong), and wrong 6.5-7.5% without one; 78 of 106 rename-free identity errors were the window reader's binding
across documents, which linked on the window's reference without a question, and a record wholly of the window's
other particulars was never offered (`research/ontology-apps/diagnosis/README.md`). Amended: the window's reference
is sufficient within one document and evidence across documents, so a statement bound across documents is asked,
its window-mate's record offered first and marked as the reader's reading; a record of the window's other particulars
is offered, marked, not hidden. Next, in its own step: after the pick, one forced choice over the protocol fold's own
moves, shown the chosen record's state so far (moves it, acts under it without moving it, ends it because another
record replaces it, resumes it), applied by the fold, so the supersession line of `window_prompt.md` can go (taken
instead as statement kinds the reader writes, §Lifecycle, 2026-10-18); then,
deferred to a decision on the margin, an unsettled statement stays held and surfaced instead of forced at settling.
Windows stay independent, every answer is recorded, and the change is judged by replaying saved reads with only
RESOLVE re-asked (`research/ontology-apps/lean-d-prereg.md`). The two-pass re-read with records in view is the next
ring out if this does not move the cells. Step 1 landed in 446136f33 and is reverted: its replay fails bar 2 by one
deal, a rollover the model read as the new deal once asked; the inner ring's current reads hold one error of the kind it
targets, and the latest reads' misses are on deals whose history changes, broken in the reader before RESOLVE
(the prereg's Result; `research/ontology-apps/diagnosis/README.md`).

## Reading (agreed 2026-10-08; the window reader since 2026-10-15)

The reader asks; it never decides identity. There is one reader, chosen by the declaration (every claim kind
readable, or the general extractor; a mix refuses): the window reader (`document_read/window.rs`, prompt
`window_prompt.md`). The passes reader before it, a fixed plan of small closed questions per document, was deleted
on 2026-10-16 after the window reader served more through the default commands (DEFAULTS_LEDGER); the one-shot
reader before that, on 2026-10-09.

`enrich init` cuts the corpus into windows: every document in declared-clock order, cut by size where a document
ends; a document longer than a window (a novel, a long report) is cut where the recipe's chunker cut it, its parts
read in their own windows and its statements anchored in the whole, so prompt and reasoned answer fit 128k
(`sovereign-enrichment-build/src/window_manifest.rs`). A call writes about as many statements whatever it is shown, so
the window's size decides how much of a document becomes statements, and with it how often two reads, and two
recipes' reads, agree: at 20,000 characters as often as the one-shot agrees with itself (`determinism-prereg.md`); a recipe may set its own (`window_chars`, 2,000-40,000). One call reads a window, reasoning before it answers,
prompted from the contract alone, a pure function of it, unchanged by renaming: the ontology rendered (each
statement kind with its subject type and fields, each field's family and closed values), then each document under
its label with its declared facts above its text: the fields the declaration stamps or its metadata sources read,
and as `Named here:` the records those sources name of each type a read reference targets (`window.rs::compose`,
`pick.rs::candidates`).

What a document shows as its own is decided by structure, never by a format word (`document_read/line_classes.rs`).
A line it carries from one dated earlier by the declared clock is not its own: it lies in a passage, two
neighbouring lines that document holds in the same order, or it is that document's line under a mark the line was
not written under there (compared from first to last letter or digit). Repetition alone shows nothing (amended
2026-10-10 on the outside review, note 2347f4c6): a tracker's act and an author's greeting repeat and each is read,
and who wrote a line suppresses nothing. Nor is a line before the extractor's `body_start` (a header block, whose
facts are declared) or inside a segment the extractor marks as an embedded message: the format knowledge stays in
the extractor (amended 2026-10-12). An embedded message is a document of its own, dated and authored by its own
header, so every line has one owner and is read once, under the message that wrote it (amended 2026-10-17: until
then an embedded message no stored document held was read nowhere). In place of each run of quoted lines the window shows a pointer to the earlier
document (`[quoted from d3]`, or its date outside the window), so a reply's referent is read where it was written
and the quoted text stays unshown and uncitable (2026-10-15); code refuses a claim whose quote is in its document's
quoted or embedded lines and not its own (2026-10-16), so no prompt line carries that guarantee. Every document of a
selected section is read, whatever its length: the body-word floor (`min_section_body_words`) guards the general
extractor only.

The answer is typed: particulars, each named once across the window with its type, name and identifying fields,
and claims, each of a declared kind, about a particular, citing its document and exact words, with its fields. One
reading rule is the method's own, not a recipe's: a document that says one particular is replaced by, rolled into or
superseded by another speaks of two, and states the first's end beside the second's beginning (supersession). A
thing a document says has another name is a typed field, not a prompt rule (2026-10-16): a particular lists its
`other_names`, and code reads a reference, a subject's sourced identity and an unstated particular's line by any of
them, carries them on each statement (`OTHER_NAMES_ATTRIBUTE`) for RESOLVE to show and never compare, and lands a
reference reading on a record by any (`read_fields.rs::record_read`). No name is split. Who asserts a statement is
likewise a typed fact on every claim, never a condition of writing it (2026-10-16, note 14546b4d: readers dropped
narrated and unnamed-voice statements, a third of their drop lines on a novel): `said_by` is the document's own
words for who says it, or its own voice; and a kind that declares grades gets one of them (`window_voice.rs`). It
offers no particular to cite: offered one, a mail reader minted a person per sender and crowded out the deals
(ward-story, 50 persons and 4 deals against 4 and 10; note 2dde9c77). A label written anyway reads as its name.

Code keeps the contract: every declared field answered or unknown with a reason; a closed value one of its declared
values; a reference a record the document's fields name, a particular's label, or words the document holds
(`window_refs.rs`); each field's evidence in the claim's own document; a claim whose words are not in its document
refused by validation; a voice words the document holds, else dropped with its claim kept (`speaker_in_body`), kept
on the claim (`SAID_BY_ATTRIBUTE`, shown by its row) and bound at build only to a sourced record (`attributed_to`);
a grade one of its kind's, by the general extractor's rule (`validated_choice`). A malformed value costs the
particular or claim holding it, counted, never the window
(`window_answer.rs`). A particular no claim is about reaches RESOLVE as a statement of its own, a claim of no kind
citing the line that names it, so the document is a member of its record. A state or relation whose holder RESOLVE
decides is read as a statement about it (2026-10-15): a state carries its value, a field named for the state type,
and a relation its other endpoint, `to` (`OntologyTypeDecl::implied_fields`); after RESOLVE each is also a State or
Relation atom beside its claim (`resolution_records/statement_atoms.rs`).

A window's particular reaches RESOLVE as a sufficient key over its chapter (`CHAPTER_SUBJECT_KEY`), so RESOLVE
decides identity only across windows, statement by statement against bounded candidates. Two particulars the reader
read in one document stay two: a record holding one is never offered the other (`apart.rs::read_beside`), a
guarantee code makes with no question line restating it (2026-10-16). A reading of a subject's field stays on its
claim, cited, never on the record (`SUBJECT_FIELDS_ATTRIBUTE`). A read reference holds a record or nothing: the
record the document's fields name, else the one RESOLVE put the same document's statements read under that name on,
else none, the reading staying on the derivation line, cited (`read_fields.rs`, `statement_refs.rs`). Every read
value carries its source, `reader_window`, at precision `unmeasured` (C3, `atlas/precision.rs`): a text-reading
source's precision is not identifiable without labels (E2b, ecb58efcc). The `Asker` answers every question from the
daemon, a replay or gold, so every question is an oracle at its own boundary (`enrichment/asker.rs`, `--asker` on
extract and atlas-resolve: every answer recorded by its question's content and stamped with its model; a replay with
no daemon reproduces the run's records, C5; `recorded` reuses what the same model answered and asks only the rest).

`recipe validate` names each declared type the reader makes no record of (`document_read::unread_types`;
saltgrass's first blind recipe lost `vessel` and `condition` that way) and what the declaration asks that the reader
leaves inert (`validate_layer.rs::reader_inert`, 2026-10-15): argument reconstruction and voices (the general
extractor's alone), a pattern over an edge type that is no read relation kind, a table source, and a type that
specializes a decided one with no criterion of its own; for that reason the ten templates stay on the general
extractor. The reader sends no mention of a type RESOLVE decides from mentions (`decides_mentions`), so a company
named only in prose is not bound to the record its header seeds (DEFAULTS_LEDGER, known gap).

Why windows: the race of 2026-10-13 (REVIEW_BRIEF.md) read five generated mail worlds four ways with one hosted
model; windows of whole documents served 76/93 deals against the one-shot's 66/93, and on the local model windows
served 5/15 of world-s4's deals where the passes reader served 1/15. Through the default commands the window reader
then served 81/93 on the worlds and 9/14 on Ward, and graduated (2026-10-15).

## Lifecycle (agreed 2026-10-18)

A record whose state a protocol fold derives can end because another replaces it, and can be acted under without
changing state; neither is a stage the recipe lists, so a reader asked for one guesses. At 20k windows world-s1's
reader wrote nominations under won deals as leads and proposals and left a rollover's end unwritten when the deal's
history sat in another window; at 40k the same reader served them (`research/ontology-apps/diagnosis/README.md`).
The method supplies two statement kinds for such a record, `<type>_replaced` and `<type>_act_under`
(`corpus-engine/src/recipe_ontology/lifecycle.rs`); the reader reports which it read and the fold applies what it
means, so no prompt rule stands in for either (the window prompt's supersession line went with this). The defaults
are the sources': a replaced record ends, in the state the recipe's own rules give it (the replaced kind is a kind of
the fold's claim kind), as a rollover is a new contract with lineage and the old one ends (CDM, Conga renewals); an
act under a record, a nomination or a draw under a trade, changes nothing (CDM's reset). Where a recipe's world reads
otherwise its owner says so: `replaced` and `acts_under` pin the state each moves to, `lifecycle = false` drops both.
Pre-registered in `research/ontology-apps/lifecycle-prereg.md`.

## The generality test

A piece belongs in the layer only if it is stated without a domain word and the examples exercise it:
crm-ward mail; uv-support issue cases (`research/ontology-apps/support/GOLD_SPEC.md`); and a public
benchmark we did not label, so best in class is read against the literature: the Gun Violence Corpus
(cross-document event coreference, CC BY 4.0), with ECB+ as the literature anchor, each read twice — gold
mentions as statements (RESOLVE alone) and end to end. Code naming a type, attribute
or state is domain-shaped; the declaration carries those. A code change needed to fit one example marks
where the layer is still domain-shaped.

## Pieces agreed so far

- **A composed record is event coreference.** An act introduces a new particular or refers back to an open
  one (new or given), decided by RESOLVE: a declared anchor key settles it; otherwise a cited answer under
  the criterion does. Salience is evidence: it orders candidates and counts only for its measured precision. The protocol limits legal moves.
- **Participant is not mention.** The other side of a conversation comes from fields through derived roles.
- **An act in a record is not a report about it.** Who speaks to whom is part of the reading.
- **A role is a JOIN on stable keys** (employer: address domain to company). Dated, never identity.
- **Code enforces identity decoding.** What a model's answer names is never silently dropped or duplicated.

## The example checks the work

Classify each disappointing residual, after reading it, as an example gap or a method gap. Example gaps:
gold granularity, labeller blind spots, a bar that rewards splitting, a corpus unlike the target user, n
too small for a one-record delta. Method gaps: missing primitives, conflated concepts. Never price a
concept by its bar delta alone; read B-cubed and CEAF beside any bar a splitter can game, and measure the
reader's variance before trusting a small delta.

## The loop

Scaffold first (operator, 2026-10-09): the thinnest pipeline that runs read, resolve and fold through the
default commands on all three examples with the six contracts holding, whatever its quality. Then extrude
one refinement at a time inside it, where the stage ladders (read, place, fold, per example) show the
loss, structure before model. The ladders are one table from each run's own atlas and logs
(`research/ontology-apps/ladder.py`); GVC's aligns the reader's cited lines to gold's token mentions. A refinement is kept when its own tests and the contracts pass and no stage
gets worse without its residual read and classed; otherwise it is tuned or reverted alone, and the
scaffold stays. Bars are tuning targets read on those runs, not gates on a build step. Research drives the
core through recipes; it never reimplements the core with domain heuristics. Each example's own bars are
lagging checks. Open the held-out folds once, at the acceptance run.

Until 2026-10-09 the loop was one hypothesis per ring with an adopt rule on a table, and it rejected
structure it could not measure: Ring 2's decider was refused on a test where two of three examples had no
decision for it to change (`research/ontology-apps/resolve-prereg.md`, Ring 2 H1 and its correction).
