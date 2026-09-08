# knowledge-gym RUNBOOK

The knowledge-gym is a correctness harness for chat-side tools
(`knowledge_lookup` and any future read-side front-door tools). It
replays each fixture N times against a live daemon, scores each
transcript with the fixture's `pass.toml` predicates, and reports pass
rates.

It exists for one reason: **bench-to-production parity**. The fixtures
encode shapes we want to handle correctly forever; the gym is the
iteration loop that proves changes to the tool framework don't regress.

## Which surface a fixture measures — `production_path`

Every fixture's `pass.toml` MUST declare `production_path`. There is no
default: a fixture that declares none is refused at load, by name.

| value | what it drives | is it the product? |
|---|---|---|
| `executor` | `Executor::execute_reason_with_tools` — the `ReasonWithTools` step a planner emits | yes |
| `attached-doc` | `Runtime::handle_attached_doc_turn` — a turn on a conversation with a `DocumentSession` | yes, but **no headless driver yet** (see below) |
| `raw` | `POST /v1/chat/completions` with an OpenAI `tools[]` array | **no** |

**Why this key exists.** Until 2026-09-07 every fixture drove `raw`,
and nothing said so. No product turn takes that route: `knowledge_query.rs`
passes `tools: None` on both synthesis routes (`:1301`, `:1352`), and the
two paths that DO offer `knowledge_lookup` render it as a prose line and
parse an inline `<tool_call>` marker back out. So a green gym was a
statement about the daemon's native function-calling adapter, and the
lane read it as a statement about the product. Every report line for a
`raw` fixture now says `not-the-product`.

The three fixtures in the `knowledge-gym-v1` smoke subset
(`sovereign/bench/smoke.toml`) run on `executor`. The other eight still
declare `raw` and are labelled accordingly; retarget one before reading
its number as a product fact.

`attached-doc` has no driver in this lane. `handle_attached_doc_turn`
needs a Ready `DocumentAsset` plus a `DocumentSession` on the
conversation — `bench_cmd::book_report::dispatch_question` is the
working headless recipe, and the ingest that produces the asset does not
fit a 50-second lane. A fixture declaring it is **refused at load**, and
the run exits non-zero.

That refusal is deliberately not a per-replay could-not-judge. ARCH
§18.2 as amended: the two verdicts that make no claim are owed, not
free, and an abstention nobody has watched be necessary is not rigor. A
fixture whose path has no driver is a fact the LOADER already knows, so
turning it into three unjudged replays would make the lane read as
careful while measuring nothing. Give `attached-doc` a driver and delete
the guard; until then it cannot silently become nine abstentions.

The same rule refuses a multi-turn fixture that declares `executor` (the
`ReasonWithTools` step is single-shot). Both are load-time errors with
the reason in the message.

**Report the distribution, not just the failures.** Every run prints
`verdicts: passed N failed N could-not-judge N (of N replays)` — in both
human and `--json` mode — and the JSON carries `total_failed` beside
`total_passes` and `total_errored`. A gym where most replays abstain has
not been retargeted, it has been silenced, and this line is what tells
the two apart.

## The tool ledger — one decider for "did the tool fire"

Predicates read `ToolLedger` (`knowledge_gym_cmd/ledger.rs`), never a
path's private shape. Each driver projects its own record into it:

- `executor` → the step's `search_log` (`SearchLogEntry`), which is
  production's own record of tool id, the query the model wrote, and how
  many results production counted in what it handed back.
- `raw` → the OpenAI `tool_calls` array off the response.

The evidence ids on a ledger row are what the GYM's canned envelope
returned, recorded by the mock — never read back off what the model
said it saw. A guard that asserts on a field the subject supplies is not
a guard (ARCH §18.1).

`path_result_count` is the row that catches a path which dispatched the
tool, got rows back, and delivered none of them to the model. When it is
`Some(0)` beside a non-empty returned set, the report prints
`! EVIDENCE NOT DELIVERED` before any predicate line — because that is
the path losing the evidence, not the model failing a citation contract,
and folding the two together blames the wrong half.

## Run the gym

```bash
# All fixtures, 3 replays each (default), each on its declared path:
sovereign knowledge-gym run

# All fixtures, more replays (less variance, slower):
sovereign knowledge-gym run --replays 6

# One fixture:
sovereign knowledge-gym run --fixture 01_corpus_definitional

# Custom daemon URL:
sovereign knowledge-gym run --base-url http://localhost:9742

# MODEL-ONLY measurement: force every fixture onto the raw endpoint.
# Every line says not-the-product. Never read this as a product verdict.
sovereign knowledge-gym run --raw

# Watch the lane go red on a break you can name (ARCH §18.1):
# the ReasonWithTools step is built with an EMPTY tool list, so the
# production prompt never offers knowledge_lookup.
sovereign knowledge-gym run --sabotage no-tool-offered
```

Output is per-fixture pass rates and overall summary, each line naming
the path it ran on, plus the four-verdict distribution. Aim for ≥ 90% on
every fixture; 100% on the structural ones (citation faithfulness, no
fabrication).

## What the subset scored when it was retargeted (2026-09-07)

Three runs, one binary, n=3 per fixture, same daemon and same model
(`primary`), so the columns differ only in the surface and the fixture
edit:

| run | 01 | 05 | 06 | total | abstained |
|---|---|---|---|---|---|
| `--raw` before the retarget (old 05 question) | 3/3 | 0/3 | 3/3 | 6/9 | 0 |
| `--raw` after (new 05 question, widened gap judge) | 3/3 | 3/3 | 3/3 | **9/9** | 0 |
| production path (`executor`) | 0/3 | 0/3 | 0/3 | **0/9** | 0 |
| production path, `--sabotage no-tool-offered` | 0/3 | 0/3 | 0/3 | 0/9 | 0 |

Zero abstentions on all four. Every replay rendered passed or failed.

The 9/9 → 0/9 gap is the whole point of the retarget: the model's
function-calling adapter holds every contract these fixtures assert, and
the product holds none of them. Two mechanisms in the ledger, both
production-side and both outside this order's seam:

1. **The path loses the evidence.** `knowledge_lookup` returns
   `StepOutput::Json(KnowledgeLookupResponse)`, which has no `answer`
   key (`sovereign-tools/src/knowledge_lookup/mod.rs:122-126`, `:561`).
   Both consumers read exactly that key and substitute a string:
   `executor.rs:1192-1195` → `"No results."`, and
   `runtime/handlers/attached_doc.rs:292-296` → `"(no answer field)"`.
   So the model is handed a literal "No results." for a 2-row envelope.
   The gym reports it as `! EVIDENCE NOT DELIVERED — knowledge_lookup:
   2 row(s) returned, path counted 0` (4 of 9 replays), and the model
   retries the same lookup 2-3 times chasing nothing. Zero evidence ids
   were cited on any of the 9 replays.
2. **The prompt names a tool that is not offered.**
   `build_retrieval_reasoning_prompt` hardcodes
   `<tool_call>{"tool":"search",…}</tool_call>` as its one worked
   example, whatever `available_tools` holds. A model offered ONLY
   `knowledge_lookup` still emits `search` first on 6 of 9 replays and
   gets `Tool 'search' not available.`

**The sabotage is watched red at the ledger, not just the total.** The
totals are both 0/9, so the aggregate alone proves nothing; the ledger
does. Un-sabotaged, the tool fired on 6 of 9 replays and the evidence
note appeared 4 times. Sabotaged, the tool fired on **0 of 9** and the
note appeared 0 times — nothing fired, so nothing was lost.

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
