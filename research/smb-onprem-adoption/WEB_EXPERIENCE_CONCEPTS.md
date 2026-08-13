# Web experience concepts — mesh-native, corpus-featuring

_2026-08-13 · RuggedFox · research status: concept exploration from code read, no runs_

Four concepts for an actual web-based experience that suits the mesh and
features the corpus-engine, rather than re-deriving what the desktop app
already does. Companion to `MULTI_TENANT_SMB_ADOPTION.md` (deployment)
and `FIFTEEN_MINUTE_COMMON_CASE.md` (the funnel). The honest constraint
these concepts are designed inside: there is **no web UI anywhere in the
platform today** — `svrn serve` is the code-intelligence MCP server
(`sovereign-cli/src/main.rs:487`), not a chat. Every concept below is a
claim on what to build first, and all four share the same substrate.

## 0. The shared substrate (inventory that makes all four cheap)

- **`@sovereign/chat-ui`** — a transport-agnostic Svelte 5 chat render
  surface (components + FSM + utils), already shared by desktop and
  mobile as *source* via Vite/tsconfig alias (`packages/chat-ui/package.json`).
  The desktop (`sovereign/crates/sovereign-desktop/`) is Tauri + Svelte
  5 with `App.svelte`/`screens.ts` and Playwright harnesses for demo,
  real, and fault modes. A web app consuming the same package inherits
  the chat surface and the FSM discipline; the harness pattern ports
  directly.
- **The three-tier shape already exists and generalizes.** The onprem
  kit is browser → nginx → `sovereign-server` → daemon loopback. The
  daemon's richer routes (knowledge search fan-out, landscape digest,
  watch status) are loopback-gated (`loopback_guard`), which is exactly
  why the kit put a reverse proxy in front — any web concept keeps that
  shape: the browser talks to the *server* (per-tenant bearer auth); the
  server proxies the daemon. Nothing new to invent, one posture to
  enforce.
- **Mesh hits already carry provenance.** Fan-out hits stash
  `metadata["peer_name"]`; local hits omit it
  (`sovereign-mesh/src/knowledge_client.rs:124-131`) — so "this answer
  came from a corpus on X" is already wire data, not a UI dream.
- **The daemon already serves assembled views to attached surfaces**
  (`POST /v1/knowledge/landscape_digest`, `landscape_digest_http.rs` —
  built so the desktop in attach mode does not reconstruct the
  knowledge-view itself). Precedent for "UI pulls a daemon-assembled
  digest over HTTP."

## Concept A — "Ask the Firm": the employee surface

The SMB common case made web-native. A rail of three, inherited from the
desktop's Ask · Library · Reflect coherence work, but minimal:

```
┌─ Ask ────────────────────────────────────────────┐
│  question                                        │
│  answer · citations · [Source: firm-docs, §12]   │
│  "(I can't find this in your documents)" —       │
│  designed, not an error                          │
├─ Library ────────────────────────────────────────┤
│  firm-docs   us-code   ·  + my documents         │
│  watch status: 3 files could not be read ▸       │
├─ My documents ───────────────────────────────────┤
│  drop files · they become a private corpus       │
└──────────────────────────────────────────────────┘
```

**Who:** every employee. This is the product the previous two docs
converge on.

**Mesh hooks (quiet, not the subject):** retrieval fans out to peers
per `query_sharing`; citations carry `peer_name` where served remotely —
the mesh appears in the provenance line, not as a map. Per-employee
tenancy rides the existing key→tenant machinery (`MULTI_TENANT_SMB_ADOPTION.md`
§3).

**Corpus-engine hooks:** the Library tab is the corpus-engine's first
browsable surface — installed corpora, watch status with *failures
surfaced* (`scanned_no_text` etc. — today that honesty lives in a CLI
command nobody runs), the `[Source: …]` chunk links.

**Cost:** Gap 1 (browser surface) + Gap 2 (client-bytes upload →
`Private { owner: tenant }`) from the multi-tenancy doc — the two
bounded gaps, nothing more. The rail must stay three panes; the desktop
already learned that lesson, the web app should inherit the decision,
not re-litigate it.

**Risk:** scope creep into "the desktop app, but in a browser." The
desktop remains the power surface (workshop, reflect); A is the
employee-grade common case. Cut anything that does not serve ask/library/
my-documents.

## Concept B — "The Mesh Console": the mesh as the subject

A web dashboard where the mesh is the product, not the plumbing. For the
operator, the demo, and the buyer's IT team.

```
┌─ Nodes ──────────────────────────────────────────┐
│  box-a (anchor)  35B resident · hosts firm-docs  │
│  box-b (consumer) 4B · asks across peers         │
│  shared-model cluster: forming (2/3 anchors)     │
├─ Custody ────────────────────────────────────────┤
│  firm-docs: query_sharing=yes · mesh_sharing=no  │
│  last hour: 41 peer-served answers · ledger ▸    │
└──────────────────────────────────────────────────┘
```

**Who:** the org's operator and the buyer. This is the concept that
*shows* the investment: "chunks moved; the corpus didn't" rendered as a
living picture.

**Mesh hooks (maximal):** node roles and resident models, `query_sharing`/
`mesh_sharing` flags per corpus as visible custody state, shared-model
cluster formation (the `quorum_anchors` "forming (k/N)" state,
`setup_config.rs`), peer-served attribution and the contribution ledger.

**Corpus-engine hooks:** corpora-as-inventory across nodes — which
corpus lives where, its snapshot provenance, what a peer hosts vs what
it only queries.

**Cost:** the largest new read-surface of the four. Mesh status exists
as CLI output, not as an aggregate HTTP view; `landscape_digest_http.rs`
is the precedent to extend (a daemon-assembled digest over loopback,
proxied by the server). Do not build this by teaching a browser to talk
to N daemons — one daemon assembles, the server proxies.

**Risk:** operator-grade niche — valuable for the demo and the pilot,
but it is not what employees open. Build it second, keep it read-only
to start (every mutation in the mesh has a CLI verb that already works).

## Concept C — "The Evidence Pane": trust as the design language

Not a standalone app: a rendering stance every answer surface (A, B, and
the desktop) adopts. The answer is always shown with its epistemic
verdict and its evidence, and the abstention is the designed moment:

```
answer paragraph
─────────────────────────────────────────
verdict: sourced · gate τ≥0.9 · from 3 chunks
▸ firm-docs §12.4 (served by box-b)
▸ firm-docs §12.7
▸ my-documents §2

(or)

I can't find this in your documents.
— what I searched · why I'm not guessing
```

**Who:** everyone who asks, and the buyer deciding whether to trust it.
This is the kit's thesis made visible — the refusal IS the product
(`deploy/onprem/README.md` step 6).

**Mesh hooks:** the provenance line per citation — corpus, chunk,
`peer_name` where served remotely. "Served by box-b" is the mesh's
contribution, visible in every answer without a dashboard.

**Corpus-engine hooks:** chunk-level citation links (the kit already
allowlists a chunk-read route for exactly this), watch-status honesty,
the verdict language (`cannot_know_from_here` vs `gk_rescue`) projected
as plain words.

**Cost:** mostly presentation over existing wire data — with one real
gap: the raw `grounding_gate.action` is *not* projected onto the wire
today (`PLAN.md` workstream 4, check 4's first trap), so the server
must project the gate verdict; `epistemic_state` is the structured
handle that already exists. Small server projection + a shared
citation component in `chat-ui` (built once, used by desktop, mobile,
and web).

**Risk:** exposing internals (scores, τ, gate actions) invites the
customer to read the wrong numbers. The pane should show verdict +
evidence, not a telemetry dump; the scores belong in tracing, not the
UI. This is the concept most likely to be built wrong, and it is also
the most differentiated — the one no off-the-shelf RAG product has.

## Concept D — "The Corpus Library": corpus-engine as a product

A web-native workshop for the corpus itself: catalog browsing with
one-click install, watch registration, snapshot provenance, enrichment
runs, and — the engine's signature honesty — failures surfaced, never
silent.

```
┌─ Catalog ────────────────────────────────────────┐
│  us-code · sep · gutenberg …        [install]   │
│  recipes: what's in it · license · embed model   │
├─ This box ───────────────────────────────────────┤
│  firm-docs  sweep ok · 3 failures ▸ (why, named) │
│  snapshot provenance: built 2026-08-10, Qwen3-E  │
└──────────────────────────────────────────────────┘
```

**Who:** the power user and the operator — the person who today lives in
`svrn corpus …` verbs.

**Corpus-engine hooks (maximal):** the catalog (`svrn corpus list`),
recipes as browsable data, watch registration and per-corpus status,
snapshot publish/restore between nodes, embed-model provenance (the
dimension-mismatch hard error is *good*; the UI should say so).

**Mesh hooks:** snapshot publish/restore and canonical pull as visible
actions with their `mesh_sharing`/`query_sharing` gates named — the
custody story *before* the bytes move, not after.

**Cost:** the catalog/recipe data is CLI-only today; it needs an HTTP
surface. Watch status exists daemon-side (loopback). Snapshot
publish/restore is CLI + a filesystem archive. The largest new backend
surface of the four.

**Risk:** becoming a build-your-own-corpus IDE. The value is
browsing + status + provenance, not a recipe editor; recipe authoring
stays where it is (TOML files) until someone demands otherwise.

## Synthesis

The four are not competitors; they compose into one build order, and
C is the design stance the others adopt:

1. **A with C baked in** — the employee surface, with the evidence pane
   as its rendering stance. Rides the two bounded gaps from the
   multi-tenancy doc; inherits `chat-ui`, the FSM, the harness pattern,
   and the rail decision from the desktop. This is the 15-minute funnel's
   destination.
2. **D deepens A's Library tab** — the corpus-engine gets its browsable
   surface where the employee product already puts it; catalog/status/
   provenance surfaces land there before anywhere else.
3. **B when the mesh must be shown** — the demo-grade and buyer-grade
   console. It exists to make the investment legible, not to be the
   product; read-only first.
4. The desktop keeps the power surface (Workshop/Reflect) and the
   single-machine experience. The web app does not re-implement the
   desktop; it re-uses its packages and its decisions, per the
   re-parent-don't-rewrite rule that the desktop UX arc already earned.

One posture governs all four: browser → `sovereign-server` (per-tenant
bearer) → daemon loopback. No browser ever talks to `:9741`/`:9742`
directly, no mesh route becomes an unauthenticated web route, and the
three-tier shape the kit proved generalizes unchanged.
