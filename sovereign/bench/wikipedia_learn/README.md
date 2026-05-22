# Wikipedia multi-turn learner bench

Multi-turn extension of `bench/wikipedia/`. Where the single-shot bank
measures one-question retrieval+synthesis, this bench measures
**conversational memory and degradation under growing context**:
sequential follow-up turns of a curious learner asking about an
article, where each turn assumes the prior turn's subject and chains
forward.

Driven by the same insight surfaced in `gym/FINDINGS_2026-05-13.md`:
the model performs best at zero context one-shot and degrades as
context accretes. This bench is the retrieval+synthesis analogue.

## Schema

`threads.toml` carries `[[threads]]` blocks (not `[[questions]]`).
Each thread is a chain of `[[threads.turns]]` items replayed under
**one `conversation_id`**:

```toml
[[threads]]
id = "einstein_1905"
category = "learner_factual"
description = "Student builds from 'who is Einstein' → 1905 → photoelectric"

[[threads.turns]]
question = "Who was Albert Einstein?"
expected_facts = ["physicist", "relativity"]
expected_sources = ["Albert Einstein"]

[[threads.turns]]
question = "What did he do in 1905?"   # coref "he" must resolve
expected_facts = ["Annus Mirabilis", "photoelectric"]
expected_sources = ["Albert Einstein"]
```

Turn 0 is standalone; turns 1+ depend on prior turns via pronoun,
ellipsis, or topic continuity. **Coreference resolution is part of
the test** — a turn whose `expected_facts` rely on the prior subject
will score 0 if conversational memory is broken.

## Run

```sh
sovereign eval run --threads \
    --bank sovereign/bench/wikipedia_learn/threads.toml \
    --output sovereign/bench/wikipedia_learn/baselines/threads-v0.json
```

Drives the same `runtime.handle_message_stream` path as desktop chat
(intent classifier → router → retrieval → synthesis → conversation
store), turn by turn under a single conversation_id.

Skip the judge for fast iteration:

```sh
sovereign eval run --threads --no-judge --bank …
```

## Scoring

**Per turn (deterministic, no LLM):**
- `fact_recall` — keyword substring count of `expected_facts` in the
  synthesised answer. Same scorer as `bench/wikipedia/`.
- `source_recall` — title-normalised match of `expected_sources` in
  retrieved-chunk titles.
- `stream_wall_ms`, `total_latency_ms`, `intent`, `retrieved_chunks`,
  `reasoning_chars` — diagnostic.

**Per thread (one LLM call on primary slot, end of thread):**
- `coverage` — fraction of the union of `expected_facts` the judge
  found *somewhere* in the transcript.
- `per_fact.evidence_turn` — the 0-indexed turn the judge attributes
  each fact to. Lets the bench show per-turn coverage at LLM-grade
  resolution without 6× the cost.

**Headline output — `degradation` per thread:**
- `first_failure_turn` — first turn whose deterministic `fact_recall`
  drops below 1.0.
- `fact_recall_slope` — linear regression on (turn_index,
  fact_recall). Negative = degrading across the thread.
- `latency_ms_per_turn` — wall growth as the conversation accretes.

## Why thread-level judge (not per-turn)

Per-turn judge × 5–8 turns × 12–15 threads = 60–120 grading LLM
calls per bench run. Falling back to Fast slot for cost compromises
grading on multi-paragraph synthesis answers (Fast is a 2B; it
mis-judges paraphrased coverage). One judge call per thread on the
primary slot reads the full transcript and attributes per-fact
evidence to specific turns. Deterministic per-turn `fact_recall`
keeps the degradation curve cheap and free of judge variance.

See `feedback_wikipedia_learn_thread_judge.md` in operator memory.

## Authoring rules

- Each turn 1+ **assumes** the prior turn's context. The point of
  this bench is to test memory across turns.
- 2–4 keyword-rich `expected_facts` per turn. Proper nouns + dates
  + numbers travel cleanly through the substring scorer.
- 1–2 `expected_sources` per turn, from titles known to exist in
  the wiki corpus (Vital Articles L5).
- Include at least one turn that could be answered standalone —
  later we'll compare in-chain vs standalone score to surface
  **context-poisoning cost** as a directly fixable subsystem signal.

## v0 contents

- `einstein_1905` — factual drill: figure → 1905 papers → photoelectric → Nobel resolution → why-not-relativity.
- `wwii_causal` — causal regression: WWII outbreak → rearmament → Versailles failure → Keynes prediction.

Both are 5 turns. Scale to 12–15 threads across six categories
(factual / synthesis / causal / comparative / boundary / contested)
in P3.

## Cost

5–15 s per turn × ~10 turns per thread × 12 threads + 12 judge calls
on primary ≈ 15–30 minutes wall on the current Qwen3.6-35B primary.
Heavyweight bench; run pre-merge for conversation-memory changes,
not per-commit.

## Baseline diff

Same pattern as `bench/wikipedia/baselines/`. `--output` writes the
full JSON; commit each as `baselines/threads-<date>.json` and diff
across changes to the conversation-memory subsystem.

## Related

- `bench/wikipedia/` — single-shot sibling.
- `gym/FINDINGS_2026-05-13.md` — the gym findings that motivated
  the more-context-is-worse hypothesis this bench measures.
- `eval_cmd::runner_threads` — the runner + thread judge.
- `eval_cmd::bank::EvalThreadBank` — the schema parser.
