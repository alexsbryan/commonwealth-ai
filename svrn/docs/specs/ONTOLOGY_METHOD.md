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

## The generality test

A piece belongs in the layer only if it is stated without a domain word and at least two examples exercise
it (crm-ward mail; uv-support issue cases, `research/ontology-apps/support/GOLD_SPEC.md`). Code naming a
type, attribute or state is domain-shaped; the declaration carries those. A code change needed to fit the
second example marks where the layer is still domain-shaped.

## Pieces agreed so far

- **A composed record is event coreference.** An act introduces a new particular or refers back to an open
  one (new or given). A given act resolves by anchor, then by matching description, then by salience within
  its conversation segment. The protocol limits legal moves.
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
code to find its ceiling; name the piece that could fill it; write bars before data; run once; adopt or
refuse (a refusal ships its data). Open the held-out fold once per adopted design.
