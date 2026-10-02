<!-- Seat, 2026-10-01: a DRAFT for pb-distribution-ship-gate's release note (ralph/PHASE_B_RELEASE_NOTE.md), written by a read-only census of 18f783f44..30aa81286. Verify each item at C before it goes in. Seat triage of its Findings: F4 (retired verbs), F5 (CI release list), F6 (guest door seal), F7 (on-prem upgrade unit) are pre-gate fix rows (phase-b-96). `svrn ring checkpoint` does NOT 404: ring_cmd/mod.rs:339 dials rails_base() and cw-rails mounts /internal/ring/checkpoint/{ns} (ring_routes.rs:60). `install.sh --force-config` regenerating keys is documented (install.sh:80), not a defect. The rest are filed to phase-c (phase-b-96). -->

# Phase B: what changes for users and operators (draft)

Range: main at `18f783f44` to `cut` at `820c4e051` (1,635 commits). Each item cites the commit that
made it, and file:line references are at `cut`. This draft answers the ship gate's "one release note
listing every user-visible change" (ralph/PHASE_B_SHIP_GATE.md). It was read from the tree and the
log; nothing was run. A claim taken from a commit body without tracing it in code is marked as such.

Phase B splits the node into programs (docs/FIVE_PROGRAMS.md §2): `svrn`, the knowledge daemon on
:9741; `serve`, the model server on :9748; cw-rails, the mesh endpoint on :9747; and `svrn ingest`,
`svrn code` and `svrn bench`. A stock install still runs as one process. The mesh now belongs to
cw-rails, and nothing starts cw-rails unless you ask it to.

## Upgrading an existing node

1. Reinstall with `curl … | sh` or `svrn update`. A release now ships 12 binaries instead of 3
   (4a0809a6c). A release with only the old three cannot start the daemon (Findings 11).
2. Restart the daemon with `svrn daemon stop && svrn daemon start`. On first boot it moves svrn's
   own rows from `notes.db` to `sovereign.db`, after writing `notes.db.pre-pb-notes-memory`
   (ea8850079, 9df4750c4).
3. On a mesh node, run `svrn mesh up` once. The node is off the mesh until you do. The command moves
   the node key, the mesh membership, the ring history and several config sections to cw-rails. It
   then starts cw-rails and, on Linux, enables a `cw-rails.service` user unit (272c14999, 7d9a28436,
   1c120f23d). The operator's own cutover is recorded in b71fdd08b.
4. If `config.toml` sets `[discovery] join_key`, remove it and `seed_addrs`, then join with
   `svrn mesh join <invite>`. The daemon refuses to boot while `join_key` is set (1c120f23d;
   sovereign-daemon daemon_cmd/start.rs:23-34).
5. On-prem: re-run the kit's `install.sh`. One unit replaces two, and it issues new API keys; the
   old `[auth.keys]` keys do not carry over (0be7666a6).

## 1. Install, binaries and services

A release install ships these binaries (landing/install.sh, scripts/release-cli-local.sh;
4a0809a6c):
- `sovereign-cli`, run as `svrn`, with the transitional `sovereign` alias;
- `sovereign-cli-daemon`, `sovereign-cli-llm`, `sovereign-cli-dev`, `sovereign-cli-mesh`,
  `sovereign-cli-bench`;
- `sovereign-stock`, `sovereign-cli-llm-stock`, `sovereign-serve`;
- `svrn-ingest`, `sovereign-pod-worker`, `cw-rails`.

`sovereign-agent-bench` is dev-only and does not ship. An xtask test,
`release_lists_carry_every_exec_d_binary` (a125568a0), fails if any binary a verb execs is missing
from either list.

New binaries:
- `sovereign-stock` is the stock distribution: svrn with serve hosted in the same process.
  `svrn daemon run` execs it (fbe73a5fb). Its second binary, `sovereign-cli-llm-stock`, is what the
  dispatcher execs for LLM verbs (c3432453a).
- `sovereign-serve` is the model server. Standalone, it binds 127.0.0.1:9748 (d66686a89).
  `svrn mesh warm-cache|fetch-model|fetch-ner|plan|bench` and `svrn daemon vram-plan` exec it
  (c2529c94c, 8aa8d2c5d, 4e9e3e647, c04d4aa2f).
- `svrn-ingest` (ingest's CLI; 2f14fefff, 2587be58d), `sovereign-cli-mesh` (mesh, ring, job;
  e20d2edd2) and `sovereign-cli-bench` (bench, eval, quality lane; 3c44858e7).
- `sovereign-pod-worker` is what `svrn daemon run --worker-mode` execs. The pod contract is
  unchanged (f1549e884).
- `sovereign-onprem` is the on-prem distribution. It runs svrn, serve and ingest in one process,
  with no code program and no mesh. It withholds svrn's web reach, the wikipedia bundle and `/mcp`,
  and names each refusal (eee281aaa). The kit's unit runs it as `sovereign-onprem run --config …`.
- cw-rails (crate commonwealth-rails) already existed. It now holds the node key and is the node's
  only mesh endpoint (1c120f23d). It gains `found` and a solo `run` (5c2aade38, 0a7dc126d,
  55546ac07).

Removed:
- `sovereign-server`, the tenant HTTP server that also hosted on-prem and mobile (5cb09f22b). The
  daemon now does its job (section 6, keyed daemon).
- The mobile host. `sovereign-mobile` and `sovereign-studio` were deleted (5cb09f22b), then restored
  by operator direction (1427d414c). What stays gone is the host. `svrn mobile` and the desktop's
  Mobile access toggle now say "the mobile host was the sovereign-server binary, which was deleted;
  no mobile host ships" (4c1f684fa, af57170c8).
- The ATOS feature layer: sovereign-atos, corpus-engine-atos, its CLI and its tools, about 27.9k
  lines (2ea67a59f).

Services and processes:
- `svrn daemon run` execs `sovereign-stock`, so the stock install is one process. If the binary is
  missing, it exits 127 and names it. `SOVEREIGN_DAEMON_BIN` still overrides the path (fbe73a5fb;
  sovereign-cli-daemon daemon_bin.rs:19-23).
- svrn starts no other process. Boot no longer starts cw-rails (45c89832b) or a separate serve
  (a378380fc). A bare `sovereign-daemon` with no serve to reach refuses to boot, naming the absence.
  `svrn daemon stop` and `restart` stop the daemon only.
- Only `svrn mesh up` starts cw-rails. It writes and enables
  `~/.config/systemd/user/cw-rails.service`; `svrn install-service` does not. Off Linux there is no
  unit, so after a reboot the node is off the mesh until `svrn mesh up` runs again. `svrn doctor`
  gains `rails_boot_unit`, which warns "off the mesh after a reboot" (7d9a28436).
- The packaged desktop now carries `sovereign-stock` beside its `sovereign-cli-daemon` sidecar.
  Before, a packaged app had nothing for `daemon run` to exec (d1f3e1765, 917166da1).
- The container images (Containerfile, Containerfile.cuda) now also copy `sovereign-cli-daemon`,
  `sovereign-stock` and `sovereign-pod-worker` (f7bd83fb4).
- On-prem kit (sovereign/deploy/onprem; 0be7666a6, e3a2bd6f5, 46226ec78):
  - One unit, `firm-rag.service`. `server-config.toml` and `firm-rag-server.service` are retired,
    and install removes the old server unit.
  - `package.sh` builds `sovereign-onprem` (with OCR), `svrn`, `sovereign-cli-daemon` and
    `svrn-ingest`.
  - nginx proxies the daemon on :9741 instead of :8080. Its allowlist is pinned to the daemon's key
    scope, and the body limit drops from 64m to 1m.
  - Clients lose `/v1/tasks/{id}/approve`, `/v1/search` and `/v1/documents/{id}/state`. They gain
    `/v1/conversations/search`, `/v1/documents/{id}/progress` and the document ask poll.
  - `install.sh --port n` puts the client API on n, internal on n+1, rails on n+6 and serve on n+7.
  - The kit's nginx config had not loaded since 2026-08-03, because of a duplicate
    `proxy_http_version`; 30aa81286 fixes it.

## 2. CLI verbs

The dispatcher does not parse verbs with clap. An unknown verb prints the full `svrn` help to stderr
and exits 1 (sovereign-cli main.rs:1224-1225). In a release build, which has no dev-tools, a verb in
`DEV_VERBS` is refused with a "developer toolchain" message and exit 2 (main.rs:845-858).

### Added
- `svrn ingest <recipe.toml>` builds a corpus against any OpenAI-compatible endpoint. It takes
  `--base-url`, or `--chat-url` with `--embed-url`, plus `--no-enrich`, and execs `svrn-ingest`
  (2f14fefff).
- `svrn mesh up`; see section 1 (272c14999, 7d9a28436).
- `svrn mesh pod up|pool|list|down`, renamed from `svrn pipeline pod` (47a8392d8, 29a38a9fd).
- `svrn mesh fetch-ner [<model_id>]` fetches serve's GLiNER model (8aa8d2c5d).
- `svrn daemon key --add <sub> [--group G]… | --revoke <sub> | --list`. It writes the key store with
  no daemon running, and the daemon reads keys at start (a456354e7).
- `svrn code mcp` is the code program's MCP server. `svrn serve` and `svrn project serve` now run
  the same handler (2c134a1d3).
- `cw-rails found <mesh-name>` and `cw-rails run [--listen] [--local-only] [--mdns]` (5c2aade38,
  0a7dc126d, 5a7af2e6e).

### Removed, and what `cut` prints

| typed | cut prints | exit | commit |
|---|---|---|---|
| `svrn atos …` (provision, next, start/end-milestone, archive, status, promote, diff, run-ab, probe-driver, report, teardown, feature approve, spec diff/accept, doctor, install-plugin) | full `svrn` usage, no retirement text. Main's release build refused it as a dev verb | 1 | 2ea67a59f |
| `svrn design` | full `svrn` usage, no retirement text | 1 | bdd22b846 |
| `svrn project design`, `project plan` | dev build: "Unknown project subcommand: design" plus help. Release build: "svrn project design: not available in this build." plus the available list | 1 / 2 | 2ea67a59f |
| `svrn drift accept <id>`, `svrn drift <feature-id>` | dev build: "svrn drift requires a subcommand." plus `drift detect` usage. Release build: the dev-toolchain refusal, as at main | 2 | 2ea67a59f |
| `svrn plan` (compose) | "note: `svrn plan` has been retired." and points to `svrn plan validate <path>`. Release build: dev-toolchain refusal, as at main | 0 | bdd22b846 |
| `svrn mobile serve\|status\|pair` | "svrn mobile: the mobile host was the sovereign-server binary, which was deleted; no mobile host ships". `status` and `pair` no longer write `~/.svrnmesh/mobile-host.toml` | 1 | 4c1f684fa |
| `svrn pipeline pod <x>` | "svrn pipeline pod: moved to `svrn mesh pod`. Run `svrn mesh pod <x>`." | 2 | 93f66f8b4 |
| `svrn corpus extract-entities --download-model` | points to `svrn mesh fetch-ner` | 2 | c4f8726e4 |
| `svrn milestone <feature-id> <N>` | "sovereign milestone requires --project." plus usage | 2 | 2ea67a59f |
| `svrn notes promote` | "Unknown flag: promote" plus help | 1 | 2ea67a59f |
| `svrn amend design` | silently runs the charter amend flow | – | 2ea67a59f |
| `svrn audit <feature-id> [--archive]` | silently runs the project-wide audit | – | 2ea67a59f |
| `svrn project audit` "Share your recipe" footer | no longer printed | – | 933190f79 |

### Moved to another binary (same spelling)
- `mesh`, `ring`, `job`, `publish`, `unpublish` and `run` go to sovereign-cli-mesh (e20d2edd2).
- `corpus`, `enrich`, `atlas`, `meta-atlas`, `recipe`, `pipeline` and `alignment` go to svrn-ingest.
  The sub-verbs in ingest_bin.rs `SVRN_SIDE` stay with the LLM sibling (2587be58d).
- `bench`, `eval` and `quality lane` go to sovereign-cli-bench (3c44858e7).
- These exec sovereign-cli-dev: `notes`, `reflect`, `rough-edges`, `git-archaeology`,
  `archaeology-eval`, `refresh`, `claim`, `solve`, `backlog` and every `code` subcommand (9ec9a2f85,
  ea5d479c5, cc3c271cc, ce3dfc421). A release build now runs `code` subcommands where it used to
  refuse them (ce3dfc421).

### Behaviour changed on surviving verbs
- `svrn daemon run|start` runs one process with serve inside, and `svrn daemon status` prints a
  `serving:` line (fbe73a5fb, a378380fc).
- `svrn setup` probes and plans through `sovereign-stock --setup-probe`. It needs that binary and
  refuses by name without it. An unknown `--quant` is now refused by the loader rather than at
  argument parsing (0a72b04d0, e2d67fe37). `setup --terminal <invite>` brings cw-rails up before
  joining (a1aaa0463, 1c120f23d).
- `svrn mesh create` and `svrn mesh join` bring cw-rails up first.
  - `create` founds an encrypted mesh, and a plaintext mesh is refused by name (from 1c120f23d's
    body and phase-b-36).
  - Invites cw-rails mints expire after 24 h; `svrn mesh rotate` renews them (110dfde2c).
  - `svrn ring roster add --self` reads cw-rails' key.
- `svrn serve`, `svrn project serve` and `svrn code mcp` default to the daemon's :9741. Whichever of
  the two binds second refuses by name ("error: bind code MCP on 127.0.0.1:<port> failed
  …", exit 1). The daemon no longer kills a standalone serve (e927c1e36).
- `svrn init` and `svrn project init` index through `sovereign-cli-dev code index --fts-only`, which
  execs svrn-ingest. You see those tools' output lines instead of a progress bar (5a63a3fe9,
  666290f75).
- `svrn chat`, `router fit`, `router-cache rebuild` and `corpus extract-entities` load no model in
  the CLI. Embedding, rerank and NER go to serve (c4f8726e4).
  - `chat` prints "Reranker: none — …" or "NER: none — …" when serve has none.
  - `router fit` needs a serve that holds the model.
  - `extract-entities` refuses a `--model`, `--threshold` or `--labels` that differs from serve's.
- `svrn portfolio` and `svrn newsworthy` go through cw-rails. Each migrates its legacy SQLite file
  once. An unreachable store is now an error rather than "no portfolio named X" (3f57442a9).
- `svrn quality check --distribute` submits through cw-rails' work doors (b77fd209e).
  `svrn mesh pod up|down` record the pinned-pod snapshot through serve, in the same file and schema
  (5c06be1a8).
- When cw-rails is down, every surface that uses it (rings, KV, work atlas, `declare_scope`,
  portfolio) says "cannot reach the mesh's rails daemon at …; bring it up with `svrn mesh up`"
  (17caa1c4f).
- `svrn notes` and `svrn reflect` read only `<data root>/notes.db`, or the store named by
  `--data-dir`. The per-repo `.sovereign/notes.db`, the cwd walk and the `active_notes_db` pointer
  are gone (9e2eb17de).

## 3. Environment variables

quality/env-flags.toml has 220 rows at both ends: 11 removed, 11 added.

Removed. Each one gated code that was deleted with it. Setting one now does nothing, and nothing
warns:
- `SOVEREIGN_QUERY_DECOMP`, `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND`, `SOVEREIGN_TITLE_EXPAND`,
  `SOVEREIGN_META_BRIDGE`, `SOVEREIGN_DEMAND_PLAN`, `SOVEREIGN_DEMAND_PLAN_FANOUT` and
  `SOVEREIGN_DECOMP_DECAY`: five dark retrieval steps, all default-off experiments (ac032e5bc).
- `SOVEREIGN_AGENTIC_KQ` and `SOVEREIGN_AGENTIC_KQ_THRESHOLD` (experiments, off),
  `SOVEREIGN_CONV_PPR_WEIGHT` (deprecated) and `SOVEREIGN_FRONTDOOR` (deprecated alias for
  `SOVEREIGN_HARNESS=opencode`) (cc78b933b).

Added, all with status `shipped`:
- `SOVEREIGN_SERVE_PORT`: serve's loopback port, default 9748 (51c21aa16).
- Binary paths: `SOVEREIGN_SERVE_BIN` (c2529c94c), `CW_RAILS_BIN` (ef0c1ed3d),
  `SOVEREIGN_POD_WORKER_BIN` (f1549e884), `SOVEREIGN_INGEST_BIN` (2f14fefff),
  `SOVEREIGN_CLI_BENCH_BIN` (3c44858e7), `SOVEREIGN_CLI_MESH_BIN` and `SOVEREIGN_AGENT_BENCH_BIN`
  (f92667f5e).
- Developer paths set by `.cargo/config.toml`: `SOVEREIGN_BENCH_ROOT` (7079485f6),
  `SOVEREIGN_WORKSPACE_ROOT` (e20d2edd2) and `SOVEREIGN_CLI_CONTRACT` (84ed1e1dc).

Changed meaning:
- `SOVEREIGN_CLI_LLM_BIN` now names `sovereign-cli-llm-stock`. An override still pointing at a
  `sovereign-cli-llm` path execs the wrong binary (c3432453a).
- `SOVEREIGN_RERANK_MODEL_PATH` is read only when serve starts (c4f8726e4).
- `SOVEREIGN_DAEMON_BIN`, which is unregistered, now defaults to `sovereign-stock` (fbe73a5fb).

The SOVEREIGN_* ↔ SVRNMESH_* bridge is unchanged. The two prefixes are mirrored with a one-line
deprecation notice. The bridge runs in the dispatcher, the CLI siblings, the desktop and the
daemon's process entry (sovereign-daemon process.rs:103). It does not run in a directly launched
`sovereign-serve`, `cw-rails` or `sovereign-pod-worker` (Findings 10).

## 4. Config, on-disk state and ports

`config.toml` (SetupConfig) ignores unknown keys, and a save drops them.
- Added `[retrieval] corpora`. On a keyed daemon it is every key's corpus grant, and an empty list
  grants nothing (c79a991ca).
- Added `[models.kinds]` (kind → GGUF path). `[[compute.slot]] role = "rerank"` is now accepted
  (f8d12bd69).
- Added `[daemon] rails_base`, default `http://127.0.0.1:9747` (bc576f8cd).
- `[node] entry`, when set, is also serve's base URL (d39687f09).
- `[discovery] join_key` is refused at boot, by name (1c120f23d).
- `svrn mesh up` moves these keys to cw-rails' `rails.toml`. Until it runs, the daemon ignores them:
  - `[compute.work_offer]`, backed up to `config.toml.bak`. A donor node stops donating until then
    (c9b6c61a2).
  - `[iroh] relay_urls`, `discovery`, `media_origin` and `media_allow`, backed up to
    `config.toml.iroh.bak` (1c120f23d).
  - `[iroh] media_viewer_user`, with no backup (24c5bf5b9).
- `svrn mesh up` warns that `[iroh] enabled` and `transport` have no reader, and leaves them in
  place.

On disk:
- cw-rails' root is `~/.commonwealth-rails`, or `CW_RAILS_DIR`. It holds `rails.toml`, `rails.lock`,
  `node_key`, `node_id`, the mesh and `rings/`.
- `svrn mesh up` copies the daemon's `node_key` there and renames the original
  `node_key.handed-over`. It keeps cw-rails' prior files as `*.pre-handover` and moves the ring
  journals from `~/.svrnmesh/rings`.
- If cw-rails is already running, the handover is deferred with "stop cw-rails and run
  `svrn mesh up` again" (1c120f23d; sovereign-cli-mesh identity_handover.rs).
- `notes.db` → `sovereign.db` (`memory_notes`). At daemon boot, svrn's lessons, tool-decision
  dossier and session todos and commitments move after a `VACUUM INTO` backup. A failure leaves
  every row where it was and retries at the next boot (ea8850079, 9df4750c4, 3210f56d9). After the
  move, `notes.db` belongs to the code program alone.

Ports:
- Unchanged: 9741 (svrn client API), 9742 (internal), 9743 (rail), 9744 (guest door) and 9745
  (desktop bridge).
- 9747 is cw-rails, which the daemon now dials (bc576f8cd).
- 9748 is new: serve (d66686a89).
- 8080 is gone. It was on-prem's sovereign-server upstream and the desktop's mobile host (0be7666a6,
  4c1f684fa).

## 5. HTTP and MCP surfaces

Unchanged on :9741 (sovereign-daemon server.rs:139-256):
- chat, responses, completions, embeddings, models, knowledge search;
- `/status`, `/oicp/v1/*`, the Ollama shim `/api/*` and `/v1/rail/{append,log,live}`;
- the turn, document, atlas and notes routers.

Added on :9741:
- `GET /health`, exempt from auth (46226ec78).
- `GET /v1/corpora`, `GET /v1/corpora/{corpus}/chunks/{chunk_id}` and `GET /v1/tools`, for a
  loopback caller or an admitted key (46226ec78).
- `POST /v1/rerank` and `POST /v1/ner`, forwarded to serve. Without serve they answer a named 503
  (3cf38cd16, f58e119b8).
- `GET /v1/mesh/venues` (85b6088a5).

Moved, with a named answer left at the old path:
- 20 `/v1/mesh/*` paths now answer 410 with
  `{"error": "… is served by cw-rails … call <rails_base><path>", "moved_to": …}`. They are status,
  create, join, join/preview, rotate, switch, forget, leave, forget-member, relay-candidates, media,
  app, offers, fanout, media/fanout, the three publish forms, kv/entry and kv/entries (1c120f23d;
  mesh_http.rs:29-50).
- `/v1/projects*`, `/v1/solve/jobs*` and `/v1/edit_predictions` now belong to the code program. The
  stock binary still serves them on the same port. A bare `sovereign-daemon` or the on-prem binary
  answers 503 naming `svrn code mcp` (c25b16fb7, e96a3a83f, 3825c6342).
- `/v1/knowledge/landscape_digest` answers 503 when no ingest is composed (8aeadb016).
- On-prem: `/mcp`, `/mcp/message` and `/mcp/stats` answer 503 "this distribution does not serve MCP"
  (3b6ad7b7e).

Removed, with a bare 404:
- `/v1/apps*` and `/app/{id}/*`. The proxy behind them always answered 503, and no in-repo client
  used them (ae2bf7ddc, 6aaab8009).
- `/v1/mesh/measurements`. Measurements now travel on a cw-rails rail namespace (3ec99625f).

Internal port :9742:
- It binds 127.0.0.1 only. `[daemon] internal_bind` is logged but not bound (1c120f23d).
- `/internal/gossip`, `/join`, `/ring/*`, `/v1/models/*` and `/rpc-warm` moved to cw-rails or serve
  (1c120f23d, d30c17f1b, 7bb3fd1ae, f9b6325cd).
- Peers reach nine registered internal prefixes through cw-rails. Any other path gets cw-rails' 404
  "no origin is registered for <path>" (c46124cd5; peer_origin.rs:49-59).
- The peer listener is gone. A member that dials a node's client ALPN reaches serve's member face.
  That face serves only chat/completions, embeddings, completions, models and
  `/oicp/v1/capabilities` (1c120f23d, c0e2e5265).

serve (new, 127.0.0.1:9748) serves the OpenAI routes, `/oicp/v1/capabilities`, `/v1/rerank`,
`/v1/ner`, `/v1/admin/{hardware,setup/catalog,setup/slot}`, `/v1/engine/{state,self,reload}`,
model-file and asset transfer, and loopback-only internal routes (e47823aac, d3f044abc, 70973a6c5,
3f9749490). Every `/internal/*` route on a standalone serve refuses a non-loopback caller
(4bf702b69; ship gate F3).

cw-rails (127.0.0.1:9747; loopback is its only auth) gains the membership routes
`/v1/mesh/{create,join,join/preview,rotate,leave,switch,forget,forget-member}`, plus
`/v1/mesh/{reach,origins,offers,relay-candidates,kv/*}`, `/v1/rail/*`, `/v1/work/*` and
`/v1/ledger/*` (26e82ea3a, ea90afb11, f09642bae, 9f081beb1, b8112b780, 036c48339, 909e1f219). A join
cw-rails cannot save is refused by name and rolled back (52bb2daf5; ship gate F2).

MCP:
- The exposed tool ids are unchanged, and there is still one `/mcp` on :9741.
- The registration is split: svrn registers 19 tools, and the code program registers 17 plus `spec`
  and `drift` (162cbaece, c25b16fb7). The stock install lists all of them.
- The ATOS tools (`project_context`, `atos_verify`, `record_atos_event`, …) were deleted
  (2ea67a59f). None was MCP-exposed at main, so MCP clients see no change.

## 6. Behaviour a user will notice

- **Keyed daemon (on-prem).** This applies to a daemon holding any API key
  (`<data>/client-tokens/<sub>.key`, written by `svrn daemon key`).
  - Every caller is identified by key. A key resolves to `Principal::Asserted { sub, groups }`
    (ad17735cf, ad84995ab).
  - Loopback grants nothing: a request with no key gets 401, even from 127.0.0.1.
  - A key outside `admin` reaches only its own conversations, document reads and document ask.
    Anything else gets 403, naming the key and the group.
  - Conversations are scoped `{sub}:{id}`, and retrieval stays inside `[retrieval] corpora`.
  - A daemon with no keys behaves as before (34ba2051c).
  - The CLI sends no key, so CLI verbs against a keyed daemon get 401 (0be7666a6 body).
- **On-prem acceptance.** Through nginx at 30aa81286, 57 of 58 checks passed. Check 4 (a
  partial-decline verdict) failed; the operator closed it by ruling phase-b-95 (ship gate,
  Readings).
- **Mesh.** cw-rails, not the daemon, holds the endpoint and the key (1c120f23d). The IP overlay and
  plaintext joins are gone (phase-b-36, -37). Splitting a large model across machines over ggml RPC
  still works, through cw-rails' rpc_tensor bridge; the serve-mesh lift proved it twice after the
  switch (1c120f23d body).
- **Single-turn prompts.** A request carrying one user turn (plus system turns) now reaches the
  model verbatim. It used to be relabelled "User: …\n\nAssistant:" inside the chat template
  (51b547655).
- **Rerank.** When serve holds a rerank model, daemon turns now use it. A default install holds
  none, so its turns are unchanged (03e7657a9).
- **Logging.** A host `RUST_LOG` now adds to the daemon's tracing allowlist instead of replacing it
  (e093024d9).
- **Code tools.** The code program now serves `symbols`, `callers` and the rest, and the stock
  install mounts them on the same `:9741/mcp`, so harness configs do not change. A bare
  `sovereign-daemon` answers a code-tool call with -32601 naming `svrn code mcp` (c25b16fb7,
  0c336f211).
- **Windows.** The data-root lock is now enforced (6d76bea1c).

## Findings

Removed verbs with no replacement or retirement message (the Tier 1 bar):
1. `svrn atos …` prints full usage and exits 1 in every build. At main, a release build at least
   named it as a developer verb. Its forwarding banners (`svrn atos status` → `svrn status`, …) are
   gone too (2ea67a59f; "atos" is in no verb list at cut).
2. `svrn design` prints full usage and exits 1 in every build (bdd22b846).
3. `svrn project design|plan` answers "Unknown project subcommand" in a dev build and "not available
   in this build" in a release build. The second wording implies some other build has it
   (project_registry.rs:110).
4. `svrn drift accept` answers "svrn drift requires a subcommand", exit 2, with no word that
   `accept` was retired. The dev-tools help still reads "Architectural-drift detection + spec
   accept" (sovereign-cli main.rs:429).
5. Silent substitutions:
   - `svrn amend design` runs the charter amend (charter_amend.rs:215 ignores the positional).
   - `svrn audit <feature-id>` runs the project-wide rollup (audit_cmd.rs:31-32).

   `svrn notes promote` and `svrn milestone <feature-id> <N>` give generic usage errors.
6. sovereign/docs/cli-contract.toml still declares `mobile serve|status|pair`. Several of its
   `binary` labels are stale, and so is the verb → binary table in AGENTS.md.

Env vars:
7. All 11 removed vars are ignored without a warning, yet these files still tell users to set some
   of them: sovereign/docs/inference.md:214 (`SOVEREIGN_FRONTDOOR`), GROUNDING_GATE_ENV.md:52 and
   :99 (`SOVEREIGN_AGENTIC_KQ`), DEFAULTS_LEDGER.md:2494 (`SOVEREIGN_CONV_PPR_WEIGHT`),
   scripts/overnight-batch.sh:138-145 and :263-264 (whose two arms now run the same), and
   bench/enron/DEMO_RUNBOOK.md:56, :245 and qa_demo.toml:11 (`SOVEREIGN_TITLE_EXPAND`,
   `SOVEREIGN_DECOMP_DECAY`).
8. Four registry rows that docs/ENV_FLAGS.md shows as working have no reader at cut:
   `SOVEREIGN_BIND` and `SOVEREIGN_DB_PATH` (their only reader was sovereign-server),
   `SOVEREIGN_SERVER_PATH` and `SOVEREIGN_SUFFICIENCY_CHUNKS`.
9. `CW_RAILS_DIR` appears in cw-rails' `--help` but is not registered. `SOVEREIGN_DAEMON_BIN` is not
   registered either.
10. A directly launched `sovereign-serve`, `cw-rails` or `sovereign-pod-worker` does not run the
    SVRNMESH_ bridge, so ENV_FLAGS.md's "both spellings work" is false there. With only
    `SVRNMESH_SERVE_PORT` set, the daemon's dial target moves but serve's listener does not (read
    from code).

Unannounced or easy-to-miss breaks:
11. `.github/workflows/cli-release.yml:285,295` still builds and packages only `sovereign-cli`,
    `-daemon` and `-llm`. A CI-built release could neither `daemon run` nor `mesh up`. 4a0809a6c's
    body names this gap and leaves it open.
12. A mesh node restarted on cut without `svrn mesh up` is off the mesh, and its
    `[compute.work_offer]` donor stops with no message. `svrn daemon reload` still lists
    `iroh.enabled`, `transport`, `media_origin` and `media_allow` as restart-required, though the
    daemon reads none of them (admin_http.rs:333-352).
13. Backup gaps:
    - The `media_viewer_user` move rewrites `config.toml` without a backup.
    - `config.toml.bak` is overwritten on each run.
    - On-prem `install.sh --force-config` runs `rm -rf "$DATA/client-tokens"` (install.sh:375),
      deleting every issued key.
14. There is no automated rollback for the notes move, the identity handover or
    `rails.toml [work_offer]`. Main's cw-rails parses rails.toml with `deny_unknown_fields`, so it
    would refuse the migrated file (inferred).
15. Repo-local `.sovereign/notes.db` stores are no longer found without `--data-dir`, and they are
    not migrated (9e2eb17de).
16. On-prem upgrade: install.sh removes main's `firm-rag-server.service` but not main's
    `firm-rag-daemon.service` (install.sh:283-287). The new `firm-rag.service` and the old daemon
    unit would contend for :9741 and `daemon.lock` (inferred, not run).
17. Docs:
    - CLI_REFERENCE.md has no entry for `svrn mesh up`, `svrn daemon key` or `svrn code mcp`, and
      still calls `mesh create` "Promote the solo mesh".
    - RUNBOOK.md §2 says to keep cw-rails across reboots with a unit of your own, but `svrn mesh up`
      has installed one since 7d9a28436.
    - container/entrypoint-tailscale.sh now runs `sovereign-cli mesh join`, but no image ships
      `sovereign-cli-mesh` or `cw-rails`. That path is already marked superseded, so the impact is
      low.

HTTP and MCP:
18. Bare 404s with no named absence (FIVE_PROGRAMS §2c, §4 rule 3): `/v1/apps*`, a deliberate
    removal (ae2bf7ddc); `/v1/mesh/measurements`, which is not in `MOVED_TO_RAILS` (3ec99625f); and
    the `/internal/*` routes the daemon gave up on :9742 (1c120f23d).
19. `svrn ring checkpoint` probably fails with a 404. It calls
    `127.0.0.1:9742/internal/ring/checkpoint/{ns}` (sovereign-cli-base rail.rs:159-163), a route the
    daemon no longer mounts. scripts/ring-room-demo.sh:1479 runs it.
20. A peer's wipe-after-pull POSTs `/internal/corpus/partition_evict` (shard_manager.rs:515). That
    path is not a registered peer prefix (c46124cd5), so it likely gets a 404. The call is
    fire-and-forget, and the peer also evicts on its own.
21. **Check before shipping on-prem.** The guest door's TCP listener is built without
    `api_keys::seal` (guest_door.rs:429). It listens on 0.0.0.0:9744 while a guest grant is live.
    The GUEST_ALPN copy of the same router is sealed (daemon.rs:1650). client_auth.rs:360-364 admits
    any `Asserted` caller, on the stated premise that every client listener is sealed. So on a keyed
    daemon with a live grant, a non-admin key could reach routes outside its scope. Traced from
    code, not exercised.
22. On the stock `/mcp`, `spec` and `drift` are now listed only when the workspace has a spec
    (sovereign-code face.rs:491). Main listed them always, and c25b16fb7 does not mention the
    change.
23. Members dialing over the client ALPN lost `/v1/responses`, `/v1/knowledge/search`, `/status`,
    `/api/*` and `/oicp/v1/corpus/*`. The flip's body says "no peer listener" but does not list
    these routes.
24. Some refusals point to the wrong place:
    - On on-prem, `/v1/projects*`, `/v1/solve/jobs*` and `/v1/edit_predictions` name
      `svrn code mcp`, which on-prem does not ship. nginx returns 404 for them first, so only local
      callers see this.
    - `GET /v1/mcp/servers` reports MCP as mounted while `/mcp` answers 503
      (mcp_config_http.rs:120-141).
    - `svrn mesh status --help` still says it reads the daemon on 9741 (mesh_cmd.rs:790).

## Unverified, check

- After a downgrade, what a main-era binary does with `node_key` renamed to `node_key.handed-over`.
  It probably mints a new key.
- Whether a handover retried after a mid-way failure overwrites the first `*.pre-handover` backup
  (identity_handover.rs:122).
- Whether conversations stored by the old sovereign-server are visible under `{sub}:{id}` scoping.
- What doctor's `mesh_member` check suggests on an upgraded member that has not run `svrn mesh up`.
  checks_commonwealth.rs:125-131 may say "Run `svrn mesh create`".
- Whether `/v1/mesh/status` (now cw-rails' `RailsMeshStatus`) and `/status` keep the response shapes
  main's clients parse.
- Whether on-prem's `GET /v1/conversations/search` covers the old `POST /v1/search`, which was a
  corpus search. 46226ec78 says it does.
- Whether `svrn mesh create` always founds an encrypted mesh. The CLI still parses `--encrypt`
  (mesh_cmd.rs:420). The claim comes from 1c120f23d's body and phase-b-36.

