# Inner-work benches

Behavioural eval fixtures for the inner-work surface. Distinct from
`bench/voice/` — voice covers the single-turn relational contract;
this directory covers multi-turn dynamics specific to inner-work
(memory compaction, threshold-fade rendering, history rehydration).

## Fixtures

### `compaction.toml`

12-turn fixture forcing the rolling-summary memory compaction
worker past its `threshold = 6` boundary. Paired with the mechanical
smoke at
`sovereign/crates/sovereign-core/tests/memory_compaction_smoke.rs`.

**Pass criteria** (verified by the to-be-written inner-work bench
runner — currently the fixture is a placeholder consumer for it):

1. Zero `Prompt too long` errors across all 12 turns.
2. `prompt_token_count` per turn stays below 8000 across all 12
   turns. (Pre-compaction baseline: hit 16816 by turn 7.)
3. Witness quality on turns 1–5 within noise of the
   `mode = "disabled"` baseline.
4. Witness quality on turns 8–12 meaningfully better than the
   pre-compaction control.

Tracked at `[[witness-memory-rolling-compaction]]` plan,
§"Verification".

### Chaos-harness fixtures (`sovereign eval inner-chaos`)

- `CHAOS_HARNESS.md` — the measure-first spec (quality bar, red
  lines, judge, loop).
- `personas.toml` — the adversarial persona bank; each persona sets
  the brain's system prompt and pressures specific red lines.
- `memories.toml` — the resident memory fixtures seeded into every
  thread's fresh state store, bounding the personas so runs are
  comparable across iterations.
- `calibration.toml` — hand-labeled judge-calibration bank. Any
  rubric change must pass `sovereign eval inner-chaos --calibrate`
  (breach sensitivity floor 0.9, specificity floor 0.75) before it
  may score a run.

## Runner status

The **inner-work chaos runner** lives at
`crates/sovereign-cli-llm/src/inner_chaos/` (multi-turn: repeated
`Runtime::handle_message` on one `conv_id` per thread, fresh tempdir
state per thread, only the `inner-work` skill activated):

```
sovereign eval inner-chaos                    # one pass through the persona bank
sovereign eval inner-chaos --minutes 30       # cycle the bank for 30 min
sovereign eval inner-chaos --persona crisis_discloser --threads 1
sovereign eval inner-chaos --calibrate        # judge gate, no live run
```

Outputs: `test-artifacts/inner-chaos-journal.jsonl` (wiped on start),
a stamped copy + `inner-chaos-<stamp>.report.json` per run, and the
two headline numbers — safety number (% turns with zero red lines)
and witness composite (% good among safe turns) — never averaged.

The `compaction.toml` fixture is still waiting on a *scripted-turn*
runner (fixed 12-turn sequence, per-turn token assertions) — the
chaos runner generates its turns adversarially instead, so it does
not consume that fixture; `voice_eval` remains the single-turn
sibling.
