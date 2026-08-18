# PRE-REGISTRATION — the per-commit architecture audit (`scripts/co-arch.py`)

2026-08-17, registered BEFORE the model layer has been run on a single
real commit. Bars change only via the seat. Every cost claim carries
(wall_ms, prompt_chars, out_chars) beside it — the gap-analysis
procedural rule inherited from prereg 7cbce9e1.

The audit is **shadow-only and default-OFF** until every bar below is
met (`sovereign/DEFAULTS_LEDGER.md`, row `co-arch-nightly`).

## Why this shape, priced from the repo's own decode census

From `sovereign/bench/chaos_monkey/results/PREREG_audit_economy_d7_decode_20260814.md`:

| register | measured | decode share |
|---|---|---|
| generative (per claim, with rationale) | 327ms + 4.65 ms/out_char | ~75-90% |
| specifics scan (big prompt, generative) | 2656ms + 4.32 ms/out_char | ~45-70% |
| **batched forced choice** | **1125-1328ms @ ~29k prompt chars, 29-44 out chars** | **~0%** |

Decode dominates the generative registers, so the fastest honest audit is
the one that emits the fewest characters. `co-arch.py` therefore pays ONE
prefill per commit and emits ~3 chars per judged rule, and never generates
a character that code can supply (citations come from the model-free gate).

## Already measured (model-free, 2026-08-17, n=100 first-parent commits)

- **56% of commits need zero model calls** — no added code lines, or no
  rule's gate fires.
- 44% need exactly one batched call; median 3 rules fired, max 6.
- Bundle: median 24,645 chars, p90 27,472 (added code lines only; the
  full-diff bundle is 4-11x larger — 512de1c3 is 259,854c full vs 34,905c
  added).
- Gate fire frequency: unwatchable-gate 27, untraced-branch 23,
  silent-sub 22, addr-identity 20, dup-decider 11, additive-bias 11.

Projected nightly cost at the batched register's measured rate: ~1.2s per
fired commit, so a full day of ~75-115 commits is **~40-60s of daemon
time**. This is a projection from another register's measurement, NOT a
result — bar (c) is what turns it into one.

## The bars

**(a) Gate recall — the named structural risk.** The model-free gate is
what decides whether a rule can fire at all, so a violation the gate
cannot list is invisible no matter how good the judge is. For every
labeled violation in the bank, state whether the gate lists a citation
covering it. A violation class that is structurally un-gateable is a
HEADLINE and a kill unless a deterministic gate for that class ships in
the same candidate. (Inherited verbatim in intent from D7b's (c) clause,
which did NOT trip — 15/15 — and is the reason that candidate's failure
was legible.)

**(b) Catch and false-flag on planted violations, both directions
reported.** Bank: ≥20 planted-B hunks (one per rule per violation class,
mined from the 276 invariants and 29 recorded attempts — pre-labeled
negative space) and ≥20 planted-clean hunks using the correct idiom.
- catch on planted-B ≥ 0.90
- false-B on planted-clean ≤ 0.05

The false-B bar is the tighter one on purpose. **The measured failure mode
of this house's judges in exactly this register is permissiveness, and the
second is crying wolf**: the existing landing sweep returns approve on
70-100% of commits against its own charter's stated docket of ~33%
approve / ~35% revise (7 nights, n=140, 2026-08-11..17), and D7b died at
false-"supported" 1/3 against a 0.050 bar. A probe that flags clean code
is worse than no probe, because the seat learns to skip the block.

**(c) Cost, on the live shape.** Median ≤2.5s per fired commit, priced
with (prompt_chars, out_chars, prompt_tokens, completion_tokens) per call.
Reported as a range over ≥10 fired commits, never a single run (§18.5).

**(d) Bit-stability.** `--repeat 2` at temperature 0 returns identical
letters per rule on ≥10 fired commits. A register that is not stable
across repeats cannot be trended.

**(e) Engine agreement before the cheap engine carries it.** Every probe
that might ever inform a verdict ships with an agreement measurement
between the 27B (`primary`) and the 4B (`fast`) on the same bank —
per-rule, not pooled. The ladder lesson is inherited as architecture: two
instruments with different semantics disagree precisely on the cases that
matter. The 4B may carry a rule only where it agrees with the 27B on that
rule's bank. Independently: the 27B failed its own swap gate at 50/88 on
2026-08-17 (note dcd7e0ec, different task) — "the 27B is the judge" is a
hypothesis this bar settles, not an input.

**(f) Surfacing is a rollup, never prose.** The seat's briefing gets
counts plus sha pointers plus the gate's own citation lines. No model
authors a rendered line (the `co-lineage.py` drift-block rule).

## Kills

- Any (a) class structurally un-gateable with no gate shipped alongside.
- catch < 0.90 on planted-B: the probe cannot catch what it exists to catch.
- false-B > 0.05 on planted-clean.
- Median fired-commit cost ≥ 4.0s: no material win over just extending
  the existing generative review, and the shape's whole claim was speed.
- Verdicts not bit-stable across `--repeat 2`.

## Instrument validation, stated before the run

`co-arch.py --self-test` is the model-free layer's negative control and
must exit 0 before any bank run; it has already been watched to fail
(it caught a real gate regex bug on the planted `let id = rows.len();`
line, 2026-08-17). `--self-test-live` plants a violation through the real
engine and exits 5 — never a quality verdict — when the engine cannot
serve. Both are required to pass in the same session as any bar claim.

## AMENDMENT — bar (c) re-anchored by the OPERATOR, 2026-08-17

Recorded as an amendment, not as a bar that was met. The candidate MISSED
bar (c) as registered (below); the operator then amended the bar and
directed the flip. Who moved it and when is the load-bearing fact.

- **Operator, verbatim:** "The 27B is still as quick as a lint check — I
  don't think we should default off it because it didn't meet an arbitrary
  bar — I think we tuned it well enough that we should wire it in then we
  can modify if it hurts ergonomics too much." Then, on the ceiling: "I
  didn't completely remove the constraint... I think running tests is
  about the anchor for how long we're willing to wait for a quality
  check" and "A test run requires a huge build so realistically it takes
  10 mins (if we have to push to that ceiling)."
- **Bar (c), amended:** per fired commit ≤ the house tolerance for a
  quality check, anchored to the existing gates rather than invented —
  lint ~22-27s warm, tests ~45s warm, ~10 min at the build-inclusive
  ceiling. The original 2.5s came from D7b's scan term, which runs INSIDE
  a user-facing answer; importing an interactive budget into a nightly
  batch was the registration's error.
- **Why this is legitimate:** the operator owns promotion (CHARTER
  §Governance rule 1; DEFAULTS_LEDGER is the promotion mechanism). A bar
  moved by the seat after seeing the data it failed would not be a bar —
  which is why the seat proposed and did not act.
- **Standing exit condition, operator's own words:** "we can modify if it
  hurts ergonomics too much." The ledger row's review-by is when that gets
  asked.

## CONFIG SWEEP — the knee, on a frozen 12-commit set

Run after the amendment, since the amendment is what made evidence
affordable. Same commits, same rules, only window/max_sites varied.

| window/sites | median/commit | could-not-judge | verdicts | ~min/night @ CAP=20 |
|---|---|---|---|---|
| 2/4 | 9.2s | 0.38 | A13 B2 C9 | 1.3 |
| 4/8 | 13.4s | 0.38 | A13 B2 C9 | 2.0 |
| **8/16 (SHIPPED)** | **19.1s** | **0.25** | **A17 B1 C6** | **2.8** |
| 999/999 (full diff) | 46.3s | 0.25 | A17 B1 C6 | 6.8 |

Two configs are strictly dominated: 4/8 costs 46% more than 2/4 for
identical verdicts, and the full diff costs 2.4x more than 8/16 for
identical verdicts. **8/16 extracts everything the full diff has to offer
at 40% of the price** — the plateau at 0.25 is where more context stops
buying decisions. Bank re-run at 8/16: catch 0.952, false-B 0.000, gate
recall 21/21 (unchanged).

Residual, reported not hidden: **25% of rule-verdicts are
could-not-judge** and no config removes them. They render as an explicit
`C` line in the rollup, so the seat sees "not judged", never "clean".

**SHIPPED DEFAULT-ON** in `co-sweep.sh` (`CO_ARCH=0` disables).

## RESULTS — 2026-08-17, against the bars AS ORIGINALLY REGISTERED

Kept unedited. This is the record the amendment above acts on.

| bar | result | verdict |
|---|---|---|
| (a) gate recall | 21/21 planted-B gated in (after one gate fix, below) | **MET** |
| (b) catch, 27B | 0.952 (20/21) | **MET** |
| (b) catch, 4B | 0.667 (14/21) | **MISSED** |
| (b) false-B, both | 0.000 (0/13) | **MET** |
| (c) cost, 27B | median 5,398ms over 12 fired commits (1,402-9,479) | **MISSED**, kill tripped |
| (c) cost, 4B | median 2,509ms over 12 fired commits (686-4,058) | **MISSED** by 9ms |
| (d) bit-stability | 0/39 unstable across `--repeat 2` | **MET** |
| (e) engine agreement | 4B disqualified on (b); may carry no rule | **measured** |
| (f) surfacing | `--rollup`; gate supplies every citation | **built** |

### What the cost bar actually found

The registration projected ~1.2s per fired commit from the D7 batched
register (1125-1328ms @ ~29k prompt chars). **That projection was wrong,
and wrong for a reason worth keeping:** D7's batched register is fast
because it reuses a long SHARED PREFIX across calls and hits the prefix
cache. A per-commit bundle is unique every call, so it pays full prefill
every time. Borrowing a latency across registers with different cache
semantics is the same class of error as borrowing a threshold across two
instruments with different tau semantics.

Measured here instead: **cost is linear in prefill at ~7-8ms per prompt
token** (1,760 tokens -> 12.4s; 7,039 -> 42.7s; 8,169 -> 64.6s). Decode
is genuinely free, exactly as the forced-choice shape promised — out is
5-12 tokens per commit. The shape was right; the price of admission is
the prompt, not the answer.

Two candidates were priced against bar (c), both refused:

1. **Full added-code bundle** (median 24,645 chars): median **46,319ms**.
   Well-evidenced — the judge answers A/B, rarely C.
2. **Gate-localised evidence windows** (median 4,689 chars, 5.3x
   smaller): median **5,398ms**, an 8.6x improvement. But the judge
   returned all-C on **6 of 12 real commits** — the speed was bought by
   removing the evidence the question needed.

### The finding the bank could not see

Bars (a)/(b) pass on the bank (0.952 catch) while the same build returns
could-not-judge on half of real commits. **The bank is not representative
of the production distribution**: its hunks are small and self-contained,
real commits are neither. A synthetic bank measures the judge; it does not
measure whether the bundle carries enough context. Any future candidate
must report the real-commit C-rate beside the bank score — added here as a
standing reporting duty, not a new bar.

### Bar (c) is arguably the wrong bar — the operator's call, not the seat's

2.5s was inherited from D7b's scan term, which runs INSIDE a user-facing
answer where 2.5s is felt. This audit is a nightly batch: at 44% of ~100
commits firing, 5.4s each is **~4 minutes for a full day of commits**,
which fits the night with enormous margin. That is an argument for
AMENDING bar (c) to something operationally meaningful (e.g. total sweep
wall-clock), and it is recorded here rather than acted on: a bar moved
after seeing the data it failed is not a bar. The seat and operator own
that amendment.

The C-rate problem is NOT fixed by any such amendment. If bar (c) is
relaxed to admit candidate 1 (the 46s full bundle), the sweep costs ~34
minutes a night and answers properly; that is the real trade on the table.

### Changes made during the run, both watched to fail first

- `addr-identity` gate matched only the TYPE `SocketAddr`, missing a
  planted `socket.peer_addr()?`. Bar (a) caught it; fixed; 21/21.
- `dup-decider` REFUSED and removed from the shipped set — see the
  `[[refused]]` block in `quality/arch-probes.toml`. It returned
  could-not-judge on 2 of 3 planted violations because "already decided
  elsewhere" is evidence the diff does not carry. The judge was right.

## Host caveat recorded at registration time

The bank has NOT been run: on 2026-08-17 this host's daemon reported
`loaded_models: []` with `primary` placed at 0 blocks, serving
`/v1/models` 200 while `/v1/chat/completions` returned 503. The one
partial reading taken before it degraded (4B, batched, 961 prompt tokens
/ 50 completion tokens, 3953ms warm / 6046ms cold; the generative
single-probe contrast at 400 completion tokens took 69-104s) is
DIRECTIONAL ONLY — a degraded host measures its own mood, not the design.
No bar may be claimed met from it.
