# RESOLVE on three systems, gold mentions as statements: bars before data

Registered 2026-10-06, before any number below it was read. Supersedes CDCR-only judging
(`cdcr/prereg.md` v4 verdict): every arm is read in one table over three systems.

Settings, each produced by its `statements.py` and resolved by `svrn enrich resolve-statements`, scored by
`score_resolve.py` and tabled by `resolve_table.py`:

- **ward**: Kim Ward's mail, gold stage-update quotes as statements, gold deal as chain (`ward/statements.py`,
  `recipe.toml` type `deal`). Tune fold 90 statements, 57 messages, 42 deals. The read fold is the final gate.
- **uv**: uv-support, gold case-state quotes as statements, gold case as chain (`support/statements.py`,
  `recipe.toml` type `case`, thread declared as `change.document.thread`). Tune fold 384 statements,
  382 documents, 83 cases. The read fold is the final gate.
- **gvc**: GVC dev event mentions (977, 271 chains), as in `cdcr/prereg.md`; knobs are chosen on GVC train
  (5313) and ECB+ train (3808), never dev.

**Knobs, no model.** `--answer proposed` takes the proposed answer (declared thread, then same wording at
`same_wording`, then optionally any wording at `similar`) with no call. The rule is chosen on GVC train, ECB+
train, ward tune and uv tune over same_wording {.2 .. .6} and similar {off, .2 .. .7}: the setting that
maximises the smallest, over the four, of its CoNLL F1 divided by that fold's best CoNLL F1 in the grid.
`min_similarity` (candidates from less alike documents are not shown) is the largest of {0, .1, .2, .3}
whose proposer recall on every one of the four is within .01 of recall at 0. The **floor** of each system is
the proposed engine at the chosen setting on its judged fold (GVC dev, ward tune, uv tune). Each floor's
order band is |floor - floor at `statements.py --salt 1`| per measure.

**v4 on ward and uv** (`cdcr/prereg.md` v4, the binary built for it, run 2026-10-06 05:00Z): read against the floors,
no bar; it places RESOLVE-as-built on the two systems it had not seen.

**v5** = the model with the declared-thread proposer, the chosen rule shown as its proposed answer, and
candidates bounded at the chosen `min_similarity`. One run on each judged fold. **Adopted** only if, on all
three systems, CoNLL, B3, CEAF-e and LEA each beat that system's floor by more than its order band (GVC's
model order band where larger: .007, .000, .022, .001). A system where v5 is below its floor is named in the
verdict with its residual read (example gap or method gap, ONTOLOGY_METHOD.md §The example checks the work).

## Verdicts (2026-10-06)

**Knobs** (`--answer proposed`, 84 runs, no model). The grid run was a cross, not the product the bar names:
same_wording {.2 .. .6} with similar off, and similar {.2 .. .7} at same_wording .4. On it the rule chose
same_wording .4, similar .7 (smallest ratio .851, ward's); `min_similarity` .1 (at .2 ward's recall fell from
.698 to .581). The Rust floor joins only through the top three similar documents, so it is below the
pairwise Python floor of `cdcr/prereg.md` (GVC dev .572 vs .583, ECB+ train .646 vs .672). The declared
thread alone moves uv's floor from .187 to .802. Every floor is identical under `--salt 1` (band 0; ward's
salt is a no-op, its messages have no date ties), so GVC's model band governs.

| system | arm | CoNLL | MUC | B3 | CEAF-e | LEA | calls/doc | tokens |
|---|---|---|---|---|---|---|---|---|
| ward tune | floor | .436 | .151 | .654 | .504 | .250 | 0 | 0 |
| ward tune | v4 | .501 | .345 | .621 | .538 | .297 | 1.00 | 123k |
| ward tune | v5 | .551 | .380 | .659 | .616 | .340 | 0.98 | 117k |
| uv tune | floor | .801 | .905 | .845 | .652 | .781 | 0 | 0 |
| uv tune | v4 | .450 | .468 | .488 | .394 | .290 | 0.99 | 438k |
| uv tune | v5 | .615 | .722 | .647 | .476 | .486 | 0.99 | 578k |
| GVC dev | floor | .572 | .647 | .616 | .452 | .404 | 0 | 0 |
| GVC dev | v5 | .477 | .547 | .509 | .375 | .289 | 1.00 | 293k |

**v5: not adopted.** Above its floor beyond the band on ward only (B3 by .005). Residuals, read: on uv, of
311 statements continuing a thread that already held a record, gold keeps 257 (.83) in the thread's case; v5
departed from the thread's record about 152 times and was right about 29 (.19), the precision the one-comment
spin-off read was refused at (.206). On GVC, v5 made 2261 cross-document links at precision .128 where the
floor made 662 at .618. On ward, where no free signal is strong (proposer recall .744), the model adds what the
floor cannot: within-message F1 .500 against .222. Method gap, not example gap: shown a strong signal, the
model departs from it far more often than it is right to, on two of three systems.

## Ring 0: one forced choice per statement (registered 2026-10-06, before its data)

`--answer select` asks, per statement in document order, one forced choice over the shown candidates (v5's
proposers, rule and bounds) plus the records this document already opened, read in one forward pass as a
distribution over single-token labels (`oicp_types::forced_choice`); the most probable label decides. The
information the verdict carries is measured as for the partition, by `score_resolve.py`'s verdict block over
shown candidates: LR+ and LR- in nats with 90% bootstrap intervals over documents. v5 (the partition):
ward +1.74 [1.12, 2.29] / -0.33; uv +1.42 [1.23, 1.64] / -0.30; GVC +1.83 [1.49, 2.13] / -0.19.

**Bar.** On all three systems the forced choice's LR+ is above v5's and its LR- below v5's. Met: the forced
choice is the model's evidence form for Rings 1 and 2. Otherwise refused with its data. Not controlled: the
argmax shapes which records exist, so the candidate sets differ from v5's. Read beside it, no bar: Brier and ECE
of p(candidate), the clustering table against floor and v5, calls and wall seconds per document.

**Ring 0 verdict (2026-10-06): refused.** LR+ / LR- in nats [90% CI] against v5; the forced choice's own
read beside it (`score_resolve.py`: AUC of p(candidate) over pairs; per statement, detect AUC of 1 - p(none)
and the share of statements shown a same candidate whose most probable candidate is one):

| system | LR+ v5 / select | LR- v5 / select | AUC p | detect AUC | top right | CoNLL floor / v5 / select | tokens v5 / select |
|---|---|---|---|---|---|---|---|
| ward tune | 1.74 / 1.77 [0.48, 3.31] | -0.33 / -0.05 [-0.13, -0.01] | .771 | .647 | .688 | .436 / .551 / .450 | 117k / 140k |
| uv tune | 1.42 / 0.97 [0.76, 1.21] | -0.30 / -0.15 [-0.19, -0.11] | .656 | .750 | .751 | .801 / .615 / .536 | 578k / 448k |
| GVC dev | 1.83 / 2.31 [1.88, 2.81] | -0.19 / -0.14 [-0.24, -0.08] | .781 | .579 | .628 | .572 / .477 / .524 | 293k / 2.61M |

LR- misses on all three, LR+ on uv. Read, no bar. The distribution carries what the argmax drops: ward's
p(none) dominates (mean p .021 against a same rate of .102; "none" on 85 of 90), and on uv each wrong "none"
opens a duplicate record that later statements' mass splits across (137 of 382 were shown two or more same
candidates). A Platt map of log(p_top / p_none), fitted on either tune fold or both, leaves link precision
where the argmax had it (ward about .35, uv about .62), so the read, not the decision rule, is the limit.
Its errors, read on all three, are one method gap: a candidate is shown as the words of the statement that
opened it, and the model matches words or incidents instead of applying the criterion's fields. uv:
"labeled bug" and "closed as not planned", bodies with no title, join another case's identical words at p
near 1, the thread that decides them never shown. ward: a statement naming no product, point or term ("Here's
a chart that may help you explain our proposal") gets p(none) = 1.00 beside the one open deal with its
counterparty, and two deals in one message, deal numbers differing, are joined. GVC: 242 of 337 false links
are inside one article, "death" and "gunshot" joined to the shooting the criterion says they are apart from.
That is Ring 1: READ the declared fields (from declared metadata by code, from text by pointing), compare per
field, show fields rather than mentions; the distribution is one input to the decider, not the decision.

## Ring 1a: declared document fields weighed by code (registered 2026-10-06, before its data)

Ring 0 showed uv's model "(same thread)" beside the gold record in 50 of its 85 wrong links, and it chose
another thread's words. So a declared field is not shown to the model and hoped for: code weighs it
(ONTOLOGY_METHOD invariant 2). A type lists `identity_evidential` fields, each a document stamp
(`document_thread`, `document_date`) with its measured precision, and declares `identity_bar`, set to .5 in
all three recipes before any precision was measured (link when more likely right than wrong). Before any
answerer, a statement whose field value is held by exactly one open record from an earlier document is
linked to it when that field's precision clears the bar (`Decision::Field`); held by two or more, the field
settles nothing and the statement goes to the answerer as before. Precision is measured under gold, no
model, by `field_precision.py` for exactly that rule (earlier documents, exactly one gold chain holding the
value) on each system's knob fold: ward tune, uv tune, GVC train. GVC declares `change.document.date`.

**Arms.** Control: Ring 0 (`--answer select`, the run above). Treatment: the same binary and flags with the
recipes' `identity_evidential` and `identity_bar`. The verdict block counts a field link as saying "same".

**Bar.** On ward tune and uv tune, the treatment's CoNLL, B3, CEAF-e and LEA are each at or above the
control's, and LR- is below the control's. Met: GVC dev is run at the gate under the same bar, unless no GVC
field clears .5, in which case the treatment equals the control by construction and is not run. Read beside
it, no bar: each system against its floor and v5, and on uv how many field links gold keeps.

**Ring 1a verdict (2026-10-06).** Measured precision under gold (`field_precision.py`): uv `document_thread`
.905 (190 of 210 links), `document_date` .938 (15 of 16); GVC train `document_date` 0 of 11; ward none
declarable (each message its own `thread_id`, 57 of 57; dates never repeat). So uv declares both, GVC its
date below the bar (equal to the control by construction, not run), ward nothing.

| uv tune | CoNLL | MUC | B3 | CEAF-e | LEA | LR+ | LR- | calls/doc | tokens |
|---|---|---|---|---|---|---|---|---|---|
| floor | .801 | .905 | .845 | .652 | .781 | | | 0 | 0 |
| control (Ring 0) | .536 | .594 | .563 | .450 | .381 | 0.97 | -0.15 | 1.00 | 448k |
| 1a | .778 | .910 | .800 | .624 | .752 | 2.17 [2.02, 2.28] | -5.33 [-5.39, -4.19] | 0.19 | 92k |

Met on uv. Ward, rerun with the 1a binary and nothing declared, reproduced the control decision for decision
and token for token (140,256): the run is deterministic at this setting, and ward's LR- equals the control's
rather than falling below it, so the bar's letter, which did not foresee a system with nothing to declare, is
not met there; no system regresses (invariant 5). Adopted as the decider's evidential clause.

Instrument, read: live, the thread links were right 208 of 293 (.71), not .905. Gold assumes each thread's
first decision right; live, a field follows whatever put the thread in a record. Into records the thread
opened, 186 of 223 (.83); into records the model's selection brought it to, 22 of 70 (.31). The 21
selections were right 10 times (.48): an argmax at .48 deciding a join, and the thread carrying its error.
And a field that links a spin-off keeps its thread held by one record, where gold's second chain would stop
it. Precision must be measured on the live rule, not under gold. uv stays below its floor (.778 vs .801) for
that reason: the model's choice decides without clearing the bar, which invariant 2 forbids.

## Why the forced choice is below .5 (diagnosis, 2026-10-06, no bar)

Read on Ring 0's prompts, captured exactly (a local proxy on the daemon wire; 21 GVC, 88 ward, 80 uv calls,
each reproducing Ring 0 decision for decision) and replayed. Not the cause: thinking (no free generation opens
a think block; the client sends `think_budget: 0`); label position (with the candidates reversed the argmax
stays on the same candidate 13 of 17 GVC, 71 of 87 ward, 56 of 72 uv; the first-listed record's pull on GVC
is mostly its content, .396 as A against .335 listed last).

The cause, by gold structure: the model resolves one level coarser than the criterion. GVC: of 451 wrong
argmax links, 347 join two happenings of one incident and different kinds (firing, a person hit, an injury,
a death, the incident as a whole; the kind is in the gold chain id), 41 differ only in participants, 34 cross
incidents; without the kind errors precision would be .73 in-document and .66 across. Ward: 11 of 13 wrong
links keep the counterparty and miss the deal; the misses are statements whose counterparty is only in the
message's participants (the "Chart" mail to ci.mesa.az.us), which no question shows. uv: 43 of 85 are
content-free events joined across threads by their words (Ring 1a's thread field decides them now).

Reading the field apart works where the whole question does not. Asked only the kind, one forced choice over
five declared kinds, the model is right .767 on 400 GVC-train mentions (.875 at p >= .9) and .721 on GVC
dev's 848 gradable ones (death .92, firing .57). As a veto on Ring 0's 697 GVC links its own reads lift
precision from .353 to .616, losing 58 of 246 right links; gold kinds give .703. Reasoning before answering,
by contrast, does not fix GVC (7 of 21 right reasoned against 9 by the argmax) but does on ward: of 32
statements shown their deal, the argmax finds 6 and a reasoned answer 16 (link precision .32 to .48); free
generation opens with reasoning on 43% of ward prompts, 38% of GVC's, 10% of uv's.

## Ring 1b with the gate: a read necessary field, and the model's choice weighed (registered 2026-10-06, before its data)

From the diagnosis above. A type lists `identity_necessary` attributes; one with declared `values` is READ per
statement as one forced choice over those values (and "none of them", read as unknown), through RESOLVE's
census funnel. A record carries the value read for its opening statement. Before the forced choice, a
candidate whose value differs from the statement's, both known, is not offered (counted). GVC declares
`kind` on `happening` with the five kinds the diagnosis read (firing; a person hit; an injury or condition;
a death; the incident as a whole), necessary. And the model's choice links only where the precision measured
for it on the live rule clears the bar, declared like a field (`model_choice`): uv .48 (10 of 21 under 1a)
and ward .32 (6 of 19 under Ring 0) are below .5, so on them the model's choice links nothing; GVC's is
measured in a first run with the kind field and an ungated choice, then declared, then run again.

**Bar.** Each system's CoNLL, B3, CEAF-e and LEA at or above its floor's (GVC .572/.616/.452/.404, uv
.801/.845/.652/.781, ward .436/.654/.504/.250). Read beside it, no bar: GVC's live link precision of the
model's choice against the counterfactual's .616, and kind-read calls and tokens. Not in this arm: ward's
counterparty from participants (needs registrable-domain folding and an owner side, rung 7's unbuilt part).

*Amended before any data, same day:* gated off, the model's choice would leave ward's statements singletons,
so the proposed answer (thread, then same wording, then similarity, `ProposalRule` as chosen) enters as an
evidence source too (`proposed_answer`), at the live precision of the floor run, where it alone decides: ward
.80 (4 of 5), uv .826, GVC .619. Of the sources that name a candidate, the most precise that clears the bar
decides; none, the statement opens. With the floor's rule inside the decider, "at or above the floor" is
nearly met by construction; the read that matters is GVC's: what the model's choice, kind-vetoed, adds over
the floor where the proposed answer names nothing.

**Ring 1b with the gate, verdict (2026-10-06).** GVC's measuring run (kind READ and veto, the argmax deciding):
the model's links right 310 of 542 (.572; Ring 0 .353, the counterfactual .616), 10,761 candidate offers
vetoed, 15 reads unknown, CoNLL .570. Declared at .572 and gated:

| system | CoNLL | MUC | B3 | CEAF-e | LEA | LR+ / LR- | calls/doc | tokens |
|---|---|---|---|---|---|---|---|---|
| GVC floor | .572 | .647 | .616 | .452 | .404 | 2.95 / -0.21 | 0 | 0 |
| GVC gated | .599 | .682 | .624 | .492 | .440 | 2.71 / -0.26 | 24.9 | 1.63M |
| uv floor | .801 | .905 | .845 | .652 | .781 | 2.68 / -2.26 | 0 | 0 |
| uv gated | .799 | .902 | .842 | .652 | .778 | 2.66 / -2.22 | 0 | 0 |
| ward floor | .436 | .151 | .654 | .504 | .250 | 3.50 / -0.09 | 0 | 0 |
| ward gated | .436 | .151 | .654 | .504 | .250 | 3.50 / -0.09 | 0 | 0 |

Met on GVC, beyond the order band on every measure (+.027, +.008, +.040, +.036): the first model arm above a
floor. Met on ward, equal (the model below the bar is never asked). Missed on uv by one decision of 384: a
merge event in thread 1619 and closing 1624 in the same second, where the date field (15 of 16) outranked the
thread (190 of 210) on point precision. Ward's deal kind read the same way is no lever (the veto keeps 13 of
Ring 0's 19 links and 5 of their 6 right; 14 of 42 gold deals read as more than one kind).

## Ring 2a: precision from counts (registered 2026-10-06, before its data)

A source declares its measured counts (`right`, `of`), and code weighs and gates it on the expected precision
given them, the posterior mean under a uniform prior, (right + 1) / (of + 2), not on the point estimate, which
overstates few links. Prediction: uv's one decision returns to the thread (date 16/18 = .889, thread 191/212 =
.901), so uv equals its floor; every other gate and order is unchanged (GVC model 311/544 = .572 against
proposed 178/288 = .618; ward proposed 5/7 = .714, model 7/21 = .333; uv model 11/23 = .478), so GVC and ward
equal their 1b runs by construction and are not rerun. **Bar:** uv's four measures at or above its floor's.

**Ring 2a verdict (2026-10-06): met.** uv under expected precision equals its floor on every measure (.801,
.905, .845, .652, .781; the one decision returns to the thread); ward equals its floor; GVC's gates and orders
are unchanged by construction, so its gated run stands (.599 against .572). Every system is now at or above
its floor, GVC beyond the band on all four measures; the model adds over the floor on GVC only, and on uv and
ward is below the bar and never asked. Where it can add next: uv's 73 records against 83 cases (the floor's
own ceiling; the model's cross-thread choices are .478), ward's misses (counterparty only in participants,
rung 7) and its in-message deal splits (reasoning first found 16 of 32 against the argmax's 6).

**Held-out read folds (2026-10-06), opened once for the adopted design (Ring 2a), no model call on either:**
uv read .801 / .927 / .826 / .648 / .785 against its floor's .799 / .927 / .826 / .645 / .785; ward read equal
to its floor (.414 / .182 / .579 / .481 / .300). The fields and counts measured on the tune folds hold out.

## Ring 1c on ward: the term READ, and a choice read after the model's own reasoning (registered 2026-10-06, before its data)

Operator-agreed amendment to §Identity: where the one-pass choice cannot clear the bar, the model may reason
first, and its answer is still read as a forced-choice distribution, after its reasoning, weighed as its own
source (`reasoned_choice`, `--answer reason`). Ward's residual is deal granularity inside one counterparty, which
its criterion names (product, delivery point, term): read on Ring 0's prompts, reasoning first found 16 of 32
deals against the argmax's 6 (link precision .48), and a term-shape read as a veto kept 26 of those 33 links
with 14 of the 16 right (.54); only 8 of 42 gold deals read as more than one term. Ward declares `term` (month
to month or spot; a fixed term under a year; a year or more; a master or enabling agreement with no term),
necessary. Measured in a first ward-tune run (`--answer reason`, term veto, the reasoned argmax deciding),
declared, then run gated beside the proposed answer (5 of 7 expected .714).

**Bar.** Ward tune's CoNLL, B3, CEAF-e and LEA each above its floor's (.436 / .654 / .504 / .250), beyond 0
(the runs are deterministic). Read beside it, no bar: reasoning tokens per statement, the live precision of the
reasoned choice against the probe's .54. Met: ward's read fold, once.
