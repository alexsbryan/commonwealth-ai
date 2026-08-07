# Inline Code Completion (FIM) — Design Spec

Status: **v1 IMPLEMENTED** (2026-07-21). Ghost-text inline completion
served by the resident Sovereign daemon over `POST /v1/completions`,
consumed by the first-party VSCode extension
(`packages/vscode-sovereign/`). JetBrains port deliberately deferred
(the daemon contract is IDE-agnostic; ~2-3 days of Kotlin/Gradle when
wanted). SCIP context injection deferred to a measured v2 (§5).

Non-goal (explicitly out of scope): the agentic IDE seat (chat-with-repo,
file-editing agent). That seat is gated on the lights-out tool-calling
bench; this seat is not, and must not be coupled to it. See §8.

---

## 1. Thesis (validated): mostly configuration + prompt-shaping over existing machinery

Fill-in-the-middle for the coder models we ship is a **plain-text
prompt** using the model's own special-token markers, tokenized with
special-token parsing on and no chat-template wrapping. The build
decomposed onto existing surfaces exactly as surveyed:

| FIM need | Surface | Location |
|---|---|---|
| Request type + raw lever | `CompletionRequest.prompt_shape: Option<PromptShape>` | `sovereign-contracts/src/types/completion.rs` |
| Code sampling profile | `SamplingMode::Code` | same |
| Named-slot routing | `model_id` → `select_slot_for_request` named-slot match | `embedded/engine.rs` |
| Typed streaming + cancel | `complete_stream_with_finish` → `StreamFrame` | `contracts/src/traits.rs` |
| Always-resident named slot | extras machinery, **pinned** under reserved name `"edit"` | `embedded/engine.rs` |
| HTTP surface | `client_router` on :9741 (solo AND mesh) | `commonwealth-api/src/server.rs` |
| Tokenizer/sampler/decode | first-party llama wrapper | `sovereign-inference/src/llama.rs` |

The genuinely-new surface: `sovereign-inference/src/fim.rs` (marker
table + prompt builder + vocab probe + stop tracker),
`sovereign-mesh/src/fim_adapter.rs` (seam impl),
`commonwealth-api/src/routes_completions.rs` (HTTP handler), and the
VSCode extension.

## 2. The two gaps (as solved)

### 2.1 Raw-prompt path — `PromptShape::Raw`

Every generation funnels through `format_prompt`
(`embedded/prompt_helpers.rs`), which now early-returns the prompt
verbatim for `PromptShape::Raw`. The serving tokenize sites use
`AddBos::Never` for Raw (`add_bos_for`) — Mellum/Qwen-Coder declare
add_bos=false, and a prepended BOS would be an untrained token at
position 0.

### 2.2 Residency — dedicated pinned slot OR fast-slot alias (D8)

The hot-swapping code slot was disqualifying for keystroke latency.
Two supported arrangements:

- **Alias mode (lean)**: when `[models.edit].path` equals the fast
  slot's resolved path (`ModelsSection::fast_path()`), no model is
  loaded — FIM probes the resident fast slot's tokenizer and routes
  via the named-slot match. One model in RAM total. Chat traffic
  shares the slot (documented caveat).
- **Dedicated mode**: any other path loads a **pinned** extras slot
  under the reserved name `"edit"` — skipped by both the idle monitor
  and LRU eviction (`extras_slot_is_evictable`), though its bytes
  still count toward `max_extras_memory_gb`. Explicit
  `unload_extra("edit")` stays allowed (operator action).

The pre-rename reserved name `"fim"` (`LEGACY_EDIT_SLOT_NAME`) is
**still pinned**. An upgrade must not silently make a running editing
slot evictable just because the reserved name moved: un-pinning is
invisible until the slot is dropped mid-keystroke, which is the exact
failure the pin exists to prevent.

### Marker detection is a vocab probe, NOT family-keyed

`ModelFamily` is `Unknown` on all production slots, so family tables
can't decide the marker convention. `detect_fim_style` instead
requires every marker to tokenize to EXACTLY ONE token at slot
install. The table (`sovereign-inference/src/fim.rs`) currently
carries Qwen-Coder, Mellum (JetBrains — `<fim_prefix>` spelling,
disambiguated from StarCoder2 by the `<|im_start|>` vocab token), and
StarCoder2. **Validated artifact:
Mellum2-12B-A2.5B-Instruct-Q6_K** (recommended; Thinking variant also
validated but slower per token).

**A failed probe withholds the FIM lane, not the slot.** The probe
used to refuse the whole slot, which meant a user whose only model was
an ordinary chat model got no editing assistance at all — including
next-edit, which needs no markers whatsoever. Now the install keeps
the slot and leaves `EditSlotInfo::fim` as `None`: `/v1/completions`
503s with the actionable fix (point `[models.edit].path` at a coder
GGUF), and `/v1/edit_predictions` is unaffected. See `NEXT_EDIT.md`
for the other lane; the two-lane split is spec'd on `EditSlotInfo`
(`sovereign-contracts/src/types/edit_slot.rs`).

## 3. As-built surface

### 3.1 Engine raw entry

`PromptShape::{Templated, Raw}` on `CompletionRequest` (additive,
serde-defaulted — every legacy caller unchanged). Raw → verbatim
tokenization, special-token parsing on, no template, no BOS.

### 3.2 FIM prompt builder

`fim::build_fim_prompt(style, prefix, suffix)` — PSM ordering
`{prefix-marker}{prefix}{suffix-marker}{suffix}{middle-marker}`:
Qwen's documented shape AND prefix-cache friendly (the prefix section
only appends as the user types — see §4).

### 3.3 Stop conditions — `FimStopTracker` (pure, unit-tested)

Mode is decided HERE, not by the model: `decide_mode(prefix_tail)` →
Multi only on a trailing block opener (`{`/`(`/`[`/`:`/`=>`), Single
otherwise. The tracker applies, earliest-position-wins:

1. stop strings (family markers ∪ client `stop`) with a holdback
   buffer — a stop string split across token boundaries never leaks;
2. suffix-duplication trim (first ≤40 chars of the caller's suffix,
   ≥3-char probe);
3. Single: first newline;
4. Multi: net-negative bracket depth (closing the construct
   containing the cursor — depth-0 deliberately does NOT stop:
   nested opens would fire prematurely), blank line, or 8 lines.

The adapter (`sovereign-mesh/src/fim_adapter.rs`) runs the tracker as
a stream combinator, synthesizes `Finish{Stop}`, and drops the inner
stream (receiver-drop cancels the decode). Zero changes to the shared
decode loop.

### 3.4 HTTP route: `POST /v1/completions`

Lives in **`commonwealth-api`'s `client_router`** (:9741, loopback
tokenless) — NOT `worker_inference_proxy` (that's the mesh-pod-only
:9742 tunnel; the original spec draft had this wrong). Dual request
shape: OpenAI-legacy `{model, prompt, suffix, max_tokens, stop,
stream}` for generic clients, and rich `{prefix, suffix, path,
language, debug}` (`prefix` wins). Response is the OpenAI
`text_completion` object; streaming is SSE chunks + terminal
`finish_reason` chunk + `[DONE]`.

The route crosses the existing `LocalInferenceService` seam via the
defaulted `fim_completion_stream` / `edit_status` methods — only the
embedded llama.cpp adapter overrides them. Provider wrappers forward
`InferenceProvider::edit_slot_info` (mesh wrapper, compute facade) so
the arrangement survives the daemon's decoration stack. The route
serves iff `EditSlotInfo::fim` is `Some` — that lane is the single
decider for "can this daemon do FIM", and nothing re-derives the
answer from a model name or a marker enum.

### 3.5 Config

```toml
[models.edit]
path = "~/.sovereign/models/Mellum2-12B-A2.5B-Instruct-Q6_K.gguf"  # required; presence = opt-in
context_size = 4096        # optional; defaults shown
max_tokens = 48
temperature = 0.2
max_prefix_chars = 8000    # server keeps TAIL of prefix
max_suffix_chars = 2000    # server keeps HEAD of suffix
```

`[models.fim]` is accepted as a **deprecated alias** (serde), so
configs written before the rename keep working unchanged. The key was
renamed because next-edit — not FIM — is the lane most users reach
for, and a section named `fim` made the common case look like the
exotic one. Write `[models.edit]` in new configs.

Residency shows at `GET /status` → `inference.edit` `{slot, model_id,
aliased_to_fast, degraded, next_edit_format?, fim_style?, advice?}` —
the extension's status bar reads exactly this (works in both modes).
`fim_style` is **omitted when the FIM lane is absent**, so a status
payload without it means "this daemon does next-edit only", not "the
field is missing". `inference.fim` remains as a byte-identical
deprecated mirror for one release, because shipped extensions read it
by JSON path.

### 3.6 IDE plugin (VSCode; JetBrains deferred)

`packages/vscode-sovereign/` — zero-runtime-dep esbuild bundle:
`InlineCompletionItemProvider` with 120ms debounce, single-flight
abort (CancellationToken → AbortController → SSE socket close →
daemon receiver-drop cancels mid-token), line-stable context capture
(60/20 lines), status bar over `/status.inference.fim` (the deprecated
mirror — moving it to `inference.edit` is the extension's own
follow-up), and the
glassbox commands **Explain Last Suggestion** / **Diagnose Completion
Setup** (sequenced PASS/FAIL probes with the copy-pasteable fix).
Install: `code --install-extension sovereign-fim-0.1.0.vsix`; the
README in that directory is the hand-over doc.

## 4. Latency (measured 2026-07-21, Mellum2-12B-A2.5B-Instruct Q6_K, Strix Halo Vulkan)

Mechanisms: pinned slot (no load stall), PSM append-only prefix +
**LCP partial-keep on the FIM route** (`generate_stream_sync_fim` —
steady-state re-prefill is the suffix window + typing delta, not the
full window), `max_tokens` 48, near-greedy T=0.2 + `SamplingMode::Code`,
ctx 4096, no retrieval/grounding/template/think.

`scripts/fim-smoke.sh` (both modes, 5 samples each):

| mode | TTFT p50 | TTFT p95 (cold-skewed) | total p50 |
|---|---|---|---|
| alias | 100ms | 423ms | 251ms |
| dedicated | 99ms | 448ms | 251ms |

The p95 is the first (cold-cache) request; steady-state TTFT is
~100ms, comfortably inside the sub-300ms budget. The two latency
regimes matter: the eval bank (`scripts/fim_eval.py`, 60 masked real
functions with ~6k-char prefixes, every case a DIFFERENT prefix so
the LCP never hits) measures the full-prefill floor at ttft p50 ≈
1.25s / total p50 ≈ 1.35s — real keystroke traffic sits in the
high-overlap regime (the smoke's ~100ms), and the gap between the
two numbers IS the F2 LCP win.

Quality eval (2026-07-21, same model, 60 cases): 27% exact / 33%
normalized / 45% first-line overall; typescript-single 75%, rust-
single 36%; multi-line block cases 0% exact (expected — valid
reimplementations don't byte-match; first-line is the accept proxy).
Stop-rule histogram: newline 26, max_lines 22, depth_close 7, none 4,
blank_line 1 — the max_lines share is the F1-tuning signal for this
model family. Bank: `gym/fim/cases.jsonl`, regenerated
deterministically by `gym/fim/harvest.py`.

## 5. Deliberately deferred to a measured v2 (do NOT build without an A/B)

- **Repo-aware context injection** (SCIP signatures at the cursor) —
  the real differentiator vs a generic local server, but "should
  help" is a hypothesis; measure accept-rate lift against the latency
  cost on the same bench discipline.
- **JetBrains plugin** — the daemon contract is final; the port is
  mechanical.
- **Marketplace publish** — the .vsix attaches to a GitHub release
  for now.
- **Accept-rate telemetry + compile-check scoring** in the eval.

## 6. Glassbox (shipped)

Every completion response (opt-in `debug: true`, always on from the
first-party extension) carries `sovereign_debug`: `{model_id, slot,
fim_style, mode, stop_rule, trimmed_chars, prompt_chars,
emitted_chars, timings_ms{ttft,total}, finish_reason}`. The daemon
also logs per-request under the `fim` tracing target (slot, prefill
scope incl. LCP hit tokens, stop outcome) — `fim=info` is in the
daemon's default tracing allowlist, pinned by tests.

## 7. Quality eval

§4 — `gym/fim/cases.jsonl` + `scripts/fim_eval.py`. This axis moves
independently of the tool-calling bench (§8).

## 8. Relationship to the bench (why this seat is decoupled)

Inline FIM never calls a tool — it is pure infill. None of the
tool-calling hardening or epistemic/abstention work gates it. That
work gates the *agentic* IDE seat (§ non-goal). FIM shipped on a code
model + latency budget; the agent seat ships when the lights-out
bench clears.

## 9. Verification surface (what proves v1)

1. Weight-free units in `cargo test --workspace`: `fim.rs` table +
   ~20-case tracker table, serde round-trip (legacy JSON without
   `prompt_shape` deserializes), `extras_slot_is_evictable` pin test,
   adapter mapping tests (stub provider, split stop string),
   commonwealth-api route tests (503 / dual-shape / SSE / `[DONE]` /
   debug), tracing-allowlist pin tests.
2. `scripts/fim-smoke.sh` — both serving modes on a real GGUF
   (isolated `$HOME`, scratch ports); asserts slot routing, marker
   non-leak, stream shape, prints TTFT/total percentiles.
3. `#[ignore]` integration test keyed on `SOVEREIGN_FIM_TEST_GGUF`
   (`sovereign-inference/tests/fim_raw_path.rs`) — Raw through
   `EmbeddedLlamaCpp` directly, two sequential requests proving the
   LCP path doesn't desync.
4. Extension: `npm test` in `packages/vscode-sovereign` (vitest +
   mock daemon, incl. abort-actually-closes-socket).
