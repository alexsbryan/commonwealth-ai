# llama-server adoption: replacing the embedded engine with a supervised sidecar

Operator request 2026-09-17, following the build-vs-buy audit of the same day.
Read-only due diligence plus a hands-on feasibility check; no product code lands
from this report. It is a decision record with pre-registered bars, not a plan
of record.

Sources: this repo at `ralph/domains-campaign` HEAD (2026-09-17); the upstream
llama.cpp prebuilt release **b11026** (commit `b49650adb`), run on macOS arm64
(Metal) with raw outputs committed under
[`evidence/llama-server-b11026/`](evidence/llama-server-b11026/); the llama-server
README at that tag; ggml-org/llama.cpp issues and PRs via `gh` (fetched
2026-09-17); the notes store (ids cited in place).

Verification status: every claim marked **(run)** was executed on b11026 and
has a file in the evidence directory. Claims about this repo carry `file:line`;
the load-bearing ones (the judge fail-open, the remote client's forwarded
fields, the degradations doc, the vendored RPC bug, sizes and churn of
`src/embedded`, the throughput lane's gating status) were re-checked by the
author. The rest come from a migration-surface review pass and are cited so
they can be checked.

## BLUF

Adopt llama-server **as a hybrid, gated on a parity spike, and fix the remote
seam first regardless of the outcome.**

The reasons the embedded engine was chosen (note 94eff39e, 2026-06-05) have
mostly expired: upstream now ships prebuilt `llama-server` and `ggml-rpc-server`
for macOS arm64, Linux Vulkan and Windows Vulkan, and has draft-MTP, router
mode, idle sleep, slot save/restore, rerank and `/infill` built in. Everything
upstream claims that we could test on one machine worked **(run)**, with four
exceptions that shape the design: the prebuilt has **no llguidance, and a
`%llguidance` grammar aborts the server process**; router mode **returns HTTP
500 when two cold loads race** under `--models-max 1`; router `POST /models`
**ignores `--offline`**; and embeddings match ours **only with our input
preparation** (literal EOS appended), cosine 0.989 without it and 1.000 with.

Three things stand between us and a swap, and none is a missing llama-server
feature:

1. **Our remote seam is dishonest today.** On `kind="remote"` the grounding
   judge fails open silently, citation and URL allowlists are dropped without a
   trace, and the node keeps advertising embedded-only features. That is live
   for anyone who follows `docs/USE_YOUR_OWN_INFERENCE_SERVER.md` now, and it
   is a principle-6 defect independent of this decision.
2. **Sampler-level allowlists have no upstream equivalent.** Evidence-id and
   URL allowlists are per-token logit masks; they must be re-expressed as
   grammars or replaced by validate-and-retry, and fabrication safety has to be
   re-proven.
3. **Our primary host is upstream's weakest platform.** Open issues on Strix
   Halo / RADV include an abort at first decode for `qwen4exp` (#29028, filed
   today; our vendored pin is the qwen4exp PR head) and draft-MTP `DeviceLost`
   (#27306).

What the swap buys: about 24.9k raw lines of `src/embedded` (92 commits in 90
days) and the vendored binding (19.4k raw Rust in `vendor/llama-cpp-4`, plus
`vendor/llama-cpp-sys-4`'s build script, shims, patches and the vendored
llama.cpp tree of about 817k lines) move out of our maintenance; llama.cpp stops compiling in CI and in every
release leg; and a llama.cpp security fix becomes a binary bump instead of a
vendor re-sync. The vendored ggml-rpc today still carries the use-after-free
that upstream PR #24292 fixed on 2026-09-16.

## 1. The decision, precisely

In scope: replace the in-process engine (`sovereign/crates/sovereign-inference/src/embedded/`
and `vendor/llama-cpp-4`, `vendor/llama-cpp-sys-4`) with a `llama-server`
process per node that the daemon supervises, reached through the existing
`[engine] kind="remote"` seam (`RemoteApiProvider`, `oicp-client/src/lib.rs`).

Out of scope: the daemon, the OpenAI-compatible gateway on `:9741`, routing,
the grounding gate, retrieval, the mesh. Those consume the engine; they are
not the engine.

## 2. Why the original reasons no longer hold

Note 94eff39e recorded why the embedded path beat a subprocess. Status of each
on 2026-09-17:

| Reason given (94eff39e) | Status |
|---|---|
| llama-server will not build from the sys crate's pruned tree | Still true of our tree (`tools/server` is excluded), and irrelevant: upstream publishes prebuilt servers for all three targets **(run: archive listings)**. |
| The embedded path keeps the MTP shim | Upstream `--spec-type` lists `draft-mtp` **(run: `--help`)**. Not executed: the on-disk MTP models exceed the test's size limit. |
| Keeps the daemon's streaming path | The daemon keeps its streaming path; only the token source moves. |
| No extra binary for an RPC worker | Every archive ships `ggml-rpc-server` **(run)**. The worker becomes a sidecar too. |
| Backend singleton makes reloads possible in-process | Router mode runs one child process per model and reloads by process **(run)**. |
| Warm cache works around the RPC upload deadlock | Unchanged: upstream RPC is the same protocol. The warm-cache logic survives as a planner. |

New pressures since June:

- **Security.** The vendored `ggml-rpc.cpp` (`RPC_PROTO` 5.1.0, pinned to the
  head of PR #27742 per `vendor/llama-cpp-sys-4/LLAMA_CPP_COMMIT` line 1) lacks
  the #24292 fix: `rpc_server::free_buffer` frees without clearing
  `stored_graphs`, and `graph_recompute` reads them. Any node serving as a
  tensor-split worker is exposed.
- **Build cost.** CI's cold `Check + Test (workspace)` median was 56.7 minutes
  with llama.cpp/ggml rebuilt every run (`.github/workflows/ci.yml:20-24`).
  The review pass attributes about 20 minutes of that to C++, about 200 of 250
  billed minutes on the desktop arm64 release leg, and about 60 minutes on the
  cold Linux qemu leg (note 7e999109). The desktop itself no longer links the
  engine; it compiles llama only because it bundles the daemon.
- **Churn.** `src/embedded` had 92 commits in 90 days; its fixes cluster in
  work llama-server owns (admission park/shed, prefix-cache keying, a stalled
  SSE pin, a too-long-prompt abort).

## 3. What llama-server b11026 does (hands-on)

All rows **(run)** on macOS arm64, Metal, Qwen3.5-2B Q6_K unless stated. Files
are in [`evidence/llama-server-b11026/`](evidence/llama-server-b11026/).

| Capability | Result | Evidence |
|---|---|---|
| `response_format` json_schema; top-level `json_schema` | Works. 16/16 completed outputs valid; 4/20 truncated in reasoning (`finish_reason=length`) because the template opens `<think>` and the schema applies after it | `20-*.json`, `20b-*.json` |
| Forced choice: `/completion` + GBNF `"A" \| "B"` + `n_probs` | Works. Probabilities are **pre-grammar** (forbidden tokens appear in `top_logprobs`); normalize over A and B client-side. With `<think>` left open, B fell out of the top 10 | `21-*.json`, `21b-*.json` |
| `/v1/chat/completions` `logprobs` + `top_logprobs` + `grammar` | Works, pre-grammar | `21b-*.json` |
| llguidance (`%llguidance`) | **Not compiled in; aborts the process** (`sampling.cpp:217 ggml_abort`, 3/3) | `10-chat-server.log`, `22-*.txt` |
| `/v1/responses`, `/tokenize`, `/detokenize`, `/props`, `/slots`, `/metrics` | Work | `23-*.txt`, `24-*.txt` |
| Slot save / restore / erase | Works (42 tokens, 20.7 MB written, restored) | `24-*.txt` |
| `--api-key` | Works; `/health` unauthenticated. Router does not forward `--api-key` to children (#28820, open) | `97b-*.txt` |
| Router: `--models-dir`, `--models-max 1`, LRU eviction, `POST /models/load` and `/unload`, `/models/sse` | Work | `41-*.txt`, `42-*.txt`, `44-*.txt` |
| Router: `--sleep-idle-seconds` | Works; child RSS fell from ~1.7 GB to ~100 MB, next request woke it | `42-*.txt` |
| Router crash isolation | Works; an llguidance abort kills only that child, router `/health` stays 200, next request reloads | `43-*.txt` |
| Router shutdown | Children gone within 2 s of SIGTERM and 5 s of SIGKILL to the router | `46-*.txt` |
| **Router: concurrent cold loads, `--models-max 1`** | **Fails**: one of two racing requests gets HTTP 500, 4/4 trials (matches #28774, open) | `48-*.txt`, `48b-*.txt` |
| Router lists models from `~/.cache/huggingface/hub` outside `--models-dir` | Observed | `41-*.txt` |
| Embeddings (`--embedding`), same GGUF as the daemon | Parity only with the daemon's input preparation (below) | `62-*.json`, `63-*.json` |
| Rerank (`--reranking`, qwen3-reranker-0.6b) | Works (`/v1/rerank` and `/rerank`); embeddings on a rerank server return an all-zero vector with 200 | `71-*.txt` |
| FIM `/infill` (sweep-next-edit-1.5b) | Works, including `input_extra` | `71-*.txt` |
| `--offline`, single model | Works (exits in ~2 ms, no download) | `90b-*.log` |
| **`--offline`, router `POST /models`** | **Fails**: network requests made, child started without `--offline`, a failed download logged as success | `91-*.log` |
| Update checks or telemetry | None found by `strings`; not proven by packet capture | `90-*.txt` |
| Release contents | `llama-server`, `ggml-rpc-server`, `llama-fit-params` and tools on macOS arm64 (11.2 MB), Ubuntu Vulkan x64 (30.3 MB), Windows Vulkan x64 (31.8 MB) | `04-archive-contents.txt` |

Embedding parity against the running daemon, same `qwen-embedding-0.6b.gguf`
(F16), three fixed strings: raw text into llama-server gives cosine
0.989 / 0.989 / 0.989 (max abs diff up to 0.0197); text with the literal
`<|endoftext|>` appended, as `EmbedQuirks::qwen3_embedding` does
(`sovereign/crates/sovereign-contracts/src/embed_quirks.rs:89`), gives
0.999999 / 1.0 / 1.0 (max abs diff ≤ 0.00012). Both sides L2-normalize. So
stored vectors carry over **only if the client keeps our quirks**. Note
500f1229 measured the same effect independently (0.9956 raw, 0.9998 with EOS).

Not run: anything on Linux or Windows, anything on Strix Halo, `--rpc` /
`ggml-rpc-server` end to end, `draft-mtp`, `--fit`, `--cache-ram`,
`--ctx-checkpoints`, `--lazy-mode`, `--models-preset`, sustained load.

## 4. Upstream issues that bear on our hardware

Open ggml-org/llama.cpp issues naming Strix Halo / Radeon 8060S / gfx1151 on
Vulkan (RADV), from `evidence/llama-server-b11026/93-issues-vulkan-strixhalo.txt`:
#29028 `qwen4exp` / deepseek-v4 abort at first decode (filed 2026-09-17);
#27306 draft-MTP `DeviceLost` during prompt processing; #27604 server-wide hang
after a client aborts a stream while another is in flight; #27505 deadlock
under sustained chat load (labelled regression); #26744 stale K/V affecting
output with flash attention. #28160 (`--lazy-mode auto` halving prompt
throughput on the AMD iGPU) closed 2026-09-08.

Router-mode issues open in the last 60 days (`92-issues-router.txt`): #28829
deadlock with `--models-max 1` on a non-resident preset; #28774 concurrent
cold-start race (reproduced above); #27456 crash when a request hits a busy
slot; #28337 wrong GGUF loaded from a shared subdirectory; #28820 `--api-key`
not forwarded.

These are not reasons to refuse; our embedded engine rides the same ggml
kernels. They are reasons the spike must run on the Strix Halo box, with our
primary model, under sustained load.

## 5. First finding: the remote seam is dishonest today

`RemoteApiProvider::build_request` (`oicp-client/src/lib.rs`, around 589-848)
decides what survives the trip. Standard fields go through (`max_tokens`,
`temperature`, `response_format`, `chat_template_kwargs`, `tools`). Private
fields are sent under names no third-party server reads (`lark_grammar` at
:793, `stable_prefix_len` at :807). And several are never sent:
`evidence_id_allowlist`, `url_allowlist`, `cmd_prefix`, `assistant_prefix`,
`sampling_mode`, `top_k`, `top_p`.

The consequences, from the review pass, with the first re-checked by the author:

- **The grounding judge fails open without a trace.** `forced_choice_ab`
  sends `{"type":"string","enum":["A","B"],"x_forced_choice":true}` as
  structured output. A remote server returns a sampled string, and
  `serde_json::from_str(resp.text.trim()).ok()?`
  (`sovereign/crates/sovereign-core/src/runtime/grounding/judge.rs:166-167`)
  returns `None` with no log line, so the gate releases the answer unverified.
  The call sites are the KnowledgeQuery gate, the deep-research audit and the
  evidence-sufficiency loop.
- **Citation and URL fabrication stop being structurally impossible** in agent
  loops and deep-research drafts, silently.
- **The node still advertises embedded features** through its OICP manifest,
  while advertising no resident models, so it is invisible to mesh routing.
- **FIM returns 503**, contradicting the doc's "editor completion … unchanged".
- **Embeddings** keep an `"unknown"` model id in single-endpoint mode, so
  memory embeddings are not persisted, and the mesh embed advertisement names
  the wrong pooling.

`docs/USE_YOUR_OWN_INFERENCE_SERVER.md:118-143` promises "A feature that needs
local weights reports itself unavailable rather than pretending it worked."
The code does not keep that promise. Fixing it is Phase 0 below and is owed
whether or not we adopt llama-server.

## 6. Migration surface and disposition

Capabilities the rest of the workspace consumes from the embedded engine
beyond plain chat, with the path on llama-server. Difficulty is S/M/L.

| Capability | Where it matters | llama-server path | Difficulty |
|---|---|---|---|
| Forced-choice A/B probabilities | Grounding judge (`judge.rs:132` funnel), deep-research audit, evidence loop; mechanism-fidelity and chat-ask instruments | `/completion` raw prompt, GBNF A\|B, `n_probs`; normalize pre-grammar probabilities over A and B **(run)**. Must keep A and B in the top-N: no open `<think>` | M, plus calibration re-validation |
| JSON-schema decoding (34 set sites: intent routing, query expansion, enrichment Phase-1) | Router, retrieval, enrichment | `response_format` → GBNF **(run)**. Unexplained yield gap on record: 6 claims via daemon vs 0 via bare llama-server on the same prompt and schema (note 64cf428b); candidate cause is optional-property ordering (note 5c06bc92) | M |
| Lark grammars (document skeletons, RAPTOR summaries, agent tool-call envelope) | Agent-coding reliability | Translate to GBNF, or build our own llama-server with `LLAMA_LLGUIDANCE=ON`. The daemon must never forward `%llguidance` to a prebuilt (process abort, **run**) | M–L |
| Evidence-id / URL allowlists, `cmd_prefix` | Fabrication safety in agent and deep-research loops | No equivalent. Compile to a prose-or-allowed-token grammar, or validate and retry | L |
| Assistant prefill | Titles, caveats, refusal retry, long-form rewrite | Trailing assistant message | S |
| Thinking control, sampler profiles | Latency; per-family repetition control | `chat_template_kwargs`; `top_k`/`top_p`/`presence_penalty` resolved client-side | S–M |
| Declared stable prefix (judge latency on hybrid models) | Per-claim gate wall time | `cache_prompt`, slot save/restore **(run)**, `--ctx-checkpoints` (not run) | M |
| MTP / speculative decoding | Decode speed | `--spec-type draft-mtp` (present, not run; #27306 on RADV) | L if it fails on Strix Halo |
| Admission, park/shed | Accepted turns never shed | Daemon keeps admission in front; serialize cold loads (explicit `POST /models/load`, then request) to avoid #28774 | M |
| Embeddings + `EmbedModelInfo` | Corpora, memory, atlas, federated search | `--embedding` with our quirks client-side **(run: parity 1.000)**; fix the id and pooling advertisement | S–M |
| Rerank | Retrieval quality | `--reranking`, `/v1/rerank` **(run)**; map score scale | S–M |
| FIM / next-edit | Editor completion | `/infill` **(run)** | S–M |
| Token counts, context size | Compaction, bundle truncation | `/tokenize`, `/props` **(run)** | S |
| Slot lifecycle, residency, idle unload, mesh advertisement | `/status`, extras hot-swap, mesh routing | Router `/models`, `/slots`, `/props`, `--sleep-idle-seconds` **(run)** | M–L |
| VRAM fit gate | Refuse loads that won't fit | `--fit` / `llama-fit-params` (present, not run); keep our capacity planner | M |
| Multi-host tensor split | Models bigger than one node | `--rpc`, `-ts`, `-ot` driven by our planner; `ggml-rpc-server` sidecar replaces the in-process worker; warm cache unchanged (not run) | L |

## 7. What stays ours, what goes

Stays (the thin layer): the models manifest and profiles, `setup_planner`,
`capacity`, GGUF metadata parsing, `EmbedQuirks`; the constraint policy (as
grammar compilers or validators rather than logit masks); the forced-choice
client; admission and shed in front of the sidecar; the RPC shard planner and
warm cache (pure-Rust parts of `rpc_distribution.rs` and `rpc_warm_cache.rs`);
sidecar supervision, reusing `sovereign-compute`'s supervisor (health
handshake, heartbeat, backoff, crash-loop limit; it lacks a Windows Job Object
today).

Goes: the rest of `src/embedded` (24,885 raw lines today, before subtracting
what is kept), `vendor/llama-cpp-4` (19,445 raw Rust), `vendor/llama-cpp-sys-4`
(a 2,640-line `build.rs`, 777 lines of C++ shims, 6 patch files, the vendored
tree), the C++ compile in CI and in every release leg, and the per-release
llama.cpp re-sync. Net line savings are not measured; the kept layer and the
new grammar compilers are real code.

## 8. Plan with pre-registered bars

### Phase 0: make the remote seam honest (do regardless)

- The engine reports which request features it honours; the request builder
  refuses or downgrades visibly (a `tracing` event and a counter) instead of
  dropping a field. Forced choice, allowlists and lark are capability-gated.
- `forced_choice_ab` traces and counts every parse failure; a fail-open is a
  named verdict, not a `None`.
- The OICP manifest advertises only honoured features and the models the
  remote actually holds.
- `docs/USE_YOUR_OWN_INFERENCE_SERVER.md` describes what the code does.

Done when: `oicp-conformance` passes against a daemon on `kind="remote"`, and a
chat-ask run on `kind="remote"` reports zero silent judge fail-opens (every
failure is counted and named).

### Phase 1: parity spike (no default change)

Daemon on `kind="remote"` → `llama-server` router on the same GGUFs, on the
Strix Halo box (Vulkan) and on macOS (Metal). Bars, fixed before any data:

| Bar | Pass condition | Kind |
|---|---|---|
| Judge fidelity | Verdict agreement ≥ 98% with the embedded engine at the calibrated threshold on the chat-ask and mechanism-fidelity banks; mean \|Δp(A)\| ≤ 0.02; control gap ≥ 0.99; silent fail-opens = 0 | new, gating |
| Structured-output yield | Phase-1 claims on the wessex-hoard fixture ≥ the embedded count (6); enrichment-f1 lane within its noise band (RUNBOOK §6) | gating |
| Fabrication safety | Fabricated citation and URL rate = 0 on the agent and deep-research banks with allowlists re-expressed | gating |
| Tool-call reliability | agent-coding `parse_failed_envelope` rate ≤ embedded baseline | gating |
| Embedding space | Cosine ≥ 0.999 against stored vectors, document and query side; mesh `EmbedModelInfo` identical to embedded peers | gating |
| Stability | 24-hour soak on Strix Halo with concurrent and aborted streams: zero server hangs, zero aborts outside router children | gating |
| Lifecycle | No orphaned sidecar after daemon SIGKILL on macOS, Linux and Windows; crash-to-serving time recorded | gating |
| Decode and TTFT | Interleaved embedded/sidecar arms at matched host load, load recorded per reading | TRACKED only: this host's decode moves 2.8x across its load range, so wall-clock bars do not gate (`sovereign/bench/quality-check/throughput.toml` header, since 2026-09-08) |
| Build and release | Cold CI and each release leg's wall time recorded before and after | TRACKED |

Kill conditions: judge fidelity cannot meet its bar through probabilities alone;
a Strix Halo Vulkan issue from §4 reproduces on our primary model and has no
upstream fix within the spike; allowlist safety cannot be re-expressed without
a sampler hook.

### Phase 2: ship dark

Sidecar engine behind config, default off, with a `sovereign/DEFAULTS_LEDGER.md`
row naming the flip condition (Phase 1 bars green on both hosts) and a
review-by date. Nightly lanes run both engines.

### Phase 3: flip, then delete

Flip the default with the embedded engine kept one release as fallback; then
delete `src/embedded` and the vendored crates, and drop the C++ compile from CI
and release legs.

## 9. If the swap is declined

Phase 0 is still owed. The vendored tree still needs a re-sync: drop the PR-head
pin (#27742 merged upstream), take the #24292 fix, and bump the binding. Until
then, apply the five-line `free_buffer` fix as a vendored patch.

## 10. Not verified

- Anything on Linux, Windows or Strix Halo; any model over ~2 GB.
- `draft-mtp`, `--fit`, `--cache-ram`, `--ctx-checkpoints`, `--lazy-mode`,
  `--models-preset`, `--rpc` / `ggml-rpc-server` end to end.
- The cause of the 64cf428b structured-output yield gap.
- Whether pre-grammar top-N probabilities reproduce the embedded
  `forced_choice_probs` distribution; only feasibility was shown.
- Where the router's `--offline`-ignoring requests went (no packet capture).
- Whether prebuilt binaries need hardened-runtime entitlements once the desktop
  is notarized (it is ad-hoc signed today).
- Line savings net of the kept layer.
