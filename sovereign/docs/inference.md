# Inference — slots, OICP scoring, harness adapters, cutoff legibility

How Sovereign chooses **what model runs where**, advertises its
capabilities to peers, accepts requests from coding agents like
codex/opencode, and tells the user when a response was cut off.

The runtime sees one trait — `InferenceProvider`
(`sovereign-core/src/traits.rs`). Everything below is what hides
behind that trait when the request actually executes.

---

## 1. Slots — local model loading

`sovereign-inference/src/embedded.rs` wraps `llama-cpp` with a
lazy-loaded slot system. A *slot* is a model-loading position.

| Slot     | Purpose                                  | Typical model           |
|----------|------------------------------------------|-------------------------|
| Quick    | Routing, working-memory compression      | Qwen3 0.6B–1.7B         |
| Main     | Planning, synthesis, evaluation          | Qwen3.5 4B / 9B / 27B   |
| Code     | Hint-routed code work; shares Main mutex | DeepSeek-Coder etc.     |
| Embed    | Vector embeddings                        | Qwen3-Embedding 0.6B/4B |

Code and Main share a mutex (hot-swap on hint switch); only one is
resident at a time. Embed stays on its own `Arc<EmbedSlot>` — it's
a cross-peer interoperability contract (`EmbedModelInfo` must match
between nodes sharing a corpus) and never folds into chat.

`models.toml` declares five hardware profiles (`cpu_only`,
`low_mem`, `default`, `high`, `very_high`). Each profile names a
gguf per slot with `repo`, `file`, `family`, `quant`, `size_gb`,
`thinking`. Per-slot `quirks_override` tunes family defaults from
`model_family.rs`.

### Polished slot management

`models.toml` declares multiple slot configs and `model_id`
routing; the daemon exposes runtime endpoints:

| Endpoint                          | Purpose                                       |
|-----------------------------------|-----------------------------------------------|
| `POST /internal/models/load`      | Load an extra model (bounded by `max_extras_memory_gb`) |
| `POST /internal/models/unload`    | Unload a non-primary slot                     |
| `GET  /internal/models/inventory` | Which slots are resident, idle, evictable     |

Eviction is LRU over non-primary slots. Primary chat + embed are
pinned. `build_self_manifest` uses `resolve_primary_model_name(provider)`
— a bigger Code slot must not shadow peer attribution.

### Optional sibling pool

`SOVEREIGN_PRIMARY_SIBLINGS=N` (N≥2) eager-loads N independent Main
contexts sharing one `Arc<LlamaModel>`. Weights load once; each
sibling pays only its own KV cache. Non-streaming chat round-robins
across siblings so N callers can be in `generate_sync` concurrently.
Streaming, embed, and warmup still use the single lazy slot.
Incompatible with a configured Code specialist; construction
refuses when both are configured. Intended for measuring throughput
on a single fat node (e.g., Strix Halo's 124 GiB GTT).

### Decode paths

Default: `generate_sync` with two-tier jump-forward decoding (Tier 1
single-survivor token from mask cache; Tier 2 FSM byte-walk +
`VocabTrie` longest-match). When the loaded gguf carries MTP heads,
the dispatcher routes schema/no-tools requests through
`generate_sync_mtp` (speculative decoding via the MTP head). Wiring
jump-forward into the MTP loop is tracked.

Telemetry: per-request `inference: end-of-generation` carries
`jump_fwd_n` / `jump_fwd_runs` / `jump_fwd_bytes_n` so throughput
decomposes by path.

`SlotContext` is a sum type — `SingleToken { ctx }` or
`Speculative { target_ctx, draft_ctx, session, n_draft_max }`. Illegal
hybrid state is unrepresentable. `try_upgrade_to_speculative` runs
`probe_mtp_roundtrip` at construction so MTP-named ggufs that lack
real head tensors degrade with one warn line. Escape hatch:
`SOVEREIGN_MTP_DISABLE=1`.

### Known invariants

- **Gemma 4 + Metal is unsupported** on llama-cpp-2 0.1.145 —
  ggml-metal lacks the matmul kernels and decode SIGSEGVs. CPU
  fallback via `SOVEREIGN_FORCE_CPU_CHAT=1`.
- **Daemon doesn't auto-resolve `fast`/`primary` slot aliases** on
  `/v1/chat/completions` — pass the actual model name from config.
- **`AppState::with_*` installers must run before `inner.clone()`**
  in `EmbeddedDaemon::start_daemon` — `Arc::get_mut` fails silently
  if `inner` is already shared, and chat returns 503 on every request.
- **FastShort skip for recurrent-arch ggufs** — `from_existing_model`
  doesn't propagate `n_rs_seq`; the daemon skips FastShort
  construction on qwen*moe/mamba/etc. Escape hatch:
  `SOVEREIGN_FAST_SHORT_DISABLE=1`.

---

## 2. OICP v0.3 — capability advertisement + scheduler scoring

Spec: `commonwealth/docs/oicp-v0.3.md`. Types:
`oicp-types/src/lib.rs`, re-exported as `sovereign_core::oicp`
and `commonwealth_core::oicp` — never import the types crate
directly downstream.

### Wire types

- `CapabilityHint` — validated tag. Standardized: `general`, `code`.
  Open-vocabulary specializations use `x:<tag>`.
- `LatencyClass` — `Fast`, `Normal` (default), `Extended`.
- `CapabilityClaim { hint, latency_class, max_context, max_output, affinity }`
  — one claim per kind-of-work a node serves well.
- `InferenceRequirements { oicp_version, capability_hint, latency_class, context_tokens, max_output_tokens, privacy, request_id }`.
- `ProviderManifest` — what a backend advertises (with `knowledge`
  + `federation` sections).
- `ShardingPrivacy` — `LocalOnly` (default), `MeshAllowed`.

### Three schedulers, one scoring pipeline

`commonwealth-inference::scheduler::oicp_select`,
`sovereign-inference::selector::CapabilityAwareSelector`, and
`sovereign-mesh::oicp_select` all call
`score_claim_for_request(&CapabilityClaim, &InferenceRequirements)`
from `oicp-types`. Operational adjustments fold in per-scheduler:

| Adjustment              | How                                                          |
|-------------------------|--------------------------------------------------------------|
| Hint match              | exact `1.0`; specific req + general claim `0.5`; reverse `0.0` |
| Context / output        | hard gate                                                    |
| Latency class           | `1.0` exact / `0.8` adjacent / `0.5` two-class gap           |
| Affinity                | clamped `[0.0, 1.0]`, final multiplier                       |
| Observed health         | blends self-reported affinity with rolling failure rate; trusts claim at zero samples, fully observation-driven past 50 |
| Load penalty            | hyperbolic `1 / (1 + 0.05 * in_flight)`                      |
| Locality bonus          | `Local` 1.15× / `Near` 1.05× / `Far` 1.0× (from manifest-fetch RTT) |
| Cold-start ramp         | new nodes start at `0.7×`, ramp to `1.0×` over 20 observations |
| Throughput factor       | `[0.3, 1.0]` from observed tok/s (≥5 samples) or benchmark fallback; reference 20 tok/s |
| Inference availability  | gossiped, clamped `[0.2, 1.0]`                                |

Observations are local per scheduler — never advertised between
nodes.

### Benchmark advertisement

Each node runs a one-shot probe at startup (re-runs when
`HardwareProfile` fingerprint changes), measuring prompt processing
+ token generation against a fixed prompt. Result rides
`NodeCapabilities.benchmark` (serde-default so older peers ignore)
and feeds the throughput-factor extrapolation when a peer hasn't
accumulated observation samples yet. See
`sovereign-inference/src/benchmark.rs` and
`sovereign-mesh/src/peer_inference.rs::ThroughputObservedStream`
(TTFT + tg_tok/s EWMA, α=0.3).

### Advertisers

- `commonwealth-api::routes_oicp::synthesize_default_claim` —
  one claim per `ModelInfo`.
- `sovereign-mesh::inference_adapter` — synthesises slot-claims
  (Quick → `Fast`, Main → `Normal`) and an optional Code-specialist
  claim (`code` hint, `Normal`, affinity floor 0.5 for
  filename-signalled coders).

### Extension governance

`MeshInferenceProvider` carries an `ExtensionRegistry` (in
`oicp-types`) that passively records `x:*` hints seen on outgoing
requests and incoming claims. Consumed by an external governance
review process — **not** the scheduler.

---

## 3. Coding-agent harness adapter — `/v1/responses` + Harness frontdoor

`commonwealth-api` exposes `/v1/chat/completions` (OpenAI chat) and
`/v1/responses` (OpenAI Responses API — codex 0.130+ requires it).
The Responses adapter is a wire-format translator over the
chat-completions handler — same slot routing, OICP gating, ATOS
middleware, grammar-constrained tool calls. Code lives in
`commonwealth/crates/commonwealth-api/src/routes_responses.rs`
(handler) + `responses_types.rs` (wire shapes).

### Harness polymorphism (`frontdoor::Harness`)

Different agentic harnesses have different contracts; one reshape
strategy that works for opencode actively fights codex. The adapter
resolves the active harness per request from `User-Agent`
(or `SOVEREIGN_HARNESS` env override) and applies a per-profile
pipeline:

| Profile  | Trigger                                              | Distiller | Catalog filter | Synthetic write_file | Grammar lock | Coherence baseline |
|----------|------------------------------------------------------|-----------|----------------|----------------------|--------------|--------------------|
| Codex    | UA: `codex_cli_rs/*`                                 |     -     |      yes       |          -           |     yes      |        yes         |
| Opencode | UA: `opencode/*` (or legacy `SOVEREIGN_FRONTDOOR=1`) |    yes    |      yes       |         yes          |     yes      |        yes         |
| Generic  | unknown UA, no env                                   |     -     |       -        |          -           |     yes      |        yes         |
| Bare     | `SOVEREIGN_HARNESS=bare`                             |     -     |       -        |          -           |       -      |         -          |

**Why codex passthrough.** Codex 0.130 teaches the model to write
files via `exec_command` running heredoc `apply_patch <<'EOF' ...`
The teaching lives in codex's verbose system prompt;
`include_apply_patch_tool` is `false` by default, so apply_patch is
NOT a function tool in the catalog. Distilling the prompt or
injecting synthetic write_file tools strips the contract the model
was trained against — verified 2026-05-13 (v11-v14 oicp-types smokes:
18/18 turns shell-only, 0 file writes). The Codex profile passes
the contract through untouched; v15 same task → 31 turns, 30/30
tool_calls parsed clean, 7 heredoc writes, 1076 bytes landed.

### Coherence baseline

`frontdoor::apply_baseline` runs unconditionally on every non-Bare
profile:

- **History compression** — folds older items into a synthetic
  `# Conversation so far\n…` message when `items.len() > 8` OR
  `total_byte_size > 20480`; primary-slot inference pass, cached by
  SHA-256. Keeps the most recent 4 verbatim.
- **Per-session telemetry JSONL** at
  `~/.sovereign/codex-sessions/sessions.jsonl`. `inbound` +
  `terminal` records per turn, joinable by `response_id`. Terminal
  `function_calls[]` carry `args_bytes` + `args_parsed_ok`;
  `apply_patch` calls carry a `heredoc` sub-object (body markers,
  file-op counts, escape-coherence smell counters) — extractor at
  `frontdoor::extract_heredoc_diagnostics`.

### Opencode-only full reshape

`frontdoor::apply` plus per-profile flags in `translate_request`:

- **Distiller** — primary slot rewrites codex's verbose system
  prompt into a structured directive (task / constraints / done_when
  / files_to_touch) plus tool-usage policy. Cached by SHA-256 of
  the original instructions + first user message.
- **Catalog filter** — `CODEX_TOOL_KEEPLIST` (`exec_command`,
  `web_search`) drops agent-management / interactive tools the
  local model can't usefully dispatch.
- **Synthetic file tools** — `write_file(path, content)`,
  `read_file(path)`, `write_file_begin/chunk/end(path)` injected.
  Outgoing tool_calls get rewritten to codex-compatible
  `exec_command` shell invocations before SSE events reach codex.
- **Grammar lock** — promotes `tool_choice` to `"required"` so the
  inference adapter's tool-envelope JSON-Schema grammar engages
  every turn. Closes the args-as-stringified-JSON over-escape class.
- **Chunked-write state detection** — when the prior assistant
  emission was `write_file_begin` or `write_file_chunk`, filters
  the outbound catalog down to `[write_file_chunk, write_file_end]`.

### Operator harness brief

`frontdoor::apply_brief_from_env_path` — a per-harness env var
(`SOVEREIGN_CODEX_BRIEF` for Codex) points at a UTF-8 file whose
contents prepend to `req.instructions`. Brief lives on disk so
upstream-contract drift is a one-file edit, not a rebuild.
Re-read every turn; idempotent; no-op on unset / missing.

### Streaming SSE state machine

One chat-completions chunk → 0..N Responses events. Emits the full
lifecycle: `response.created` + `in_progress` at start;
`output_item.added` + `content_part.added` on first text;
`output_text.delta` per token; `function_call_arguments.delta` +
`.done` + `output_item.done` for tool calls;
`response.completed` at terminal.

### Known invariants

- **`force_tool_calls=true` locks the model into a tool-call loop**
  — daemon flag makes every tools-using request emit a tool
  envelope; codex/opencode default `tool_choice=auto` → infinite
  loop. Default off.
- **Balanced-envelope stop needs grammar gate** — the brace-balance
  tool-stop in `embedded.rs` must be gated on
  `request.structured_output.is_some()`; LaTeX/markdown `{...}` in
  prose triggered false-positive truncation.

---

## 4. Cutoff legibility — typed `FinishReason` end-to-end

When synthesis hits `max_tokens` and the model stops mid-sentence,
the desktop chat surface renders an explicit "Response was cut off
mid-thought — hit the N-token limit (M generated). [Continue from
here]" chip. Three load-bearing pieces:

1. **Typed `FinishReason` on every provider surface.**
   `sovereign-core::types::FinishReason { Stop, Length, ToolCalls,
   ContentFilter, Cancelled, Error(String) }` serialises to
   OpenAI-compatible lowercase strings.
   `CompletionResponse.finish_reason` + `.completion_tokens` carry
   typed signal on the non-streaming path;
   `StreamFrame::Finish { reason, usage }` carries it on the
   streaming path via `complete_stream_with_finish`.
2. **`MeshInferenceProvider::complete_stream_with_id_and_finish`**
   mirrors the routing structure of `complete_stream_with_id`
   (explicit model_id → peer match → OICP peer pick → local
   fallback) but calls `complete_stream_with_finish` on every
   terminus. Local termini get the real `EmbeddedLlamaCpp`
   sampler-reason; peer termini get the real SSE `finish_reason`
   parsed by `RemoteApiProvider::complete_stream_with_finish`. The
   runtime streaming spawn consumes typed frames directly — no
   chars-per-token heuristic.
3. **The model is told its budget up front.**
   `build_response_length_directive(max_tokens)` is spliced into
   KnowledgeQuery synthesis prompts so the model picks a shape that
   lands within the budget rather than opening a multi-section essay
   it can't close. Pairs with the cutoff chip — chip catches "model
   still ran out"; directive catches "model planned poorly".

`ThroughputObservedStream` is generic over `S: Stream + Send + Unpin`
where `S::Item: IsDataFrame`. The predicate distinguishes data
frames (count toward `chunk_count`) from terminal frames (Finish,
Error). Both legacy `Result<String>` and typed `StreamFrame` shapes
flow through one wrapper — peer-routing throughput stats survive
the typed swap.

Pinned by: `AssistantMessage.svelte`'s cutoff chip renders only
when `provenance.finish_reason === "length"`; `runtime.rs`
streaming spawns populate `ResponseProvenance.{finish_reason,
max_tokens_budget, completion_tokens}`; the SettingsPanel
"Response length" entry tunes `max_tokens` (Concise 2048 / Standard
4096 / Long 8192 / Essay 16384 presets + free-form input).

---

## Conversation-history compaction (chat layer)

The slot trims its own decode; the chat layer trims its own prompt.
Two arms gate `maybe_compact_dropped_history`
(`sovereign-core/src/runtime/retrieval.rs`):

- **Turn-count arm** — `CONV_HISTORY_TURNS = 8`. Fires when ≥ 2
  messages would otherwise be dropped. Default everyday path.
- **Budget arm** — `COMPACTION_PRESSURE_THRESHOLD = 0.9` of
  `effective_context_size`. Emergency-only. `estimate_compaction_pressure`
  sums conv-history + memories + the existing compacted preamble; system
  message + retrieval bundle are intentionally excluded (they fire later
  in the handler). The narrow sensor is documented inline as a known
  limitation — see the constant's docstring for the trial history
  (v1@0.55 → v2@0.7 both regressed `judge_coverage` on the
  marathon_graceful bench, reset to 0.9 = effective off until the sensor
  measures full-prompt pressure).

The visible window is age-aware: `chars_for_message_age` caps the
per-message body at 1000 / 600 / 300 chars across ages 0-1 / 2-3 / ≥4.
The age=≥4 tier was bumped to 500 in a single-trial v3 run that tied v0
on judge coverage; reverted to 300 per ARCH §11.1.

Glassbox: every compaction emits a `runtime:compaction.*` tracing event
plus a `NarrationPhase::GapCheckFired` chip (suppressed below
`COMPACTION_CHIP_MIN_DROPPED = 3` to avoid spam on short chats).

Bench: `sovereign/bench/wikipedia_learn/threads.toml#marathon_graceful`
is the 21-turn fixture covering Phase A (topic Q&A) → pivot → Phase B
(second topic) → Phase C (third topic) → callbacks across all three.
Baselines under `bench/wikipedia_learn/baselines/threads-marathon-graceful-*.json`.

## Retrieval-over-history (default-on, 2026-05-26)

`Runtime::maybe_retrieve_relevant_history` surfaces directly relevant
earlier turns when the conversation grows past the visible window
(`CONV_HISTORY_TURNS = 8` most recent). Replaces what would otherwise
be a re-summarisation spiral: instead of folding the dropped tail into
a fresh Fast-slot summary every turn, retrieve the 1-5 most relevant
prior (user, assistant) pairs by hybrid score.

### Pipeline

1. **Query enrichment.** Concatenate current user message with
   `topic_context.topic` + `topic_context.domain` (added as
   `[topic: …]` / `[domain: …]` markers). Captures pivots like
   "switching back to Linnaeus" where bare phrasing wouldn't cosine-
   match the right earlier turn.
2. **Pair construction.** Walk message pairs OUTSIDE the visible
   window in (user, assistant) order. Each pair becomes one
   indexable unit, body truncated at 600 chars per side.
3. **Embed batch.** `InferenceProvider::embed_batch` over the unit
   bodies + `embed_query` over the enriched query (qwen-embedding-0.6b
   today).
4. **Entity extraction (when GLiNER installed).** GLiNER ONNX (5-label
   tag set: Person, Organization, Work, Location, Event) extracts
   entity sets from query + each pair. Trait `EntityExtractor` in
   `sovereign-core::traits`; impl on `GlinerExtractor` in
   `sovereign-tools::gliner_ner`. Cycle-break — runtime holds
   `Option<Arc<dyn EntityExtractor>>` so core doesn't depend on tools.
5. **Hybrid scoring.** `0.6·cosine + 0.4·jaccard(entity_set)` when
   GLiNER is wired; pure cosine fallback when it isn't.
6. **MMR top-K selection.** K=5, similarity floor 0.30, λ=0.5
   (balanced relevance/diversity). Keeps cross-subject diversity
   on synthesis turns ("compare across Curie / Linnaeus / Galileo").
7. **Render.** `build_system_message` injects hits BEFORE the visible-
   history block as "Relevant earlier turns from this conversation
   (selected by similarity to your current message):" with turn index,
   similarity, and pair body.

### Toggle

Default on. `SOVEREIGN_HISTORY_RETRIEVAL=0` disables for A/B compares.
When GLiNER model `gliner_small-v2.1` isn't installed under
`~/.sovereign/models/gliner/`, the system falls back to pure cosine +
MMR (entity-aware retrieval silently disabled, no error).

### Bench evidence (marathon_graceful)

Single-trial v14 produced the highest judge score across the entire
v0-v16 spike (judge 0.792 vs v0 baseline 0.764). Subjective read of
transcripts: v14 caught factual details v0 hallucinated (e.g. Curie's
1934 death date). Three-trial mean was hurt by one judge-instrument
outlier (v15 judge 0.278); judge variance on this fixture saturates
the measurement. See `baselines/threads-marathon-graceful-v{14,15,16}-gliner.json`.

---

## See also

- `commonwealth/docs/oicp-v0.3.md` — OICP wire-protocol spec.
- `commonwealth/docs/routing-field-guide.md` — end-to-end
  `/v1/chat/completions` priority ladder (embedded vs standalone,
  `MeshInferenceProvider` gates, gotchas).
- `sovereign/docs/MESH_LOAD_AWARENESS.md` — peer load shedding +
  foreground-yield primitive.
- `sovereign/docs/PINNED_WORKER_AS_INFERENCE_PEER.md` — Vast L40S
  pods as scheduler-visible inference peers.
