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
points at is in the text, WEIGHS evidence by its measured precision, and FOLDS by the declared protocol. The
model reads declared fields and answers closed questions as distributions. Neither decides identity any other
way: no undeclared pattern (no phrase matcher for "duplicate of", no owner by domain, no thread from a subject
line), and no model verdict taken as a decision. Domain knowledge lives
in the recipe. Best-in-class results come through this architecture, not through knowing a domain.

## Invariants

1. **The code knows no domain.** Rename every type and attribute in a recipe; the composition is the same up
   to the renaming.
2. **Identity is decided by declared fields, in one place** (§Identity): a sufficient field that agrees links,
   a necessary field that differs forbids, and evidential agreement links only where its measured precision
   clears the type's bar; the rest is unsettled and counted.
3. **Candidates come from declared structure** (the same thread, the same party, a declared reference) **or
   from generic, domain-free retrieval** (similar names, similar descriptions); a proposer never decides, and
   its recall (how often the right record is among the candidates) is reported.
4. **Every decision is traced and counted;** an answer code cannot verify (a passage not in the text, a
   distribution that does not parse) is counted as refused, never defaulted.
5. **One table per loop over three different systems,** model calls per document beside the measures; a
   change is adopted only if no example regresses.

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
comparison (1a built: declared document fields weighed by code, `identity_evidential` and `identity_bar`; 1b
built for closed sets: `identity_necessary` attributes READ as one forced choice over their `values`, a
differing value forbidding the candidate; the model's choice and the proposed answer weighed as evidence at
their measured precision); **2** the decider: constraints, calibrated
evidence, the unsettled band; **3** the layer's other identity deciders onto it (about fifteen today, with
three ways of combining keys). A precision is measured on the rule as it runs, never under gold alone: a field
follows whatever decision put its value in a record (uv thread .905 under gold, .71 live).

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

Instrument every decision; judge each against gold; let gold decide one class at a time through the same
code to find its ceiling; name the piece that could fill it; write bars before data; run once on every
example's tune fold, one table (B-cubed, CEAF-e, LEA, state); adopt or refuse (a refusal ships its data).
Research drives the core through recipes; it never reimplements the core with domain heuristics. Each
example's own bars are lagging checks. Open the held-out folds once per adopted design.
