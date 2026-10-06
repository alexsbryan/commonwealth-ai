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
