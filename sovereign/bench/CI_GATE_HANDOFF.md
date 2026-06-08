# CI-Gate Baseline + Quality-Iteration — HANDOFF

_Self-contained. A fresh session should read this top-to-bottom, then start at "Next-session plan."_
_Date: 2026-06-08. Also loaded automatically: the three `invariant_*`/`project_*` memory notes referenced below._

## Mission

Drive the `sovereign bench gate` CI suite to a **full baseline on the corrected stack**, then run **≥ a few rounds of quality iteration** off that baseline. The "very-high-confidence" gate push (search/knowledge/agent gyms + chaos v2) is what turns over the rocks.

## What's DONE + committed

- **CI gate infra** (`sovereign bench gate <lane>` + `bench_cmd/{gate,lane_baseline,baselines}.rs`): baseline-relative gate for 6 lanes — `chaos-monkey`, `mechanism-fidelity`, `multiturn`, `search-gym`, `knowledge-gym`, `agent-coding`. Self-describing metric/direction/tolerance primitive; reads each lane's artifact, diffs vs `sovereign/bench/<group>/baselines/<id>/latest.json`, exits 0/1 (first-run passes).
- **CI suite** `scripts/sovereign-ci-bench.sh`: HARD deterministic lanes + SOFT synth + the 6 absolute-verdict lanes as TRACKED-run + HARD-`*-gate`. Gym lanes 8-10 wired (sample hardest fixtures). The flywheel lane 7 is opt-in (peer's).
- **Stable chaos corpus**: `sovereign-recipes/chaos-secret-agent/recipe.toml` → fixed corpus_id (was a path-hash `corpus watch`). `scripts/setup-chaos-corpus.sh` installs it.
- **chaos v2**: 4 provenance_trap + 3 distractor questions in `chaos_monkey/secret_agent.toml`; gate surfaces `citation_fidelity` + `distractor_evasion` (guarded `.is_finite()`).

### THREE production P0s found + fixed this session (all via the new coverage)
1. **Retrieval** returned a single-doc corpus's OPENING chunks for every query → neighbour-window fix, committed `e3d027b2`. See `[[invariant_dominant_source_expansion_single_doc]]`.
2. **Daemon VRAM OOM** = two 30B+ models → the IQ4+4B config below.
3. **Tool-calling broken system-wide** = `alternation_grammar=true` traps tool-callers in an endless envelope loop → disabled. See `[[invariant_alternation_grammar_breaks_tool_calling]]`.

## THE CORRECTED STACK — verify this FIRST every session

- **Models** (`~/.sovereign/config.toml [models]`): primary `Qwen3.6-35B-A3B-UD-MTP-IQ4_NL`, fast `Qwopus3.5-4B-v3-MTP-Q8_0`, embed `qwen-embedding-0.6b`. Two 30B+ models do NOT fit 64GB (jetsam/`Decode Error -3`).
- **`~/.sovereign/config.toml`**: `alternation_grammar = false` (CRITICAL — `true` breaks ALL tool-calling), `yield_to_foreground_secs = 15` (≥30 starves ingest).
- **Daemon**: exactly ONE instance (two share the Metal ctx → `Decode Error -3` on every prompt). Clean restart:
  `sovereign daemon stop && pkill -9 -f 'cli-daemon daemon run' && sovereign daemon start`, then **wait ~60-120s** (SCIP merge + lazy model load) — poll `lsof -iTCP:9741 -sTCP:LISTEN` then a decode probe before running benches.
- The CI benches do retrieval **in-process** (`sovereign-cli-llm`, local CorpusEngine); only inference hits the daemon. The retrieval fix is in `sovereign-core` → present in both the rebuilt CLI and daemon (rebuilt this session).

## Baseline status (committed) — what's VALID vs STALE

| Lane | Committed baseline | Status |
|---|---|---|
| chaos-monkey | `secret_agent/2026-06-08` (5 metrics) | ~valid (IQ4 + fixed retrieval; chaos has no tools so the grammar fix doesn't change it). **honesty 0.45** (IQ4 answers OOD), competence 0.67, citation_fidelity 0.25, distractor_evasion 0.33 |
| search-gym | `ci/2026-06-08` = **0.80** | VALID (re-captured `1aff3e26`, grammar-fixed) |
| mechanism-fidelity | `dev/2026-06-07` | **STALE** — captured on the old 36B-Q6 model. Re-run. |
| multiturn | `wikipedia_learn/threads/2026-06-08` | **STALE** — old model + broken retrieval. Re-run (retrieval fix should help). |
| knowledge-gym | — | **NOT captured** |
| agent-coding | — | **NOT captured** |

## Next-session plan

**Step 0 — verify the corrected stack** (config models + `alternation_grammar=false`; one healthy daemon; decode probe returns clean text; `corpus list` shows `chaos-secret-agent`).

**Step 1 — re-baseline every lane on the corrected stack.** Run each, capture with `--update-baseline`, commit. Sequence (sequential — daemon contention; agent-coding last/longest):
```
# chaos (re-confirm on full corrected stack):
target/debug/sovereign-cli-llm bench chaos-monkey run --bank sovereign/bench/chaos_monkey/secret_agent.toml \
  --manifest sovereign/bench/chaos_monkey/manifest.toml --corpus chaos-secret-agent --out target/ci-bench/chaos.jsonl
target/debug/sovereign-cli-llm bench gate chaos-monkey --report target/ci-bench/chaos.jsonl --bench-root sovereign/bench --update-baseline
# mechanism (~3m):
target/debug/sovereign-cli-llm bench mechanism-fidelity run --models primary --pool dev --n-cases 30 \
  --manifest sovereign/bench/mechanism_fidelity/manifest.toml --out target/ci-bench/mechanism.jsonl
target/debug/sovereign-cli-llm bench gate mechanism-fidelity --report target/ci-bench/mechanism.jsonl --bench-root sovereign/bench --update-baseline
# search-gym (full set, ~3m) → gate:
target/debug/sovereign-cli-llm search-gym run --json --replays 3 > target/ci-bench/search-gym.json
target/debug/sovereign-cli-llm bench gate search-gym --report target/ci-bench/search-gym.json --bench-root sovereign/bench --update-baseline
# knowledge-gym (~1m) → gate:
target/debug/sovereign-cli-llm knowledge-gym run --json > target/ci-bench/knowledge-gym.json
target/debug/sovereign-cli-llm bench gate knowledge-gym --report target/ci-bench/knowledge-gym.json --bench-root sovereign/bench --update-baseline
# agent-coding (3 hardest, ~12m) — separate binary:
target/debug/sovereign-agent-bench run --problems 3.2-lights-out,3.2-lights-out-python,5.1-minilang-multifile-python \
  --judge-trials 1 --report target/ci-bench/agent-coding.json
target/debug/sovereign-cli-llm bench gate agent-coding --report target/ci-bench/agent-coding.json --bench-root sovereign/bench --update-baseline
# multiturn (full marathon, ~30m, can crash a loaded daemon under sustained load — watch for all-zero threads):
target/debug/sovereign-cli-llm eval run --threads --bank sovereign/bench/wikipedia_learn/threads.toml --output target/ci-bench/threads.json
target/debug/sovereign-cli-llm bench gate multiturn --report target/ci-bench/threads.json --bench-root sovereign/bench --update-baseline
```
Then `git add sovereign/bench/*/baselines && git commit`. (Tip: `scripts/sovereign-ci-bench.sh --update-baseline --no-synth` does the whole sweep + validates the script end-to-end — but it overwrites the deterministic-lane baselines too; prefer the per-lane commands if you want surgical baselines.)

**Step 2 — quality iteration (≥ a few rounds).** Highest-value known gaps the baselines expose:
1. **IQ4 honesty 0.45** — the canonical model answers OOD ("capital of Australia", "margarita") instead of abstaining. Either a humility-gate/prompt fix, or revisit the quant. This is the chaos honesty red-line.
2. **Alternation grammar is broken** (the actual tool-loop root cause) — its `text|tool_envelope` + injected `done` escape is unreachable (`inference_adapter.rs:423-472`, `llguidance_constraint.rs:415 build_tool_alternation_grammar`). It was meant to close agent-bench `loop_trap`/`parse_failed_envelope`; fix it so it can be re-enabled safely, then re-validate against search-gym + knowledge-gym. Until then it stays OFF.
3. **search-gym 02_stock_price + 07_multicorpus_tangential** fail even grammar-off (genuine tool-judgment edge cases).
4. **chaos v2 citation_fidelity 0.25 / distractor_evasion 0.33** — grounding precision; the neighbour-window catches the region but not always the exact supporting chunk (try `EXPANSION_NEIGHBOR_RADIUS` tuning or the FUTURE_RESEARCH grounding verifier).
5. Whatever **knowledge-gym + agent-coding** reveal once baselined.

## Gotchas
- `~/.sovereign/config.toml` is local (not in repo); the two config fixes (models, `alternation_grammar=false`) live there. Repo defaults are already correct (`setup_config.rs:498`).
- This repo runs on a Commonwealth mesh with **concurrent agents** — commits land underneath you (the "flywheel" + "fix retrieval" commits this session swept work in). Always `git status` + stage explicitly; never `git add -A` blindly.
- MTP is a red herring for tool-loops (gate `model_slot.rs:2124` skips MTP when `tools` is set).
- corpus_id: `chaos-secret-agent` (recipe-install); bank `[meta].corpus` + manifest `[meta].default_corpus` already default to it.
