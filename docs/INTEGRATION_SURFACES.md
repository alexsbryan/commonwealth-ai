# Integration surfaces

Which parts of this system are contracts you can build against, and
which are internals that happen to be visible. Read this before
writing an integration — it exists so you don't spend a day on a
surface that was never meant to hold your weight.

If you just want to point a tool you already run at a local daemon —
Claude Code, Codex, an Ollama client, an OpenAI SDK, your editor —
start with [INTEROP.md](./INTEROP.md) and come back here before you
build anything load-bearing.

## Build on these

**OpenAI-compatible inference API** — `:9741` serves
`POST /v1/chat/completions`, `POST /v1/responses` (the OpenAI
Responses dialect), `POST /v1/embeddings`, and `GET /v1/models`. Any
OpenAI-SDK client works. Non-loopback callers need a bearer token
(`[daemon] client_token`).

**Ollama-native shim** — the same port serves `/api/chat`,
`/api/generate`, `/api/tags`, and friends, so Ollama-native clients
(Open WebUI's Ollama mode, IDE plugins) connect unmodified. One
caveat in v1: `chat`/`generate` compute the full answer and frame it
as a single NDJSON turn — incremental streaming is a tracked
follow-up.

**MCP server** — `POST :9741/mcp` (JSON-RPC 2.0, Streamable HTTP,
with the legacy HTTP+SSE path at `/mcp/message`). Negotiates protocol
revisions 2025-06-18, 2025-03-26, and 2024-11-05, and accepts batch
requests. Tools only — no resources or prompts yet — and deliberately
loopback-only: it exposes local dev tooling, not a remote service.

**OICP** — `GET /oicp/v1/capabilities` plus the ingest extension. The
spec ([commonwealth/docs/oicp-v0.4.md](../commonwealth/docs/oicp-v0.4.md),
v0.3 as fallback) is CC0 — implement it freely on either side.
`commonwealth/crates/oicp-conformance` is a standalone certifier you
can lift wholesale to test your own implementation.

**Recipes** — the corpus-ingestion TOML format. The schema reference
([sovereign-recipes/SCHEMA.md](../sovereign-recipes/SCHEMA.md)) is
generated from the code and test-gated, and the loader keeps old
recipes working by convention (serde defaults, aliases, versioned
opt-ins). Start from
[sovereign-recipes/GETTING_STARTED.md](../sovereign-recipes/GETTING_STARTED.md).

**Corpus snapshots** — `.tar.zst` archives with a versioned manifest
(`_snapshot_manifest.json`); restore refuses on embedding-model
mismatch rather than corrupting silently. Prebuilt corpora ship the
same way via the recipe `[prebuilt]` block from Hugging Face.

**Mesh apps** — corpus-explorer web apps running in the desktop. The
contract is the `window.meshApp` bridge plus a `meshapp.json`
manifest; see
[sovereign/docs/MESHAPP_AUTHORING.md](../sovereign/docs/MESHAPP_AUTHORING.md)
and copy a shipped example.

**Ring rail** — shared, signed, append-only state belonging to a
group of people rather than to a host. `POST /v1/rail/append` and
`GET /v1/rail/log` on the rail listener (`:9743` by default), reached
with a grant scoped to exactly one namespace. The rail carries an
opaque JSON payload and never reads inside it; what it promises is
that every act it hands back was signed by a key the ring's roster
claims, that duplicates and equivocations are gone, that corrections
are applied and never resurrect, and that the order is identical on
every node. `log` returns its **gaps** alongside the acts — a total
shown without them is a confident number over a subset. Payloads are
canonical by construction (sorted keys, whole numbers only) because a
signature covers bytes: the route refuses a fraction rather than sign
something two nodes would spell differently. For a web app the
contract is `window.ring` — `log`, `record`, `correct`, and a `fold`
that walks the rail's order and skips voided acts. Start from
[HOUSE_EXPENSES.md](./HOUSE_EXPENSES.md); the reference is
[MESHAPP_AUTHORING.md](../sovereign/docs/MESHAPP_AUTHORING.md).
Three M0 limits, none of them permanent: the rail is mounted
loopback-only (no `Peer` or `Guest` surface), rosters are per-node
files rather than gossiped state, and there is no publish verb — `svrn
ring dev` serves the bundle, so each member runs the app from their own
copy of the folder.

**CLI scripting** — `svrn tools call <id> --format json` is the
stable machine-readable path (stdout stays clean; logs go to stderr).
Other commands print for humans unless they document a `--format
json` of their own — don't parse human output.

**Governance oplog** — `governance_oplog.jsonl` is a versioned,
internally-tagged, append-only act log, and the fold that derives current
law from it is pure (no IO, no inference). The format is stable in
practice; its location under `~/.svrnmesh/` is not yet a contract — see
the caveat in
[GOVERNANCE_INTEGRATION.md](./GOVERNANCE_INTEGRATION.md), which lays out
the full spectrum from adopting the stack to speaking only this file.

## Internal — no compatibility promise

**`:9742` and everything under `/internal/*`.** The mesh-internal
API is perimeter-trusted plaintext: no per-request auth, by design,
on the assumption it rides a private network or WireGuard/Tailscale
overlay (see [THREAT_MODEL.md](./THREAT_MODEL.md)). Its routes and
wire shapes change without notice.

**Gossip and join wire types.** Serde structs, not a published
protocol. A non-Sovereign node can't speak the mesh today; if you
want to build one, open an issue first — the OICP treatment (spec +
conformance) is the intended path for anything that graduates.

**The `~/.svrnmesh/` directory layout**, beyond what the snapshot
manifest documents.

## Experimental — here today, different tomorrow

**iroh encrypted transport** (`cwth/http/0` ALPNs) — feature-gated,
excluded from default builds.

**`[[inference.backends]]` in `sovereign-server.toml`** — the only
way to put Sovereign in front of an external OpenAI-compatible server
(vLLM, SGLang, llama.cpp, TGI, a LiteLLM proxy). Real and working, but
`sovereign-server` is build-from-source and not one of the three
shipped binaries, so treat the key names as unsettled. Documented in
[INTEROP.md](./INTEROP.md#9-going-the-other-way).

**The desktop command bridge on `:9745`** — a debug-build-only,
env-gated Playwright test harness. It must never ship enabled in a
release binary and is not an integration point, however much it
looks like one.

---

If the surface you need isn't here, say so — see
[CONTRIBUTING.md](../CONTRIBUTING.md). An issue describing what you're
trying to integrate is the fastest way to get a surface promoted from
internal to contract.
