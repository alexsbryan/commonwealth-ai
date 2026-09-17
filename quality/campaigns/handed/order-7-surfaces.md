---
schema: work-order/v1
id: handed-7-surfaces
status: open
drafted: 2026-09-17
approved: pending
serves: handed
campaign: handed
lane: proof — the bench serves one grounded and one generative turn in-process and pastes both verdicts plus the commission trace
engine: ralph pool; REVIEW row, main workdir
budget: 1 row; no build rows, no HUMAN row
---

# Order: handed-7-surfaces — one in-process turn pair, three pasted lines

## Objective

Predicate half 2 and bar `hd-surfaces` (target 1): the bench serves one grounded and one generative turn
**in-process**, and both terminal values show the verdict hd-1 put on them, under the profile hd-2 made
structural. This order builds nothing. It runs one command and pastes three lines: two
`turn.verdict: serve_turn complete` lines — the grounded one reading `verdict=passed` or `verdict=failed`,
the generative one `verdict=never-ran` — and the one `runtime: commissioned launch=…` line.

Cut at round 1 (2026-09-17): the desktop driver script, the daemon-restart HUMAN row, and the chat / HTTP /
desktop DEMOs. All four surfaces terminate in ONE `TurnFrame::Complete` builder (serve.rs:306, :425, :506),
so after hd-1 a frame without a verdict does not compile on any of them; what a live DEMO adds beyond
compile and test is real-model liveness on ONE surface, and the bench is the surface that needs no daemon
at HEAD. "Under a named profile" also left the predicate: once hd-2 lands, `commission(&Launch, …)` is the
only door to a `Runtime`, so every served turn has a named profile structurally.

## Premises (verified 2026-09-17, file:line)

- **The bench turn is in-process and does not use the daemon's turn wire.**
  `bench chaos-monkey run` → `cmd_chaos_monkey` (sovereign-cli-llm/src/bench_cmd/mod.rs:197) →
  `build_session_sealed` (chaos_monkey.rs:444, defined chat_cmd/bootstrap.rs:167) → per question
  `run_live_pinned` (chaos_monkey.rs:767), which calls `sovereign_core::runtime::collect_turn`
  (bench_cmd/live_runner.rs:100-108) with `TurnMode::Grounded`. `collect_turn` is "`serve_turn` with a
  collecting sink" (serve.rs:607-644). Only INFERENCE leaves the process: `build_session_scoped` probes the
  daemon and builds an HTTP provider (`build_inference`, bootstrap.rs:194-206), and hd-1/hd-2 change no
  part of that wire. **So no daemon restart is needed and no HUMAN row exists.**
- The bench verbs install a tracing subscriber **only when `RUST_LOG` is set**
  (sovereign-cli-llm/src/lib.rs:168), and `RUST_LOG` then replaces the default filter entirely
  (`EnvFilter::try_from_default_env`, sovereign-cli-shared/src/tracing_init.rs:28).
  `.with_target(false)` at :35 — a printed line carries no target, which is why the two greps below match
  MESSAGE text, not targets. A dotted directive parses: the daemon's own filter already carries
  `gate.call=info` (sovereign-cli-daemon/src/lib.rs:65) and its pinning test expects "dotted targets
  included" (:358).
- CLI args, read from the parser: `--bank`, `--corpus`, `--judge-model`, `--critic-model`, `--base-url`,
  `--manifest`, `--out`, `--transcripts`, `--limit`, `--smoke-subset`, `--naked`, `--warm-atlas`
  (sovereign-cli-llm/src/bench_cmd/chaos_monkey.rs:238-258). With neither `--naked` nor a bridge client,
  the live path is `run_live_pinned` (:765-769).
- **The bank contract.** `ChaosBank { meta: BankMeta { corpus, description }, questions }`
  (sovereign-eval/src/chaos_monkey/question.rs:188-200); `ChaosQuestion { id, qtype, question,
  gold_keywords, …, rationale }` (:128-147); `qtype` is snake_case `PressureKind` (:30-31), `present` at
  :34. `validate` REFUSES an answerable question with no `gold_keywords` (:226-235), so the generative
  probe carries one.
- **The probes.** Grounded: `sovereign/bench/chaos_monkey/saltgrass.toml:22-27` — id `present-victim`,
  question "Who is found drowned in the lock basin at Glasswater Stave?", gold `["Pellow"]`, over the
  held-out original novella (`[meta] corpus = "chaos-saltgrass"` :16-17; contamination-free by
  construction, :4-9). The index exists on this host: `~/.svrnmesh/indexes/chaos-saltgrass`.
  Generative: "Write a poem about the ocean" — the router's own creative fixture
  (sovereign-core/src/router.rs:3048).
- **The two lines the paste must match**, each a requirement on the row that emits it:
  - `quality/campaigns/handed/order-1-render.md` step 2 / row `REVIEW-build-hd-1-complete`:
    `tracing::info!(target: "turn.verdict", conversation_id, message_id, verdict = …, reason = …, source = …, "turn.verdict: serve_turn complete")`,
    emitted once per turn at the one `Complete` builder.
  - `quality/campaigns/handed/order-2-assemble.md` step 4 / row `REVIEW-build-hd-2-commission`:
    `tracing::info!(target: "capability", launch = launch.as_str(), "runtime: commissioned")`, emitted
    exactly once per process.
- **The pool runs this row in the main workdir.** `Pool.run` takes `first_ready_review` before
  `pick_wave`, and both test `r.id.startswith("REVIEW-")` (scripts/ralph.py:256-261, :276);
  `run_review` runs it in `self.paths.workdir` (:770). A non-`REVIEW-` id would be scheduled as a LANE in a
  fresh `git worktree` (:811-815) where `target/debug/` does not exist — nothing in `.cargo/config.toml`,
  `scripts/with-cargo-lock.sh`, `scripts/dev-build.sh` or ralph.py's `Session` env sets
  `CARGO_TARGET_DIR`. Hence the `REVIEW-DEMO-` prefix.
- **CLEAN does not guarantee a built binary.** `dev-build.sh --clean --gate-only` skips the build whenever
  the debug target is under `RALPH_CLEAN_MB` (default 51,200 MB) — scripts/dev-build.sh:104-120 — and LINT
  is `cargo check`. So the row builds explicitly with the sanctioned debug build
  (`scripts/dev-build.sh`, `--workspace --features corpus-engine/treesitter,sovereign-cli/dev-tools`, :123-135).
- `sovereign-cli` is a dispatcher that `exec`s `sovereign-cli-llm` for `bench`; a full workspace build
  produces both, and the environment (including `RUST_LOG`) survives the exec.
- `svrn` is not on this host's PATH; `~/.local/bin/sovereign -> target/debug/sovereign-cli`. The row calls
  `target/debug/sovereign-cli` directly.
- The resident daemon answers on 127.0.0.1:9741 (`GET /status`, node `node-37f17554b6c4ff29`, 2026-09-17);
  loopback callers skip client auth (sovereign-api/src/client_auth.rs:215-218). ralph/PROMPT.md §7 forbids
  the loop stopping or restarting it — this row does neither.

## Steps

1. **Write the two-question scratch bank** to `target/ralph/hd7-bank.toml` (untracked; §5 already
   requires `mkdir -p target/ralph`):

   ```toml
   [meta]
   corpus = "chaos-saltgrass"
   description = "hd-7 surface probe: the verdict is read, the score is not"

   [[questions]]
   id = "present-victim"
   qtype = "present"
   question = "Who is found drowned in the lock basin at Glasswater Stave?"
   gold_keywords = ["Pellow"]
   rationale = "saltgrass.toml:22-27 verbatim — Corwin Pellow, the harbormaster, stated in chapter I"

   [[questions]]
   id = "generative-ocean"
   qtype = "present"
   question = "Write a poem about the ocean"
   gold_keywords = ["ocean"]
   rationale = "hd-7 surface probe: the router's own creative fixture (router.rs:3048); the verdict is read, the score is not"
   ```

2. **Build HEAD**, because CLEAN may skip it (Premises):
   `./scripts/with-cargo-lock.sh ./scripts/dev-build.sh > target/ralph/hd7-build.log 2>&1; echo exit=$?`

3. **Run the bench in-process with both traces on:**
   `RUST_LOG=turn.verdict=info,capability=info target/debug/sovereign-cli bench chaos-monkey run --bank target/ralph/hd7-bank.toml --corpus chaos-saltgrass --out target/ralph/hd7-bench.jsonl > target/ralph/hd7-bench.log 2>&1; echo exit=$?`

4. **Paste** `grep -nE 'turn\.verdict: serve_turn complete|runtime: commissioned' target/ralph/hd7-bench.log`
   — expected exactly THREE lines: one `runtime: commissioned launch=…`, then two
   `turn.verdict: serve_turn complete … verdict=… reason=… source=…`, the first `verdict=passed` or
   `verdict=failed` with `source=gate`, the second `verdict=never-ran` with `source=absent`.

The bench's own `exit=` is recorded but is NOT the gate: after each turn the run calls a judge and a critic
model (`--judge-model` defaults to `"fast"`, chaos_monkey.rs:200), and this row reads no score. A judge
error in the log is not a failure of this row; a missing or wrong-verdict `turn.verdict` line is (§6).

## Seams

- Touches no tracked file. It writes `target/ralph/hd7-*` only.
- Depends on hd-1's `turn.verdict` trace and on hd-2's `capability` trace, both quoted in Premises. If
  either message text differs from the literal there, the grep is the instrument that says so — that is a
  finding against the row that emitted it, not a reason to loosen the grep.
- The seat places this row LAST in `ralph/STATE.md` and adds the final `REVIEW-audit-hd-*` id to its
  `depends` when the audit rows are inserted (PROMPT §4: the seat inserts audit rows). Without that, the
  pool's `first_ready_review` will run it as soon as its two named dependencies are `[x]`, which may be
  mid-campaign — the evidence is still valid, but a later lane could regress it unobserved.
- Reads the operator's resident corpora and models over loopback and starts, stops and restarts nothing.
- Three of the four surfaces named in the campaign's `today` line (chat, daemon HTTP, desktop) go through
  the daemon's turn wire, which hd-1 changes with no serde default. They are not exercised here, and the
  operator's next daemon restart is what makes them work again — named in
  `quality/campaigns/handed/order-1-render.md` Seams, owned by neither rung.

## Done when

- The row is `[x]` with the three lines of step 4 pasted in its commit body, alongside both `exit=` values.
- Bar `hd-surfaces` measures **1** (this row is the instrument the bar names).
- This rung lands no `enforced_by` and deletes no ambient path — stated rather than implied. It is bar
  `hd-surfaces`'s measurement, and its Done-when IS the pasted evidence. The predicate's structural half
  and every PLANT belong to rungs hd-1 … hd-6.
- Read back as a shape check, printing nothing: `grep -c 'verdict=' target/ralph/hd7-bench.log` is **2**,
  and `grep 'turn\.verdict: serve_turn complete' target/ralph/hd7-bench.log | grep -v 'verdict='` is empty.

## Kill

- The grounded probe reads `never-ran` or `could-not-judge` on two consecutive runs. The gate is not
  reaching the turn, or the judge is failing open on a one-token factual probe: stop, and reopen the rung
  the `source=` field names (`source=absent` → hd-1's stamp; `source=gate` with `could-not-judge` → the
  judge, which is hd-1's Kill).
- No `runtime: commissioned` line appears. `commission` is not the only door, or the trace is not where
  hd-2 put it: stop and reopen hd-2.
- The generative probe does not read `never-ran` on two consecutive runs — the router put it on a grounded
  path inside a corpus-scoped conversation. Pick the probe by operator decision, not by retry.
