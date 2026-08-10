# atlas_throughput — model-selection bench for the atlas pipeline

Driver + archived results for picking which `[models].primary` to load
for long atlas-enrichment batches (the SEP 1800-article ingest is the
motivating workload).

The bench measures **throughput** (decode tokens/sec) and
**correctness** (does the output parse via the real
`LiteraryAtlasPipeline::parse_phase1`?) across four representative
tasks:

| task | description | what it measures |
|---|---|---|
| `phase1_short` | Atlas Phase 1 prompt against the shortest chapter in the corpus | Phase 1 floor for short sections |
| `phase1_medium` | …against the median chapter | typical Phase 1 cost |
| `phase1_long` | …against the longest chapter | Phase 1 ceiling + truncation risk |
| `cluster_name_synth` | Small Phase 3-style structured prompt → single-object output | short-call loops (name / resolve / configure) |

## Why these four

The reference run (`sep-al-farabi`, 38 min total) was 79% Phase 1 and
21% short calls. The three Phase 1 tasks span the real input-size
range; `cluster_name_synth` stands in for every short-call phase
(they're all 500–900 input tokens, sub-3000 output, structured-output
with `response_format`). A model that handles all four well will
handle the production pipeline; a model that fails any of them is
rejected before you spend a week on a bad ingest.

## Run

The bench is a `sovereign-cli` subcommand — no Python deps, no
secondary harness. It hits the running daemon's
`/v1/chat/completions` so the model under test is whichever
`[models].primary` the daemon was started with.

```bash
# 1. Pick a candidate model.
$EDITOR ~/.svrnmesh/config.toml             # set [models].primary

# 2. Restart the daemon so it loads the new model.
systemctl --user restart sovereign.service

# 3. Run the bench. 30+ min for a 27B-class model on Strix Halo.
sovereign bench atlas --output bench/atlas_throughput/run-<label>.json

# 4. Repeat for each candidate.
```

The stdout summary is always printed; `--output` writes the same data
as JSON for archival + diffing. Each run carries the
**daemon-reported `model_id`** (from the chat completions response,
not the user-supplied label) so an archive can't be silently
mislabelled.

### Useful flags

| flag | use |
|---|---|
| `--tasks <ids>` | Run a subset (e.g. `--tasks phase1_medium` while iterating prompt tweaks) |
| `--no-warmup` | Include lazy-slot load tax in the first task's timing — useful for "cold-start cost" measurements |
| `--max-tokens-cap N` | Override the per-task max_tokens (default 16384). Tighten to test how the model handles a smaller output budget |
| `--corpus <id>` | Source Phase 1 chapters from a different ingested corpus (default `sep-al-farabi`) |

## Reading the output

```
  --- summary (Qwopus3.5-27B-v3.5-Q6_K) ---
  decode tok/s avg ...............    29.00
  phase1 decode tok/s avg ........    18.50
  phase1 success rate ............   100.0%
  phase1 secs/chapter avg ........   620.4
  est. 1800 articles × 5 ch ......   1551.0 h  (64.6 days)
```

- **`phase1 decode tok/s avg`** is the headline number for picking a
  model. The cluster_name task's tokens/sec is usually higher than
  Phase 1's because short calls hide load-tax in proportion; trust
  Phase 1 for batch projection.
- **`phase1 success rate`** below 100% is a hard reject signal — a
  model that can't reliably produce parseable atlas JSON wastes
  every retry pass on top of its base latency.
- **`est. 1800 articles × 5 ch`** assumes Phase 1 is the dominant
  cost (~80% in our reference run); the short-call phases add
  ~25%. So if the table says 65 days, plan for ~80 days end-to-end.

## Comparing runs

`jq` is enough for first-cut comparison until we have more than two
or three candidates:

```bash
for f in run-*.json; do
  jq -r '[.model_id, .summary.phase1_decode_tps_mean,
          .summary.phase1_success_rate,
          .summary.est_hours_1800_articles_5_chapters] | @tsv' "$f"
done | sort -k2 -nr
```

A proper `bench atlas compare` subcommand can land later if the
`jq` recipe gets unwieldy.

## Caveats

- The bench uses `temperature: 0.0` (greedy) for reproducibility;
  production runs at 0.2. Decode speed is essentially identical;
  correctness may be slightly more conservative at greedy.
- Slot reload tax is hidden by the warmup step — first-task latency
  reflects steady-state, not cold-start. Use `--no-warmup` to
  measure cold-start instead.
- Per-task results are single-shot; we'd want N=3 medians for
  publication-grade numbers, but for picking-a-model decisions a
  single sample at the chapter sizes the model will actually see
  is good enough. Re-run any task that lands far from your prior
  for that model to sanity-check.
- **Chat templates**: until 2026-04-26 the daemon's
  `format_prompt` silently fell back to plain-text
  `{system}\n\n{user}` concat when llama.cpp's built-in
  `apply_chat_template` rejected a model's gguf-embedded
  template. That's the case for any template that uses Jinja2
  macros / loops / complex control flow — including Gemma 3/4
  (template starts `{%- macro format_parameters() -%}...`). Models
  in that bucket would role-play multi-turn output ("User: ...
  Assistant: ...") because they never saw their real
  `<start_of_turn>user|model<end_of_turn>` special-token
  boundaries, never emitted EOS, and decoded to `max_tokens` on
  every request. Symptoms: phase1 fails with mid-string JSON
  truncation or hallucinated `{"//": "..."}` commentary; raw
  chat completions take many minutes for trivial prompts. The fix
  retries via `apply_chat_template_oaicompat` with `use_jinja:
  true` (llama.cpp's minja path, same as `llama-server --jinja`)
  before falling through to the loud-warned plain-text concat.
  If you're benching a model whose template is macro-based AND
  you see hallucinated role markers in `response_head`, your
  sovereign-cli build is from before that fix.
- Daemon reports `prompt_tokens=0` and ALSO mislabels
  `completion_tokens` (it actually carries total = prompt +
  generated). Discovered 2026-04-26 when GLM-18B with
  `--max-tokens-cap 8000` reported `completion_tokens=12394`
  (apparent cap violation) — real decoded output was ~7300 tokens
  under the cap; the extra ~5000 was the prompt. Implication:
  bench `decode_tokens_per_sec` is **inflated by ~50–75%** because
  the numerator includes prompt tokens. Comparative ranking
  between models is still valid (the bug applies equally) and
  `phase1_seconds_per_chapter` / `est_hours_1800_articles_5_chapters`
  are accurate (those measure wall-clock, not tokens). Track-and-fix
  in the daemon's chat-completions response assembly when convenient.
- On a failed task the result's `response_head` field carries the
  **full** model output (not a 500-char preview) so post-mortem can
  find corruption that lives deep in the body. Successful tasks
  drop the response to keep result files small. If you ever need
  the raw response for a passing task, run with `--tasks <id>`,
  trip the validator deliberately, and inspect.

## Known model results (build out as you test)

| model | Phase 1 success | Phase 1 tok/s | atoms (medium) | est. 1800 × 5 | verdict |
|---|---|---|---|---|---|
| Qwopus3.5-27B-v3.5-Q6_K | 3/3 | ~15 | ~28–50 | ~64 d | production-ready |
| Qwopus-GLM-18B-Healed-Q6_K | 2/3 (long fails) | 22–38 | 32 | ~14 d if you fix long | conditional |
| FINAL-Bench_Darwin-35B-A3B-Opus-Q8_0 (pre-fix, pre-token-bug-fix) | 2/3 (short fails) | 161–180 (inflated) | 12 | ~3.2 d | superseded — tok/s was prompt+completion summed, not honest decode |
| FINAL-Bench_Darwin-35B-A3B-Opus-Q8_0 (post-masker-fix 2026-05-04) | **3/3** | 24–26 | 15–28 | **~7.0 d** | **rich extraction** (23/15/28 atoms across short/med/long), masker fix transferred cleanly |
| FINAL-Bench_Darwin-36B-Opus-Q6_K (dense, 2026-05-04) | **3/3** | 23–27 | 11–20 | **~4.7 d** | balanced (20/15/11 atoms), faster per chapter than 35B-A3B-Q8, richer on long than Nemotron |
| Darwin-9B-Opus.Q8_0 | 0/1 | 42.7 | — | n/a | rejected |
| gemma-4-31B-it-Q5_K_M | 3/3 | 25–31 | 14 | ~20 d | viable; most reliable phase1 yet (no failures) |
| Bonsai-8B-Q1_0 | 1/1 structural | 399 | 9 (empty filler) | 1.2 d | rejected for extraction |
| Nemotron-Cascade-2-30B-A3B.Q6_K (pre-fix) | 2/3 (short truncated, long early-stop) | 12–30 | 16 | ~2.6 d *if* short retried | A3B early-EOS family; **superseded by post-fix row** |
| Nemotron-Cascade-2-30B-A3B.Q6_K (post-masker-fix 2026-05-04) | **3/3** | 12–31 | 16 | **~2.8 d** | A3B early-EOS root-caused as masker `,` bug, fixed in `json_constraint.rs:step_object`; production-viable |

### Failure-mode notes

- **Darwin-9B-Opus.Q8_0**: long structured outputs ship with
  whitespace corruption (`"betwee n"`, `"an d"`, `"Fârabì's"`) and
  a missing-quote at byte 11409. Balanced-brace scan ends
  `depth=2, in_string=true`. 9B+Q8 envelope can't sustain
  14k-char strict JSON on Strix Halo.
- **Qwopus-GLM-18B-Healed-Q6_K**: passes short (25 atoms) and
  medium (32 atoms) cleanly, drops a comma between key-value pairs
  on the longest chapter (line 380: `"label": "..."` then line 381
  `"anchor": ...` with no `,`). Decode also slows from 38 → 22
  tok/s as context grows, consistent with attention quadratic cost.
  Recoverable if you clamp output (`--max-tokens-cap 8000`, terse
  Phase 1 variant) or fall back to 27B for the chapters it drops.
- **Bonsai-8B-Q1_0**: technically passes the Phase 1 parser, but
  the schema only requires `questions_raised` non-empty so a
  shape-conformant response with mostly-empty arrays slips
  through. 9 atoms vs GLM-18B's 32 for the same chapter is a
  3.5× quality cliff. Cluster_name task fails with "trailing
  characters" — Bonsai keeps emitting after the close brace.
  Pattern: producing valid-shape filler, not actually answering
  the prompt. Use as the fast slot, not a primary candidate.
- **FINAL-Bench_Darwin-35B-A3B-Opus-Q8_0**: A3B (3B active of
  35B MoE) gives the fastest Phase 1 throughput we've measured
  (~170 tok/s, est. 3.2 d for 1800 × 5). Medium and long pass
  cleanly, but the SHORTEST chapter ends mid-string ("...Cecilia
  Mart" cut off in `relations_introduced[].participants`) — looks
  like an early-EOS / premature-stop on small inputs, distinct
  from Darwin-9B's whitespace-corruption failure mode. Discovered
  2026-04-26. Atom count (12 medium, 17 long) is well below
  GLM-18B's 32 on the same medium chapter — could be coarser
  granularity or under-extraction; spot-check the actual atoms
  before adopting as primary.
- **gemma-4-31B-it-Q5_K_M**: 100% phase1 success on all three
  chapters, 25–31 tok/s, 14 atoms on medium (similar to
  Darwin-35B-A3B's 12). At ~27 tok/s avg phase1 the est. 1800 ×
  5 ch is ~20 d — slower than Darwin-A3B (3 d) and GLM-18B
  (~14 d if fixed) but the only candidate so far that didn't drop
  any phase1 chapter. Trade-off pick: pay 6× the wall time of
  Darwin-A3B for a strict no-retry pipeline. Atom count is on the
  low side (14 vs GLM-18B's 32) — same spot-check caveat as
  Darwin-A3B. NOTE: this run was unblocked by a chat-template
  fix landed 2026-04-26 (see Caveats §"Chat templates"). On the
  initial run Gemma role-played multi-turn output because the
  daemon's `format_prompt` was silently falling back to plain
  text concat after `apply_chat_template` failed; the fix retries
  via the Jinja2 oaicompat path which handles Gemma's macro
  template. If you're re-benching another macro-template model,
  you need a sovereign-cli build at or after that commit.
- **A3B-MoE early-EOS RESOLVED 2026-05-04 — verified on Darwin too**
  (was thought to be a family-wide model failure mode; turned out
  to be a constraint-masker bug). Post-fix re-bench on
  Darwin-35B-A3B confirms the fix transfers: Phase 1 success 2/3
  → 3/3 with rich extraction (23 / 15 / 28 atoms on short / medium
  / long, vs 12 atoms on medium pre-fix). Honest tok/s after the
  prompt-token bookkeeping is correct: ~25 tok/s steady-state, NOT
  the ~170 tok/s the pre-fix bench claimed (that number was
  inflated by ~7× because `completion_tokens` carried prompt +
  generated). Real SEP projection: Nemotron-A3B ~2.8 d (sparse
  output), Darwin-A3B ~7.0 d (rich output, ~2× atom density). Root cause: in `sovereign-inference/src/json_constraint.rs`,
  `step_object`'s `AwaitCommaOrClose → b','` arm accepted `,`
  unconditionally. The recursive validator at `parse_object` line
  437/448 correctly rejects `,` after the last typed property when
  `additionalProperties: false` (no more pairs possible). The
  disagreement triggered the diagnostic latch (`emitted_invalid =
  true`) which forces EOS-only mode on the next mask call,
  truncating the JSON document. Dense-attention models tokenize
  structural separators (`],\n`) into smaller tokens that don't
  trigger the disagreement at the same state-machine boundary; A3B
  models tokenize them into single multi-byte tokens that do.
  Fix: mirror the validator's `more_pairs_possible = next_idx <
  properties.len() || additional` check inside `step_object`.
  Locked in by three regression tests
  (`comma_after_last_typed_property_is_invalid_in_both_paths` and
  variants). Post-fix Nemotron-Cascade-2-30B-A3B Phase 1 success
  jumped 66.7% → 100% with no other changes.
- **Nemotron-Cascade-2-30B-A3B.Q6_K** (benched 2026-05-04, pre-fix):
  same A3B early-EOS failure family as Darwin-35B-A3B, but at much
  lower throughput (~30 tok/s on the medium task vs Darwin-A3B's
  ~170).
  phase1_short produced a rich, well-formed JSON body for sec_0003
  of al-farabi (al-Farabi entity, Muhsin Mahdi, Reisman, Parens —
  good extraction quality at the body level) but cut off after 979
  completion tokens mid-`claims` array, leaving the six-facet
  document unclosed → strict parser rejects. phase1_long stopped
  after 102 completion tokens (atoms_extracted=2, naturally — not
  budget-truncated), suggesting the model bails entirely on the
  longest input. cluster_name_synth also stopped early at 52
  tokens, mid-object. Pattern: A3B-class MoE models on Strix Halo
  ROCm appear to early-exit on long structured outputs regardless
  of remaining budget — same root cause as Darwin-A3B's
  short-chapter failure. The 16 atoms on the medium chapter is on
  par with Darwin-A3B (12) but well below GLM-18B's 32, so atom
  density is also coarser. Verdict: **rejected for primary** at
  current weights — falls in the same risk basket as Darwin-A3B,
  with worse throughput. Not worth pursuing further unless either
  the early-stop can be addressed (sampler config, EOS-token
  inspection) or atom density turns out to be acceptable on a
  spot-check across more chapters.
- **NOTE — bench harness fix 2026-05-04**: the bench was sending
  `model: "primary"` as a sentinel; after a daemon-side mesh-router
  change this returns 503 ("no node in this mesh advertises model
  'primary'"). Fix in `bench_cmd/atlas.rs:651` switches to empty
  string, which routes to the loaded local primary and lets the
  daemon report back the actual model_id. Any pre-fix bench run
  against the current daemon will look like a 0/3 wipeout — that's
  the harness, not the model.

