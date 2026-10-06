# GVC dev, gold mentions as statements: bars before data

Setting: `statements.py ~/.svrnmesh/bench-corpora/gvc --split dev` (78 documents, 977 event mentions, 271
chains), resolved by `svrn enrich resolve-statements --recipe recipe-gvc.toml --type happening`, model
commonwealth/primary, proposer similar_documents(neighbours 3, max_candidates 12); scored by `../score_resolve.py`
(er-score: MUC, B3, CEAF-e, LEA, CoNLL F1; refused statements scored alone).

Zero-model floors on the same mentions (measured 2026-10-05, CoNLL F1): all singletons .239, same surface
corpus-wide .337, one record per document .432, same surface within a document .480.

- **v0** (records shown as surfaces and cites): first read, no bar.
- **v1** (records keep and show the passage around each statement, with the document title): run twice.
  Adopted if both v1 runs beat v0 on CoNLL F1 by more than the gap between the two v1 runs, and the
  refused share does not rise by more than one point. Otherwise refused, with the data.
- **Amended after v0, before any further data.** v0 (CoNLL .387) refused 192 of 977 statements as
  `cite_not_found`: 134 were a verbatim passage wrapped in quotation marks, and about 50 carried U+FFFD
  where the multi-byte marker `⟦s3⟧` had been copied. The instrument was fixed first (one enclosing
  quote pair is dropped before matching; markers are ASCII `[s3]`); neither change can accept text that
  is not in the document. **v0b** is v0's display under the fixed instrument (no context lines), run
  once, and is the baseline v1 is held to under the rule above. v1 shows v0's cites plus the context
  lines, so the context lines are the only difference.
- **v1 verdict: refused** (v1a CoNLL .403 < v0b .413; v1b run only to measure the run-to-run spread).
  Read before v2: within-document links (gold 1825) were found at P .517 R .133 by v0b and P .487 R .074
  by v1a, against P .677 R .242 for same-surface-within-a-document; context cut cross-document links from
  7663 at P .037 (v0b) to 956 at P .178 (v1a). The weak piece is grouping within one answer.
- **v2** (the answer is a partition of the document's statements into particulars, each the same as one
  candidate or none, a cite per statement; v0b's display, no context lines; output budget 256 + 128 per
  statement, capped at 4000): run once. Adopted if CoNLL F1 beats v0b by more than the v1a/v1b gap AND
  within-document link F1 rises above v0b's .211 (the mechanism's own prediction). Context lines are
  re-tested on top of an adopted v2 as v3, against v2.
- **Amended while v2 was running, before its result was read.** v1a and v1b are identical decision for
  decision (temperature 0; the daemon is deterministic), so a repeat measures no noise and "the v1a/v1b
  gap" is zero. The margin bar for v2 and v3 is instead the CoNLL difference between v0b and **v0b-salt1**,
  the same arm run with document ties broken by a different hash (`statements.py --salt 1`; 41 of 78 dev
  documents share a date).
- **v2 verdict: refused** under ONTOLOGY_METHOD.md §The loop (one table of B3, CEAF-e, LEA; adopt on no
  regression), which governs where the rule above was narrower: CoNLL .422 vs .413 and within-document
  F1 .280 vs .211 (R .237 vs .133) as predicted, but B3 .425 vs .462, CEAF-e .321 vs .355, LEA .188 vs
  .201. The CoNLL gain is MUC's (.520 vs .422), bought by over-merging: 246 records, 35881
  cross-document links at P .011.
- **v3** (v2 + context lines), stated while it was running and unread: judged against v0b on the
  method's table, adopted only if CoNLL, B3, CEAF-e and LEA each beat v0b by more than |v0b − v0b-salt1|.
- **The method on GVC dev:** CoNLL F1 at or above .480. Below it, RESOLVE as built is behind a
  zero-model heuristic on this example, and that is the finding.

## Verdicts (2026-10-05)

| arm | CoNLL | MUC | B3 | CEAF-e | LEA | within-doc F1 | cross-doc links (P) | tokens |
|---|---|---|---|---|---|---|---|---|
| v0b | .413 | .422 | .462 | .355 | .201 | .211 | 7663 (.037) | 137k |
| v0b-salt1 | .407 | .424 | .462 | .334 | .200 | | | |
| v2 | .422 | .520 | .425 | .321 | .188 | .280 | 35881 (.011) | 135k |
| v3 | .454 | .514 | .485 | .364 | .255 | .277 | 3405 (.083) | 280k |

v3 is not adopted: CoNLL, MUC, B3 and LEA beat v0b beyond the order-noise band (.007/.002/.000/.001),
CEAF-e (+.008) is inside its band (.022). No arm reaches the .480 floor. v3's wrong links: 2429
cross-document links to another incident (the dominant error, under any reading of identity), and
685 within- plus 580 cross-document links between different GVC event types or victims of one
incident (the criterion says an occurrence and what it caused are different; the model lumps them).
The proposer is not the bottleneck (recall .98-.995).

## v4: the free signals as the model's input (registered 2026-10-05, before its data)

The floors above were too low: a zero-model system that groups documents at TF-IDF similarity >= 0.4
and links same-wording mentions within a group scores CoNLL .583 on GVC dev. The threshold was chosen
on train, where 0.4 is best on both corpora (GVC train .547, ECB+ train .672), and dev was not used to
choose it. Floors at 0.4, CoNLL / MUC / B3 / CEAF-e / LEA:
GVC dev .583 / .655 / .627 / .467 / .423; ECB+ dev events .687 / .740 / .683 / .636 / .537.

**v4** = v3 + each candidate shown with its document similarity + a proposed answer computed by code from
wording and similarity alone (same wording in a document is one particular; it is proposed the same as
the most similar shown record said in that wording, at similarity >= 0.4), which the model edits; the
model's cited partition decides. One run each on GVC dev and ECB+ dev events. **Adopted** if on both
corpora CoNLL, B3, CEAF-e and LEA each beat the floor by more than the GVC order band (.007, .000, .022,
.001). Beating v3 on GVC but not the floor: not adopted.

**v4 verdict: not adopted** (2026-10-06). Below the floor on every measure of both corpora:

| corpus | arm | CoNLL | MUC | B3 | CEAF-e | LEA | within-doc F1 | cross-doc links (P) | tokens |
|---|---|---|---|---|---|---|---|---|---|
| GVC dev | floor 0.4 | .583 | .655 | .627 | .467 | .423 | | | 0 |
| GVC dev | v4 | .477 | .559 | .495 | .379 | .282 | .344 | 3277 (.092) | |
| ECB+ dev events | floor 0.4 | .687 | .740 | .683 | .636 | .537 | | | 0 |
| ECB+ dev events | v4 | .556 | .608 | .574 | .486 | .367 | .269 | 5617 (.296) | 538k |

Shown the floor's own answer, the model edits it into a worse one on both. From here arms are judged on
all three systems in one table (ward mail, uv-support, GVC), never on CDCR alone.
