# Using this with the tools you already run

You already have an agent harness, an editor, a chat UI, a RAG script.
This page is how to point them at a local Commonwealth daemon without
rewriting any of them.

It is the task-oriented companion to
[INTEGRATION_SURFACES.md](./INTEGRATION_SURFACES.md), which answers a
different question — *which surfaces are contracts and which are
internals that happen to be visible*. Read that one before you build
something load-bearing. Read this one to get working in five minutes.

## The rule we build by

**Integrate at the standard layer, never the product layer.**

We do not ship per-harness adapters, and we do not want to. Speak the
four protocols below and every client that speaks them works — the ones
that exist today and the ones that replace them next year. A bespoke
integration with any single tool is a bet on that tool's governance,
and integration surface is liability as much as reach.

So: there is no "Commonwealth plugin" for your agent framework. There
is an OpenAI-compatible endpoint, an Ollama-native endpoint, an MCP
server, and an open protocol spec. Pick the socket your tool already
has a plug for.

## The four sockets

| Socket | Where | Reach for it when |
|---|---|---|
| **OpenAI-compatible** | `POST :9741/v1/chat/completions`, `/v1/responses`, `/v1/embeddings`, `GET /v1/models` | Your tool takes a `base_url`. This is most tools. |
| **Ollama-native** | `POST :9741/api/chat`, `/api/generate`, `GET /api/tags`, `/api/ps`, `POST /api/show`, `/api/embed` | Your tool speaks Ollama and has no OpenAI mode. |
| **MCP** | `POST :9741/mcp` (Streamable HTTP) | You want *your* agent to use *our* code intelligence, cross-session memory, and peer coordination. |
| **OICP** | `GET :9741/oicp/v1/capabilities`, `POST /v1/knowledge/search` | You want retrieval over local corpora, or you're implementing the protocol on the other side. The spec is CC0. |

Everything is on **one port, `9741`**, loopback by default.

## Four facts before you start

**Auth.** Callers from loopback are admitted with no token. Remote
callers must send `Authorization: Bearer <token>` — set `[daemon]
client_token` in `~/.svrnmesh/config.toml` (`~/.sovereign/` is the
legacy name for the same directory), or read the auto-generated one
from `<data_dir>/client-token`. `GET /status` and `GET
/oicp/v1/capabilities` are exempt so a peer can probe before
authenticating. No token configured plus a remote caller fails closed.

**Some routes are loopback-only regardless of token.** `/mcp`,
`/v1/mesh/*`, `/v1/admin/*`, `/v1/solve/*` and everything under
`/internal/*` 403 a non-loopback caller even with a valid bearer. They
expose local tooling, not a remote service. If you need them from
another machine, tunnel — don't widen the bind.

**Never expose `:9742`.** The mesh-internal port has no per-request
auth by design, on the assumption it rides a private network or a
WireGuard/Tailscale overlay. See [THREAT_MODEL.md](./THREAT_MODEL.md).

**Model names.** Use the stable aliases `primary`, `fast`, `embed`, or
whatever `GET /v1/models` lists. Aliases keep working when you swap the
underlying GGUF; concrete filenames don't.

**Licensing.** The code is AGPL-3.0-or-later — relevant if you plan to
embed it, not if you're calling it over HTTP. The OICP spec itself is
CC0: implement it freely on either side, no obligations.

---

## 1. Point any OpenAI client at the local model

The universal recipe. Base URL, any non-empty key (loopback ignores it):

```sh
export OPENAI_BASE_URL=http://localhost:9741/v1
export OPENAI_API_KEY=local
```

```sh
curl http://localhost:9741/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"primary","messages":[{"role":"user","content":"hello"}],"stream":true}'
```

That covers the OpenAI SDKs, LangChain, LlamaIndex, aider, LiteLLM
(as a backend behind the proxy), and every agent harness that exposes a
custom-endpoint setting — put the same two values wherever your tool
keeps them. Streaming is real SSE, terminated with `[DONE]`.

### What we honor, and what we quietly drop

Honored: `model`, `messages`, `max_tokens`, `temperature`, `top_p`,
`stream`, `tools`, `tool_choice`, `response_format` (both `json_object`
and `json_schema`, enforced with a grammar-constrained sampler).

**Silently ignored — this is the part that bites:**

| Field | Reality |
|---|---|
| `stop` | Ignored on chat. Use `response_format` or `lark_grammar` to bound output. It *is* honored on `/v1/completions`. |
| `frequency_penalty`, `presence_penalty` | Parsed, never forwarded. Model-family defaults win. |
| `n`, `seed`, `logprobs`, `logit_bias`, `user` | Dropped by serde. |
| non-text content parts (`image_url`, `input_audio`) | Dropped. Text parts survive. |
| `encoding_format` on embeddings | Always float. |

Nothing errors on these — you get a normal answer computed under
different settings than you asked for. If your tool relies on `stop`
to delimit output, that's the first thing to check.

**`POST /v1/completions` is FIM, not legacy text completion.** It is
the fill-in-the-middle route: it takes `prefix`/`suffix` (or
`prompt`+`suffix`), and returns `503 fim_unavailable` unless a
`[models.fim]` slot is configured. Pointing a legacy completions client
at it will not do what that client expects.

## 2. Ollama-native clients

Open WebUI's Ollama mode, IDE plugins, Raycast, Enchanted — anything
that speaks the Ollama wire connects unmodified. Point it at
`http://localhost:9741` and it will find `/api/tags`, `/api/chat`,
`/api/generate`, `/api/ps`, `/api/show`, `/api/embed`.

**One caveat that matters for UX:** `/api/chat` and `/api/generate`
compute the whole answer and frame it as a single NDJSON turn. The
shape is correct and clients parse it fine, but you get no token-by-
token reveal — the reply lands all at once after the full generation.
Incremental streaming here is a tracked follow-up. If your client can
speak OpenAI instead, use §1 and get real SSE.

This direction is server-only: we *serve* the Ollama wire, we don't
*call* Ollama through it (see §9 for the outbound direction).

## 3. Codex CLI

Codex dropped `wire_api="chat"`, so it needs the Responses dialect:
`POST :9741/v1/responses`, translated over the same handler as chat —
same routing, same grammar-constrained tool calls, full streaming event
lifecycle.

Two deliberate deviations: `previous_response_id` returns **400** (we
keep no server-side response state, and Codex correctly falls back to
resending history), and non-function tools (`web_search`,
`local_shell`) are dropped rather than faked.

`gym/` in this repo is the regression suite for exactly this path —
real request bodies frozen from observed failures, replayed with
per-fixture pass predicates. If you hit a Codex-shaped bug, that's
where a reproduction belongs.

## 4. MCP — give your existing agent our code intelligence

The other direction: your harness stays in charge, and gains our tools.

```json
{
  "mcpServers": {
    "sovereign": { "type": "http", "url": "http://localhost:9741/mcp" }
  }
}
```

That is the whole config for Claude Code (`.mcp.json` in your project
root — this repo ships one). For opencode, the same URL goes under its
`mcp` key. Any client with an HTTP MCP transport takes the same URL.

**Three constraints to know:**

- **HTTP only — there is no stdio server.** Streamable HTTP plus the
  legacy HTTP+SSE path at `/mcp/message`. Protocol revisions
  `2025-06-18`, `2025-03-26`, `2024-11-05`, and JSON-RPC batches are
  accepted. A stdio-only client needs an external bridge.
- **Loopback-only, no auth.** Same box, or a tunnel.
- **Tools only.** No resources, no prompts.

What you actually get (query `tools/list` for the live set — it varies
by which server you run, and the daemon serves the largest set):

| Group | Tools |
|---|---|
| Code intelligence | `symbols`, `callers`, `callees`, `blast`, `code_search`, `facts`, `capability_map`, `arch_report`, `arch_posture` |
| Durable memory | `note`, `notes`, `retire_note`, `briefing`, `session_state` |
| Peer coordination | `work_in_flight`, `declare_scope`, `release_scope` |
| Spec/narrative drift | `drift_findings`, `drift_posture`, `atos_verify` |
| Build feedback | `lint_status`, `get_lint_output`, `build` |
| Autonomous solve | `solve`, `solve_status`, `solve_cancel` |

Six deprecated aliases (`symbol_lookup`, `find_callers`,
`find_callees`, `blast_radius`, `read_notes`, `write_note`) still
resolve and are marked as such in `tools/list`. Use the short names.

`callers`/`callees` are compiler-resolved from a SCIP graph, not
grep — they catch trait dispatch. That is the reason to wire this up
rather than let your agent read files.

Note that `corpus_search` on this surface is a pipeline primitive: it
takes a pre-computed query **vector**, not text. For text-in retrieval
use §5.

## 5. Retrieval over local corpora

`POST :9741/v1/knowledge/search` is the thin-client retrieval endpoint.
Send text; the host embeds it with its own advertised query-instruction
prefix, so you never need to know or match the embedding model:

```sh
curl http://localhost:9741/v1/knowledge/search \
  -H 'Content-Type: application/json' \
  -d '{"query":"what is a corpus recipe","limit":5}'
```

```json
{
  "results": [
    {
      "content": "...",
      "title": "…",
      "corpus_id": "sovereign",
      "score": 0.71,
      "chunk_id": 342,
      "source_doc_id": "…",
      "metadata": {}
    }
  ],
  "corpora_searched": ["sovereign", "wikipedia-simple", "…"],
  "corpora_unavailable": []
}
```

Optional `corpora: ["id", …]` scopes the search; omit it and every
installed corpus is searched. `limit` defaults to 20.

Two things make this more than a vector-search endpoint. It is
**mesh-aware**: corpora hosted by a peer are fanned out to over the
mesh and merged by score, so a machine that doesn't hold a corpus can
still cite it. And a peer that times out lands in `corpora_unavailable`
rather than failing the query — one sleepy node never takes the search
down.

`chunk_id` and `source_doc_id` are what let a citation be dereferenced
back to the exact source chunk. If you're building a UI, keep them.

## 6. Embeddings

`POST :9741/v1/embeddings`, OpenAI shape, `input` takes a string or an
array (batched internally). Ollama-shaped aliases at `/api/embed` and
`/api/embeddings`.

**The `model` field is echoed but does not route.** You get whichever
embed slot is loaded. Multi-embed-model dispatch isn't built; don't
design around per-request model selection.

**There is no `/v1/rerank`.** Reranking exists but is in-process only
(a local cross-encoder GGUF slot) and is consumed internally by
knowledge search. There is no remote rerank path, and an attempt to
use one degrades silently to un-reranked fusion rather than erroring.

## 7. Inline completion in your editor

`svrn setup --fim` configures the FIM slot, and
`packages/vscode-sovereign` is the extension for VS Code, Cursor, and
Windsurf. JetBrains is deferred. Under the hood it is `POST
/v1/completions` (§1) — any editor plugin that can be pointed at an
OpenAI-compatible FIM endpoint will work, whether or not we ship a
plugin for it.

Design notes and measured latency: [../sovereign/docs/INLINE_COMPLETION.md](../sovereign/docs/INLINE_COMPLETION.md).

## 8. Scripting from the shell

`svrn tools call <id> --format json` is the stable machine-readable
path: stdout stays clean JSON, logs go to stderr. `svrn tools list` and
`svrn tools describe <id>` give you the manifest and each tool's
parameter schema and output keys.

Other commands print for humans unless they document a `--format json`
of their own. Don't parse human output — it will change.

---

## 9. Going the other way

### Consuming external MCP servers

Sovereign is also an MCP *client*. Add HTTP MCP servers under
`[[mcp_servers]]` in `~/.svrnmesh/config.toml`, or:

```sh
svrn mcp add <name> --url https://… [--bearer <token>]
svrn mcp list
svrn mcp test <name>
```

External tools are namespaced `mcp_<prefix>_<tool>` and become
available to `svrn chat`, the desktop app, and workflows. Bearer, API
key, and basic auth are supported; credentials resolve at connect time
and are never written to the config file.

Two limits: the config surface offers HTTP only — stdio parses and the
client can drive it, but nothing creates one, because Sovereign
deliberately does not supervise subprocesses. And external MCP tools
are **not** re-exposed through our own `/mcp`; there is no proxy or
aggregator behaviour.

### Pointing Sovereign at an external inference server

An OpenAI-compatible remote provider exists and works against vLLM,
SGLang, llama.cpp's server, TGI, Ollama, or a LiteLLM proxy — endpoint,
optional API key, model id, context size.

**Read this before planning around it.** The main `svrn daemon` is
**embedded-only** — its model config is GGUF paths with no endpoint
key. The remote path lives in the separate `sovereign-server` binary,
which is **build-from-source and not one of the three shipped
binaries** (`cargo build -p sovereign-server`). If you installed from
the CLI tarball, you do not have it.

```toml
# sovereign-server.toml — backends compose into a health-checked router
# with priority fallback. Presence of [[inference.backends]] overrides
# the single-model `[inference] model = …` form entirely.

[[inference.backends]]
name          = "local"
type          = "embedded"        # "embedded" | "remote"
primary_model = "models/primary.gguf"
priority      = 1                 # lower wins; default 1

[[inference.backends]]
name          = "vllm-box"
type          = "remote"
endpoint      = "http://gpu-host:8000/v1"  # default http://localhost:8000/v1
api_key       = "sk-…"                     # optional
model_id      = "Qwen3-32B"                # default "default"
context_size  = 32768
priority      = 2
```

An unrecognised `type` only logs a warning — it does not fail startup,
so a typo silently drops that backend.

Client-side, `svrn chat --daemon <url>` points the CLI at any
OpenAI-compatible host with no server config at all. Aiming it at a bare
vLLM or SGLang server that doesn't speak OICP is a supported
degradation, not an error: the manifest fetch returns nothing and it
falls back to protocol defaults.

### Web search

Off by default. Backends: DuckDuckGo, Brave, Tavily.

---

## What we don't do, and why

Honest list, so you don't spend a day discovering it.

| Not supported | Status |
|---|---|
| **Agent Skills (`SKILL.md`)** | No reader. We *write* one file for our own dev loop, and it isn't even in the spec's format. If you want us to load skills, open an issue — this is the standard we're most likely to adopt next. |
| **OpenTelemetry / `gen_ai.*` spans** | Nothing. Not a dependency, not transitive. Deliberate: no telemetry leaves the machine. Everything observable is local files — crash bundles, per-turn answer reports, session frames, eval history — plus `GET /status`. A local-only OTLP exporter is a reasonable ask; a phone-home one is not. |
| **`/metrics` / Prometheus** | None. `GET /status` is the health surface, including resident model slots. |
| **A2A** | Not implemented. Peer-to-peer coordination goes over our own mesh, and a non-Sovereign node can't speak it yet. OICP is the path anything graduates through. |
| **MCP resources, prompts, stdio** | Tools only, HTTP only. |
| **MCP aggregation** | We consume external MCP servers and we serve our own; we don't proxy one through the other. |
| **`/v1/rerank`** | In-process only (§6). |
| **Native Anthropic / OpenAI provider APIs** | Not implemented — the remote path is OpenAI-*compatible* HTTP, aimed at self-hosted servers. |

---

## If your tool doesn't fit any of this

Say so. An issue describing what you're actually trying to connect is
the fastest way to get a surface promoted from internal to contract —
and the fastest way to get a standard onto the list above. See
[CONTRIBUTING.md](../CONTRIBUTING.md).
