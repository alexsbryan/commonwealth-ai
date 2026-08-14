# Routing Field Guide — `/v1/chat/completions`

A short map of where a chat-completion actually goes after the daemon
accepts it. Written because the same handful of confusions keep
recurring in code review and debugging:

- "I asked for model X, why did model Y answer?"
- "I'm in a mesh with peer Y, why does the routing never reach Y?"
- "I added an OICP envelope, why am I still local?"
- "`/v1/models` lists model X, why does requesting it 503?"

The wire-protocol spec for OICP itself lives in `oicp-v0.3.md`. This
file is the *implementation* map: which file runs when, what gates
are in place, and where each routing decision is made.

---

## 1. There are two daemon shapes

This is the single largest source of confusion. The same HTTP route
(`POST /v1/chat/completions`) takes a different path through the
codebase depending on which daemon binary is running.

| Shape | Binary | Wired in | `state.local_inference` | How chat completes |
|---|---|---|---|---|
| **Standalone** | `commonwealth daemon …` | `commonwealth-daemon/src/main.rs` | `None` | Orchestrator spawns separate `llama-server` processes; handler forwards over TCP via `forward_to_llama_server` |
| **Embedded**   | `sovereign desktop` / `sovereign daemon` | `sovereign-mesh/src/daemon.rs::start_daemon` | `Some(SovereignInferenceAdapter)` | In-process. Adapter wraps `MeshInferenceProvider` which wraps the local `EmbeddedLlamaCpp`; peer routing happens inside the adapter, not at the HTTP handler |

The desktop and CLI daemons are **always** embedded. `local_inference`
is set unconditionally when `sovereign-mesh::EmbeddedDaemon` is the
host. Standalone Commonwealth nodes (intended for headless mesh
peers without a Sovereign runtime) are the only callers that leave
`local_inference == None`.

If you don't know which shape you're looking at, check `state.rs:196`
— `local_inference: Option<Arc<dyn LocalInferenceService>>`. That's
the discriminator.

---

## 2. The priority ladder (`routes_inference.rs::chat_completions`)

```
POST /v1/chat/completions
        │
        ▼
   pipeline alias?            ─── yes ──▶ run ATOS middleware chain,
   (e.g. commonwealth/                    rewrite request.model to
    sovereign-coder)                      concrete model id, fall through
        │
        ▼
   Priority 0:                ─── yes ──▶ serve_local_{stream,non_stream}
   local_inference is Some?               (calls SovereignInferenceAdapter
        │                                  — see §3 for what happens inside)
        │ no
        ▼
   request has OICP envelope
   with sharding=LocalOnly?   ─── yes ──▶ 400 (cannot forward LocalOnly)
        │ no
        ▼
   Priority 1:                ─── yes ──▶ route_with_oicp →
   request has OICP                      forward_to_model →
   capability requirements?               forward_to_llama_server
        │ no                              (TCP to a spawned llama-server)
        ▼
   Priority 2:                ─── yes ──▶ forward_to_model
   model name matches a
   loaded model exactly?
        │ no
        ▼
   Priority 3:                ─── yes ──▶ synthesize OICP from alias,
   model name matches a                  route_with_oicp,
   model_aliases entry?                  forward_to_model
        │ no
        ▼
   Priority 4:                          forward_to_model with
   default_model_id                      state.default_model_id()
```

Read `commonwealth-api/src/routes_inference.rs:26` for the source.

**Critical asymmetry:** in embedded mode, **Priority 0 is unconditional
and short-circuits everything below it**. Priorities 1–4 only run on
standalone. Any developer reasoning "but I have an OICP envelope, so
Priority 1 will pick the right model" is wrong in the embedded case
— Priority 0 fires first, and routing then happens inside
`MeshInferenceProvider` (§3) under a *different* set of rules.

---

## 3. Embedded routing — inside `MeshInferenceProvider`

Once `serve_local_{stream,non_stream}` calls `service.chat_completion()`,
the request flows through:

```
ChatCompletionRequest (OpenAI shape)
      │
      ▼
SovereignInferenceAdapter::build_completion_request
      │   inference_adapter.rs:159
      │   - flattens messages → prompt + system
      │   - copies request.model into CompletionRequest.model_id
      │   - copies request.oicp into CompletionRequest.oicp
      │   - picks Speed via pick_slot_for_oicp(provider, request)
      │     (oicp_select.rs:214 — Fast vs Slow vs Medium for local slots)
      ▼
MeshInferenceProvider::complete (or complete_stream_with_id)
      │   peer_inference.rs:714 / 775
      ▼
   STEP 1 — explicit-name dispatch                       peer_inference.rs:730
      │   when request.model_id is set & non-empty
      │   → locate_named_model(model_id):
      │       Local    → self.local.complete(request)
      │       Peer     → POST to that peer; on transport
      │                  failure return Error::Routing
      │                  (no local fallback; do NOT pretend
      │                   to serve a model the peer owns)
      │       Unknown  → Error::ModelNotLoaded with the
      │                  requested name in the message
      │
      │   This bypasses every gate below. An explicit
      │   `model: "X"` is stronger than any OICP signal —
      │   the user named a specific model and we honour it.
      │   Empty/whitespace model_id falls through.
      ▼
   STEP 2 — OICP-driven dispatch (no model_id, or empty)
MeshInferenceProvider::select_peer                       peer_inference.rs:532
      │
      ▼
   Three sequential gates, ALL must pass to route to a peer:
      │
      ├─ Gate A: REMOVED 2026-08-13. It was
      │           has_routing_signal(request) — an envelope-less
      │           request never reached the scorer. That gate fired on
      │           100 of 100 turns in the mesh-serve-50 fleet-scaling
      │           measurement (MESH_SCALE_100_USERS_1000_CORPORA.md
      │           §9.1.1) and was the reason a second serving node
      │           added zero admitted concurrency: a plain OpenAI
      │           client sends no envelope, so no peer was ever scored.
      │           The envelope question is now asked once, at Gate B.
      │
      ├─ Gate B: oicp.sharding != LocalOnly               peer_inference.rs:540
      │   ↪ LocalOnly → return None (privacy)
      │
      ├─ Gate C: request.preferred_speed == Speed::Slow   peer_inference.rs:548
      │   ↪ Fast or Medium → return None
      │     (latency-critical paths — router, compression, title gen
      │      — never cross the network)
      │
      ▼
   Score local + every reachable peer:
      score_manifest_for_request(manifest, request.oicp)   oicp_select.rs:96
      → ModelCandidate { score, size_gb, model_id, claim_affinity }
      Then fold in operational adjustments (load, locality,
      cold-start, throughput, observed health) per oicp-types
      helpers.
      │
      ▼
   pick_better(local, peer) under (score, then size_gb)    oicp_select.rs:64
   policy. **Local wins ties** — no round-trip cost,
   no attribution churn.
      │
      ▼
   Route to peer iff peer is STRICTLY better than local.
   Otherwise → return None (stay local).
```

If a peer is selected (either path), the request is serialized and
POSTed to `peer.base_urls[i]/chat/completions` via `RemoteApiProvider`.
The response's `model_id` is rewritten to `<advertised-id> @ peer
<name>` so logs and provenance show where the completion came from.
On any transport error, the next address is tried.

**Failure-mode asymmetry between the two paths:**

- **Explicit name:** if all peer addresses fail, return
  `Error::Routing` — do NOT fall back to local. The user named a
  specific peer model; substituting silently is the bug we're
  closing.
- **OICP-driven:** if all peer addresses fail, fall back to
  `self.local.complete(request)`. The caller didn't pin a specific
  model; serving on any reasonable local slot is preferable to a
  hard failure.

**Wiring requirement:** the daemon's serving provider must be
wrapped in `MeshInferenceProvider`. The CLI daemon does this in
`sovereign-cli/src/daemon_cmd.rs::run_daemon` (cold start) and
`LlamaCppFactory::build_provider` (hot reload). The desktop does
it in `sovereign-desktop/src-tauri/src/state.rs:649`. A bare
`Arc<EmbeddedLlamaCpp>` handed to `set_inference_provider` would
re-introduce the silent-substitution bug — `EmbeddedLlamaCpp` has
no peer awareness, so an unmatched `model_id` would fall through
to its `pick_slot` default and answer with the local primary.

---

## 4. Where the `model` field actually matters

This is where the most production confusion happens. The OpenAI
`model` field has *different* power depending on which path runs.

| Path | `model` field consulted? |
|---|---|
| Pipeline alias resolution | yes — alias key |
| Embedded **Priority 0** → `MeshInferenceProvider` STEP 1 | **yes — drives routing.** `locate_named_model(model_id)` matches against `self_manifest` then each reachable peer's manifest. Local match → serve locally; peer match → forward to peer; no match → `ModelNotLoaded` error. Bypasses the OICP gates and the `Speed::Slow` constraint. |
| Embedded **Priority 0** → `MeshInferenceProvider` STEP 2 (no model_id) | no — pure OICP scoring, same as before |
| Embedded → local `EmbeddedLlamaCpp` (after STEP 1 routes Local) | yes — `model_id` selects Fast/Primary/Code/extras slot by gguf stem; falls through to `Speed`-based routing only when the name matches no slot |
| Standalone Priority 1 (OICP) | indirectly — only via OICP scoring |
| Standalone Priority 2 (exact-name) | yes — `find_model_by_name` |
| Standalone Priority 3 (alias) | yes — `model_aliases.resolve` |
| Standalone Priority 4 (default) | no — fallback |

**Routing recipe** — "send this to peer X's model Y":
just POST `{"model": "<peer-Y's-advertised-id>", "messages": [...]}`
to your local daemon. The daemon's `MeshInferenceProvider` looks up
that id in each peer's manifest and forwards the request. The
response's `model` field will be stamped `<id> @ peer <X>` so it's
clear where the completion came from.

**Two contracts the `model` field gives you in embedded mode:**

1. *Honour the name.* If you ask for model X, you either get model
   X (from whichever node serves it) or a 503 telling you no node
   does. No silent substitution to a different model.
2. *Local-first when local has it.* If both local and a peer
   advertise the same id (rare but possible — same gguf on two
   nodes), local wins. No round-trip, no attribution churn.

**Knobs that don't affect explicit-name dispatch:**

- The `oicp` envelope is ignored for routing when `model_id` is
  set. (It's still forwarded into the request body; peer-side slot
  pickers may consult it.)
- `preferred_speed` (`Fast` / `Medium` / `Slow`) is ignored. The
  Speed-gate that keeps OICP-driven routing local for latency-
  critical paths does not apply when the user named a specific
  model.

---

## 5. Three OICP scorers, one shared scoring function

OICP scoring runs in *three* places, each with its own
operational-adjustment fold-in:

| Scorer | Where | Used for |
|---|---|---|
| `commonwealth-inference::scheduler::oicp_select` | Standalone scheduling — which `llama-server` to send work to | Inside the orchestrator's plan builder |
| `sovereign-inference::selector::CapabilityAwareSelector` | Hybrid provider — pick a backend among local + remote APIs | Inside Sovereign's runtime when multiple `InferenceProvider`s are configured |
| `sovereign-mesh::oicp_select` | Embedded mesh peer routing — pick local-vs-peer per request | The Joiner-side selector and the peer-side slot picker |

All three call `oicp::score_claim_for_request(claim, request)` — the
spec-defined hint × latency × context × output × affinity formula.
Each then folds in its own operational signals (load, locality,
cold-start, throughput, observed health). **Observations are local
per scorer and never advertised.** Don't try to share an
observation pipeline across scorers; you'd violate the v0.3 §4 rule
that observations stay node-local.

Tie-break (`oicp_select.rs::pick_better`):
1. Strictly higher `score` wins.
2. On score tie (`SCORE_TIE_EPSILON = 1e-3`): smaller known
   `size_gb` wins.
3. Annotated size beats unknown size.
4. Full tie: incumbent wins.

For the joiner-side selector, "incumbent" means local. Local wins
duplicate-cost ties — never cross the network for free.

---

## 6. `/v1/models` is "what we can serve right now"

`routes_inference.rs::list_models` returns models from
`inference_store.list_models_with_origins()` filtered to entries
whose owning peer is currently `Online` or `Busy`, OR for which
this daemon currently holds the weights.

What this means for debugging:

- A model in `/v1/models` is *advertised* by the daemon; it is
  **not** a guarantee that any specific peer can complete a request
  for it right now. It only guarantees the advertising peer is in
  the live set.
- Quantization differences matter: `gemma-4-E4B-it-Q4_K_M` and
  `gemma-4-E4B-it-Q5_K_M` are *different* model ids. Two peers
  running different quants of the same base model show up as two
  distinct entries.
- Embedded mode lists local slots only by their gguf file stems;
  there's no glob aliasing at the `/v1/models` layer.

---

## 7. Diagnostic recipes

```sh
# What does THIS node think the mesh looks like?
curl -s http://localhost:9741/v1/mesh/status | jq

# What does THIS node advertise via OICP?
curl -s http://localhost:9741/oicp/v1/capabilities | jq

# What does PEER advertise? (Tailnet IP, no auth, loopback-OK)
curl -s http://<peer-ip>:9741/oicp/v1/capabilities | jq

# What does the Joiner-side aggregate `/v1/models` look like?
curl -s http://localhost:9741/v1/models | jq

# Confirm the peer's daemon is alive but its chat slot may be wedged:
# embeddings should be ~50ms; chat-completions hanging for >30s is a
# wedge, not a slow load.
curl -s --max-time 5 http://<peer-ip>:9741/v1/embeddings \
    -H 'content-type: application/json' \
    -d '{"input":"x","model":"<peer-embed-model-id>"}' | jq '.data[0].embedding | length'
```

---

## 8. Common gotchas — quick reference

| Symptom | Likely cause |
|---|---|
| Sent `model: X` (peer-only), got `503 model_not_loaded` saying "no node in this mesh advertises model 'X'" | The id you sent isn't in any node's `/oicp/v1/capabilities`. Check spelling and quant suffix — `gemma-4-E4B-it-Q4_K_M` ≠ `gemma-4-E4B-it-Q5_K_M`. (§4) |
| Sent `model: X` (peer-only), got `503 model_not_loaded` saying "advertised by a peer, but … its mesh hop budget is spent" | Different cause, and `/v1/models` **will** list the model — don't go looking there. The request's OICP envelope arrived with `forward_budget: 0`, meaning some node already forwarded it once, and a second forward could bounce between nodes holding stale manifests. Either raise `forward_budget`, or send to a node that holds the model. (Before 2026-08-06 this case reported the absence message above and sent you to a dead end — M6-B finding B1.) |
| Sent `model: X` (peer-only), got a 503 saying "peer routing is disabled by SOVEREIGN_DISABLE_PEER_INFERENCE" | Exactly what it says: this node refuses to look outward, so a peer may well hold the model. Unset the variable, or load the model locally. |
| Sent `model: X` to the local daemon and got the local primary instead | The daemon's serving provider isn't wrapped in `MeshInferenceProvider`. Cold start → check `daemon_cmd.rs::run_daemon`; hot reload → check `LlamaCppFactory::build_provider`. A bare `Arc<EmbeddedLlamaCpp>` handed to `set_inference_provider` re-introduces the silent-substitution bug. (§3) |
| Sent `model: X` (peer-only), got a 503 `routing` error after a delay | Routing reached the peer, peer's chat slot is wedged or never loaded. Check the peer's `/v1/embeddings` (should be ~50ms) — if that works but chat hangs, the chat slot itself is the problem, not the network |
| Added an OICP envelope (no `model`), still local | Either `preferred_speed != Slow` (Gate C) or local wins on score-then-size (§3) |
| Set `latency_class: fast`, expected fast slot, got slow | `pick_slot_for_oicp` returns Fast only when a Fast slot is *loaded*; falls back to Slow when not |
| `/v1/models` lists model X, but POST returns 503 `model_not_loaded` | The advertising peer dropped to Offline between gossip ticks; `list_models` filter is stale by up to one gossip interval. Re-check `/v1/mesh/status`. |
| Two peers with same base model, different quants — only one shows in routing | Different model ids; the scorer scores them separately. Check `/oicp/v1/capabilities` on each |
| Want to test peer routing without changing capabilities | POST directly to the peer's `:9741/v1/chat/completions` — bypasses our routing pipeline entirely |
| Chat hangs but embeddings work on the same daemon | Embed slot is independent of chat slots (`Arc<EmbedSlot>` vs `Arc<Mutex<…>>` for chat). A chat-slot wedge is a llama-cpp / loader bug, not a daemon-level outage |

---

## 9. File map

| Concern | File:line |
|---|---|
| HTTP route + priority ladder | `commonwealth-api/src/routes_inference.rs:26` |
| OpenAI ↔ Sovereign translation | `sovereign-mesh/src/inference_adapter.rs:293` (impl) / `:159` (build_completion_request) |
| Self-manifest construction | `sovereign-mesh/src/inference_adapter.rs:553` |
| **Explicit-name dispatch** (`MeshInferenceProvider::locate_named_model` + branch in `complete()` / `complete_stream_with_id()`) | `sovereign-mesh/src/peer_inference.rs` (search for `locate_named_model` / `explicit_model_id`) |
| OICP-driven peer routing decision | `sovereign-mesh/src/peer_inference.rs::select_peer` |
| Offload gate incl. the envelope-absent case (OICP path) | `sovereign-mesh/src/oicp_select.rs::offload_verdict_opt` (replaced `peer_inference.rs::has_routing_signal`, removed 2026-08-13) |
| Speed::Slow gate (OICP path) | `sovereign-mesh/src/peer_inference.rs::select_peer` (~line 548) |
| Score function (shared) | `oicp-types/src/lib.rs::score_claim_for_request` |
| Score + tie-break helper | `sovereign-mesh/src/oicp_select.rs::pick_better` |
| Slot picker (Fast/Slow/Medium) | `sovereign-mesh/src/oicp_select.rs::pick_slot_for_oicp` |
| Manifest-fetch + RTT cache | `sovereign-mesh/src/peer_inference.rs::get_peer_manifest` |
| Live-peer filter on `/v1/models` | `commonwealth-api/src/routes_inference.rs::list_models` |
| **CLI-daemon wrapping** (cold start + reload factory) | `sovereign-cli/src/daemon_cmd.rs::run_daemon` (search for `MeshInferenceProvider::new`) and `LlamaCppFactory::build_provider` |
| **Desktop wrapping** | `sovereign-desktop/src-tauri/src/state.rs` (search for `MeshInferenceProvider::new`) |
