# knowledge-gym RUNBOOK

The knowledge-gym is a single-turn correctness harness for
chat-side tools (`knowledge_lookup` and any future read-side
front-door tools). It replays each fixture N times against a
live daemon, scores each transcript with the fixture's
`pass.toml` predicates, and reports pass rates.

It exists for one reason: **bench-to-production parity**. The
fixtures encode shapes we want to handle correctly forever; the
gym is the iteration loop that proves changes to the tool
framework don't regress.

## Run the gym

```bash
# All fixtures, 3 replays each (default):
sovereign knowledge-gym run

# All fixtures, more replays (less variance, slower):
sovereign knowledge-gym run --replays 6

# One fixture:
sovereign knowledge-gym run --fixture 01_corpus_definitional

# Custom daemon URL:
sovereign knowledge-gym run --base-url http://localhost:9742
```

Output is per-fixture pass rates and overall summary. Aim for
≥ 90% on every fixture; 100% on the structural ones (citation
faithfulness, no fabrication).

## Add a fixture from a real bug

The methodology that the coding-tools gym (gym/FINDINGS_2026-05-13.md)
proved: **user bug → gym fixture → mechanical fix → verify**.
Every chat-side bug should follow the same loop.

1. **Capture the transcript.** When the desktop chat surface
   produces something wrong (fabricated citation, missed prior
   evidence, retried a known-failed query), grab the
   conversation. Future Tier 5a infrastructure will add a
   "Report this turn" button to do this automatically; until
   then, copy the transcript manually.
2. **Build the fixture directory** at
   `sovereign/bench/knowledge-gym/fixtures/NN_<slug>/`:
   - `input.json` — the chat completion request that reproduces
     the issue. Includes the system prompt, the user message,
     the tool declarations. The simplest path: copy the JSON
     body sent to the daemon's `/v1/chat/completions` from the
     bad turn.
   - `mock_evidence.json` — the evidence envelope the tool
     returned (so the replay is deterministic — same evidence
     every run, regardless of corpus state).
   - `pass.toml` — predicates that capture what the model SHOULD
     have done. See `PASS_SCHEMA.md` for the predicate
     vocabulary. Start with the structural predicate that
     matches the failure (e.g.
     `must_not_cite_evidence_id_outside_returned = true` if the
     bug was fabrication).
3. **Run the fixture.** Confirm it FAILS without the fix:
   ```bash
   sovereign knowledge-gym run --fixture NN_<slug>
   ```
4. **Decide on the fix shape.** Categorise:
   - **Prompt discipline**: update the tool's `system_prompt.md`
     or `tool_description.md` asset.
   - **Dossier**: update the renderer to surface the right
     prior-turn context.
   - **Constraint**: add a sampler-level mask (Tier 2 pattern
     for citation; Tier 1's URL allowlist pattern for any
     other structural emission).
   - **Cache**: the bug might be "model re-fetches identical
     data"; the Tier 4 cache should fire.
5. **Land the fix + re-run.** Confirm the fixture now PASSES
   without regressing the other fixtures.

## Pre-flight checklist before shipping a tool-framework change

- [ ] `cargo test -p sovereign-core --lib` — green
- [ ] `cargo test -p sovereign-inference --lib evidence_id_constraint` — green
- [ ] `cargo test -p sovereign-tools --lib knowledge_lookup` — green
- [ ] `sovereign knowledge-gym run --replays 3` — ≥ 90% per fixture
- [ ] `sovereign eval run --bank sovereign/bench/routing/cells_v1.toml --routing-only`
      — within 1 of baseline (25/27)
- [ ] `sovereign eval run --threads --bank sovereign/bench/wikipedia_learn/threads.toml`
      — thread baselines hold (T14: 4/6, T15: 3/4)

Skipping any of these is a regression-shape audit you owe future
you.

## Triage when a fixture fails

1. **Look at the failing predicate.** Which one tripped? If
   it's a structural predicate (citation, fabrication, tool
   shape), the fix is usually in the asset prompts or the
   sampler constraint. Semantic predicates (`answer_acknowledges_gap`)
   tend to mean the model didn't reach the right SHAPE — that's
   prompt-discipline territory.
2. **Look at the transcript.** The runner emits per-replay
   per-predicate breakdowns. Find a failing replay and read
   the actual model output. Often the failure mode is obvious
   from one read.
3. **Cross-reference with bench results.** If `cells_v1` and
   `wikipedia_learn` are also worse, the regression is system-
   wide (likely a router or dossier change). If only the gym
   fixture regressed, the change touched something narrow.
4. **DO NOT teach to the test.** The fixture's `pass.toml`
   predicates describe SHAPES, not specific phrases. If you
   find yourself editing an asset prompt to mention the
   fixture's question vocabulary, you've drifted. Keep the
   asset prompts general; let the model find the right shape.

## Sister gyms

- `sovereign/bench/routing/*.toml` — routing-only banks
  (cells_v1, voice_routing_v1, future_timeline_v1, skills_migration_smoke).
  Run via `sovereign eval run --bank <path> --routing-only`.
  Routing is the surface-level dispatch correctness check;
  knowledge-gym is the deeper tool-mastery check.
- `sovereign/bench/wikipedia_learn/threads.toml` — multi-turn
  dossier-loop validation. Run via `sovereign eval run --threads
  --bank <path>`.
- `gym/` (repo root) — the coding-tools gym (codex CLI on local
  Qwen). Different architecture (apply_patch heredocs, 3-mode
  sampler, frontdoor canonicalizers) but the same iteration
  philosophy.
