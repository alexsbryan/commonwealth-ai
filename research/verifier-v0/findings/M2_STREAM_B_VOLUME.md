# M2 Stream B — volume run: yield arithmetic, substrate quality, contamination (2026-07-31)

Successor to `M2_STREAM_B.md`, which built and validated the harness on a
15-window saltgrass batch. This doc covers the production generation run on
the Strix Halo and **corrects that doc's volume model**, which was wrong in a
way that would have mis-planned the whole milestone.

Spec: `sovereign/docs/specs/VERIFIER_V0.md` §3 (Stream B), §7 M2.

---

## 1. Headline

The spec's 20–40k-pair target is reachable, but not by the route
`M2_STREAM_B.md` proposed. Volume is a function of **how much substrate
clears the grounded gate**, and of nothing else — not `--n`, not how many
corruption kinds are enabled, not the entity/distractor side tables.

Three corpora harvested, 1,073 evidence windows, **0 failed windows**:

| corpus | windows | claims | cases | cases/claim | grounded-gate |
|---|---|---|---|---|---|
| chaos-saltgrass | 15 | 103 | 207 | 2.01 | 42/103 = **40.8%** |
| chaos-secret-agent (Conrad) | 158 | 1,118 | 2,173 | 1.94 | 405/1,118 = **36.2%** |
| SEP | 900 | 5,362 | 19,451 | 3.63 | 3,384/5,362 = **63.1%** |
| **total** | **1,073** | **6,583** | **21,831** | 3.32 | — |

All measured. The SEP export dropped exactly **1** case of 19,452 on the site
contract (a `cross_chunk_chimera` whose connective "which is why" appeared in a
chunk, so the fused relation might have been genuinely supported) — the
contract working as intended, at a negligible rate.

Merged substrate is `all/cases_all.jsonl`: 21,831 cases, all ids unique,
labels 10,917 ungrounded / 10,914 grounded. Rows are **shuffled (seed 17), not
concatenated** — at ~25 h of labeling the run will likely be interrupted, and a
truncated prefix of a concatenated file would be all-SEP. Shuffled, the first
2,000 rows carry 89.4% SEP against 89.1% overall, so any partial labeling run
is still a balanced, usable dataset.

---

## 2. The volume identity — why `--n` does nothing

`generate_cases` alternates the label it asks for on every iteration:

```
adversarial.rs:455    let want = if out.len() % 2 == 0 { … }
```

and `make_case` ids each case `{kind}:{item.id}` (`:990`), so dedup admits
**one case per (kind, item)**. The consequence is an identity, not a tendency:

> **total cases ≈ 2 × grounded cases**

It held exactly on all three corpora — saltgrass 207 = 2×103 + 1, secret-agent
2,173 = 2×1,086 + 1, **SEP 19,451 = 2×9,725 + 1**. The `+1` is the odd final
iteration. The SEP run is the strong confirmation: the model was written from
the two small corpora and then predicted a 19k-case run to the case.

So the ungrounded side can never outrun the grounded side, however many
ungrounded kinds are enabled. The grounded side is three kinds behind two
distinct gates:

| grounded kind | gate | anchor |
|---|---|---|
| `verbatim` | all of `salient_terms(claim, 3)` `value_present` | `:914-915` |
| `reframe` | *the same gate, same claim* | `:934-935` |
| `multi_hop` | `salient_terms(a, 2)` + `salient_terms(b, 2)` | `:950-952` |

`verbatim` and `reframe` share one gate over one claim, so they **always
return identical counts** — 42/42 saltgrass, 405/405 secret-agent, **3,384/3,384
SEP**. That identity is the tell, and it is the cheapest way to confirm the
model on any future corpus: if those two counts ever diverge, the gate has
changed and this arithmetic no longer applies.

Hence:

```
total ≈ 2 × (2·G + M)      G = claims clearing the 3-term gate
                           M = pairs clearing the looser 2-term multi-hop gate
```

Raising `--n` beyond the dedup ceiling changes nothing. Measured and
confirmed; do not re-litigate it.

### What this corrects

`M2_STREAM_B.md` closes with: *"both corpora + entity/distractor tables raise
cases/claim toward 10."* **That is false.** The side tables feed only
`entity_swap` (`:545`) and `distractor_absorption` (`:841`) — both ungrounded,
and the ungrounded side is capped by the grounded side regardless. The tables
move the **mix**, never the **total**. Recorded as invariant note `1eb7ec59`;
the source doc has been amended.

Planning consequence: 20k cases needs ~5k gate-clearing claims, which needs
~10k harvested claims at a 40–60% gate rate. That is a substrate-acquisition
problem, and it is why SEP entered the plan at all.

---

## 3. Grounded-gate pass rate is the substrate-quality metric

The one number that predicts yield per unit of harvest cost:

| corpus | gate rate | cases/claim | genre |
|---|---|---|---|
| SEP | ~61% | ~3.44 | encyclopedic exposition |
| chaos-saltgrass | 40.8% | 2.01 | incident-report narrative |
| chaos-secret-agent | 36.2% | 1.94 | literary prose |

SEP yields **1.7×** per claim. The mechanism is the gate itself: it demands
that three salient terms from the extracted claim appear verbatim in the
evidence chunks. Expository prose restates its own terms; narrative prose
pronominalises them and moves on. SEP also clears the looser 2-term multi-hop
gate far more often, which is the second half of the 3.44.

**Use this as a screening test.** For any candidate corpus, harvest ~150
claims and read off `verbatim` count ÷ claims before committing GPU hours.
It costs minutes and it is the whole yield story.

---

## 4. What the side tables actually bought: coverage, not volume

Both chaos corpora exercise **all 10 kinds**. Without the tables,
`entity_swap` and `distractor_absorption` are silently skipped by design.

| kind | saltgrass | secret-agent | SEP | merged |
|---|---|---|---|---|
| verbatim | 42 | 405 | 3,384 | 3,831 |
| reframe | 42 | 405 | 3,384 | 3,831 |
| multi_hop_conjunction | 19 | 276 | 2,957 | 3,252 |
| distractor_absorption | 25 | 297 | 2,005 | 2,327 |
| unsupported_addition | 25 | 290 | 1,969 | 2,284 |
| ocr_garble | 23 | 278 | 1,887 | 2,188 |
| entity_swap | 16 | 123 | 1,926 | 2,065 |
| cross_chunk_chimera | 5 | 35 | 789 | 829 |
| number_perturb | 7 | 30 | 581 | 618 |
| negation_flip | 3 | 34 | 569 | 606 |
| **total** | **207** | **2,173** | **19,451** | **21,831** |
| label balance | 104 / 103 | 1,087 / 1,086 | 9,726 / 9,725 | 10,917 / 10,914 |

Class balance is ~50/50 by construction, as spec §3 requires.

The long tail is thin *proportionally* — `negation_flip` and `number_perturb`
fire only where the claim actually carries a polarity marker or a numeral, and
`cross_chunk_chimera` needs two fragments that fuse. That is correct behaviour
(the corruption must be checkable at its site), so the taxonomy is **not**
uniformly covered and the eval card should report per-kind counts rather than
implying an even spread.

SEP volume does rescue the tail in *absolute* terms — the three rarest kinds go
from 15/72 cases on the chaos banks alone to 606/618/829 merged, which is
enough to score per-kind rather than only in aggregate. That is the second
thing the substrate expansion bought, after raw volume.

### Sourcing the tables — two things that were wrong in the handoff

- **Entities come from the corpus atlas** (`atlas/atoms.json`, Entity atoms),
  not `out/<corpus>.named-clusters.json`. The named-clusters file is
  *thematic* — prose labels, no surface forms — so it cannot drive a swap.
- **Distractors come from the document** (`.txt`), not the `.toml` question
  bank sitting beside it.

Same-referent atoms must merge by token-set containment or `entity_swap`
rewrites a claim into a still-true statement and labels it ungrounded
("Doctor Fosk" ⊂ "Doctor Imbrey Fosk"). Handled in `scripts/side_tables.py`.

For SEP the entity table is **patched in after harvest** (`side_tables.py
patch`), never passed to `harvest --entities`: `entity_swap` rescans every
cluster on every attempt, so the raw 49,522-cluster pool is a quadratic
blowup. Filtered to 4,829 (4,629 occurring in claims + 40 partners/etype).

---

## 5. Cross-genre distractors were manufacturing easy negatives

`distractor_absorption` is the second-largest ungrounded kind, so its quality
matters disproportionately.

The handoff pointed it at the meridian postmortem for every corpus. Lifting
`"04:31 — Full recovery declared."` into a Saltgrass or SEP context produces a
case a verifier can solve on **register alone** — incident-report vocabulary
in a philosophy article is a genre mismatch, not an unsupported claim. That
teaches vocabulary discrimination, which is not the capability under test, and
it inflates apparent accuracy on exactly the kind that mirrors an
*observed production failure*.

SEP now draws distractors from SEP itself: 10 articles, 3,349 usable sentences
(30–240 chars, ≥2 salient terms — `:854`).

**The two chaos corpora have no same-world adjacent document**, so their 322
`distractor_absorption` cases retain the weakness — against 2,005 same-genre
SEP cases. So 14% of that kind is contaminated by the easy-negative problem
and 86% is sound. Not silently counted as coverage: it goes on the eval card
as a known limitation, and if the trained model over-performs on
`distractor_absorption`, scoring the chaos and SEP subsets separately is the
first diagnostic to run.

---

## 6. Contamination

Gating labeling on the contamination pass was the operator's call and it is
the right one: seconds of CPU protecting ~20 h of GPU.

**Chaos banks — CLEAN** (`findings/contamination_report_chaos.json`): 2,380
rows, 13-gram shingles against LLM-AggreFact (29,320 rows) + FaithBench (15
release batches), 8,405 unique test docs. **0 canary hits, 0 doc collisions, 0
claim collisions** in both groups. Expected — these corpora are
machine-stable banks written for this project.

**SEP — CLEAN** (`findings/contamination_report_sep.json`): 19,451 rows,
**0 canary hits, 0 doc collisions, 0 claim collisions**. 2m25s of CPU.

**All 21,831 cases are clean.** No case ids were dropped.

### The prediction was wrong, and the reason matters for the eval card

The plan of record expected SEP to come back *nonzero* — "SEP is heavily
crawled." It did not. That reasoning conflated two different risks, and only
one of them is what this pass measures:

- **Test-set overlap** — does training text appear in the AggreFact/FaithBench
  *test documents*? This is what invalidates an eval card, and it is what the
  pass detects. Those benchmarks are built from news and dialogue
  summarization sources; SEP is philosophy-encyclopedia prose. There was never
  a strong reason to expect 13-gram overlap, and there is none.
- **Pretraining memorization** — has the *base model* already read SEP? Almost
  certainly yes. This pass **structurally cannot detect that**, and it remains
  live. A verifier trained on SEP may look strong on SEP-like content partly
  because the base model memorized the source. It does not invalidate the
  external card (§5's LLM-AggreFact/RAGTruth numbers are on other corpora),
  but it does mean **internal SEP-derived results overstate generalization**.

Carry the second bullet onto the eval card. The gate did its job; it just
does not cover the risk the prediction was actually about.

### The detector is positive-controlled

A CLEAN verdict from a detector that never fires is worthless, and `canary_hits`
is a benchmark-GUID check, not a positive control — so one was run. Fixture:
one row with a real LLM-AggreFact test doc planted in `evidence_chunks`, one
with a verbatim 20-word FaithBench source span as the `claim`, and one genuine
SEP row alongside as a negative.

| control | expected | result |
|---|---|---|
| AggreFact doc in evidence | doc collision | ✓ 1 |
| FaithBench span in claim | claim collision | ✓ 1 |
| genuine SEP row | no collision | ✓ 0 |
| verdict | flips | ✓ `COLLISIONS FOUND` |

**Bug it surfaced, now fixed.** `colliding_training_rows` was incremented only
on the evidence path (`contamination_pass.py:177`), never the claim path
(`:181`), so a claim-only collision showed in `per_group.claim_collisions` but
was **invisible in the top-line count** — the two halves of the report
disagreed. Fixed with a both-surfaces guard so a row colliding on both is still
counted once; re-verified against the same fixture (`FaithBench: 1`). Neither
the chaos nor the SEP verdict changes, because both have `claim_collisions: 0`
in every group — but the top line should not have been trusted to cover claims
before this, and any earlier report read that way needs re-checking.

---

## 7. SEP is an amendment to spec §3

Spec §3 names the Stream B substrate as *"the public machine-stable bank
corpora (Secret Agent, Saltgrass)"*. SEP is neither, and it now contributes
the large majority of cases. Stating it plainly rather than letting it pass as
implied scope:

- **Why:** the §3 corpora yield ~2.4k cases combined against a 20–40k target.
  The gap is ~8×, and no amount of `--n` or taxonomy work closes it (§2).
- **What changes:** SEP is public and permissively licensed (see
  `LICENSES.md`), so the provenance rule is unaffected — labels remain
  Constructed, the teacher never relabels.
- **What it costs:** genre concentration. The trained verifier sees mostly
  expository philosophy. Register generalisation is now an open question the
  eval card must answer, and the LLM-AggreFact 11-subset average (§5) is the
  instrument that answers it.
- **Where it is recorded:** eval card, substrate section — not a footnote.

---

## 8. Does this hit the spec's 20–40k? Not quite — and that is now a wall-clock choice, not a substrate limit

Spec §3 asks for **20–40k pairs**. Cases are not pairs: the teacher discards
every case whose verdict disagrees with the constructed label (never a
relabel), at a measured keep rate of 79% (secret-agent) to 90% (saltgrass),
~87% on the merged run so far.

```
21,831 cases × 0.871 keep  =  19,019 pairs      (MEASURED, run complete)
```

So this run landed **just under the 20k floor** — short by 981. State that
plainly rather than rounding up to "20k".

**Final measured run** (22.7 h wall-clock, 0 auto-resumes, 0 malformed rows,
0 rows with an empty side, 3 transient teacher 503s across 21,831 cases):

| | |
|---|---|
| pairs / discards | 19,019 / 2,809 (87.1% keep) |
| by corpus | SEP 17,007 · secret-agent 1,830 · saltgrass 182 |
| grounded share | **55.0%** — inside the 45–55% band, but at its edge |
| A+B mix | 76,674 A + 19,019 B = 95,693 (**19.9% B**) |

Two things to carry forward. The **grounded share drifted to 55.0%** from the
bank's balanced 50.0%, because discards are asymmetric — `ocr_garble` alone
contributes 2,001 of the 2,809 discards and is entirely ungrounded (§9). Spec
§3 asks for ~50/50, so this sits at the edge of tolerance rather than
comfortably inside it; dropping `ocr_garble` from generation on this substrate
would return the balance to ~50/50 on its own.

And the **19.9% B share is well-powered for the mix study** — a straight
A vs A+B comparison no longer needs the equal-size-substitution design that
would have been required at the handoff's original ~1.9k pairs.

The important part is *why*, because it is no longer the problem it was. We
harvested **900 of SEP's 93,984 available windows** — about 1%. At the measured
3.63 cases/claim, substrate is effectively unbounded; another ~250 windows
would clear 20k comfortably and the full corpus could produce hundreds of
thousands of cases. The binding constraint has moved from *substrate* (the
problem this session started with) to *labeling wall-clock*: ~24.7 h for
21,436 cases at 4.15 s/case.

Three ways to close the gap, in the order I would try them:

1. **Accept ~19k.** It is within the spec's order-of-magnitude intent
   ("order-10⁴ is what the small-verifier literature actually used") and the
   harness can always run longer. Cheapest, and the mix study does not need
   the last 1k.
2. **Harvest ~250 more SEP windows** (~15 min) and append. Clears 20k with
   margin, costs another ~1 h of labeling. Do this if the card wants to state
   "20k" without an asterisk.
3. **Raise labeling concurrency above 6.** Untested — 6 was chosen by
   measurement, and the teacher is a resident 35B, so this trades against
   everything else on the box. Do not do this blind.

Recommendation: **option 1 now, option 2 only if the eval card needs the round
number.** The marginal 1k pairs will not move the mix study, and the 24.7 h
already committed is the expensive part.

## 9. Known weaknesses to carry onto the eval card

1. **Pretraining memorization of SEP is undetected and undetectable here.**
   Test-set contamination is CLEAN and positive-controlled (§6), but the base
   model has almost certainly read SEP. Internal SEP-derived results overstate
   generalization; the external §5 card is the honest instrument.
2. Genre concentration — SEP is 89% of the mix (§7). Register generalization
   is an open question, answered by the LLM-AggreFact 11-subset average.
3. Cross-genre distractors in 322 of 2,327 `distractor_absorption` cases —
   14% of that kind (§5). Diagnostic: score chaos and SEP subsets separately.
4. Proportionally thin taxonomy tail — `negation_flip` / `number_perturb` /
   `cross_chunk_chimera` are site-gated, so they are rare by construction (§4).
   Absolute counts (606/618/829) are adequate; the *proportions* are not
   uniform and per-kind reporting is required.
5. Grounded side is gate-selected, so grounded cases are systematically the
   ones whose terms appear verbatim. The verifier sees an easier support
   distribution than production. No fix proposed; report it.

---

## 10. Reproduction

`data/` is gitignored; every artifact is deterministic from `(harvest, n, seed)`.

```bash
svrn bench verifier harvest --corpus <id> --stride <k> --out …/claims.json
python3 research/verifier-v0/scripts/side_tables.py …      # entities + distractors
svrn bench verifier export --harvest …/claims.json --n 20000 --seed 17 --out …/stream_b.jsonl
python3 research/verifier-v0/scripts/contamination_pass.py --stream stream_b --input … --out …
python3 research/verifier-v0/scripts/teacher_label.py --cases … --out … \
    --teacher-model primary --rejected-model Qwen3.5-0.8B-UD-Q6_K_XL --concurrency 6
```

`--stride` was added this session: `harvest --limit N` alone walks chunk-id
order, which on a many-document corpus reads only the first few *documents*.
Always pair them.

**Measured costs** (the handoff's estimates were 4–5× pessimistic — measured
twice, on two corpora, before scheduling anything):

- teacher labeling **3.6 s/case** at concurrency 6 (handoff assumed 15–22 s)
- harvest 3–11.5 s/window, chunk-length dependent
- teacher keep rate 79% (secret-agent) / 90% (saltgrass); mismatches go to
  `.discards.jsonl` with the teacher's class — never a relabel
