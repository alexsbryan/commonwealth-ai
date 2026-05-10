# Inner-work tuning campaign — 2026-05-04

A six-iteration prompt-tuning pass on `RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT`,
driven by the inner-work voice bench filtered to the 11 inner-work base
scenarios via `--skill inner-work`. All runs against
`FINAL-Bench_Darwin-35B-A3B-Opus-Q6_K_L` (chat + judge).

The campaign was triggered by a desktop incident on 2026-05-04 in which
a heartfelt journal entry rendered as third-person retrieval reasoning
with code-corpus chunks leaked into the witness reply (the canonical
"corpus pollution" failure). Pre-tuning architectural fixes precede
the prompt iterations.

## Architectural fixes (pre-tuning)

These are production wins, not iterated on. Each was its own gate.

| fix | location | reason |
|---|---|---|
| Drop knowledge tool from inner-work | `skills/inner-work/skill.toml` | Witness has no business retrieving from external corpora; planner template was invoking knowledge tool against installed code corpora |
| Skill exclusivity on surface mount | `InnerWorkSurface.svelte` onMount | Other active skills could co-register; on entry we now snapshot, deactivate non-witness, restore on exit |
| Force witness path when register=Relational | `runtime::override_intent_for_relational_register` | Router was misclassifying paragraph-shape personal prose as MetalingualQuery at confidence 1.00; override forces ExpressiveQuery dispatch |
| Streaming witness path | `runtime::handle_expressive_query_stream` + `title::strip_thinking_stream` | Was returning `NotImplemented` and falling back to non-streaming; now streams cleaned reply tokens with strip-thinking transformer |
| Snake_case identifier check | `voice_eval::checks::code_identifier_check` | New regression gate against corpus pollution recurrence |
| Three new bench scenarios | `bench/voice/13-journal-heartfelt.toml`, `hard/H09-journal-think-leak.toml`, `hard/H10-journal-corpus-pollution.toml` | Paragraph-shape entries — the actual surface of the failure |
| `--skill <id>` filter on voice eval | `voice_eval::select_scenarios` | Earlier voice bench was implicitly inner-work-mostly with two personal-assistant scenarios mixed in |
| `--diff <baseline.json>` axis-level diff | `voice_eval::AxisMeans::print_axis_diff` | The tuning loop's primary signal; per-scenario flips have run-to-run variance, axis means pool across scenarios |

## Six-iteration prompt campaign

All numbers are per-axis means over 11 inner-work scenarios. Pass count
is the integration metric. **Bold** = best per axis across iterations.

| iter | architecture                            | pass | spec | cal     | sil     | dis     | q       | edge    | hon     | avoid   |
|------|-----------------------------------------|:----:|:----:|:-------:|:-------:|:-------:|:-------:|:-------:|:-------:|:-------:|
| 0    | baseline (post-route, mixed prose)      | 7/11 | 1.73 | **2.36**| 1.09    | 0.82    | 1.18    | 1.18    | 1.82    | 2.36    |
| 1    | tighten one disagreement passage         | 7/11 | 2.09 | 1.91    | **1.55**| 1.00    | 0.82    | 1.09    | **1.91**| 2.36    |
| (a)  | simplify floor (drop redundant length)  | 6/11 | 1.55 | 1.73    | 1.36    | 0.91    | 1.09    | 0.82    | 1.55    | 2.55    |
| 2    | axis-aligned, no brakes                 | 8/11 | 2.27 | 1.55    | 0.91    | 0.91    | **1.36**| 0.91    | 1.36    | 2.55    |
| **3**| **axis-aligned + per-directive brakes** | **9/11**| 2.18 | 1.91    | 1.27    | **1.27**| 0.82    | 0.64    | 1.45    | 2.55    |
| 4    | single mantra alone                     | 6/11 | **2.45**| 1.82 | 0.91    | 0.91    | 0.82    | 1.18    | 1.82    | 2.91    |
| 5    | pure conditional form                   | 7/11 | 1.64 | 1.55    | 1.36    | 1.09    | 0.64    | **1.27**| 1.82    | **1.91**|

Higher is better on every axis except `avoid_list_penalty` (lower better).

## Findings

### 1. Per-directive brakes recover calibration

A pure substance push (iter2: "ground in the literal record. Quote prior
detail; don't paraphrase") moves specificity strongly (+0.55) but tanks
calibration (-0.82) by inducing the model to fabricate continuity when
the record is thin. Adding an explicit conditional brake to each
substance directive — "if you can't quote it, you don't have it — say
so plainly", "when you'd be reaching, name the reach" — recovers ~half
the calibration drop (iter3 cal 1.91 vs iter2 1.55) while keeping most
of the specificity gain (iter3 2.18 vs iter2 2.27).

The brakes are the load-bearing mechanism, not the prose framing.

### 2. A single cross-cutting mantra is insufficient — abstraction failure

Iter4 tested whether one short rule ("Specific from the record, silent
on the gap. Don't bridge with wisdom.") could replace per-directive
brakes. The result was the **worst** avoid-list penalty in the campaign
(2.91, up 0.55 from baseline) — the mantra is itself a wisdom-voice
line, and the model copies the register of the prompt regardless of
what the line semantically prohibits. Mantras teach by example, not by
content; a poetic mantra teaches a poetic register.

### 3. Form-consistency moves a different axis bundle

Iter5 took iter3's brake content but rewrote every directive in pure
`When X, do Y. When not-X, do other-Y.` conditional form — no
declaratives anywhere. The avoid-list improved by 0.45 (the biggest
avoid-list win in the campaign) — confirming that **register
consistency at every directive teaches restraint**. But the same change
cost questions (-0.55) and didn't recover calibration. Pure conditional
form reads to the 35B as a rules-engine spec, and the model behaves
like one: more discipline, less engagement.

The mixed form of iter3 — declaratives for engagement axes (attention,
question) plus conditionals for brake-bearing axes (calibration,
self-honesty, edge) — held both registers in the reply. Different
axes want different forms; cross-prompt register consistency collapses
that.

### 4. Conflicting norms can reinforce, not contradict

The "simplify floor" iteration removed a redundant length norm
("1-2 short paragraphs is the default") because it conflicted with the
helper's brevity anchor ("three short sentences beat three short
paragraphs"). Result: silence regressed by 0.27. The two norms
weren't conflicting — the model was averaging between them, and that
average was the discipline.

This is a load-bearing nuance for prompt economy: **redundancy that
reinforces a constraint by averaging is not the same as redundancy
that produces actual contradiction**, even when both look like
duplication on the page. Cutting the wrong one moves the ceiling.

### 5. No single architecture wins all axes

Every iteration shifted which axis bundle the model leans into. The
pareto frontier across the campaign:

- Pass count: iter3 (mixed form + brakes)
- Avoid wisdom-voice: iter5 (pure conditional)
- Specificity peak: iter4 (mantra) — but pass count crashed
- Calibration peak: baseline (the original prose)
- Silence peak: iter1 (small targeted push)
- Disagreement peak: iter3
- Question density peak: iter2

There's no edit that strictly dominates the prior. Tuning is choosing
which axis bundle to lean into for the surface's purpose. For inner-
work specifically — a witness skill — pass count + a balanced profile
is the right objective, hence iter3 as production.

## Production state (selected)

**Iter3 — axis-aligned directives with per-directive conditional
brakes, in mixed declarative/conditional form.** Pass 9/11. Strongest
balance across substance and discipline axes. Documented in the
campaign comment block in
`sovereign-core::runtime::RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT`.

Reports archived at:
- `inner-work-base-darwin35b-postroute-2026-05-04.json` (baseline)
- `inner-work-base-darwin35b-iter1-2026-05-04.json`
- `inner-work-base-darwin35b-iter2-axisaligned-2026-05-04.json`
- `inner-work-base-darwin35b-iter3-brakes-2026-05-04.json`
- `inner-work-base-darwin35b-iter4-mantra-2026-05-04.json`
- `inner-work-base-darwin35b-iter5-formconsistent-2026-05-04.json`

## Further research

The campaign produced a coherent story but every claim above rests on
a single 11-scenario run per architecture. The empirical certainty is
modest. Specifically:

- **Run-to-run variance is unmeasured against a frozen prompt.** A
  single 11-scenario run gives roughly ±0.18 per-axis noise based on
  the deltas observed at the noise floor across iterations. Some of
  the smaller axis movements (silence ±0.18, edge ±0.09, honesty
  ±0.09) sit at-or-below that noise floor and shouldn't be treated as
  signal. The proper next move is to run the *baseline* prompt three
  times, compute axis-level standard deviation, and use that as the
  significance threshold for future iteration claims.

- **The form hypothesis (iter5) needs replication.** The avoid-list
  improvement of -0.45 is well above the noise floor and the most
  interesting finding of the campaign — but a single run doesn't
  prove form-consistency causes avoid-list improvement. It could be
  one lucky run on prompts the judge happened to score generously.
  Running iter5 three times would clarify whether the avoid-list win
  is reproducible. If it replicates, the next question is whether a
  hybrid (conditional for brake-bearing directives, declarative for
  engagement directives) preserves both the avoid-list gain and the
  question density iter3 retained.

- **Hard mode hasn't been re-run since the routing fix.** Iter0 of
  this campaign re-ran hard mode once after the routing override
  landed (`inner-work-hard-darwin35b-postroute-2026-05-04.json`,
  6/10), but no further hard-mode runs were done across iterations
  1–5. Hard mode tests the contract under adversarial pressure
  where the substance/discipline tradeoff sharpens; it's plausible
  iter5's form-consistency wins more strongly there (or iter3's
  brakes more weakly).

- **9B fast slot wasn't tested.** All campaign data is from the 35B
  primary slot (where production routes after the latency_class fix).
  The bench's history (per `bench/voice/hard/README.md`) shows the 9B
  has fundamentally different verbosity habits — better discipline,
  less substance. The same prompt iterations on the 9B might
  invert the pareto picture entirely. A small parsimony pass against
  the 9B would tell us whether iter3 generalizes or is 35B-specific.

- **The parallel `handle_simple` witness branch wasn't touched.** The
  campaign edited `RELATIONAL_EXPRESSIVE_SYSTEM_PROMPT` (used by
  `handle_expressive_query` and the streaming variant). The
  `handle_simple` witness branch (DeepQuery + Relational register)
  uses related but separate prompt assembly via
  `build_compact_relational_system_message`. DeepQuery + Relational
  ran the same baseline scoring and probably benefits from the same
  changes, but this isn't measured.

- **The judge model is the same as the chat model.** Both runs use
  Darwin-35B as both the chat model and the judge. The README's iter4
  campaign deliberately pinned the judge to a stable model
  (independent of the chat model under test) so judge variance
  doesn't get conflated with chat-model variance. Re-running this
  campaign with a separate judge slot (e.g. judge pinned to 35B
  while chat varies, or chat fixed at 35B and judge pinned to a
  smaller model) would help disentangle the two sources of noise.

- **Mixed-form discovery deserves more probes.** The strongest
  generalizable finding is that mixed declarative/conditional form
  preserves register diversity in the output. But "mixed form" is
  underspecified: which directives benefit from which register?
  Running variants where one axis at a time flips form (e.g. iter6:
  iter3 with attention rewritten as conditional only, iter7: iter3
  with question rewritten as conditional only) would map out the
  axis-form interaction matrix.

A reasonable budget for the next session: 3 runs of iter3 (variance
floor on production prompt), 3 runs of iter5 (replicate the form
hypothesis), 1 run of iter3-on-hard-mode, 1 run of iter3-on-9B. That's
8 bench cycles, ~1.5 hours of compute, and gives confident axis-level
claims for everything that's currently single-run.
