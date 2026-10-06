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
