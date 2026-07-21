# Inline Code Completion (FIM) — Design Spec

Status: DRAFT / proposed. Author: (design session 2026-07-20).
Scope: a real-time, in-IDE inline code-completion ("ghost text") experience —
complete a function signature, a loop body, the next line — served by the
resident Sovereign daemon over a local HTTP surface, consumed by thin VSCode
and JetBrains plugins.

Non-goal (explicitly out of scope): the agentic IDE seat (chat-with-repo,
file-editing agent). That seat is gated on the lights-out tool-calling bench;
this seat is not, and must not be coupled to it. See "Relationship to the
bench" below.

---

## 1. Thesis: this is mostly configuration + prompt-shaping over existing machinery

The single most important finding of the seam survey is how little *new engine
code* this needs. Fill-in-the-middle (FIM) for the models we already ship
(Qwen-Coder family) is expressed as a **plain-text prompt** using the model's
own special-token markers (`<|fim_prefix|>`, `<|fim_suffix|>`,
`<|fim_middle|>`, `<|endoftext|>`), tokenized with special-token parsing on.
It does **not** require the `llama_token_fim_*` FFI or the infill sampler —
those are an alternative, model-agnostic path we can adopt later, not a
prerequisite.

So the FIM request decomposes almost entirely onto surfaces that already exist:

| FIM need | Existing surface to reuse | Location |
|---|---|---|
| Request type (prompt, max_tokens, temp, sampling profile, slot routing) | `CompletionRequest` | `sovereign-contracts/src/types/completion.rs:15` |
| Code sampling profile (T=0.6, top_p=0.95) | `SamplingMode::Code` | `completion.rs:216` |
| Route a request to a named model | `CompletionRequest.model_id` + slot router | `completion.rs:60`, engine `select_slot_for_request` `engine.rs:1696` |
| Token streaming with a typed terminal reason | `complete_stream_with_finish` → `StreamFrame::{Token, Finish{reason}}` | `contracts/src/traits.rs:367`, `completion.rs:502` |
| Cancellation on client disconnect | `StreamFrame::Cancelled` (receiver-drop) already modeled | `traits.rs` / `completion.rs:507` |
| Always-resident extra model (no hot-swap) | `SlotTarget::Extra(String)` eagerly-loaded, `extras_by_model_id` | `engine.rs:471` |
| OpenAI-shaped HTTP surface + request adapter | `worker_inference_proxy` + `inference_adapter::build_completion_request` | `sovereign-mesh/src/worker_inference_proxy.rs:160`, `inference_adapter.rs:309` |
| Tokenizer / sampler / decode wrapper | first-party llama wrapper over `llama-cpp-4` 0.4.2 | `sovereign-inference/src/llama.rs`, `crate::llama::cpp::{llama_batch, sampling}` |
| Model-slot config plumbing | `ResolvedModelSlots` (already has a `code` field) | `sovereign-desktop/.../state/config.rs:655` |

The genuinely-new surface is small and named precisely in §3.

---

## 2. The two honest gaps (design decisions, not unknowns)

### 2.1 There is no raw-prompt path — the engine always applies a chat template

Every generation today routes through `apply_chat_template` /
`apply_chat_template_minijinja` (`prompt_helpers.rs:417`, `:456`), which wraps
`CompletionRequest.prompt` in a user turn. FIM must feed the raw
`<|fim_prefix|>…<|fim_suffix|>…<|fim_middle|>` string to the tokenizer with
**special-token parsing on and no chat-template wrapping**. This is the
primary new engine entry point (§3.1).

Deliberately *not* solved by abusing `assistant_prefix`/`cmd_prefix`
(`completion.rs:123`,`:138`): those append after the template's generation
marker; they don't suppress the user-turn wrapper. A clean `raw: bool`
(or a `PromptShape::Raw` enum on the request) is the honest lever.

### 2.2 The existing code slot hot-swaps with primary — wrong for keystroke latency

`engine.rs:1573`: "ONE lazy hot-swap slot shared between the Main responder and
the Code specialist… at most one of the two roles is resident at any instant."
If the user is chatting (primary resident) and then types (FIM wants the code
model), the engine unloads primary and loads the code model — seconds. That is
disqualifying for a sub-300ms ghost-text budget.

**Decision:** serve FIM from a **dedicated, always-resident** slot, *not* the
hot-swapping `code_path` slot. The lowest-new-code way to get an
always-resident model addressed by name is the existing
`SlotTarget::Extra(String)` mechanism (`engine.rs:471`, "operator-declared
additional eagerly-loaded chat slot", routed via `extras_by_model_id` from
`request.model_id`). The FIM model is declared as an eager extra; the FIM route
sets `model_id` to it. No new slot enum arm required.

Open sub-decision to settle during Phase 0: whether a 1–3B FIM model as an
always-resident extra is an acceptable steady-state RAM cost on the low-mem
profile, or whether it should be gated behind an opt-in config flag (default
off on `low_mem`/`cpu_only`).

---

## 3. New surface (the whole build)

### 3.1 Engine: a raw-completion entry (`sovereign-inference`)

Add a raw-prompt generation path that:
- tokenizes the prompt string directly with special-token parsing on,
- skips the chat-template render,
- accepts a stop-token / stop-string set,
- returns the same `StreamFrame` stream every other caller consumes.

Reuse the existing decode/sampler/batch loop (`crate::llama::cpp::{llama_batch::LlamaBatch, sampling::LlamaSampler}`, `prompt_helpers.rs`) and `SamplingMode::Code` for sampler construction. This is the only change inside the hot decode path; keep it a sibling entry, not a fork of the templated path.

Expression on the request: extend `CompletionRequest` with a minimal, additive
signal (candidate: `prompt_shape: Option<PromptShape>` where `Raw` means
"tokenize verbatim, no template"). Additive + `Option` keeps every existing
caller unchanged.

### 3.2 FIM prompt builder (new, small, first-party)

`build_fim_prompt(prefix, suffix, family) -> String`. Owns the model-family
markers (Qwen-Coder: `<|fim_prefix|>{prefix}<|fim_suffix|>{suffix}<|fim_middle|>`).
Keyed off `ModelFamily` so a second FIM family (StarCoder2, DeepSeek-Coder)
is a table addition, not a rewrite. Lives beside the family-quirks tables the
inference crate already keeps.

### 3.3 Stop-condition logic (new — this is where the UX craft lives)

Single-line vs multi-line completion is decided here, not by the model:
- signature / single-line context → stop at newline or when the FIM/EOT token
  is emitted;
- body context → allow multi-line, stop on brace/paren depth returning to the
  opener, on a blank line, or on `max_tokens`;
- never emit text that duplicates the caller-provided `suffix`.
Terminal reason surfaces through the existing `StreamFrame::Finish { reason }`
(`stop`/`length`/`cancelled`). Budget real tuning time here per language.

### 3.4 HTTP route: `POST /v1/completions` (FIM)

Add alongside the OpenAI surface that already exists in
`sovereign-mesh/src/worker_inference_proxy.rs` (which already does
`chat_completions_proxy` `:160`, `models_proxy` `:183`, `embeddings_proxy`
`:188`) and its adapter `inference_adapter::build_completion_request`
(`:309`, already sets `sampling_mode` `:333` and `model_id` `:327`). The new
handler:
- parses `{prefix, suffix, path, language, max_tokens}` (accept the OpenAI
  legacy `/v1/completions` `prompt`+`suffix` shape so off-the-shelf IDE
  clients like Continue.dev work unmodified),
- calls `build_fim_prompt`, builds a raw `CompletionRequest`
  (`prompt_shape=Raw`, `sampling_mode=Code`, `model_id=<fim extra>`,
  small `max_tokens`),
- streams via `complete_stream_with_finish` out as **SSE** — reuse the
  proxy's existing byte-for-byte SSE forwarding (`worker_inference_proxy.rs:27`
  documents the non-buffering SSE compose), not the WS conversation path.
- carries whatever auth/middleware the `/v1` surface already applies
  (`sovereign-server/src/auth.rs`).

Decision to confirm in Phase 0: does this route live in `sovereign-mesh`'s
worker surface (co-located with the other `/v1` OpenAI routes) or in
`sovereign-server`? Co-locating with `worker_inference_proxy` is the
lower-friction reuse; confirm the daemon actually mounts that router in the
single-node (non-mesh) case.

### 3.5 Config: declare the FIM model

Extend the slot config (`ResolvedModelSlots` already has `code`;
`config_setup.rs` `get/set_setup_model_slots` `:549/:589`) with an eager FIM
extra, or reuse the `extras` map directly. On-disk in `config.toml` per the
"config.toml is the sole home for model paths" invariant. Residency then shows
at `/status.inference.resident` for free (the resident-slot enumerator at
`engine.rs:1590` already walks configured slots).

### 3.6 IDE plugins (thin, per-IDE)

- **VSCode**: `InlineCompletionItemProvider`. Debounce ~120ms; pass the
  editor `CancellationToken` to an `AbortController` on the fetch so a
  superseded keystroke drops the SSE connection (engine sees receiver-drop →
  `StreamFrame::Cancelled`). Render ghost text.
- **JetBrains**: `InlineCompletionProvider` (2023.3+). Identical daemon
  contract; same debounce/cancel discipline.

The plugins own editor events and rendering only. All model/slot/context logic
stays daemon-side so both plugins ride the same improvements.

---

## 4. Latency budget (the acceptance criterion for Phase 0)

Sub-300ms p95 end-to-end or the ghost text feels laggy and gets turned off.
This dictates: 1–3B FIM model, `max_tokens ~64`, greedy/low-temp `Code`
profile, always-resident (§2.2, no hot-swap), and **KV prefix-cache
stability** — assemble the FIM prompt so the bulk is a stable prefix and each
keystroke only decodes the delta. No retrieval, no grounding gate, no
epistemic footer on this path — it shares the *engine*, never the chat
*pipeline*.

---

## 5. Deliberately deferred to a measured v2 (do NOT build in v1)

Repo-aware context injection — enriching the FIM prefix with SCIP-graph
signatures (`symbols()`/`callees()` of types at the cursor). This is the real
differentiator vs a generic local Ollama, and the function-signature use case
is exactly where it should help — but "should" is a hypothesis. It costs
latency and can thrash the prefix cache. v1 ships file-window context only
(N lines before, M after). v2 adds the injector and **A/B-measures** whether it
lifts accept-rate enough to justify the latency, on the same bench discipline
applied everywhere else in this repo. Pull graph context from the resident
SCIP index (microseconds), never LanceDB semantic search on the keystroke path.

---

## 6. Glassbox (house principle)

Completion is a black box in every competing product. On an opt-in debug
channel, return what fed the suggestion: model id, slot, injected context (in
v2), decode time, `Finish` reason. An "explain this suggestion" hover.
Consistent with the traceable/observable mandate and something Copilot
structurally cannot do.

---

## 7. Quality eval (separate axis from the tool-calling bench)

FIM quality is its own eval, unrelated to lights-out tool-calling: hold out
real function bodies from our repos, mask them, measure exact-match +
does-it-compile + accept-rate. This axis moves independently, so the
completion seat is not gated on bench progress.

---

## 8. Relationship to the bench (why this seat is decoupled)

Inline FIM never calls a tool — it is pure infill. None of the tool-calling
hardening or epistemic/abstention work gates it. That work gates the *agentic*
IDE seat (§ non-goal), where the model drives our MCP tools (`symbols`,
`callers`, `blast`) unattended. Keep the two seats on independent readiness
gates: FIM ships on a code model + latency budget; the agent seat ships when
the lights-out bench clears.

---

## 9. Phased plan

- **Phase 0 — vertical slice, no IDE code.** Raw-completion engine entry
  (§3.1) + FIM prompt builder (§3.2) + FIM model as always-resident extra
  (§2.2/§3.5) + a `curl`-able `/v1/completions` (§3.4). Exit criterion:
  measured p50/p95 latency sub-300ms and qualitatively-useful completions on a
  real Qwen-Coder model. This de-risks the two unknowns (raw path + residency)
  before any plugin work.
- **Phase 1 — stop-condition craft (§3.3)** per language, driven by the §7
  eval.
- **Phase 2 — VSCode plugin (§3.6)** against the proven route.
- **Phase 3 — JetBrains plugin**, reusing the identical daemon contract.
- **Phase 4 (measured) — SCIP context injection A/B (§5).**

## 10. Reuse ledger (what we are NOT writing)

Not writing: a request type, a sampling profile, a streaming/`StreamFrame`
protocol, cancellation semantics, an SSE forwarder, an OpenAI request adapter,
a slot router, a residency tracker, a tokenizer/sampler/decode loop, or a
config surface. All exist and are cited above. New code is confined to: a raw
tokenization entry, a FIM string builder, stop logic, one HTTP handler, one
config field, and two thin plugins.
