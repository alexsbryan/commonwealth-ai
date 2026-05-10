# Team-pipeline architecture: experimental rejection (2026-05-03)

**Verdict: REJECT.** The "Situated Team → Presenter" five-stage chat pipeline
([plan](../../../../.claude/plans/there-s-a-fast-slot-delightful-peach.md))
was implemented end-to-end (Phases 1–4) and tuned across 10 iterations of
prompt engineering on the Presenter. The full A/B against the legacy
single-pass chat path showed the architecture as a net regression on every
named success criterion. The kill-switch (`SOVEREIGN_TEAM_PIPELINE`,
default-off) is the gate; the underlying modules (`pipeline/curator.rs`,
`pipeline/presenter.rs`, etc.) are kept as research scaffolding behind that
gate.

## Hypothesis

Per the plan's context: a user asked Sovereign "what's the difference between
objectivism and subjectivism?" The system retrieved appropriate SEP chunks but
the Primary slot, handed all 20 chunks at once, "got tangled reasoning across
positions and clipped the response mid-stream." The team pipeline was designed
to fix this by having the Fast slot do heavy assembly (Curator → per-section
budget) so the Primary slot draws inside a tight, structured task.

Expected outcome: same or better quality on the existing voice bench
("must not regress"); structured, non-tangled, non-clipped output on the
synthesis case that originally failed.

## Method

Same daemon, same models (`Qwen3.5-9B-vOP.Q5_K_S` chat,
`FINAL-Bench_Darwin-35B-A3B-Opus-Q6_K_L` judge), sequential runs of
`sovereign voice eval --all` with `SOVEREIGN_TEAM_PIPELINE` flipped between
`0` (legacy) and `1` (team iter10).

Reports:
- `ab-legacy-base.json` / `team-iter10-small.json`
- `ab-legacy-hard.json` / `ab-team-iter10-hard.json`
- `/tmp/synthesis-bench/ab-legacy-synthesis.json` / `ab-team-synthesis-rerun.json`

iter10 was the final architectural shape after 10 Presenter iterations:

| Stage | Slot | Setup |
|---|---|---|
| Curator | Fast | schema-constrained JSON, per-section budgets, sufficiency verdict |
| Drafter (Relational) | Primary | `RELATIONAL_BASE_SYSTEM_PROMPT` (witness contract), 240-token cap on passthrough packages |
| Drafter (Factual) | Primary | general prompt, section-budget cap |
| Presenter | Primary | pure 2-example few-shot, 1024-token cap, temp 0.3, `enable_thinking=false` |
| Code-side strip | — | think tags, `---`, `**Rewritten:**`/`**Analysis:**`/etc. labels, "Let me analyze"/"Looking at the"/"Key considerations" preambles, trailing meta lines |

## Results

### Pass counts

| Suite | Legacy | Team iter10 | Δ |
|---|---|---|---|
| Base voice (12 scenarios) | **9/12** | 4/12 | **−5** |
| Hard voice (8 scenarios) | **4/8** | 3/8 | −1 |
| Synthesis (free will / determinism) | clean, 3920 chars, 38s | run 1 clean (3839 chars, 71s); **run 2 broken** (1648 chars, leaked `<draft>` tag, clipped at Section 0, 155s) | same or worse |

Plan's pre-merge rule: `Δ pass count ≤ −1`. Base breach is **−5** (5× the
threshold).

### By check (base)

| Check | Legacy | Team | Δ |
|---|---|---|---|
| length | 10/12 | 7/12 | −3 |
| question_density | 11/12 | 11/12 | 0 |
| banned_phrases | 12/12 | 11/12 | −1 |
| required_content | 8/9 | 5/9 | −3 |

`required_content` is the witness-anchor signal ("only one mention", "you told
me on March 12"). The Presenter pass paraphrases these away.

### By check (hard)

| Check | Legacy | Team | Δ |
|---|---|---|---|
| length | 4/8 | **7/8** | **+3** |
| question_density | 7/8 | 8/8 | +1 |
| banned_phrases | 8/8 | 8/8 | 0 |
| required_content | 8/8 | 4/8 | **−4** |

The one place the team pipeline *wins*: length on hard scenarios. The
Curator's per-section budgets do constrain the Drafter on adversarial probes.
This is the one ingredient that earned its keep — see "What stays" below.

### Judge axes (mean over 12 scenarios, base)

| Axis | Legacy | Team | Δ |
|---|---|---|---|
| right_attention | 2.17 | 2.25 | +0.08 |
| right_specificity | 2.17 | 1.67 | **−0.50** |
| right_calibration | 2.17 | 2.00 | −0.17 |
| right_question | 0.67 | 1.08 | +0.42 |
| right_silence | 1.25 | 1.50 | +0.25 |
| right_disagreement | 1.08 | 0.75 | **−0.33** |
| right_edge | 0.92 | 1.50 | +0.58 |
| right_self_honesty | 1.42 | 2.58 | **+1.17** |
| avoid_list_penalty (lower better) | 2.58 | 3.25 | **+0.67** |

The `right_self_honesty` win is partly an artifact: the team pipeline more
reliably emits "I don't have that in the record" because the Drafter's
witness contract + the Curator's `Insufficient` verdict default to that
shape. It's not necessarily *substantively* more honest — it's mechanically
more cautious.

### Latency

- Base p95: legacy 52s → team 184s (**+253%**)
- Hard p95: legacy 73s → team 199s (+172%)
- Synthesis: legacy 38s → team 71–155s (**2–4×**)

The 2× Primary-slot calls per turn (Drafter + Presenter both on Primary)
account for most of the gap.

## The decisive datapoint

**The original motivating failure is no longer reproducible on legacy.**
Legacy handles "is free will compatible with determinism?" cleanly in 38s
with a structured 3920-char exposition that names compatibilism, hard
determinism, libertarianism, and Christian List's compatibilist libertarianism
*without tangling positions and without clipping mid-stream*.

Whatever broke when the plan was written got fixed during the iteration loop
itself — likely a combination of (a) better Drafter prompts (the
`RELATIONAL_BASE_SYSTEM_PROMPT` was iterated heavily during the same window),
(b) sane `max_tokens` defaults on the runtime path, and (c) atlas / SEP
retrieval tuning that landed in March-April 2026.

The team pipeline is now solving a problem that no longer exists, while
regressing 5/12 on the explicit regression bar.

## Iteration loop summary

10 iterations of Presenter prompt engineering, each addressing the previous
failure mode and surfacing a new one:

| iter | move | base | notes |
|---|---|---|---|
| 0 | initial: Drafter (general) + Presenter rewrite (Fast slot) | 5/12 | length blowouts, avoid-list contamination |
| 1 | verbose RULE 1/2 prompt | 4/12 | model emitted `**Analysis:**` blocks introspecting on the rules |
| 2 | shorter rule list with avoid-list strings | **0/12** | model copied avoid-list strings verbatim into output |
| 3 | witness contract on Drafter, minimal Presenter | 4/12 | meta-narration on complex drafts |
| 4 | Presenter on Primary + RELATIONAL_BASE on Presenter | 2/12 | 10/12 length blowouts (model produced full witness essays) |
| 5 | + user-message anchor + 200-token cap | 0/12 | wisdom-voice padding within cap |
| 6 | 3-step procedure + worked example | 0/12 | model emitted numbered analysis matching procedure structure |
| 7 | pure few-shot, no procedure | 2/12 | mirrored Drafter's analytical output |
| 8 | iter3+iter7 hybrid (witness on Drafter, few-shot Presenter) | 3/12 | best avoid_list_penalty (2.25); some clean responses, some truncated |
| 9 | + tighter Drafter cap + extended artifact stripper | (skipped) | folded into iter10 |
| 10 | + generous Presenter cap (1024) for synthesis | **4/12** | best non-iter0 result, but still −5 vs legacy |

Cross-iteration lessons (worth keeping even though the architecture didn't):

1. **Listing avoid-list strings in a rewrite prompt** causes small
   open-weight models to copy them verbatim into the output (in-context
   examples).
2. **Numbered procedural steps in a prompt** cause the model to emit
   numbered analysis as visible output ("Let me analyze: 1. ... 2. ...").
3. **The witness contract works as a generation constraint** (legacy
   single-pass) but **fails as a rewrite constraint** (Presenter on the
   Drafter's draft).
4. **Mechanical artifact stripping must live in code**, not prompt — listing
   strip targets in the prompt teaches the model to narrate the cleanup task.
5. **Composing two LLM passes (Drafter → Presenter) on the same Primary
   model** doubles latency without doubling quality — the Presenter rewrite
   loses anchors more than it adds value.

## What stays, what goes

### Stays (research scaffolding behind the kill-switch)

- `pipeline/curator.rs` + the per-section budget logic — the one
  measurable team-pipeline win was on hard-set length discipline
- `pipeline/stages.rs` — `CuratedPackage`, `Sufficiency`, `DraftBudget` types
- `pipeline/presenter.rs` + `strip_presenter_artifacts` — the artifact
  stripper is genuinely useful as a post-processing helper, independent of
  the Presenter LLM call
- `pipeline/judge.rs` — single source of truth for the voice judge request /
  score type, used by both team-pipeline and `voice_eval`
- The `NarrationPhase` stage frames in `runtime.rs::types` — useful for any
  future glass-box surface even without the team pipeline
- The kill-switch wiring in `runtime.rs` (two branches in
  `handle_message_stream` and `handle_turn`) — already gated, no harm in
  keeping reversibility

### Goes (when convenient)

- The plan's "default-on" target should NOT be flipped. Update
  `is_team_pipeline_enabled` doc comment to reflect the rejection.
- If the Curator's section-budget logic is wanted on the legacy path,
  extract it into a standalone helper rather than reviving the full
  pipeline.

## How to reproduce this finding

```bash
# Ensure daemon is running with current sovereign-cli
sovereign daemon status

# Legacy
SOVEREIGN_TEAM_PIPELINE=0 sovereign voice eval --all \
  --chat-model Qwen3.5-9B-vOP.Q5_K_S \
  --judge-model FINAL-Bench_Darwin-35B-A3B-Opus-Q6_K_L \
  --report bench/voice/baseline/legacy-base.json

# Team pipeline
SOVEREIGN_TEAM_PIPELINE=1 sovereign voice eval --all \
  --chat-model Qwen3.5-9B-vOP.Q5_K_S \
  --judge-model FINAL-Bench_Darwin-35B-A3B-Opus-Q6_K_L \
  --report bench/voice/baseline/team-base.json

# Diff
python3 bench/voice/baseline/diff_report.py \
  bench/voice/baseline/legacy-base.json \
  bench/voice/baseline/team-base.json
```

Same shape for `--scenarios-dir bench/voice/hard/` to reproduce the hard A/B.

## Conclusion for future hands

If you're reading this because you're considering reviving, expanding, or
deleting the team-pipeline code: this architecture was tested at 10
prompt-engineering iterations on the Presenter and found to net-regress
against the legacy single-pass chat path. The original motivating failure
(synthesis tangling) is no longer reproducible on legacy. Don't flip the
default. If you want to delete the pipeline modules entirely, do it — but
preserve `strip_presenter_artifacts` (useful generally) and consider
extracting the Curator's section-budget logic as a standalone helper before
tearing out the rest.
