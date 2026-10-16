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
(`oicp_types::forced_choice`), off a document prefilled once (questions that need nothing from one another go
as one wave, `forced_choice::wave`, each answered as its single request and recorded under its key); where the one-pass choice cannot clear the bar,
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
record is called by its earliest read name, never by its opening passage when a reader named it), and the
default path asks the reasoned choice, so where one token over the offered records joined 18 of 28 statements whose
own record was not offered, reasoning joins 4 of 27; with both, the model's own pick decided 49 of 57 such
questions, and the veto of 2026-10-13, a pick overruled when the weighed sources put none at the bar, 44, so it is
withdrawn); **3** the layer's other identity deciders onto it (about fifteen today, with
three ways of combining keys; begun: an entity or event type with a criterion and no source is RESOLVE's alone in the
atlas build, over the claims whose subject it is, `resolution_records.rs`). A precision is measured on the rule as it runs, never under gold alone: a field
follows whatever decision put its value in a record (uv thread .905 under gold, .71 live). That is why it is
estimated on the run itself, document by document in clock order, and checked against the labelled ratios
(layer-estimator) rather than declared.

## Reading (agreed 2026-10-08)

The reader asks; it never decides identity. A document is prefilled once with its declared facts (metadata
fields and the roles derived from them through declared sets, such as the sender's side; built 2026-10-10,
`document_read/prefill.rs`, first in every question about the document). A line a document carries from one
dated earlier by the declared clock is never asked, and structure must show it is carried: it lies in a
passage, two neighbouring lines that document holds in the same order, or it is that document's line under a
mark the line was not written under there (its text compared from first to last letter or digit). Repetition
alone shows nothing (amended 2026-10-10 on the outside review, note 2347f4c6): a tracker's act and an
author's greeting repeat in the same words and each is read, and who wrote a line suppresses nothing, so a
passage an author sends twice is read where it first appears. Each class is counted and traced, and the
classes hold no word of any format (`document_read/line_classes.rs`). Nor is a line before the extractor's
`body_start` (a header block, whose facts are declared) or inside a segment the extractor marks as an embedded
message (what it says is that message's, dated and authored by its own header): the format knowledge stays in
the extractor that writes those offsets into the document's metadata (amended 2026-10-12, delivery slice 1). The window
reader shows a pointer to the earlier document in place of each run of quoted lines (`[quotes d3]`, or that
document's date when it is outside the window), so a reply's referent is read where it was written, and the quoted
text stays unshown and uncitable (2026-10-15): code refuses a claim whose quote is in its document's quoted or
embedded lines and not its own (2026-10-16), so no prompt line carries that guarantee. The rest is asked a
fixed plan of small closed questions generated from the contract — a pure function of it, unchanged by
renaming: **Locate** each declared claim kind no protocol rule names (line sets or none; each kind shown with its closed-valued fields' declared
values and descriptions, the words a line states it in; the system prompt mentions values only when a kind
shows some), **Act** each kind a protocol rule names whose subject RESOLVE decides, asked of the whole document and
never of a line (on the lines that carry a stage the model answered "none" at .84-.99, REVIEW_BRIEF §4.2): per
particular About found, named by its line and words when it found more than one, and once otherwise, so About never
gates it; each field the kind's rules qualify on (`when`) is one forced choice over its values in two wordings off
one wave under a system prompt naming the type and field, a disagreement held with no statement; an agreed value is
cited by one forced choice among the document's sentence units that touch asked lines (a paragraph's lines joined
and cut after a stop before whitespace; labels A-Y and no "none", since presence is decided; one candidate cited by
code), and the statement is the lines that sentence touches (`act.rs`, `locate.rs`; built 2026-10-12, cite-probe M2
and its no-none amendment), **Mention** each entity type a read
reference field targets and no exhaustive source covers (a table source is exhaustive; a metadata source names
only who took part, so it is not), one choice per asked line then pointed at, a proposal for Pick (`mention.rs`);
of a type with an `identity_criterion` and a metadata source, each mention Pick would offer by its words is also a
statement RESOLVE decides, its words on its line, reading no field, and RESOLVE starts from the source's records
under their own ids, each offered to the mentions of the documents whose fields name it
(`resolution_records::decides_mentions`, `resolution_records/mentions.rs`; built 2026-10-12, cast instance 1), then per statement, each question opening with the declared facts and the whole document
numbered as Locate shows it, the statement named by its lines (never a window around it; `ask.rs` `opening`,
`statement_words`), **Choose** each closed-valued
field (the subject's identity fields included) as a distribution over its declared values and "not stated",
**Pick** each reference field from candidates code proposes (the records the referenced type's metadata source
reads from this document's fields, plus this document's mentions that name no listed record, less any record in
a declared exclusion set, a set a declared path drops by, and any mention naming such a record; `pick.rs`; a record
folding the field holds the record a pick names or nothing, never a mention's words and never a record minted from
them: a mention's reading holds the record RESOLVE put the same document's mention of those words on when the
type's mentions are decided, or else the record RESOLVE put the same document's statements a window read under that
name on (2026-10-15), and otherwise stays on the derivation line as a cited unresolved reading; a held record
no exclusion set can judge (a condition's attribute absent) is served and marked `unjudged` on its line (operator,
2026-10-12), `read_fields.rs`; a statement's own reference field, a relation's `to` among them, holds its record by
the same rule, `statement_refs.rs`, 2026-10-15), and Pick the subject whose type has a metadata source
among the records that source reads from this document, its answer written as the subject's identity readings
(the statement's words rarely write the address that identifies a person: of Ward's 64 commitments, 59 pointed at no
word and none reached its person, 2026-10-11), and **Point** at each open-valued field
(text, quantity, time): its start word, then its end word, among the statement's own words shown under labels,
"not stated" when no start is chosen; the value is read out of the words by the declared family, a quantity as
the one number they write in digits (refused when they hold none or several), a time as ISO 8601 when code reads
a date and as written otherwise (`point.rs`). A
located statement is one kind and at most three verified lines. **About** asks, per document and per type RESOLVE
decides (a statement kind about it or not, 2026-10-15), whether the document is about none, one or more than one particular of it (the type's description and
criterion shown), points at where each is first named or described (a line, then its words; at most five), and with
more than one asks each statement of the type which it is about (`about.rs`); a statement's local reference names
its particular, never its span, so the statements about one particular reach RESOLVE as one (`local_subjects.rs`)
and its identifying fields are read once, at its words; a statement about none stays its own. Whether a document is
about a particular is asked apart from whether any line states a claim kind: a particular no statement is about
reaches RESOLVE as a statement of its own, a claim of no kind citing the line that first names it, so the document
is a member of its record (built 2026-10-12, repair 2 of the outside review). Every document of a selected section is read, whatever its
length: the body-word floor (`min_section_body_words`) guards the general extractor from heading-only book
sections and never applies to documents. A choice decides
by its argmax and carries its source and that source's precision (C3, `atlas/precision.rs`): `unmeasured`
until the corpus supports an estimate, because a text-reading source's precision is not identifiable without
labels (E2b, ecb58efcc), so every value a choice decided says it rests on an unmeasured read (amended by the
operator 2026-10-10; it said a choice decides only once measured, which would leave most values unknown). An answer code cannot verify refuses that
question alone. Answers assemble into the stored claims RESOLVE already reads, a reading of the subject's field
beside the claim's own, cited, and never onto the record (`SUBJECT_FIELDS_ATTRIBUTE`); the `Asker` that answers takes
the daemon, a replay, or gold, so every question is an oracle at its own boundary (built 2026-10-09,
`enrichment/asker.rs`, `--asker` on extract and atlas-resolve: every answer recorded by its question's content
and stamped with the model that gave it, and a replay with no daemon reproduces the run's records, C5; `recorded`
reuses what the same model answered and asks only the rest, 2026-10-12). One reader, chosen
by the declaration (every claim kind readable, or the general extractor; a mix refuses): passes, every pass built
(Locate and Choose 2026-10-09; prefill, line classes, Mention, Pick and Point 2026-10-10, order
ontology-layer-15; About and Act 2026-10-12); the one-shot reader was deleted on 2026-10-09 with no comparison run (operator: no build step gated on a
test made up for it). Why: every one-shot read
failed the same way — the local model asked to find, label, name and cite at once (stage ~43%, party
.24-.44, terse one-shot abstaining on half its documents; crm-proof loops 7-10c). `recipe validate` names each attribute's filler (a
source, a derivation, Act, Choose, Point or Pick from the reader's own plan with Pick's candidates, RESOLVE for a
subject) and warns on what nothing fills (a type no read claim kind is about, a closed set wider than one
forced choice, a reference to a type only a table holds) and on any fold, protocol or identity key keyed on it (`validate_layer.rs::fill_analysis`; blind round 2's protocol folds keyed on `step_id`
and `decision_ref` never folded, silently). It names each declared type the reader makes no record of: a state or
relation whose holder RESOLVE cannot decide, or an entity with no criterion no statement is about and no source supplies
(`document_read::unread_types`; saltgrass's first blind recipe lost `vessel` and `condition` that way, silently). It
also names what the declaration asks that the reader leaves inert (`validate_layer.rs::reader_inert`, 2026-10-15):
argument reconstruction and voices (the general extractor's alone), a pattern over an edge type that is no read
relation kind (patterns walk relation atoms, which the reader makes of read relation statements only), a table source (no stage reads one yet) and a type that specializes a decided one with no criterion
of its own (RESOLVE inherits none); for that reason the ten templates stay on the general extractor. A
state or relation whose holder RESOLVE decides is read as a statement about it (2026-10-15): a state carries its value,
a field named for the state type, and a relation its other endpoint, `to` (`OntologyTypeDecl::implied_fields`), and
both are served as claims of their type. After RESOLVE and the reference derive each is also a State or Relation
atom beside its claim, one per statement, so trajectories, configurations and patterns read them as they read an
extracted one (`resolution_records/statement_atoms.rs`, 2026-10-15; holders' trajectories chained by
`resolution/trajectories.rs`, the pass 3b runs).

**The window reader (2026-10-13; the default reader since 2026-10-15, DEFAULTS_LEDGER; the passes reader is
`SOVEREIGN_READER=passes`).** The race of
2026-10-13 (REVIEW_BRIEF.md) read five generated mail worlds four ways with one hosted model: reading windows
of whole documents served 76/93 deals against the one-shot's 66/93, windows cut by date tied windows cut by
folder (78/93 against 76/93), and on the local model windows served 5/15 of world-s4's deals where this
pipeline served 1/15. So a second reader
is measured beside the passes reader through the default commands. `enrich init` cuts the corpus into
windows (every document in declared-clock order, cut by size where a document ends; a document longer than a
window, a novel or a long report, is cut where the recipe's chunker cut it, its parts read in their own windows and
its statements anchored in the whole; so prompt and reasoned answer fit 128k), and one call reads a window, reasoning before it answers, prompted from the contract alone
(`window.rs`, `window_prompt.md`). One reading rule is the method's own, not a recipe's: a document that says
one particular is replaced by, rolled into or superseded by another speaks of two, and states the first's end
beside the second's beginning (supersession). A thing a document says has another name is not a prompt
rule but a typed field of the answer (2026-10-16): a particular lists its `other_names`, and code reads a
reference, a subject's sourced identity and an unstated particular's line by any of its names, carries the
other names of what each statement names (`OTHER_NAMES_ATTRIBUTE`) for RESOLVE to show beside it, never to
compare, and lands a reference reading on a record by any of them (`read_fields.rs::record_read`). No name is
split. The
model names each particular once across the window and cites each claim by its document and exact words;
code keeps the contract: every declared field answered or unknown with a reason, a closed value one of its
declared values, a reference a record the document's fields name or words it holds, each field's evidence in
the claim's own document, and a claim whose words are not in its document refused by validation. A malformed
value in the answer costs the particular or claim that holds it, counted, never the window. A window's
particular reaches RESOLVE as a sufficient key over its chapter (`CHAPTER_SUBJECT_KEY`), so RESOLVE decides
identity only across windows, statement by statement against bounded candidates. Two particulars the reader read
in one document stay two: a record holding one is never offered the other (`apart.rs::read_beside`), whether
it was opened before the document or earlier in it (weighed by every particular that has joined it), and each
one's question shows the other as read, so the first asked can leave the record that fits the other; whether
particulars of different documents are one is RESOLVE's to decide, and once it lets two share a record, each
particular's later statements follow it there by key. If it wins through the
default commands it becomes the one reader and the passes and waves readers are deleted (operator,
2026-10-13); if it loses, it is deleted.

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
