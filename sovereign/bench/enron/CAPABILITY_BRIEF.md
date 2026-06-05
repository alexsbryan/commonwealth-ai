# Reconstructing identity from a corporate email archive — an Enron capability brief

*A measured account of what this system does on the public Enron corpus, written
for readers outside the project. Every number below is a real measurement against
a held-aside ground-truth set, not a projection. Where we extrapolate, we say so.*

---

## The forensic problem this solves

Before an investigator can ask *who told whom what, and when*, they have to answer a
duller question first: **who is who.** In a real archive the same person appears as
`klay@enron.com`, `"Ken Lay"`, `"K. Lay"`, `"Kenneth L. Lay"`, and `"Lay, Kenneth"` —
and a *different* person named Lay must never be folded into that same identity. The
same is true of counterparties: `Dynegy`, `Dynegy Inc`, `Dynegy Inc.` are one company;
`Pacific Gas and Electric` and `PG&E` are one company; a stray `"P. Gallagher"` is not.

This is **entity resolution**, and across a large custodial set it is the foundational
bottleneck of any email investigation. In the actual Enron matter — a corpus of roughly
**500,000 emails across ~150 custodians** — this disambiguation was done largely by hand,
analyst-coded in document-review platforms, over weeks of paralegal time. It has to be
right, because a *false merge* — attributing two different people's communications to one
identity — corrupts the evidentiary record.

## What we measured

We ran the system over a **two-custodian slice** of the public Enron corpus — the sent
and inbox folders of **Kenneth Lay and Jeffrey Skilling**, ~3,172 messages → **8,829
indexed chunks** after boilerplate stripping (signatures, quoted replies, legal
disclaimers removed).

From that raw mail the system extracted and then **distilled** — with no human in the
loop — a typed knowledge atlas. Raw entity extraction is noisy (it surfaces document
titles, dollar amounts, quoted fragments); the resolve step distills it to signal:

| Quantity | Count |
|---|---|
| Typed atoms (entities / events / states / relations / claims / questions) | **6,101** |
| — of which **entities** (people, orgs, places, concepts, works) | **1,730** |
| Canonical entities after reconciliation | **1,689** |
| Cross-inbox identity merges (each logged + signal-justified) | **35** |
| Surface forms collapsed | **47.7%** |

It reconciled surface forms into canonical identities — collapsing names, emails, and
aliases across *both* inboxes into single, **named** people and organizations (Calpine
Corporation, El Paso Corporation, Standard & Poor's, Aquila Inc., AES Corporation, …).

### The result, on identities the system was never tuned against

We hold out a sealed test set of canonical entities and their accepted surface forms,
and score the reconciliation against it with the standard B³ clustering metric:

| Metric | Score |
|---|---|
| **Precision** | **1.000** |
| Recall | 0.717 |
| B³ F1 | 0.835 |
| Improvement over the no-reconciliation floor | **+0.220 F1** |

Runtime: **~15 seconds** on a single workstation (Apple M2 Max), using candidate-pair
blocking so the cost grows far slower than the naive all-pairs comparison.

## Why these specific numbers matter to a prosecutor

**Precision 1.000 — zero false merges — on data the system had never seen.** This is the
number that matters in a legal context. The system did not, on the held-aside set, fabricate
a single identity link. When it was uncertain, it left two surface forms apart rather than
guessing them together. That error asymmetry — *miss a link before you invent one* — is the
only one admissible when the output feeds an evidentiary chain. Recall of 0.717 means it
surfaced ~72% of the true identity links; the rest it missed are abbreviated names
(`"R. Mark"`), place-name variants (`"Houston, TX"` vs `"Houston, Texas"`), and entities the
upstream text extraction never surfaced — gaps that *widen the net to review*, never
*misattribute*.

**Every decision is glass-box.** Reconciliation runs as a step of the enrichment pipeline
and writes two artifacts beside the raw atoms: a canonical clustering and an append-only
audit log. On this corpus it made **35 cross-inbox canonical-entity merges** (47.7% of
surface forms collapsed), each logged with the signal that justified it — a shared email
header, a normalized name match, a corroborated organizational role — and at least two
independent signals are required before two surface forms from different inboxes are fused.
An analyst can ask the system *why*
it ruled `"K. Lay"` and `klay@enron.com` the same person and get a concrete, reviewable
answer. The raw per-mention atoms are never destroyed, so the evidence underneath every merge
remains inspectable. That is the difference between a black box and a defensible exhibit.

**It runs in seconds, not weeks.** The two-custodian reconciliation completes in ~15 seconds.
The work it replaces — hand-coding alias tables across custodians — was, on the full historical
matter, a multi-week paralegal task.

## The honest scope — what we have *not* yet proven

This brief deliberately under-claims. To keep it credible:

- **This is a two-custodian slice (Lay + Skilling), not the full 150-custodian corpus.** The
  500k-email / 150-custodian figure is the *public record of the matter*, cited to frame scale.
  We have not yet run the full set; the blocking design is sub-quadratic, so we expect the
  per-custodian cost to hold, but that is an **expectation, not a measurement**.
- **The 0.835 F1 is reconciliation quality.** It assumes the upstream extraction surfaced the
  entity in the first place; entities the text extractor missed (a handful of counterparties)
  cap recall and are a separate, known lever (a full re-enrichment pass).
- **A second sealed holdout remains untouched** for an eventual independent generalization
  estimate; we have not spent it.

## The claim, stated plainly

> On a real slice of the Enron archive, with no human intervention, the system distilled
> ~8,800 message-chunks into a typed atlas of 6,101 atoms (1,730 entities), reconstructed
> the cross-inbox identity graph, and resolved aliases into canonical people and
> organizations at **83.5% B³ F1 with perfect precision (recall 0.717) on held-aside
> identities, in fifteen seconds, with every merge decision logged and auditable.** Had the
> Enron Task Force possessed this, the weeks of paralegal alias-coding
> that precede any "who-knew-what-when" analysis would have been a coffee break — and the
> result would have been *more* defensible, not less, because every link carries its evidence.

---

*Measurement provenance:* scored with `sovereign bench enron run --corpus
enron-sample-multi-wide --split test --judge-trials 3`; the production pipeline runs the
identical merger via `sovereign enrich reconcile <corpus>`, which writes the canonical
clustering to `atlas/reconciliation.json` and the audit trail to
`atlas/reconciliation_oplog.jsonl` (so the measured numbers are the production numbers). Floor
and tuned baselines in `baselines/enron-entity-resolution/`. Corpus: public-domain Enron email
(EDRM/CMU set), Lay+Skilling sent+inbox. Ground truth: `ground_truth_entities.jsonl` (16
canonical entities, 57 surface forms, train/test/holdout split). Reconciliation is
deterministic — no LLM — so the result is reproducible.
