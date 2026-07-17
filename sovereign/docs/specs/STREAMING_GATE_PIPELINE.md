# Streaming grounding-gate pipeline — overlap verification under the draft

Status: **Phase A scaffold built + measured — premise DISPROVED on single-GPU**
(2026-07-17). The flag-gated scaffold (`SOVEREIGN_GATE_PIPELINE`) is net-negative
here because the 4B and 35B slots timeshare the one Metal GPU (see "Empirical
result" below). Kept as a measurement instrument; do NOT proceed to Phase B/C on
this hardware. Spec for the next latency pass on the longform grounding gate,
following the surgical-rewrite work
(commit `perf(grounding): surgical longform rewrite default-on`). It supersedes
nothing yet — it describes the target state and a phased path there.

Owner touchpoints: `sovereign-core/src/runtime/streaming.rs`,
`sovereign-core/src/runtime/grounding/{mod.rs,surgical.rs,judge.rs}`.

## Why

A measured, timestamped trace of one corpus-grounded longform answer (~90–103s
after the surgical-rewrite fix; ~124s before) attributes the cost as:

| stage | ~time | pipelines? |
|---|---|---|
| router + query-build + cold warmup | ~20s | (cold-start; mostly gone warm) |
| retrieval | ~0.5s | already fast, not the bottleneck |
| **draft synthesis (35B, streamed)** | **~44s** | it IS the stream — the floor |
| audit #1 (extract_claim_list + per-claim verify + specifics_scan, 35B) | ~15s | **yes — this is the prize** |
| rewrite/surgical | ~5s | yes |
| full re-audit (extract + per-claim + scan again) | ~15s | yes |

The gate today is a **serial barrier**: the draft streams into a held buffer,
and only *after the last token* does the whole ~15–88s gate run. But the draft
is the longest stage and the per-sentence verification is independent of the
tokens still being generated. Overlapping them hides the audit under the draft.

This is the "process each stage as a flow, block only for final presentation"
reframe: the verification flows alongside the draft; a single final barrier
holds for the holistic check + assembly.

## Current architecture (grounded in code)

Both the KnowledgeQuery and DeepQuery spawns share this path in `streaming.rs`:

```
run_synthesis_stream            (streaming.rs:74)
  'synth token loop             (streaming.rs:107)
    StreamFrame::Token(chunk) → full_text.push_str(chunk)   [HELD, gate_on]  (:132)
    throttled heartbeat → SynthBeat{tokens, delta}          → CounterCard "answer forming"
  ── loop ends: DRAFT COMPLETE ─────────────────────────────────────────────
gate_held_answer                (streaming.rs:525)
  pre-gate citation snap: attribute_citations(full_text)               (:549)
  gate_answer_with_progress(std::mem::take(full_text), …)  ◀ SERIAL BARRIER (:571)
      → grounding/mod.rs: gate_answer → gate_longform
          extract_claim_list → per-claim claim_violation_joint (∥) → specifics_scan
          → rewrite | surgical → FULL re-audit
      → emits ClaimCheckStart / ClaimVerdict / ClaimCheckComplete
        via gate_progress_tx → CounterCard verification panel
  present_answer(outcome.text)                                          (:593)
  post-gate citation attribution                                       (:602)
  ── release full_text ──
```

The infrastructure the pipeline needs already exists here: an async token loop,
a throttled progress sink running alongside it, spawned tasks, and the
gate-progress channel that drives the CounterCard.

## The load-bearing constraint: the holistic scan stays the barrier

The surgical-rewrite round tried a "scoped re-audit" that verified only changed
spans and skipped the holistic `specifics_scan`. It was faster but **leaked a
GK-caveated fabrication** the holistic scan catches (calibration 2026-07-17,
CONFAB-LEAKED 0→1) and was reverted. See
`project_grounding_gate_surgical_rewrite_2026_07_16` memory.

The lesson is binding here: **per-sentence verification may overlap the draft,
but the whole-text `specifics_scan` (and GK-caveat stripping) MUST run once as a
final barrier.** It is whole-text by nature — it catches fabricated specifics the
per-sentence view misses — so it cannot pipeline. Keeping it as the single
post-draft barrier is what makes the pipeline safe. This barrier IS the "block
only for presentation" point.

## Key design decision: verification granularity

Today `extract_claim_list` selects the most load-bearing claims *across the whole
answer* (budget-limited, sampled across sections). Pipelining forces a shift to
**per-completed-sentence** verification using `claim_violation_joint` — the same
primitive the surgical path already uses per sentence. Sentence granularity is
strictly more coverage (every sentence checked, not a sampled subset); the thing
it loses (holistic cross-sentence fabricated specifics) is exactly what the final
`specifics_scan` barrier recovers. Reuse `surgical::split_sentences` (lossless
split) for boundary detection.

## Phased plan

Each phase is independently flag-gated, measurable, and calibrated before it
advances. Longform-only — the short (non-longform) gate path is already ~one
verify and keeps its current synchronous path; no pipeline overhead there.

### Phase A — pipeline the verification under the draft stream
- In `run_synthesis_stream`'s `'synth` loop, track a sentence-boundary cursor
  over `full_text` (via `split_sentences`). When a sentence completes, hand it to
  a bounded `buffer_unordered(K)` verification stream: `claim_violation_joint`
  on the fast slot (Judge envelope), keyed by sentence index + a claim-conditioned
  corpus search.
- Collect verdicts as they return; the draft loop and the verifiers run
  concurrently. By draft-end, all-but-the-last-few sentences are already judged.
- Factor the pipeline into a shared helper so both spawns use it; gate behind
  `SOVEREIGN_GATE_PIPELINE`.
- **Win:** audit #1's ~15s hides under the 44s draft.

### Phase B — pipeline the fixes
- As a sentence's verdict returns *failed*, run the surgical fix (delete when no
  corrective evidence, else a fast-slot edit) concurrently — the fix overlaps the
  draft too. The separate "rewrite → re-audit" collapses into inline
  verify-and-fix; the over-deletion + unmappable fallbacks from `surgical.rs`
  still apply.
- **Win:** the ~20s rewrite + re-audit tail largely disappears.

### Phase C — the final barrier + frame contract
- After the draft loop: run the holistic `specifics_scan` + GK-caveat strip once
  over the assembled text, apply any late fixes, reassemble sentences in order,
  then `present_answer` + post-gate citation attribution (unchanged).
- Rewire the CounterCard `ClaimCheckStart`/`ClaimVerdict` frames to grow
  incrementally as sentences verify — claims appear as the answer forms, a better
  sushi-counter, but a deliberate UX-contract change (frame ordering, panel
  growth). Preserve the fire-and-forget `try_send` drop-on-full semantics.

## Empirical result — Phase A scaffold (2026-07-17): premise DISPROVED here

Built the Phase A scaffold behind `SOVEREIGN_GATE_PIPELINE` (`grounding/pipeline.rs`
`StreamingVerifier`; tap in `run_synthesis_stream`) — verify completed sentences
on the fast slot during the draft, verdicts glassbox-logged, NOT yet consumed.
E2E on the longform Karamazov probe:

- `gate.pipeline: verified=34 unsupported=20 tail_ms=36268` — **36s of
  verification BACKLOG after draft-end**. The overlap did not happen.
- Wall **271s vs ~103s baseline — ~2.6× SLOWER**.

Root cause: the fast slot is NOT free during the draft on a single Metal GPU.
Direct slot-concurrency test:

| workload | time |
|---|---|
| 35B (400 tok) alone | 9.4s |
| 4B (400 tok) alone | 8.2s |
| both concurrent | 14.6s |

Concurrent (14.6s) beats pure-serial (17.6s) by only ~17% — the slots timeshare
the one GPU. So 34 concurrent 4B verifications during a 44s draft (a) steal
compute from the 35B draft, slowing it, and (b) can't keep pace (the 36s tail).
The generative audit passes (`extract_claim_list`, `specifics_scan`) are on the
35B and would contend even harder.

**Verdict: Phase A's "verify for free on the idle fast slot" premise is FALSE on
shared-GPU hardware — the pipeline is net-negative here.** The serial gate
(surgical + full re-audit, ~90–103s) is near-optimal given the single-GPU
constraint. The scaffold stays flag-gated OFF (zero production impact) as the
measurement instrument. Revisit ONLY if the box gains genuine parallel verify
compute (a separate accelerator, or a much cheaper verify), or if a MODEST
variant (verify a few paragraph-level claims, not 34 sentences) measures
net-positive. Do NOT proceed to Phase B/C on this hardware.

## Expected win (grounded, PRE-empirical — see the disproof above)

- Now: draft(44) + [barrier: audit 15 + surgical 5 + re-audit 15] + router(~20) ≈ **90–103s**.
- Pipelined: draft(44, verify overlapping) + [barrier: holistic scan ~6 + last-sentence tail ~5] + router(~20) ≈ **~75s**; **~55s** on a warm router.
- The floor becomes the draft (44s). Going below it needs a different lever
  (shorter answers, or cascade-drafting the answer on the 4B — out of scope here).

## Risks and mitigations

- **Granularity shift** → mitigated by the mandatory holistic barrier (the safety floor).
- **Preserve gate semantics** — GK-caveat strip, entity-anchored short path, the
  citation snap/attribution passes, refusal-retry, and the fail-open contract
  (`gate_answer` never turns a judge hiccup into a refusal) must all survive the
  refactor. The pre-gate citation snap (`streaming.rs:549`) moves to run
  incrementally or at the barrier.
- **Two spawns** — one shared helper, not two divergent copies.
- **Backpressure** — the verify stream must not stall the token loop; verifiers
  are spawned, the loop never awaits them inline.
- **Cancellation** — the `'synth` loop's biased cancel (`streaming.rs:114`) must
  also abort in-flight verifiers.

## Regression gate (non-negotiable before default-on)

Same methodology as the surgical round: the detached, reaper-immune calibration
harness (`launch-surgical-calibration.py` shape), FORCED longform
(`SOVEREIGN_LONGFORM_CHARS=0`), A/B pipeline-OFF vs pipeline-ON over:
- `secret_agent` (43 Q, fairness contract) — the `--grounding-verify`-scoreable
  fabrication-leak metric (hallucination-rate, CONFAB-LEAKED, grounding-fidelity).
- a longform-stress bank — heavy surgery/verification; spot-check only (the
  `[gv]` critic can't score longform answers — "out of gate scope").

Pass condition: pipeline-ON matches OFF on hallucination-rate / CONFAB-LEAKED /
grounding-fidelity. Note the CONFAB metric is noisy at small N — trust it only
for gross moves, and back it with content spot-checks (both OFF and ON fabricate
occasionally on hard absent questions; neither should be clearly worse).

## Open decisions

1. **Granularity unit** — sentence vs completed-paragraph. Sentence is simpler and
   matches the surgical primitive; paragraph keeps claim-list "load-bearing"
   selection but is coarser. Start sentence; revisit if the verify volume is high.
2. **Frame contract** — grow the panel incrementally (Phase C) or keep the batch
   `ClaimCheckStart` at the barrier for v1 and only pipeline the compute. The
   latter ships the latency win without a UX change; do that first.
3. **Draft floor** — whether to also cascade-draft on the 4B (attacks the 44s
   floor) is a separate initiative, explicitly out of scope here.

## References
- `project_grounding_gate_surgical_rewrite_2026_07_16` memory (attribution,
  the scoped-re-audit leak, calibration methodology + limitations).
- `sovereign/docs/GROUNDING_GATE_ENV.md` (gate env flags).
- `sovereign/docs/CHAOS_MEASUREMENT_REDESIGN.md` (why the scorer can't see
  longform; measurement discipline).
