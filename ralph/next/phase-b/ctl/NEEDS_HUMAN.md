# NEEDS_HUMAN — pb-svrn-dials-serve: the first-token latency bar misses before the switch

## (a) The unit and its row

`pb-svrn-dials-serve` (ralph/next/phase-b/STATE.md, the `[~]` row). The row:

> BEFORE the switch commit, run the header's two latency bars and paste them:
> first-token latency p50 and embedding throughput at batch 32, n ≥ 5 each,
> in-process against the terminal arm dialing serve on loopback. Run both on
> temp roots on free ports with the smallest chat and embedding models in
> sovereign/models/ ... A miss is NEEDS_HUMAN with the numbers.

Header bars (pre-registered 2026-09-25): first-token p50 loopback at most 10%
slower; embedding throughput at batch 32 at least 90% of in-process.

Everything the switch needs except the switch is built and committed on
`cut` (052b307c5 .. e50c62bc4): serve's reload, self-report, `/v1/completions`
routes; the shared FIM renderer; the terminal arm's loopback mode in
oicp-client; the daemon's `ensure_serve`, `read_served_self`,
`loopback_provider`, `forward_reload`. The switch itself (ServingPath read at
boot, no engine on the dialing path) is NOT made. The tree compiles and every
suite touched is green.

## (b) What ran, and what it said

Harness: `sovereign/crates/sovereign-daemon/tests/main/serve_latency_bars.rs`
(e50c62bc4, `#[ignore]`d). In process = `assemble_serving` over a temp-root
config; loopback = `serve_client::loopback_provider` dialing a debug
`sovereign-serve` on a free port over the same config file. Warm-up
discarded, 7 runs each. Debug builds (what the deployed symlink runs).

```
toolbox run -c sovereign-vulkan bash -lc 'cd <repo> && SOVEREIGN_SERVE_BIN=$PWD/target/debug/sovereign-serve \
  scripts/with-cargo-lock.sh cargo test -p sovereign-daemon \
  --features sovereign-daemon/treesitter,corpus-engine/treesitter \
  --test main serve_latency_bars -- --ignored --nocapture'
```

Chat Qwen3.5-0.8B-UD-Q6_K_XL.gguf, embed Qwen3-Embedding-0.6B-Q8_0.gguf (the
smallest of each, named before the first run):

| run | first-token p50 in → loopback | ratio (bar ≤ 1.10) | embed/s p50 in → loopback | ratio (bar ≥ 0.90) |
|---|---|---|---|---|
| 1 | 28.4 → 32.9 ms | 1.157 MISS | 123.2 → 110.8 | 0.900 at bar |
| 2 | 27.4 → 31.9 ms | 1.162 MISS | 122.8 → 112.4 | 0.915 pass |
| 3 | 27.3 → 33.5 ms | 1.228 MISS | 123.4 → 110.5 | 0.895 MISS |

Diagnostic, not the bar — the same harness with chat Qwen3.5-4B.Q6_K.gguf:
first-token 77.5 → 82.8 ms (x1.067, would pass); embed 118.3 → 104.5/s
(x0.883, misses). So the first-token cost is a fixed ~4.5–6 ms per request,
and the embedding cost scales with the batch (~10–12%). Not measured: a
release build, or where inside the ~5 ms the time goes (HTTP + JSON + serve's
OpenAI adapter are the candidates). Logs:
`target/ralph/phase-b/latency-bars-{1,2,3,4b}.log`.

## (c) What the operator decides

1. **The first-token bar** (x1.16–1.23 on the 0.8B model, a fixed ~5 ms).
   Options: (a) accept the miss and let the switch land, with the numbers in
   the switch commit; (b) require the overhead found and cut before the switch
   (an instrumentation row: where the ~5 ms goes, then a fix); (c) something
   else. The bar is not re-tuned by the worker.
2. **The embedding bar** (x0.895–0.915; x0.883 on the diagnostic run). It sits
   on the line. Options: (a) accept; (b) require the embeddings wire made
   cheaper (it sends 32 × 1024 f32 as JSON) before the switch; (c) keep
   embeddings in process on the dialing path, which would keep the daemon
   linking the engine for embed only and cuts against the row's outcome.
3. Once decided: the remaining work in the row is the switch commit
   (serving_boot.rs: `ServingPath::decide`, `ensure_serve`, the loopback
   provider; no engine, discovery or warm orchestrator on the dialing path),
   the reload factory forwarding (provider.rs), the readers (mesh status
   engine rows, /v1/models, /status rows, routes_kinds, assets_http hardware
   and setup_planner, serve-side preflight), NER's route, `svrn daemon
   status` naming both processes, the exception reasons, the SYSTEM_OVERVIEW
   line, the svrn RUN smoke and the remaining PLANTs. Progress notes:
   `target/ralph/phase-b/pb-svrn-dials-serve-progress.md`.

## (d) To resume

Edit or mark the row in ralph/next/phase-b/STATE.md (for example, record the
decision under the row), then
`rm ralph/next/phase-b/ctl/STOP ralph/next/phase-b/ctl/NEEDS_HUMAN.md`.

## (e) Director review, 2026-09-26 (supervisor resolution, attempt 1)

This fork is the operator's by the charter's own example ("a pre-registered
bar a row cannot meet (for example pb-svrn-dials-serve's latency bar)"), so
the director does not decide it. It leaves this package in place and does not
remove the blocker.

What I checked: the four logs match (b) line for line, including the per-run
arrays. I reran the harness once myself
(`target/ralph/phase-b/latency-bars-director.log`, EXIT=101): first-token p50
53.2 → 63.3 ms (x1.189, MISS); embed 74.3 → 53.3/s (x0.718). That run is
contaminated. The deployed daemon and cw-rails were busy (cw-rails ~92% CPU,
load 3.2), the in-process baseline was roughly double the worker's, and
loopback had 166/176 ms outliers. It confirms the first-token direction and
says nothing reliable about embeddings. The worker's three quiet runs remain
the measurement.

Recommendation: option 1(b) and 2(b) folded into one bounded instrumentation
row before the switch, not acceptance. Two observations point there:
- The bars do not name a build profile. The worker measured debug, which is
  right for "what the deployed symlink runs". But a fixed ~5 ms per request,
  plus an embed cost that scales with a 32×1024-f32 JSON body, is what debug
  serde and HTTP look like. A release reading of both sides takes minutes and
  tells the operator whether the miss is the wire or the profile. Which
  profile the bar binds is itself the operator's call. Picking one after
  seeing the data would be re-tuning (principle 7).
- The ~5 ms is not attributed (principle 1/2). Spans at serve's request
  boundary (accept → parse → adapter → first token written) and a client-side
  span in `serve_client::loopback_provider` would locate it. The embed bar
  sits at x0.895–0.915 across three runs, inside run-to-run noise, so
  accepting or refusing it on these numbers is a coin flip.
Cost: one short row (instrument + release rerun, n ≥ 5, on a quiet host). The
switch stays unmade meanwhile; everything else it needs is already committed.
Option 2(c) (embeddings stay in process) keeps the daemon linking the engine
and defeats the row's outcome. I would not take it.
