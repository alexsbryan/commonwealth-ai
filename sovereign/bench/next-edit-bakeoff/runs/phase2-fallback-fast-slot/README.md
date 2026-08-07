# Phase 2 — the model the fallback actually serves off

**Date:** 2026-08-07 · **Verdict:** `stay opt-in` (the eval's own line)

Phase 1 asked "can a chat primary serve next-edit?" and answered yes
(21/30 useful, 0 wrong, p95 2576 ms on a 35B-A3B). This phase asks the
question that actually governs `SOVEREIGN_NEXT_EDIT_FALLBACK`, because
`install_fallback_next_edit_slot` targets `ModelsSection::fast_path()`
— the explicit `[models].fast` GGUF when one is set, and only the
primary when none is. **On any box with a distinct fast model, the
answering model is the fast slot, not the primary.**

## What ran

Unlike the Phase 1 arms — standalone `llama-server` per arm with
`--reasoning off` — this ran **end to end through the production
daemon**, which is the path a user is actually served by:

```
# daemon, flag armed, no [models.edit] configured
SOVEREIGN_NEXT_EDIT_FALLBACK=1 target/debug/sovereign-cli-daemon daemon run

# the shim serves the daemon's real ConsultPlan pipeline over that model
target/debug/examples/next_edit_score \
    --upstream http://127.0.0.1:9741 \        # BASE url — it appends /v1 itself
    --format region_instruct \
    --model-id Qwopus3.5-4B-v3-MTP-Q8_0 \
    --port 9799 --force-consult --timeout-ms 20000

python3 scripts/next_edit_gen_eval.py --endpoint http://127.0.0.1:9799
```

`[models].fast = Qwopus3.5-4B-v3-MTP-Q8_0` (operator preference,
swapped from `Qwen3.5-2B.Q6_K` the same day). Bank:
`gym/next-edit/gen/cases.jsonl`, 60 cases, consult gate forced open.

## Result

| gate | fast slot (4B) | 35B primary (Ph1) | sweep-1.5b (Ph1) |
|---|---|---|---|
| GM4 usefulness | **FAIL 14/30** | PASS 21/30 | PASS 19/30 |
| GM3 wrong-edit | PASS 0 / 17 fires | PASS 0 / 25 | PASS 0 / 26 |
| GM5 latency p95 | PASS 2194 ms | 2576 ms | 828 ms |
| GM1 structural | PASS 0 malformed | PASS 0 | PASS 0 |
| GM2 gate | FAIL 11/60 | FAIL | FAIL |

By category (fired / correct out of 10): `signature_fanout` 9/9,
`field_init` 5/5, `casing_variant` 0/0, `param_insert` 0/0,
`gate_negative` 3/2, `model_negative` 0/0.

## What it means

**The 21/30 does not transfer down a model class.** 14/30 is 47%
usefulness against the primary's 70%. The fallback is also *not*
meaningfully faster (2194 vs 2576 ms) — a 4B dense on this lane costs
about what a 35B-A3B MoE does, because ~3B active is the same decode
work. So the fallback buys neither the quality nor the speed a reader
of Phase 1 alone would have assumed.

It stays **safe**: 0 wrong edits across 17 fires, 0 malformed. That is
why the posture is opt-in rather than deleted — for a user with no edit
model the alternative is no feature, and this never proposes a wrong
edit. But it is not a default.

**GM2 is not a model result.** The gate runs *before* the model, so its
verdict is identical across every arm and says nothing about the
weights. GM4/GM3/GM5 are the model's half.

**Read GM4 as a ceiling.** `--force-consult` bypasses the consult gate,
which makes usefulness an upper bound and safety a lower bound (the
scorer's own banner says so). Both phases were measured identically, so
the comparison is fair; the absolute number is not what the shipped
gate would deliver.

## Thinking suppression: verified, not assumed

Zero `truncated` drops across 60 cases (17 noop, 6 invalid, 20
inconsistent, 17 fired). With reasoning ON, Phase 1 truncated *every*
case. A direct probe on the same slot:

| | finish | content | latency |
|---|---|---|---|
| `enable_thinking=false` + `think_budget=0` | `stop` | `READY` | 526 ms |
| model default | `stop` | `"The user wants me to reply with exactly the word: READY. Thi…"` | 1114 ms |

Unsuppressed, the reasoning leaks into `content` — which the next-edit
parser would take as the rewrite. `ConsultPlan::suppress_thinking` is
exercised end to end through the daemon transport.

## Also confirmed here

`Qwopus3.5-4B-v3-MTP-Q8_0` probes as `fim_style: qwen_coder`, so the
fallback lit the **FIM lane for free** as designed — and it genuinely
serves: `def add(a,b): return ` → ` a + b`, `console.log(` →
`` `Hello ${name}!`) ``, 292–456 ms. Qwen3-family models are FIM-trained,
so the vocab probe is not a false positive here. (One prompt with a long
suffix returned empty; four others were correct. One probe is not a
measurement.)

`verify_fallback.py` in this directory is the 12-claim end-to-end
structural check (slot installed, degraded flag, advice nudge, mirror
parity, residency, lane gating). It passes 12/12 armed and 4/4 with the
flag off.
