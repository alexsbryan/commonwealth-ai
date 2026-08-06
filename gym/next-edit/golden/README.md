# Next-edit golden set — stratified by shape, sized by power

Spec: [`NEXT_EDIT_BAKEOFF.md`](../../../sovereign/docs/specs/NEXT_EDIT_BAKEOFF.md) §2.
Built because the `gen` bank cannot decide anything (60 cases, and the
[Phase 0 results](../../../sovereign/bench/next-edit-bakeoff/RESULTS_PHASE0.md)
quantify how badly).

## Why the existing bank could not carry a decision

Three independent limits, each fatal on its own:

1. **The safety bar is unmeasurable there.** 0 wrong in 28 fires bounds
   wrong-fire below **10.7%**, not below the 1% the champion bar
   demands. Certifying 1% needs ~300 fires.
2. **Quality differences are noise.** 27/30 has a 95% CI of
   [74.4%, 96.5%]. Resolving a 5-point gap needs **685 positives per
   arm**; there are 30.
3. **It is a mirror of the gate, not a sample of the world.** All 30
   positives come from the three shapes `should_consult` admits, so an
   episode the gate declines can never register as a missed fire. **No
   sample size fixes this** — only new shapes do.

(3) is why this bank is stratified by *shape* rather than merely larger.

## What it is

**1,098 cases — 711 positives + 387 negatives**, mined from **41
permissively-licensed repositories** across **18 languages**, restricted
to commits authored after **2026-07-01** (past every candidate's
release, so no published model can have trained on them). Ground truth
for every positive is the rest of the same commit. Ambiguous negatives —
ones the deterministic rule lane fires on, where "it fired" could mean
either a mislabel or a rule bug — are pruned by the real pipeline.

## Measurement 1 — only 9% of real editing episodes reach the model lane

Run against the consult gate with a **dead upstream**, which separates
"gate declined" from "gate admitted and went looking for a model" at
zero inference cost. On the 711 positives: **36% are answered by the
rule lane, 9% reach the model lane, and 55% get nothing at all.**

| Shape | n | rule lane | reached model | nothing |
|---|---|---|---|---|
| literal_fanout | 90 | 84 | 0 | 6 |
| rename_casing | 90 | 76 | 3 | 11 |
| type_fanout | 90 | 59 | 6 | 25 |
| param_insert | 89 | 26 | 17 | 46 |
| signature_fanout | 90 | 7 | 3 | 80 |
| field_init | 64 | 0 | 32 | 32 |
| import_addition | 90 | 0 | **0** | 90 |
| delete_propagation | 90 | 0 | **0** | 90 |
| enum_match_arm | 6 | 0 | 1 | 5 |
| doc_sync / guard_insert / error_conversion | 12 | 0 | 2 | 10 |

`signature_fanout` at 3/90 is the sharpest single number: it is one of
the three shapes `should_consult` is *designed* to admit, and in the
wild it admits 3 times in 90. Adding an import and using it, and
deleting a declaration and its references, are declined completely.

## Measurement 2 — the gen bank's verdict does not survive contact

Sweep-1.5B, the Phase 0 champion, scored **27/30 useful and 0 wrong** on
the `gen` bank. On the golden set, the same model through the same
pipeline:

| Metric | gen bank (n=30) | golden set (n=711/325) |
|---|---|---|
| useful-fire | 90% | **36.0%** (95% CI 32.6–39.6) |
| wrong-fire | 0% (bound: <10.7%) | **21.2%** (95% CI 17.1–26.0) |
| missed-fire | not measurable | **60.2%** |

The largest single contributor to wrong-fire is `neg_literal_trap`:
**36 of 50** cases fired a proposal when the pattern's only surviving
occurrence was inside a comment or a string literal. That is the classic
text-engine defect, and it was invisible before.

**This is a SYSTEM measurement, not a model score.** Most of those fires
come from the rule lane, not the model — which is the right frame,
because it is what a user experiences.

`partial` (hit a real edit *and* offered extra sites) is **not** counted
as wrong: the queue deliberately offers every remaining guarded site and
`NEXT_EDIT.md` §6 reports over-offer without gating it. Folding it in
would score the design's intent as a defect and inflate wrong-fire ~3x.

### `hunk-precision` — the one number that is not a case verdict

Every metric above scores a CASE, and a case counts as a win when it
hits ONE real edit. A fire that offers 62 hunks to land 3 is a
`partial`, and `useful-fire` counts it in full. `hunk-precision` scores
the individual proposed edits instead — what the user actually tabs
through — and it is the number to read when judging queue quality:

    hunk-precision: 1062/3129 = 33.9%   (rule lane isolated, at dbbf8cd4)

**Two thirds of the edits this feature proposes are ones the author
never made**, and no case-level number showed it. By shape:

| shape | precision | hunks |
|---|---|---|
| `field_init` (anchored insertion) | **100%** | 92 |
| `delete_propagation` (repeat deletion) | **100%** | 14 |
| `rename_casing` | 41.7% | 539 |
| `type_fanout` | 41.0% | 502 |
| `literal_fanout` | 31.6% | 1236 |
| `param_insert` | 20.3% | 542 |
| `signature_fanout` | 18.2% | 132 |

The two PAIR-induced kinds are perfectly precise; all the junk is in the
literal lane, and the `partial` bucket alone is 2,430 of the 3,129 hunks
at 23.2%. A perfect site filter would score 262 useful / 0 wrong (note
`439368da`), so site selection — not more rule kinds — is where the
value is.

The scorer SELF-CHECKS this against the case verdicts on every run: a
`useful` case must read 100% and a `wrong` case 0%, and any drift is
printed. That check earned its place immediately — it caught an
added-lines test that silently scored every DELETION as junk.

## Measurement 3 — the precision caveat, closed

The soft claim under everything above was that the detectors are regex
recall filters, so some episodes might be coincidence rather than
predictable next edits. `precision.py` replaces that prose with a
number.

The false positives are **not** mislabeled shapes — every truth edit
satisfies its detector's predicate by construction, because that
predicate is what grouped it. The risk is a *loose* predicate. So the
discriminating question is whether the truth is **predictable from the
exemplars**, which is mechanically checkable:

| Tier | Test | n |
|---|---|---|
| **A** literal | the exemplars' *expanded* rule, applied to the truth's old text, reproduces the truth exactly | 396 |
| **B** shared intent | the truth introduces (or removes) tokens the exemplars introduced (or removed) | 267 |
| **C** coincidence | shares no introduced or removed token at all | 48 |

**93% (663/711) are mechanically predictable; 7% is the false-positive
ceiling.** Scoring the strict A+B bank moves the headline by under three
points — missed-fire 60.2% → **58.4%**, wrong-fire 21.2% → **19.2%**.
The caveat was worth ~2 points, not a reframe.

Building this audit corrected the audit twice, which is the reason to
run one: an earlier version scored 14% coincidence, and reading the
sample showed **five of six tier-C cases were genuine** — `radii` →
`radius` scored C because tier A tested the *minimal* diff (`i`→`us`,
which mangles the line) instead of the expanded rule; and
`file_permission_api_name` scored C because the anchor token was being
subtracted as "not independent evidence" when it *is* the intent.

## Measurement 4 — what the 55% decline rate actually costs, and why

Crossing gate admission with information availability answers whether
declining is a defect or good judgement. On the 711 positives:

| | endogenous | exogenous | share endo |
|---|---|---|---|
| rule lane fired | 247 | 5 | 98% |
| reached the model | 58 | 6 | 91% |
| **nothing fired** | **251** | **144** | **64%** |
| all | 556 | 155 | 78% |

**The gate is partly right.** Declines skew exogenous — 64% endogenous
against a 78% baseline — so it does disproportionately refuse the
unknowable. That is a real defence of the design, and it is why the raw
missed-fire number overstates the defect.

**But 251 achievable episodes still got nothing**, and a hard core of
them is indefensible:

> **91 episodes (13% of positives) are tier A *and* endogenous *and* no
> lane fired.** The exemplars' own rule literally reproduces the truth,
> every token needed was on screen, and the system said nothing. There
> is no reading under which silence was correct.

Named causes, all three with the consult gate ALSO declining — the two
lanes compound, so the model never gets a chance either:

| Cause | n | What it is |
|---|---|---|
| `below_threshold` | 55 | the §4 firing table |
| `no_rule` | 32 | induction returned nothing (insertion-shaped `field_init` / `param_insert`) |
| `no_sites` | 4 | rule induced, guards matched nothing, yet truth exists |

### The threshold was a measured trade, and the trade was taken

`NEXT_EDIT.md` §4 fired a support-2 rule only when the expanded find was
≥4 characters. Sweeping that constant against the 387 negatives first
suggested the trade:

| min chars at support 2 | recovers of the 91 | negative fires | neg-fire rate |
|---|---|---|---|
| 2 | **18** | 32 | 8.3% |
| 3 | 12 | 32 | 8.3% |
| 4 (was shipped) | 0 | 28 | 7.2% |
| 5–6 | 0 | 27 | 7.0% |

**Lowered to 2 on 2026-08-06**, after re-sweeping over the WHOLE bank
rather than the tier-A 91 (the subset above understates both sides).
Full-bank, rule lane isolated: min 2 → 35.4% useful / 16.6% wrong-fire ·
min 3 → 34.7% / 16.8% (dominated) · min 4 → 33.1% / 15.8% · min 5 →
30.0% / 14.1%. In the shipped configuration the move is **+17 useful
edits for +6 wrong fires with nothing regressing** — all 19 changed
positives came out of `missed`, and 14 of the 17 gains are `partial`,
the over-offer §6 reports and does not gate. The casualty the shipped
value used to cost — `DAG` → `Dag`, support 2, two remaining sites,
declined at three characters — now fires.

A consequence worth naming: the support tier collapsed. Once support-2
required ≥2 chars, the 3+ arm carried the same condition, so
`should_fire` is now one line and more support no longer buys a shorter
rule.

**The threshold is not the main lever, though.** It caps at 18 of 91.
The remaining 73 need induction that handles insertion-shaped and
multi-line edits (`no_rule`, 32) and a look at guard/site matching
(`no_sites`, 4). *Induction coverage beats threshold tuning* — and
neither is a model-selection problem.

(Recovery is counted strictly: the induced rule must reproduce the
held-out text exactly, so 18 is a floor.)

## Recall: hand-written tables vs. embeddings

Six of the twelve detectors are pure diff shape and language-agnostic;
six depend on a hand-written regex or idiom table. The yield splits
exactly where that predicts:

| | regex-dependent | language-agnostic |
|---|---|---|
| rust / go / java | 57 / 54 / 56 | 101 / 65 / 8 |
| c / kotlin / scala | 1 / 5 / 6 | 1 / 2 / 3 |

**All four under-quota shapes are table-dependent** (`guard_insert`,
`enum_match_arm`, `doc_sync` on regexes; `error_conversion` on a
hardcoded idiom list). This is the keyword-list failure ARCH §2.4 names,
and a centroid would degrade gracefully where a regex fails hard — a
Swift `import` sits near a Rust `use` in embedding space; it matches no
pattern in `IMPORT_RE`.

**But recall is the only half that should move.** The label must stay
mechanical: ground truth is the rest of the commit, which puts this bank
at the top of the provenance hierarchy (Constructed > Mechanical >
StrongModelJudged). An embedding deciding what *counts* as an episode
would demote the bank to judged — and judged by a model of the same
family we then evaluate, which is circular.

The tier A/B/C classifier is what makes that split safe: recall can be
as loose as you like, because precision is recovered downstream by a
test that needs no model. Measured cost of the current looseness: ≤7%.

**Not scheduled yet, deliberately.** Embedding recall would grow four
shapes and the long-tail languages; it would not change the 58.4%
missed-fire finding, which is already solid at n=663. Sequence it when
per-shape verdicts on those four shapes are what is blocking a decision.

## Shapes

`shapes.py` holds a registry (open set → registry, ARCH §4), each with a
mechanical detector and a `gate` column recording whether the current
design would even consider it. A shape marked `DECLINES` that models
handle well is a gap in *our* design, not in the field's models.

Ground truth for every positive is **the rest of the same commit** — the
edits the author actually went on to make. Labels by construction: no
teacher, no judge, no model anywhere in the pipeline.

## Sizing (from the power analysis, not from ambition)

| Target | Requirement |
|---|---|
| wrong-fire ≤1% certifiable | ≥300 fires with 0 wrong → ~350–500 positives |
| resolve a 5-point quality gap | 685 positives **per arm** |
| resolve a 10-point gap | 199 positives per arm |

Delivered: **711 positives + 387 negatives**, 325 fires on the first
arm scored — past the ~300 needed to make a 1% wrong-fire bar
evaluable, and past the 199 needed to resolve a 10-point quality gap.
A 5-point gap still needs ~685 positives *per arm*, which this reaches
for the eight shapes at quota and not for the four below it.

## Contamination

Every candidate trained on public GitHub, so the defence is structural,
not statistical: `--since` (default **2026-07-01**) admits only commits
authored after the latest candidate release (Sweep-v2-7B May 14,
Mellum2 June), and `--repos-file` is meant to be pointed at
low-visibility and non-GitHub repositories. This repo's own history is
the highest-value slice — it is in nobody's corpus and it is the actual
deployment distribution.

## Known limitations, stated up front

- **Document order is a proxy for editing order.** A commit records what
  changed, never in what sequence. Every episode assumes top-to-bottom,
  the same idealisation the CUHK benchmark documents. Fixing it is the
  session-synthesis stage (§2 Stratum 2) and is not built.
- **Detectors are regex recall filters, not parsers.** False positives
  are expected and are the reason the decline rate above is an upper
  bound.
- **Single-file only.** `test_follows_impl` and all cross-file work —
  the axis Zeta-2 competes on — are not yet detectable here.
- **Negatives are transformation-built for three of five shapes.**
  `revert`, `dissimilar` and `literal_trap` are constructed from real
  edits rather than mined, because a revert leaves no trace in a commit.
- **Language spread is 18 languages, top-heavy at rust 200 / go 187 /
  yaml 138 / typescript 112.** The per-(repo,language) share keeps any
  one project from monopolising a quota, but the tail is thin.
- **Four shapes remain under quota** even across 41 repos
  (`enum_match_arm` 6, `doc_sync` 7, `guard_insert` 4,
  `error_conversion` 1). All four need ≥3 related edits in ONE
  file+commit, which is their structural floor — an episode needs two
  exemplars plus held-out truth. `enum_match_arm` is worst served
  because the match arms usually live in a different file, which is
  cross-file work this harvester cannot yet see. The harvester reports
  under-quota strata rather than letting a bank claim 12 shapes while
  carrying 8.

## Running it

```bash
# mine (defaults to the contamination cut)
python3 gym/next-edit/golden/harvest_golden.py --repo . --limit-per-shape 60

# validate structure + measure gate admission, no model needed
./target/debug/examples/next_edit_score --upstream http://127.0.0.1:1 \
    --format sweep --model-id gate-probe --port 9799 &
python3 gym/next-edit/golden/validate_golden.py \
    --cases gym/next-edit/golden/cases.jsonl --endpoint http://127.0.0.1:9799
```
