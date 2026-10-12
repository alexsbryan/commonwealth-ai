# Cast probe: pre-registration, 2026-10-12, before any number is computed

Question. Identity across documents is decided by RESOLVE alone, offered candidates by two proposers (the declared
thread; TF-IDF over whole documents, `resolve_records/propose.rs`), and no stage keeps a register of particulars
(the cast: each type's records with the words that name them). Invariant 3 asks that a proposer's recall be
reported; none is. This probe measures, per declared type RESOLVE decides, on all six runs/about legs (ours and
blind, Ward, uv, GVC): whether the cast has the particular, whether the right record is put in front of the decider,
and whether the decider takes it; then replays generic proposers the cast would add, with no model call. It decides
which proposers the cast is built from and gives the party slice (ec, in build) its link-step instrument.

Units and gold, the ladders' own (`ladder.py` placements, one run each, runs/about): Ward, a tune message in exactly
one gold deal (49); uv, a tune gold document and its case; GVC, a gold event mention and its chain (statements
mapped to mentions by the GVC ladder's alignment of cited lines). Blind legs read gold through the evaluator's frozen
map (`blind-author/<tag>-mapping.json`). A type gold does not label (blind-gvc `involved_person`) is could-not-judge.

Order. RESOLVE's own document order, read from `resolve_decisions.jsonl`. A unit is *given* when an earlier unit of
the same gold particular already sits on a record; the *right records* are those holding an earlier unit of it.

Measures, per leg and type.
- R1 inventory: the share of gold particulars with a record (the ladder's one-to-one match).
- R2 proposal recall: of given units, the share whose document was offered a right record (the logged candidates).
- R3 decision given proposal: of given units offered a right record, the share whose statement went to one; beside
  it, the share where the model's argmax was a right record, so a weigher override is visible.
- Novelty: of units not given, the share that opened a record (R3's other half: a first naming must open).
- R2' replayed proposers, each a ranking of the records open before the unit's document, recall at 1, 3 and 10:
  N name: the words About and Mention pointed at in the unit's document, matched to the words pointed at in each
  record's documents (normalised exact, then shared-token share); E embedding: the production embedder
  (qwen-embedding-0.6b, cite-probe gates) over the unit's pointed words plus its statement's cited sentence, against
  each record's, max cosine; D declared reference: records holding a document the unit's document names by a
  declared field (uv `target_url`; Ward `base_subject` as the extractor's second thread; GVC declares none, so D is
  never-ran there); and the union with the logged candidates. Mean candidates shown, beside each.

Bars, fixed now.
1. A replayed proposer is worth building when, at recall at 10, its union with the logged candidates raises R2 by
   at least 10 points on at least two of the three systems (either author), and lowers it nowhere (it cannot: a
   union) while adding at most 10 candidates per unit on average.
2. The cast is the bottleneck where R2 is below .80 on a leg; the decider is where R2 is at least .80 and R3 below
   .80. Both are read per leg, so the slice that is built (proposers, decider, or both) is the one the leg shows.
Reported, no bar: R1, novelty, the weigher-override share, per type. One run each, so a difference of one or two
units is inside run-to-run churn (measured 2026-10-08 at about 10 items each way on Ward's stage).

## Amendment 1 (2026-10-12, after the RESOLVE-type section was scored, before this one is computed): company mentions

The party slice (ec, in build) makes a body-only company mention resolve to or open a company record. Its link step
needs Ward's recall before bars are set. Units: a Ward message (ours and blind runs/about legs) in exactly one gold
deal whose counterparty is a gold company C. A record resolves to C by the scorer's one rule (`ward/score.py`
company_resolver: a shared domain, else equal written forms under name_core), imported, never restated.
- F field-named (ec): the company records the message's own from/to/cc domains name. Hit: one is C.
- M mentioned: some Mention pick in the message names C (its words resolve to C by the same rule).
- N, E link step, for units whose message has a mention naming C and some earlier message (RESOLVE's order, else
  date) already offers a record of C: a simulated cast of company records open before the message (each header
  domain one record, each earlier mention's name_core one record), ranked by name token share (N) and by the
  production embedder (E) against the mention's words; recall at 1, 3, 10.
- F or M: the message offers C by either path (the slice's candidate ceiling at Pick).
- Exclusion: mentions resolving to a company of the declared `ours` set: the share F offers that company's header
  record for (so the mention lands on it and is dropped).
No bar here: the slice's bars are ec's, set on these numbers.

## Amendment 2 (2026-10-12, before any replay is computed): the estimator's document-field fit

Reading after the cast section was scored (no model; cast_probe.py vetoes): on GVC every right model pick RESOLVE
did not link is across articles, and the source disagreeing on it is `document_date` in 17 of 17 (ours) and 17 of
19 (blind). The fitted rates say a match shares its document's date 95% (ours) and 87% (blind) of the time, so a
different date outweighs the model's agreement. EM fits every statement-alternative pair, and a pair whose
statement's document is already one of the record's documents agrees on every document field by construction.

Correction C, one change: a source whose value is a document field (RESOLVE's `document_*` sources: date, id,
thread) does not speak on a pair whose statement's document is already one of the alternative record's documents.
Such a pair leaves that source out of the fit and out of its evidence. Nothing else changes.

Replay, first order, no model: each statement's alternatives and comparisons as the trace logged them (`atlas/resolve:
the sources weighed`), the record's documents as RESOLVE's own decisions built them; EM refitted after each document
on the pairs so far, ported line for line from `resolve_records/estimate.rs`; zones from `weigh.rs` (no bar: the
most probable of the alternatives and none, an alternative linking only if some source agreed with it).
- G1: the port reproduces every logged fit before C (each source's m and u within 1e-6).
- G2: the port reproduces the logged zone of at least 99% of weighed statements before C.
A failed gate makes the leg could-not-judge.

Measures per leg, before and after C: links made, each right (the alternative record holds an earlier unit of one
of the statement's gold particulars), wrong (it holds none, the statement having a gold particular), or unjudged
(no gold particular); R3 as the cast section reads it.
Bars, fixed now: C goes to build when on both GVC legs right links gained are at least 10 and at least twice the
wrong links gained, and on every Ward and uv leg right links lost are at most 2 and wrong links gained at most 2.
First order: later candidates and records would differ after a changed decision; the build's end-to-end leg is the
test of record.

## Amendment 3 (2026-10-12, before runs/party exists): the party leg's company decisions, and an overlap source

ec's party slice (919d4fc6b) makes each company mention a particular, offered the header records its message's
fields name (FieldNamed). Its leg (runs/party/ours/ward-party) is the first record of those decisions. Read, from its
recorded resolve_decisions.jsonl and trace, before any build:
- Units: company statements whose words resolve to a gold company C (the Ward scorer's rule, or a shared token or
  domain stem with C's forms, reported apart); the right record is a seeded header record whose domain is one of
  C's. Enron mentions (C in the declared `ours` set) are counted apart.
- R2: the right header record offered. R3: linked when offered; the model's argmax right; weigher overrides.
- Enron: linked to its header record (then excluded) versus opened (then served, marked unjudged).
- Arm O, hypothetical, replayed: one more source on each company statement-alternative pair, `name_overlap`, agreeing
  when the statement's folded word tokens share a token with the alternative's declared identity values (a seeded
  record's domain split at `.` and `-`, its display name), disagreeing otherwise, silent on alternatives that hold no
  declared identity value. EM refitted with it by the gated port; zones recomputed, first order.
- Gates: the port reproduces the leg's own logged fits and at least 99% of its zones before O is added.
Bar, fixed now: O is worth building when, replayed, links to the right header record reach at least half of the units
offered one, Enron mentions served unjudged fall by at least half, and wrong links stay at most 10% of links.

## Amendment 4 (2026-10-12, before any replay of it is computed): fit the estimator under the decider's own model

Reading (amendment 2's replay, amendment 3's party read): `estimate.rs` fits every statement-alternative pair as an
independent draw from a two-class mixture, while `weigh.rs` decides a statement over its alternatives as competing:
one of them, or none, is its particular. Under the pair fit a statement with several alternatives agreeing on a source
counts each as a likely match, so the match class can drift toward what many alternatives share rather than toward
identity (GVC document_date m .95 fitted against .54 on gold pairs; Ward model_choice weighed +3.0 at chance; on the
party leg the model's pick of the right Enron record overridden 10 of 11 times). The two halves of one decision use
two models.

Correction X, one change, generic (every type, every system, no constant): the fit's latent variable is the decider's
own, per statement one of its alternatives or none. E-step: weigh's posterior (the prior odds per alternative, plus
each alternative's evidence summed over sources, against none at 0). M-step: each source's m and u from the expected
matched and unmatched alternatives it spoke on; the prior odds by maximum likelihood under that posterior. Kept from
estimate.rs: the +1/+2 smoothing, clamps, identical-source groups, m<u pooling, iterations and tolerance. Dropped: the
global label flip (an alternative is not exchangeable with none). Unchanged: the comparisons, the decider and its bar,
and when the fit runs (after each document, over its type's statements so far).

Replay, first order, no model, through the gated port (G1/G2 as before), on the six runs/about legs and the party leg.
Measures per leg: links right / wrong / unjudged before and under X (amendment 2's judging; the party leg's deals by
the Ward particulars, its company statements by amendment 3's gold companies); per source, the end fit's m and u beside
the rates on gold-judged pairs (the calibration diagnostic), with their mean absolute difference before and under X.
Bars, fixed now: X goes to build when (a) on both GVC legs right links gained are at least 10 and at least twice the
wrong links gained; (b) on every Ward and uv leg (the party leg's deals included) right links lost are at most 2 and
wrong links gained at most 2; (c) on the party leg's company statements right links are not fewer and the wrong share
of links is not higher than logged. First order, as amendment 2; the build's end-to-end leg is the test of record.
