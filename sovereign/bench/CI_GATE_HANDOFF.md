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

**ALL 6 re-captured on the corrected stack 2026-06-08 (this session).** Values:

| Lane | Baseline (corrected stack) | Notes |
|---|---|---|
| chaos-monkey | `secret_agent/2026-06-08` — competence 0.67, **honesty 0.45**, hallu 0.55, citation_fidelity 0.25, distractor_evasion 0.33 | VALID. **Honesty diagnosis CORRECTED:** the prior handoff said "IQ4 answers OOD" — WRONG. OOD is 5/5 honest (answers WITH a "general knowledge, not your sources" caveat — the desired HYBRID). The 6 hallucinations are all `absent_adjacent` (Heat's first name, embassy country, …) answered instead of abstained — AND those answers leak un-tagged chain-of-thought (`"The user is asking…"`) that `strip_think` misses and the abstain-classifier mis-reads. Disambiguate H1 (real fabrication) vs H2 (missed abstention) with a live full-answer probe. Competence/citation gaps are a SEPARATE retrieval-grounding issue (provenance 1/4, distractor 0/3 on the single 316-chunk doc). |
| search-gym | `ci/2026-06-08` = **0.80** (24/30) | VALID — reproduced exactly on the grammar-fixed stack. |
| mechanism-fidelity | `dev/2026-06-08` — control_p1_delta 0.000, p1_collapse −0.331 | VALID. **Re-ran fresh** — the prior run RESUMED a stale Jun-7 old-model cache (`mechanism.jsonl.partial.jsonl`); purge it before re-running or you re-capture stale data. Fresh IQ4 is MORE faithful (collapse −0.005→−0.331). |
| knowledge-gym | `ci/2026-06-08` = **0.818** (27/33) | VALID (first capture). 9/11 fixtures perfect; the 2 zeros are `05_noresults_honesty` (model is honest but skips the lookup tool — strict fixture) and `06_fabricated_id_blocked` (cited a fabricated ev-id + leaked visible confusion — corroborates the reasoning-leak). |
| agent-coding | `ci/2026-06-08` = **0.333** (9/27) | VALID — but REQUIRES the env cocktail (see below). Without it: floored ~0.11 (model 503s on `commonwealth/coder` / early-exits). 3.2-lights-out-python = 9/9 perfect; Rust + 5.1 = 0 (write_thrash / no_progress — termination gap). |
| multiturn | `wikipedia_learn/threads/2026-06-08` | Re-baselined this session on the corrected (clean, force-off) daemon. |

### Agent-coding cocktail (was the suspicious 3/27) — REQUIRED to reproduce

`sovereign bench gate agent-coding` is only meaningful when agent-coding runs under the right daemon config. Two things the corrected stack dropped:
1. **`--model`** — agent-bench defaults to `commonwealth/coder`, which the corrected stack doesn't advertise (no `code=` slot) → 503 → floored 0/27. Pass `--model commonwealth/primary` (the coder model is NOT needed — confirmed). Fixed in `scripts/sovereign-ci-bench.sh` (`AGENT_MODEL`).
2. **`SOVEREIGN_FORCE_TOOL_CALLS=1`** (+ `SOVEREIGN_DISABLE_AUTO_RESUME=1`) on the DAEMON. Without it the model emits ~100 tokens of chat, no tool call, and pi's zero-tool-call exit fires (inference_adapter.rs:722). WITH it: real solving (147→2971+ tokens, python 9/9). This flag is daemon-GLOBAL and forces a tool call on every tools-bearing request → regresses search-gym judiciousness, so **agent-coding needs its own daemon pass** (can't share with the gym/chaos lanes).
   - `FORCE_TOOL_CALLS` alone re-engages but causes write_thrash/no_progress kills (forces a tool every turn → no text turn to test/terminate). The historically-"perfect-on-first-two" behavior came from the **alternation grammar** (text|tool_envelope escape, lets the model write→test→done), which is now broken (loop-trap). Repairing it (Step 2 #2) is the real fix and unblocks a single shared daemon.

### Agent-coding RESOLVED 2026-06-08 — the lever was NOT pi-runner config; it's TWO things

A deep evidence-driven session (per the operator's "don't blame the model — substantiate") landed a clean dissociation. Threshold-tuning the pi FORCE_TOOL_CALLS loop (SAME_PATH_WRITE_THRESHOLD 3→8, NO_PROGRESS 8→20) did NOT help — Rust still write-thrashed at the higher cap, re-emitting the same `lib.rs` (4× identical) without ever testing. Reverted. Then the operator pointed at `sovereign/docs/TDD_MACHINE.md` (the proven paradigm: lights-out 8.33/9 on Darwin-36B, 2026-05-24) and the real picture emerged:

1. **HARNESS BUG (fixed, commit 42735d56).** Commit 439c27e4 ("workspace lint policy") added `[lints] workspace = true` to all 6 Rust scaffolds → standalone tempdir build fails at the MANIFEST level → 0 tests build → **structural 0 for any solution, both runners**. This alone made every Rust problem unscoreable after 2026-05-24. Fixed by stripping the block; 3.2-lights-out now yields a real `0 passed; 3 failed` TDD baseline.

2. **The proper runner is `--agent search` (TDD Machine), NOT `--agent pi`.** The `search` runner (`commonwealth_tdd::run_trial`, MaximizePassing) drives the tests FOR the model — runs `verify_cmd` (= the witness's `cargo test`/`pytest`), feeds bucketed compile/test errors into the next round, gates on strict improvement, pristine-restart on plateau. `TrialConfig::default` = 4 candidates × 6 rounds (healthy search, not starved). The model just generates; the harness owns the loop. The `pi` runner (forced tool envelope, no thinking channel) was never the working paradigm.

3. **CONFIRMED: TDD Machine + IQ4 converges where the model can produce valid code.** `--agent search` on 3.2-lights-out-**python** → **8/9** (dim_a=3 full held-out correctness, dim_b=2, dim_c=3), "3/3 smoke green in 1 round", exit=completed. Matches the validated 8.33/9. The paradigm works; the model is capable.

4. **SUBSTANTIATED: the IQ4 model is specifically weak at RUST syntax/compilation** (controlled probe, identical conditions, language the only variable): Rust **0/3 even compile** (`unexpected closing delimiter`, `missing open (`, planning-prose leaking into the source at ~90–170 lines); Python clears the syntax wall (1/3 ran all tests, the rest logic/collection errors). So Rust under `--agent search` stalls at 0/3 — the loop never gets a *compiling* candidate to iterate from. This is a genuine model ceiling, not a harness artifact.

**Net for the agent-coding lane:** the committed 9/27 (pi, pre-fix) UNDER-counted (Rust was a structural 0). Proper paradigm = `--agent search` on the fixed scaffolds. The model scores strongly on Python (8/9) and ~0 on Rust (compile-level weakness). To raise the lane: weight Python problems, or use a stronger-at-Rust model — NOT more harness tuning. Re-baselining agent-coding under `--agent search` is a CI-lane-config decision (which runner + problem mix) left for the operator; the 9/27 pi baseline stands as-is until that's decided.

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

**Step 2 — quality iteration (≥ a few rounds).** Highest-value gaps the corrected-stack baselines expose, in leverage order:

1. **[REVERTED 2026-06-08 — hypothesis REFUTED, empirically dominated. See UPDATE at the end of this item before re-attempting.] Fix the alternation grammar — HIGHEST LEVERAGE (one fix, three payoffs). ROOT CAUSE FOUND 2026-06-08 — the fix is SURGICAL, not "a day or more."** The bug is NOT the grammar; it's that the working grammar is UNWIRED. `inference_adapter.rs:436-469` (the `alternation_grammar_enabled()` branch) does NOT call `build_tool_alternation_grammar` (the Lark `start: think_block? body / body: tool_envelope | plain_text` grammar with a genuine plain-text escape, `llguidance_constraint.rs:415`). Instead the "canonical 2026-05-21 path" routes a **pure JSON schema** of `inject_done_tool(envelope)` (`inference_adapter.rs:459-469`) — which grammar-locks the model to emit JSON on EVERY turn. The only escape became a `done` tool call, never plain text → any tool-caller needing a prose turn (search-gym synthesis, a chat answer) is locked out of text → trap ([[invariant_alternation_grammar_breaks_tool_calling]]). Proof the Lark grammar works: `recipe_author.rs:725` has a LOCAL COPY of it that IS wired and ships (closed the recipe-author TOML-malformation class). **The fix: route `inference_adapter.rs:436` through `sovereign_inference::build_tool_alternation_grammar(&schema_json)` (set `req.lark_grammar` to the Lark text, not the `inject_done_tool` JSON schema)** — a few lines, the function already exists + is unit-tested. Watch the tradeoff the comment at `:445-450` flags (the older Lark+%json accepted partial JSON `{...]}`); iteration-3 wraps `%json {schema}` inside literal `<tool_call>…</tool_call>` markers, which SHOULD enforce strict closure — re-validate. Payoffs: (a) agent-coding terminates cleanly (write→test→plain-text/done) instead of write-thrashing → well above 9/27 (the historical "perfect on first two"); (b) enable GLOBALLY without the trap → one daemon serves agent-coding AND gym lanes (no separate `FORCE_TOOL_CALLS` pass); (c) likely helps the un-tagged-reasoning leak. Requires daemon rebuild (`-p sovereign-mesh`) + restart + re-validate search-gym (must NOT regress) and agent-coding (must terminate).

   **UPDATE 2026-06-08 — wired the fix, validated, REVERTED. The grammar is STRICTLY DOMINATED for both lane types; do not re-attempt this lever.** The one-line fix (route inference_adapter.rs:436 through `sovereign_inference::llguidance_constraint::build_tool_alternation_grammar(&schema_json)`) compiles + works. Validated on the IQ4 stack with `SOVEREIGN_ALTERNATION_GRAMMAR=1`:
   - **Repairs the trap** ✅ — search-gym 0/30 (broken grammar, all hung at MAX_TURNS) → 18/30. The plain-text escape is genuinely reachable now.
   - **But regresses search-gym vs grammar-OFF** ❌ — 0.60 (18/30, --replays 3) < 0.80 grammar-off. Constraining tool calls to the `<tool_call>` envelope + gated plain_text costs search judiciousness (gate REGRESSED, −0.20).
   - **And regresses agent-coding vs FORCE_TOOL_CALLS** ❌ — 3/27 < 9/27. Given a text escape, the IQ4 model takes the lazy path (~64-141 tokens, no tool calls, clean `completed` exit, no solving) — the early-exit returns.
   - **Conclusion:** gyms prefer grammar-OFF (0.80), agent-coding prefers FORCE_TOOL_CALLS (9/27); the alternation grammar is the worst config for both. Reverted the inference_adapter.rs change; `alternation_grammar` stays OFF. **The real agent-coding lever is model ENGAGEMENT, not output format** — free (grammar) → won't run the agentic loop; forced (FORCE_TOOL_CALLS) → engages but can't terminate (write_thrash at `SAME_PATH_WRITE_THRESHOLD=3`). Next levers: raise that threshold so the forced write→test loop survives; a concrete first-tool-call prompt template (agent-bench HANDOFF.md:743); or a coder model. The 9/27 FORCE_TOOL_CALLS baseline stands as current-state.

2. **Reasoning-leak / honesty (chaos honesty 0.45 + knowledge-gym `06_fabricated_id`).** CORRECTED diagnosis: not OOD-answering (OOD is 5/5 honest). The failure is `absent_adjacent` answered-instead-of-abstained, AND the answers leak un-tagged chain-of-thought (`"The user is asking…"`) that `strip_think` (only strips `<think>…</think>`) misses → the abstain-classifier mis-reads it AND it's a UX bug. **First action: a live full-answer probe of one adjacent question** to disambiguate H1 (genuine fabrication → prompt/grounding fix) vs H2 (correct abstention mis-classified → fix the leak). The leak is systemic (also in knowledge-gym `06`). Do NOT just append `"The user is asking"` to `presenter.rs PREAMBLE_PREFIXES` (whack-a-mole, [[feedback_embed_router_over_keyword_classification]]); the principled fix is why the synthesis Drafter (runner.rs:584, `enable_thinking=false`, `Speed::Slow`) emits un-tagged meta-narration at all, and whether `KNOWLEDGE_SYNTHESIS_SYSTEM`'s loud "ANSWER, don't deflect" bias is overriding the "not in your sources" abstention exception for adjacent facts.

3. **chaos citation_fidelity 0.25 / distractor_evasion 0.33 / competence provenance 1/4, distractor 0/3** — retrieval-grounding precision on the single 316-chunk doc; the neighbour-window catches the region but not always the exact supporting chunk (try `EXPANSION_NEIGHBOR_RADIUS` tuning or the FUTURE_RESEARCH grounding verifier).

4. **search-gym 02_stock_price + 07_multicorpus_tangential** fail even grammar-off (genuine tool-judgment edge cases).

## Gotchas
- `~/.sovereign/config.toml` is local (not in repo); the two config fixes (models, `alternation_grammar=false`) live there. Repo defaults are already correct (`setup_config.rs:498`).
- This repo runs on a Commonwealth mesh with **concurrent agents** — commits land underneath you (the "flywheel" + "fix retrieval" commits this session swept work in). Always `git status` + stage explicitly; never `git add -A` blindly.
- MTP is a red herring for tool-loops (gate `model_slot.rs:2124` skips MTP when `tools` is set).
- corpus_id: `chaos-secret-agent` (recipe-install); bank `[meta].corpus` + manifest `[meta].default_corpus` already default to it.
