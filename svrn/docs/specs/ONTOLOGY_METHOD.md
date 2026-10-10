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
structure proposes; records FOLD their statements into state through the protocol.

RESOLVE is GROUP and JOIN as one step. Novelty is not a primitive: it is RESOLVE answering "none of these".
A question that gives the model neither the criterion nor the candidates ("is this comment a different
problem?") is not RESOLVE, and it fails (uv-support spin-offs: precision .206, 4f705dede).

## What code may do

Code READS what the recipe declares, PROPOSES candidates from declared structure, VERIFIES what a model
points at is in the text, WEIGHS evidence by its precision estimated on the corpus, and FOLDS by the declared protocol. The
model reads declared fields and answers closed questions as distributions. Neither decides identity any other
way: no undeclared pattern (no phrase matcher for "duplicate of", no owner by domain, no thread from a subject
line), and no model verdict taken as a decision. Domain knowledge lives
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
(`oicp_types::forced_choice`), off a document prefilled once; where the one-pass choice cannot clear the bar,
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
checks on the default path); **3** the layer's other identity deciders onto it (about fifteen today, with
three ways of combining keys; begun: an entity or event type with a criterion and no source is RESOLVE's alone in the
atlas build, over the claims whose subject it is, `resolution_records.rs`). A precision is measured on the rule as it runs, never under gold alone: a field
follows whatever decision put its value in a record (uv thread .905 under gold, .71 live). That is why it is
estimated on the run itself, document by document in clock order, and checked against the labelled ratios
(layer-estimator) rather than declared.

## Reading (agreed 2026-10-08)

The reader asks; it never decides identity. A document is prefilled once with its declared facts (metadata
fields and the roles derived from them through declared sets, such as the sender's side; built 2026-10-10,
`document_read/prefill.rs`, first in every question about the document). Lines code classifies by structure
are never asked: a quote (its text, from first to last letter or digit, is a line of a document dated
earlier by the declared clock) and boilerplate (a line of another document by the same author, the field
`change.document.author` names); each class is counted and traced, and the classes hold no word of any
format (`document_read/line_classes.rs`). The rest is asked a
fixed plan of small closed questions generated from the contract — a pure function of it, unchanged by
renaming: **Locate** each declared claim kind (line sets or none; each kind shown with its closed-valued fields' declared
values and descriptions, the words a line states it in; the system prompt mentions values only when a kind
shows some), **Mention** each entity type a read
reference field targets and no exhaustive source covers, then per statement **Choose** each closed-valued
field (the subject's identity fields included) as a distribution over its declared values and "not stated",
**Pick** each reference field from candidates code proposes (identity keys derivable from the document's
fields, plus located mentions, less any declared exclusion set), and **Point** at each open-valued field. A
statement is one kind and at most three verified lines. Statements reach RESOLVE ungrouped. Every document of a selected section is read, whatever its
length: the body-word floor (`min_section_body_words`) guards the general extractor from heading-only book
sections and never applies to documents. A choice decides
by its argmax and carries its source and that source's precision (C3, `atlas/precision.rs`): `unmeasured`
until the corpus supports an estimate, because a text-reading source's precision is not identifiable without
labels (E2b, ecb58efcc), so every value a choice decided says it rests on an unmeasured read (amended by the
operator 2026-10-10; it said a choice decides only once measured, which would leave most values unknown). An answer code cannot verify refuses that
question alone. Answers assemble into the stored claims RESOLVE already reads; the `Asker` that answers takes
the daemon, a replay, or gold, so every question is an oracle at its own boundary (built 2026-10-09,
`enrichment/asker.rs`, `--asker` on extract and atlas-resolve: every answer recorded by its question's content,
and a replay with no daemon reproduces the run's records, C5). One reader, chosen
by the declaration (every claim kind readable, or the general extractor; a mix refuses): passes, built for Locate
and Choose; the one-shot reader was deleted on 2026-10-09 with no comparison run (operator: no build step gated on a
test made up for it). Why: every one-shot read
failed the same way — the local model asked to find, label, name and cite at once (stage ~43%, party
.24-.44, terse one-shot abstaining on half its documents; crm-proof loops 7-10c). Until Pick and Point are built, a
field they would answer stays empty, so `recipe validate` names each attribute's filler (a source, a derivation,
Choose from the reader's own plan, RESOLVE for a subject) and warns on what nothing fills and on any fold, protocol
or identity key keyed on it (`validate_layer.rs::fill_analysis`; blind round 2's protocol folds keyed on `step_id`
and `decision_ref` never folded, silently).

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
