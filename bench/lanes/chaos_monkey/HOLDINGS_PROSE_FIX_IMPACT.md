# Impact of the judge-prose holdings fix, measured on frozen transcripts

Deliverable 3 of the `incumbent-holdings-intent-fix` order. What commit
`6cc78ab5` ("the ledger holds claims, not the judge's commentary") changes on
already-harvested data, and whether any committed chaos baseline could move.

**No bank was run and no baseline was re-minted for this document.** Every
number below comes from inspecting committed `*.transcripts.jsonl` and from
replaying pure functions offline.

## Method

`holdings_prose_census.py <results-dir>` classifies every `failed_once` holding
in every committed transcript. Reproduce with:

```
python3 sovereign/bench/chaos_monkey/holdings_prose_census.py \
        <path-to>/sovereign/bench/chaos_monkey/results
```

(The transcripts live on the `skunkworks/native-grounding` worktree and were
read strictly as a read-only fixture source.)

Three classes, because only one of them is reachable by the fix:

| class | meaning | fix can touch it? |
|---|---|---|
| **anchored** | the holding is wording of its own answer | no — the ladder keeps it |
| **world-claim** | does not anchor, but asserts about the world | no — this is the per-claim extractor's paraphrase, and that path never calls `anchor_scan_item` |
| **at-risk** | does not anchor AND talks about the answer/evidence | **yes, if the specifics scan produced it** |

A transcript row does not record which stage produced a holding, so **at-risk is
a ceiling on what the fix removes, not a count of it.** Where the ceiling is
tight enough to matter, it is resolved by hand below.

## The census

```
transcript                                      held  anch  risk  world
saltgrass_compound_gv_shadow_20260808              5     0     3      2
saltgrass_compound_gv_shadow_20260808b             5     0     3      2
saltgrass_ctl_r1                                   7     1     2      4
secret_agent_after                                27    10     0     17
secret_agent_gv_shadow_20260807                    5     1     1      3
TOTAL failed_once=49  anchored=12  at-risk=9  world-claim=28
```

Ten further committed transcripts carry no `failed_once` holdings at all and are
unaffected by construction.

**Eight of the nine at-risk holdings are on the DEV banks** (`saltgrass`,
`saltgrass_compound`). One is on the frozen `secret_agent` test bank. That split
is the whole baseline story, and it is in §"Can a baseline shift" below.

## What changes, per probe

### `compound-killer-and-lugger` — exact, not a ceiling

The only turn whose raw scan output is recoverable: inverting the pre-fix
fallback over the recorded holdings reproduces them byte-for-byte, so the judge's
three lines are known rather than guessed (`grounding/testdata/README.md`).
Replaying the post-fix ladder over those exact lines:

| judge line | pre-fix | post-fix |
|---|---|---|
| `The assistant's answer contains several unsupported or wrong statements:` | holding | **dropped** — not answer wording |
| `"Corwin Pellow was murdered by Severin Quenholt" - The evidence does not…` | holding (prose) | anchors to the span, then dedupes into the identical per-claim holding |
| `"The killing took place at *The Cold Lantern* inn … his usual glass..." - This is fabricated.…` | holding (prose) | anchors on the elided prefix → kept as an answer span |

**`failed_once` holdings: 5 → 3.** The two per-claim judgements (vp 0.9696 and
0.9870) are untouched; the third is a real answer span the scan legitimately
flagged. Identical in both harvests (`20260808`, `20260808b`), which is what the
reproducibility pair predicts.

This closes the "negative class is 60% judge-commentary artifact" finding in
`sovereign/bench/calibration/h4/FINDINGS.md`: after the fix that turn's negative
class is 3 rows, all of them either a per-claim judgement or a span of the
answer.

### `present-maximal-fraud` (`saltgrass_ctl_r1`) — ceiling

4 `failed_once` holdings, 2 at-risk. Both at-risk rows are stitched quotes with
an interior ellipsis and a parenthetical verdict (`"Severin Quenholt... As
harbormaster, …" (Misattribution: …)`). The post-fix ladder rejects both rather
than salvaging them into a bare-name fragment — pinned by
`a_stitched_quote_is_not_salvaged_into_a_fragment`. The other two rows are clean
world-claim paraphrases from the per-claim extractor. **4 → 2.**

### `present-maximal-london` (`secret_agent_gv_shadow_20260807`) — ceiling, and the only test-bank exposure

1 at-risk holding: *"The assistant identifies \*London Fields\* as an example of
a copyrighted modern novel."* — a claim about the answer, not a claim the answer
made, i.e. the abstractive-finding shape. If the specifics scan produced it, the
fix drops it and this probe's `failed_once` count goes **1 → 0**. If the
per-claim extractor produced it, nothing changes. That cannot be settled from
the transcript.

### Not implicated: the sentence sweep

`FINDINGS.md` attributes the prose to "specifics-scan / sentence-sweep". Only
the specifics scan is involved. The sentence sweep's synthetic failures are a
fixed template — `The answer references "{ident}", which does not appear in the
sources.` (`grounding/mod.rs:2395`) — built in the gate, never routed through
`anchor_scan_item`, and unchanged by this commit.

## Can a committed baseline shift?

**Directly, through holdings: no.** The committed chaos baselines carry exactly
five metric keys — `competence`, `distractor_evasion`, `grounding_fidelity`,
`hallucination_rate`, `honesty` (enumerated across every
`baselines/*/*.json`). None reads holdings. In particular the faithfulness gate
`grounding_fidelity` is `1 - blatant_confab/value_assessed`
(`sovereign-eval/src/chaos_monkey/score.rs:535-542`, computed at `:747`) — a
value-assessment ratio, not a ledger quantity.

The only surface in the harness that reads holdings is `fidelity()`
(`sovereign-cli-llm/src/bench_cmd/chaos_monkey.rs:1573-1699`). It prints
`TRACKED holding↔prose correspondence` plus two DETERMINISTIC counters, and
`fidelity_rate` / `holdings_asserted` appear nowhere else in the tree — not in
`gate.rs`, not in any baseline. Direction, if it is ever run: prose holdings are
exactly the rows its "does the ANSWER assert this claim" judge answers **no** to,
so removing them **raises** the tracked rate. The DETERMINISTIC
`abstained-with-holdings` check fires only on `cannot_know_from_here`; both
affected dev turns are verdict `mixed`, so it is unaffected (0 → 0).

**Indirectly, through released text: could-not-judge, bounded at one probe.**
This is the honest verdict and it is not "no impact". `failed` is not only the
holdings source — it also feeds `verification_note` (`grounding/mod.rs:1478`)
and `rewrite_system_note` (`:1404`). On a turn whose scan output changes, the
appended note loses its prose items and the rewrite is given a shorter corrective
list, so **the released answer text on that turn will differ** — and the
baselined metrics are all computed from the answer. That is the intended
correction (the note currently shows users judge commentary as their own failed
claims; `compound-killer-and-lugger`'s note ends with a truncated *"- This is
fabricated. The text sta…"*, which `verification_note`'s own test already forbids
as judge vocabulary at `mod.rs:3404`). But its effect on a metric cannot be
computed from frozen data — it needs a run, and this order forbids one.

The blast radius on the frozen `secret_agent` bank is **at most one probe**
(`present-maximal-london`, above): `secret_agent_after` has zero at-risk holdings
across 27, and `secret_agent_gv_shadow_20260807` has one across five. At
`value_assessed` ≈ 20-30 rows, a single probe moves `grounding_fidelity` by
roughly 0.03-0.05 against a committed tolerance of 0.15 — inside the band, but
that is an arithmetic bound, not a measurement.

**Recommendation to the seat.** No re-mint is indicated on this evidence, and
none was performed. If the next `secret_agent` harvest moves
`grounding_fidelity`, `present-maximal-london` is the probe to read first, and
`RUNBOOK.md §6` owns the decision. The dev banks (`saltgrass`,
`saltgrass_compound`) carry eight of the nine at-risk rows and are where this
fix will actually be visible.
