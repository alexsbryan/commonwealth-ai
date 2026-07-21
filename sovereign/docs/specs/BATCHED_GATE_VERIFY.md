# Batched grounding-gate verification — design + cost

Status: SUPERSEDED IN PART (2026-07-21) — see §8. The state-restore path (§8) achieves
the isolated variant's semantics at near-zero engine cost and measured **bit-exact**
calibration parity; the §3 per-position proposals are now the fallback, not the plan.
The study flag `SOVEREIGN_GATE_BATCH_VERIFY` (+ `SOVEREIGN_GATE_BATCH_SHADOW`) remains
measurement apparatus only.

Companion: `docs/specs/STREAMING_GATE_PIPELINE.md` (the fast-slot verify scaffold).
Origin: the 2026-07-20 35B-A3B soak latency investigation.

---

## 1. Problem

On the `qwen35moe` primary, `gate_longform` (grounding/mod.rs:1430) verifies each extracted
claim against the evidence with a **calibrated single-token forced choice**
(`claim_violation_joint` → `forced_choice_ab`, judge.rs). Each call re-sends the full evidence
(~10K tokens) as its prompt. Because **prefix caching is vetoed for this Gated-DeltaNet MoE**
(`prefix_cache_gate`, sovereign-inference/embedded/gates.rs — recurrent layers corrupt under
partial KV keep), the N per-claim calls each **re-prefill the same evidence from scratch**.

Measured (real longform turn, 12 claims, ~10K-token evidence): **per-claim 117.3s / 103,366
prompt tokens** vs a single batched prefill **12.5s / 9,116 tokens** — 11.3× less prefill,
9.4× faster. The prefill dominates; MTP (which accelerates *decode*) cannot touch it.

## 2. What was tried, and why it can't ship as-is

A cheap **text-batch** verify (`claims_support_batched`, judge.rs): evidence prefilled once,
all claims judged in one generation, emitting `"<n>: A|B"` lines. Behind
`SOVEREIGN_GATE_BATCH_VERIFY`. Two trust configurations were measured:

- **trust-both** (use the batch verdict for both directions): real speed — 4-question A/B
  showed **69→36 primary calls (1.92×), 774→601s (1.29×)**, gated on claim count
  (`gate_batch_min_claims`, default 6, since small answers regress).
- But the **shadow eval** (`SOVEREIGN_GATE_BATCH_SHADOW`: log the batch verdict AND the
  calibrated forced-choice per claim, keep baseline behavior) over **89 real claims** is
  decisive against a flip:

  | metric | value | meaning |
  |---|---|---|
  | agreement | 44% | not a faithful proxy |
  | false-fail (batch fails, calibrated passes) | **54.5%** | trust-both **over-trims** good content |
  | false-support (batch passes, calibrated fails) | 1.3% (1/89, borderline cal_vp=0.917) | small leak |
  | batch parse-gap | 13% | fall back to calibrated |

**Root cause of the divergence is structural, not tunable.** The calibrated gate fails a claim
only at `vp ≥ tau` (0.9 — deliberately permissive, needs high confidence). The binary text "B"
maps to `vp = 1.0`, so it fails everything it is even slightly unsure about → systematically
over-strict. **A binary text A/B cannot reproduce a calibrated continuous logit.**

Conclusion: **do NOT default-on the text-batch** in any trust config. The flag stays a study
apparatus.

## 3. Proposal: batched *forced-choice* via per-position logits

Keep the calibrated signal; only change *where the evidence is prefilled*.

`forced_choice_ab` is not a generation — `forced_choice_probs` (model_slot.rs:654) decodes the
prompt, reads `ctx.token_data_array()` (the live next-token logits), gathers the `{A,B}` token
logits, and softmaxes over just those. One O(vocab) read of the current logit state. The
reranker (`rerank_slot.rs`) reads a constrained logit at a chosen position the same way. So
"read a calibrated `{A,B}` distribution at a position" is proven, working code.

The batched form reuses that read across claims, prefilling the evidence **once** and walking
the claims via the KV cache:

```
decode  [evidence] + "Claim 1: <c1>\nSupported by the passages? A/B:"   ← evidence prefilled ONCE
for i in 0..N:
    vp[i] = 1 - softmax_A( forced_choice_probs(ctx, {A,B}) )   ← SAME calibrated read as today
    <advance to claim i+1>
```

Per turn: **1 × evidence prefill + N × (~claim-text decode)**, vs today's **N × evidence
prefill**. Same calibrated logit per claim, **position-indexed** — so the 13% parse-gaps and
the alignment slips of the text version vanish by construction, and the verdict is `vp` (the
calibrated continuous value), not a binary.

### 3.1 The crux: advancing to claim i+1 on a recurrent-hybrid model

Two ways to walk the claims after the shared evidence prefill:

**(a) Isolated (rollback KV to evidence-only between claims).** Truest to the current gate:
each claim is judged against evidence *alone*, identical to `claim_violation_joint`. Requires
rolling the KV back to the evidence checkpoint after each claim's read.

  - Blocker: **the DeltaNet recurrent layers.** `clear_kv_cache_seq` "doesn't rewind their
    recurrent state" (model_slot.rs:202) — this is the *same* reason prefix caching is vetoed.
    Attention-layer KV rolls back by position; the recurrent running-state does not.
  - Not categorically impossible: **MTP already does partial `seq_rm` on these recurrent
    layers** for its draft-rollback discipline, enabled by `with_n_rs_seq(>= n_draft_max)`
    (model_slot.rs:115-118, 988-991). So the machinery for constrained recurrent rollback
    exists — but it is MTP-specific and tuned to reject a few draft tokens, not to rewind an
    ~80-token claim span to a checkpoint. Adapting it (and proving the recurrent state is
    correctly restored, not merely the attention KV) is the deep, risky part — adjacent to the
    exact hazard that disables prefix caching.

**(b) Sequential (no rollback).** Decode evidence + claim 1 → read; append claim 2 → read; …
No KV manipulation, so **no recurrent-state hazard**. But claim *i* is read with claims
`0..i-1` (and their argmax verdicts) in context — cross-claim contamination the independent
per-claim gate does not have. Cheaper and safe to build; its calibration-faithfulness is an
**open empirical question** answerable by the shadow harness (does sequential `vp` match
per-claim `vp`?).

## 4. Cost estimate

Binding: **llama-cpp-4 0.4.2** (bundles llama.cpp b9982). Building blocks — the `{A,B}` logit
read, the decode loop, `seq_rm`/`n_rs_seq` — all present.

| Piece | Effort | Notes |
|---|---|---|
| `forced_choice_probs_seq` (evidence prefill + per-claim delta-decode + read loop) — **sequential** | ~1 day | reuses `forced_choice_probs` + the existing decode path |
| Request shape to trigger it (`x_forced_choice_seq` marker carrying the claim list) | ~0.5 day | mirrors the `x_forced_choice` sentinel contract |
| `gate_longform` integration (call it → `Vec<vp>` → use directly; no parse, no fallback) | ~0.5 day | simpler than today's text version |
| **Parity validation** (shadow: sequential `vp` vs per-claim `vp`, ~100 claims) | ~1 day | harness already built (scratchpad `shadow_run.py`/`shadow_analyze.py`) |
| **Sequential subtotal** | **~2.5–3 days** | ships iff parity holds |
| Isolated variant: per-claim recurrent-state rollback via `seq_rm`+`n_rs_seq` | **+2–4 days, HIGH risk** | may hit the DeltaNet wall; needs `with_n_rs_seq` on the verify slot + proof the recurrent state restores |

## 5. Payoff, honestly bounded

- **~1.3× end-to-end on longform turns, quality-neutral.** The trust-both A/B already measured
  what collapsing verify buys (1.29×); the per-position version gets that *same* speed with
  *zero* quality change (identical calibrated verdicts, if parity holds). It touches only
  **longform** answers (the >1800-char slow tail the soak flagged as the #1 latency issue),
  not short ones.
- Verify is one of several big-prefill passes. **Synthesis is irreducible**, but the
  **specifics-scan** (grounding/mod.rs, `scan_unsupported_specifics`) is a second candidate.
  Correction (2026-07-21): the scan is ONE judge pass over the whole text vs the full
  evidence — its per-item work is retrieval, not LLM calls — so its reuse win is one
  prefill, not N. The cheap version is aligning its PASSAGES scaffold byte-for-byte with
  the claim judge's so it restores off the same pin (§8).

## 6. Recommendation

1. Prototype the **sequential** variant (~2.5–3 days) and **parity-validate with the shadow
   harness** before any further investment. If sequential `vp` tracks per-claim `vp` (say
   ≥95% agreement, ~0 false-support), ship it default-on — it is calibration-faithful by
   construction and quality-neutral.
2. If sequential parity fails (cross-claim context shifts verdicts), the honest conclusion is
   that **calibration-faithful verification on a recurrent-hybrid MoE inherently re-prefills
   the evidence per claim** — the isolated variant is the only escape and it is a HIGH-risk,
   +2–4 day dive into recurrent-state rollback, adjacent to the prefix-cache hazard. Weigh
   that against a ~1.3× tail-latency gain.
3. Either way, extend whatever lands to the **specifics-scan** for the larger longform win.

## 7. Code map

- Gate + per-claim loop: `sovereign-core/.../grounding/mod.rs` (`gate_longform` 1430; call
  site ~1560; specifics-scan 1599)
- Calibrated verdict: `grounding/judge.rs` (`claim_violation_joint` 920, `forced_choice_ab` 36)
- Text-batch (study) + shadow: `grounding/judge.rs` (`claims_support_batched`),
  `grounding/config.rs` (`gate_batch_verify_enabled`, `gate_batch_shadow_enabled`,
  `gate_batch_min_claims`)
- The logit read to reuse: `sovereign-inference/embedded/model_slot.rs`
  (`forced_choice_probs` 654; forced-choice dispatch 1794)
- Recurrent-state / KV constraints: `model_slot.rs` (prefix-keep gate 196-206; `n_rs_seq` /
  `seq_rm` 115-118, 988-991); prefix-cache veto: `sovereign-inference/embedded/gates.rs`
  (`prefix_cache_gate`)

---

## 8. Update 2026-07-21 — the isolated variant ships via whole-context state restore

§3.1(a) assumed rollback-in-place (`seq_rm`) was the only checkpoint mechanism and priced
it HIGH-risk. It is not the only mechanism: **`prefix_state.rs`** (the pinned-prefix
full-state cache, `SOVEREIGN_PREFIX_STATE`, default OFF) already implements
checkpoint/restore via `llama_save_session_file`/`load_session_file` — whole-memory
serialization (attention KV **+ recurrent buffers**), restore-at-position-0, proven
bit-faithful on qwen35moe by `state_cartridge_spike.rs` (2026-07-12). Whole-state restore
is not partial rewind; the DeltaNet hazard does not apply. And the plumbing already
composes: forced-choice requests are excluded from MTP dispatch (`mtp_dispatch_eligible`,
gates.rs), so per-claim claim-checks flow through `generate_sync`, where the
`prefix_state.plan()` restore runs BEFORE the forced-choice logit read.

**One code change was needed** (landed with this update): `gate_longform` built each
claim's judged evidence as `extra ++ chunks` — claim-conditioned hits FIRST — which
diverged the per-claim prompts at the first passage and defeated every prefix-reuse
scheme (including §3's own variants, silently). Now `chunks[..per_claim_chunks] ++ extra`:
same judged set, byte-stable shared prefix; novel hits append after the shared window and
still widen the cap.

### Measured (de-risk step 1, 2026-07-21, live daemon, qwen35moe MTP, debug build)

A/B on the four 2026-07-20 questions, same binary, `SOVEREIGN_PREFIX_STATE` daemon-env
being the only delta (`scratchpad/arm_runner.py`, `compare.py`):

| metric | OFF (per-claim, reordered) | ON (state restore) |
|---|---|---|
| e2e latency (4 questions) | 786.3s | 584.5s (**1.35×**) |
| primary prefill tokens | 140,155 | 47,165 (**3.0× less**) |
| restore latency | — | 13–28ms @ 1.4–2.5K-token pins |
| state save (once/question) | — | 275–570ms |
| restore failures / rc≠0 | 0 / 0 | 0 / 0 |

OFF reproduced the 2026-07-20 baseline (774s/69 calls → 786s/73), so the reorder alone is
behavior-neutral. Per-question verdict-count differences between arms tracked synthesis
draft variance (both directions), not a gate bias.

**Calibration parity: bit-exact, measured** (`scratchpad/parity_test.py` — same
claim-check prompt via full prefill then via restore, production wire shape):
full-prefill `{"A":0.00039405946,"B":0.999606}` == restore == repeated restore, and the
learn-path (two-stage) decode is float-identical too. 9.6s → 0.3s per claim-check at
~2K-token evidence. The §3.1(b) sequential variant's open parity question is MOOT for
this path — there is no cross-claim contamination and no recalibration.

### Observed costs / fringe behavior (inputs to default-on)

- **State files ~64KB/token**: 641MB on disk after the 4-question session (LRU cap 6 per
  slot). 10K-token evidence → ~640MB/file. Needs a startup sweep of stale-`<pid>` dirs
  (they are not cleaned on exit) and possibly an in-RAM variant before default-on.
- **Auto-learn costs 2 full prefills per question** (sighting → learn), and the pin can
  land a few tokens INTO shared claim-opening text, causing occasional relearn churn
  (q4: 3 LEARNED events; parity test: claim Z re-learned). A **caller-directed pin**
  (request extension marking the evidence boundary the gate already knows; mirrors the
  `x_forced_choice` sentinel contract, ~0.5–1d) removes both.
- Payoff scales with evidence size: these questions carry 1.4–2.5K-token pins; the soak's
  ~10K-token persona-vault turns (the original 16–21s-per-claim finding) are the big-win
  case and remain to be soak-validated.

### Revised recommendation

1. ~~Prototype sequential~~ **DONE differently**: the state-restore path is validated
   end-to-end (this section). Sequential per-position walk (§3) is deprioritized to
   "only if restore overhead ever matters at scale".
2. Next: **caller-directed pin** (kills the second prefill + relearn churn), then a
   **desktop-soak A/B** (`scripts/desktop-soak.py`) on the 10K-evidence persona corpus to
   gate default-on, plus stale-pid state-dir sweep.
3. Align the **specifics-scan** PASSAGES scaffold with the claim judge's so it restores
   off the same pin (§5 correction: it is one pass, so the win is one prefill).

---

## 9. Update 2026-07-21 (later) — caller-directed pin + 180-min treatment soak

**Caller-directed pin SHIPPED.** `CompletionRequest.stable_prefix_len` (BYTES of the raw
user prompt shared byte-identically across siblings; advisory) → oicp-client forwards the
wire field `stable_prefix_len` → `inference_adapter` unwraps → `directed_pin_tokens`
(model_slot.rs) maps bytes→tokens by tokenizing the rendered prefix and taking its LCP
with the full stream minus a 2-token BPE back-off → `prefix_state.plan_directed` learns
IMMEDIATELY on first sighting, restores on entry match, falls back to the sighting-based
plan on any malformed directive. The gate declares ONE uniform boundary per turn — the
shared evidence window (`judge::stable_passages_prefix_len`, ordering invariant pinned by
`stable_prefix_is_shared_across_sibling_prompts`) — so extras-bearing claims restore too.

Validated live: learn-on-first-call with bit-identical vp; extras sibling HIT (0.6s vs
full prefill); the §8 relearn-churn case now restores; real gate turn = **1 LEARNED +
12 HIT + 0 WARN**, exactly one evidence prefill per turn. Gates: lint 0-fail, suite
7829/2→both isolated-pass (one over-strict new-test assertion, fixed; one known
env-race flake in `frontdoor` untouched by this diff).

**180-min dual soak, treatment arm only** (`soak-prefixpin-180`, flag + pin + reorder;
baseline = 2026-07-20 dual150):

- **Stability/correctness: PASS.** rc 0/0; 0 raw errors, 0 broke turns, 0 confirmed
  fabrication (chaos), 0 persona hallucinations — identical to baseline. Mechanism at
  scale: **76 LEARNED / 253 HIT / 0 WARN**; restore median 19ms, p90 29ms, max 177ms;
  save median 388ms, p90 594ms.
- **Latency: soak axes are question-mix-confounded; class-level + persona signals agree
  with the controlled A/B.** Chaos invents its mix: grounded chats 6→1, decline-ish
  verdicts way up, and declines run 4.6× slower than grounded on a path this change does
  not touch — so overall median 34.1→66.8s says nothing about the gate. Where the mix is
  fixed (personas): **TTFT p50 173s→66s, time-to-value median never→99s**, turns/session
  2.0→2.7. Grounded-class chaos latency 66.5s→27.1s (right direction, n=1 — indicative
  only). The §8 controlled A/B (1.35× e2e, bit-exact parity) remains the latency
  evidence of record.
- **Cost:** steady-state state dir ≈ **3.9GB for one slot** (6-entry LRU × ~650MB
  10K-token pins). Stale-`<pid>` dirs from restarts accumulate on top (manually swept
  this session).

**Default-on recommendation:** flip `SOVEREIGN_PREFIX_STATE` default ON after two small
hardenings: (1) startup sweep of stale-pid state dirs; (2) byte-capped LRU (or in-RAM
store) so the on-disk footprint is a config knob, not an emergent ~4GB/slot. Nothing in
3.5h of adversarial soak argues against the flip on correctness or stability grounds.

**Next latency target (from this soak's own data):** the DECLINE path — slow abstention
(declines 4.6× slower than grounded; 25 true stalls concentrated there). That is the
gap-check / coverage / refine ladder, untouched by this work.
