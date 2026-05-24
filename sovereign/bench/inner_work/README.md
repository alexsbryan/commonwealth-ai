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

## Runner status

No runner today. The fixture is authored so that when the inner-work
bench runner lands, it has a load-bearing first input. The voice
bench runner under `crates/sovereign-cli-llm/src/voice_eval/` is the
nearest sibling — it drives single-turn scenarios through the daemon
and scores them; the inner-work runner needs the same daemon path
plus multi-turn state threading (see `bench/wikipedia_learn/` for
the multi-turn shape on the corpus side).
