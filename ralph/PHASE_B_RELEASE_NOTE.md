# Phase B: what changes for users and operators

Range: main at `18f783f44` to `cut` at C, `3b8515d5f`, plus fix row F13 at `f548c483f`. Each item cites the commit that made it. This
is the ship gate's "one release note listing every user-visible change" (ralph/PHASE_B_SHIP_GATE.md).

How it was checked at C (pb-distribution-ship-gate, 2026-10-01). It starts from the seat's draft
(ralph/next/phase-b/release-note.draft.md), a read-only census at `820c4e051`. All 117 commits the draft
cites are in C's history except `d1f3e1765`, a same-subject commit from another branch; cut carries it
as `917166da1`, which is cited here. The 65 code-path commits from `820c4e051` to C are the ship gate's
fix rows F4-F12, the lift instrument and the O3 test successors. The items those rows changed were
re-read in the tree at C and rewritten here. The verb table was read from the built dispatcher
(target/ralph/phase-b/ship/retired-verbs.log). Items marked "(read from code)" were traced but not run.

Phase B splits the node into programs (docs/FIVE_PROGRAMS.md §2): `svrn`, the knowledge daemon on
:9741; `serve`, the model server on :9748; cw-rails, the mesh endpoint on :9747; and `svrn ingest`,
`svrn code` and `svrn bench`. A stock install still runs as one process. The mesh now belongs to
cw-rails, and nothing starts cw-rails unless you ask it to.

## Upgrading an existing node

1. Reinstall with `curl … | sh` or `svrn update`. A release now ships 12 binaries instead of 3
   (4a0809a6c). The CI release workflow builds and packages the same list, read from
   scripts/release-cli-local.sh (933709231).
2. Restart the daemon with `svrn daemon stop && svrn daemon start`. On first boot it moves svrn's
   own rows from `notes.db` to `sovereign.db`, after writing `notes.db.pre-pb-notes-memory`
   (ea8850079, 9df4750c4).
3. On a mesh node, run `svrn mesh up` once. The node is off the mesh until you do, and until then
   the daemon's boot log, `svrn daemon start|restart` and `svrn mesh status` each say so and name
   `svrn mesh up`. When `config.toml` still holds `[compute.work_offer]`, they also say the donor
   offers nothing (0d436b45d). The command moves the node key, the mesh membership, the ring history
   and several config sections to cw-rails. It then starts cw-rails and, on Linux, enables a
   `cw-rails.service` user unit (272c14999, 7d9a28436, 1c120f23d). The operator's own cutover is
   recorded in b71fdd08b.
4. If `config.toml` sets `[discovery] join_key`, remove it and `seed_addrs`, then join with
   `svrn mesh join <invite>`. The daemon refuses to boot while `join_key` is set (1c120f23d).
5. Every file the upgrade moves keeps its first original beside it, and a second run never
   overwrites that copy (ca8bd2f5c). sovereign/docs/RUNBOOK.md §9 is the written rollback to the
   main-era binaries; its handover script is run by a test on a main-era fixture handed over twice.
6. On-prem: re-run the kit's `install.sh`. One unit, `firm-rag.service`, replaces both of main's
   (`firm-rag-server.service` and `firm-rag-daemon.service`), which install stops and disables
   (ca8324ce6). It issues new API keys; the old `[auth.keys]` keys do not carry over (0be7666a6).

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
from either list, and it reads the CI workflow's list too (933709231).

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
  missing, it exits 127 and names it. `SOVEREIGN_DAEMON_BIN` still overrides the path (fbe73a5fb).
- svrn starts no other process. Boot no longer starts cw-rails (45c89832b) or a separate serve
  (a378380fc). A bare `sovereign-daemon` with no serve to reach refuses to boot, naming the absence.
  `svrn daemon stop` and `restart` stop the daemon only.
- Only `svrn mesh up` starts cw-rails. It writes and enables
  `~/.config/systemd/user/cw-rails.service`; `svrn install-service` does not. Off Linux there is no
  unit, so after a reboot the node is off the mesh until `svrn mesh up` runs again. `svrn doctor`
  gains `rails_boot_unit`, which warns "off the mesh after a reboot" (7d9a28436).
- The packaged desktop now carries `sovereign-stock` beside its `sovereign-cli-daemon` sidecar.
  Before, a packaged app had nothing for `daemon run` to exec (917166da1).
- The container images (Containerfile, Containerfile.cuda) now also copy `sovereign-cli-daemon`,
  `sovereign-stock` and `sovereign-pod-worker` (f7bd83fb4).
- On-prem kit (sovereign/deploy/onprem; 0be7666a6, e3a2bd6f5, 46226ec78, ca8324ce6):
  - One unit, `firm-rag.service`. `server-config.toml` and both of main's units are retired.
  - `package.sh` builds `sovereign-onprem` (with OCR), `svrn`, `sovereign-cli-daemon` and
    `svrn-ingest`.
  - nginx proxies the daemon on :9741 instead of :8080. Its allowlist is pinned to the daemon's key
    scope, and the body limit drops from 64m to 1m.
  - Clients lose `/v1/tasks/{id}/approve`, `/v1/search` and `/v1/documents/{id}/state`. They gain
    `/v1/conversations/search`, `/v1/documents/{id}/progress` and the document ask poll.
  - `install.sh --port n` puts the client API on n, internal on n+1, rails on n+6 and serve on n+7.
  - The kit's nginx config had not loaded since 2026-08-03, because of a duplicate
    `proxy_http_version`; 30aa81286 fixes it.
  - `install.sh --force-config` deletes every issued key before re-keying (install.sh, step 9).
    That is its documented purpose, not a change, but it is easy to run by mistake.

## 2. CLI verbs

The dispatcher does not parse verbs with clap. An unknown verb prints the full `svrn` help to stderr
and exits 1. In a release build, which has no dev-tools, a verb in `DEV_VERBS` is refused with a
"developer toolchain" message and exit 2.

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

CLI_REFERENCE.md documents `mesh up`, `daemon key` and `code mcp` (1f723d1df).

### Removed, and what C prints

Every removed spelling below prints "note: `svrn <spelling>` has been retired." and what replaced it,
from one table (`sovereign_cli_base::deprecation::RETIRED`), and exits 2; nothing runs in its place
(1f723d1df). Read from the built dispatcher at C.

| typed | replacement named |
|---|---|
| `svrn atos …` | `svrn charter`, `svrn milestone --project <N>`, `svrn audit` |
| `svrn design`, `svrn project design` | write the design doc; `svrn plan validate <path>` |
| `svrn project plan` | `svrn plan validate <path>` |
| `svrn amend design`, `svrn project amend design` | `svrn amend` amends CHARTER.md |
| `svrn drift accept` | `svrn drift detect --code <path> --narrative <doc>…` |
| `svrn audit <feature-id> [--archive]` | `svrn audit`, the project-wide rollup |

Others, unchanged by that table:
- `svrn mobile serve|status|pair`: "svrn mobile: the mobile host was the sovereign-server binary,
  which was deleted; no mobile host ships", exit 1. `status` and `pair` no longer write
  `~/.svrnmesh/mobile-host.toml` (4c1f684fa).
- `svrn plan` (compose): "note: `svrn plan` has been retired." and points to
  `svrn plan validate <path>`, exit 0 (bdd22b846).
- `svrn pipeline pod <x>`: "moved to `svrn mesh pod`. Run `svrn mesh pod <x>`.", exit 2 (93f66f8b4).
- `svrn corpus extract-entities --download-model` points to `svrn mesh fetch-ner`, exit 2
  (c4f8726e4).
- `svrn milestone <feature-id> <N>` ("requires --project") and `svrn notes promote` ("Unknown flag")
  give generic usage errors (2ea67a59f).
- `svrn project audit` no longer prints the "Share your recipe" footer (933190f79).

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
- `svrn daemon reload` reports `[iroh] enabled`, `transport`, `media_origin` and `media_allow` as
  keys the daemon no longer reads, rather than as restart-required (d16cc827d).
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
- The daemon's `/status`, `/v1/models` and knowledge fan-out no longer wait on cw-rails. A roster
  read has one 3 s bound (sovereign-serve rails_mesh.rs:47). On a miss, `/status` carries
  `mesh.roster_absent` ("cw-rails slow: no roster within 3s" or "cw-rails absent: nothing answers
  at …"), and the fan-out plan logs that the peer roster is absent instead of planning with no
  peers. cw-rails' KV pump runs off its API's workers, so a seal or snapshot no longer leaves the
  API unanswered (e9b1e7773, c8d5dc261, aa857b374; the absent and slow paths read from code and
  proved on a sandbox pair at bd4150f15). Read on the deployed node at f548c483f: `/status` p50
  18 ms, `/v1/models` p50 6 ms, and 6 of 6 fan-out plans read the roster.
- `svrn notes` and `svrn reflect` read only `<data root>/notes.db`, or the store named by
  `--data-dir`. The per-repo `.sovereign/notes.db`, the cwd walk and the `active_notes_db` pointer
  are gone, and repo-local stores are not migrated (9e2eb17de).

## 3. Environment variables

quality/env-flags.toml has 220 rows at both ends: 11 removed, 11 added.

Removed. Each one gated code that was deleted with it. Setting one now does nothing, and nothing
warns (phase-c pc-removed-env-warn):
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
daemon's process entry. It does not run in a directly launched `sovereign-serve`, `cw-rails` or
`sovereign-pod-worker` (read from code).

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
  - `[compute.work_offer]`, kept first in `config.toml.bak`. A donor node stops donating until then
    (c9b6c61a2), and the boot log says so (0d436b45d).
  - `[iroh] relay_urls`, `discovery`, `media_origin` and `media_allow`, kept first in
    `config.toml.iroh.bak` (1c120f23d).
  - `[iroh] media_viewer_user`, now kept first in `config.toml.bak` (24c5bf5b9, ca8bd2f5c).
- `svrn mesh up` warns that `[iroh] enabled` and `transport` have no reader, and leaves them in
  place.

On disk:
- cw-rails' root is `~/.commonwealth-rails`, or `CW_RAILS_DIR`. It holds `rails.toml`, `rails.lock`,
  `node_key`, `node_id`, the mesh and `rings/`.
- `svrn mesh up` copies the daemon's `node_key` there and renames the original
  `node_key.handed-over`. It keeps cw-rails' prior files, `rails.toml` included, as
  `*.pre-handover` (an empty one stands for a file that did not exist), and moves the ring journals
  from `~/.svrnmesh/rings`. A second handover keeps the first copies (ca8bd2f5c).
- If cw-rails is already running, the handover is deferred with "stop cw-rails and run
  `svrn mesh up` again" (1c120f23d).
- `notes.db` → `sovereign.db` (`memory_notes`). At daemon boot, svrn's lessons, tool-decision
  dossier and session todos and commitments move after a `VACUUM INTO` backup. A failure leaves
  every row where it was and retries at the next boot (ea8850079, 9df4750c4, 3210f56d9). After the
  move, `notes.db` belongs to the code program alone. On the operator's node all 9,987 rows were
  accounted for at C (ship gate, P1 notes).

Ports:
- Unchanged: 9741 (svrn client API), 9742 (internal), 9743 (rail), 9744 (guest door) and 9745
  (desktop bridge).
- 9747 is cw-rails, which the daemon now dials (bc576f8cd).
- 9748 is new: serve (d66686a89).
- 8080 is gone. It was on-prem's sovereign-server upstream and the desktop's mobile host (0be7666a6,
  4c1f684fa).

## 5. HTTP and MCP surfaces

Unchanged on :9741:
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
  app, offers, fanout, media/fanout, the three publish forms, kv/entry and kv/entries (1c120f23d).
- `/v1/projects*`, `/v1/solve/jobs*` and `/v1/edit_predictions` now belong to the code program. The
  stock binary still serves them on the same port. A bare `sovereign-daemon` or the on-prem binary
  answers 503 naming `svrn code mcp` (c25b16fb7, e96a3a83f, 3825c6342).
- `/v1/knowledge/landscape_digest` answers 503 when no ingest is composed (8aeadb016).
- On-prem: `/mcp`, `/mcp/message` and `/mcp/stats` answer 503 "this distribution does not serve MCP"
  (3b6ad7b7e).

Removed, with a bare 404 (phase-c pc-bare-404s):
- `/v1/apps*` and `/app/{id}/*`. The proxy behind them always answered 503, and no in-repo client
  used them (ae2bf7ddc, 6aaab8009).
- `/v1/mesh/measurements`. Measurements now travel on a cw-rails rail namespace (3ec99625f).

Internal port :9742:
- It binds 127.0.0.1 only. `[daemon] internal_bind` is logged but not bound (1c120f23d).
- `/internal/gossip`, `/join`, `/ring/*`, `/v1/models/*` and `/rpc-warm` moved to cw-rails or serve
  (1c120f23d, d30c17f1b, 7bb3fd1ae, f9b6325cd).
- Peers reach nine registered internal prefixes through cw-rails. Any other path gets cw-rails' 404
  "no origin is registered for <path>" (c46124cd5).
- The peer listener is gone. A member that dials a node's client ALPN reaches serve's member face.
  That face serves only chat/completions, embeddings, completions, models and
  `/oicp/v1/capabilities` (1c120f23d, c0e2e5265). Members lost `/v1/responses`,
  `/v1/knowledge/search`, `/status`, `/api/*` and `/oicp/v1/corpus/*` over that ALPN.

serve (new, 127.0.0.1:9748) serves the OpenAI routes, `/oicp/v1/capabilities`, `/v1/rerank`,
`/v1/ner`, `/v1/admin/{hardware,setup/catalog,setup/slot}`, `/v1/engine/{state,self,reload}`,
model-file and asset transfer, and loopback-only internal routes (e47823aac, d3f044abc, 70973a6c5,
3f9749490). Every `/internal/*` route on a standalone serve refuses a non-loopback caller
(4bf702b69).

cw-rails (127.0.0.1:9747; loopback is its only auth) gains the membership routes
`/v1/mesh/{create,join,join/preview,rotate,leave,switch,forget,forget-member}`, plus
`/v1/mesh/{reach,origins,offers,relay-candidates,kv/*}`, `/v1/rail/*`, `/v1/work/*` and
`/v1/ledger/*` (26e82ea3a, ea90afb11, f09642bae, 9f081beb1, b8112b780, 036c48339, 909e1f219). A join
cw-rails cannot save is refused by name and rolled back (52bb2daf5).

MCP:
- The exposed tool ids are unchanged, and there is still one `/mcp` on :9741.
- The registration is split: svrn registers 19 tools, and the code program registers 17 plus `spec`
  and `drift` (162cbaece, c25b16fb7). The stock install lists all of them, except that `spec` and
  `drift` are listed only when the workspace has a spec (sovereign-code `MCP_TOOLS_SPEC_GATED`).
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
  - Every listener is sealed, the guest door's own TCP bind included (c7604de72).
  - Conversations are scoped `{sub}:{id}`, and retrieval stays inside `[retrieval] corpora`.
  - A daemon with no keys behaves as before (34ba2051c).
  - The CLI sends no key, so CLI verbs against a keyed daemon get 401 (0be7666a6 body).
- **On-prem acceptance.** At C, through nginx, 58 of 58 checks passed; a direct run passed 32 of 33.
  Check 4 (a partial-decline verdict) varies run to run and is closed by operator ruling phase-b-95
  (ship gate, Readings).
- **Mesh.** cw-rails, not the daemon, holds the endpoint and the key (1c120f23d). The IP overlay and
  plaintext joins are gone (phase-b-36, -37). Splitting a large model across machines over ggml RPC
  still works, through cw-rails' rpc_tensor bridge; the serve-mesh lift proved it twice after the
  switch (1c120f23d body).
- **Collaborative ingest** completes on the stock binary: a coordinating node pulls its own queue
  over loopback (b72136134, 4327e938e).
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

## Known issues at C

Found by the ship gate's Tier 2 readings on the operator's node (e5d493486):
- Under use, cw-rails' kv pump seals and snapshots `activity-private` every ~85 s, and cw-rails
  answers no HTTP through each ~82 s snapshot. Mesh status, rings and peer fan-out time out, and
  the daemon's `/v1/models` and `/status` wait on it (3.0 s and 6.0 s), so clients with a 3 s budget
  report the daemon unreachable. While it does not answer, the daemon reads the mesh roster as empty
  and searches local corpora only, naming no absent peer. To be fixed before the merge as ship gate
  F13 (pb-distribution-f13-rails-stall, phase-b-108); this entry is rewritten when it lands.
- A corpus ingested while the daemon runs is not searched by grounded turns until the daemon's next
  start, because the corpus registry is reconciled only at boot. Main has the same boot-only
  reconcile; the ship gate read it through its own lane, not a user report. No row owns it yet.

Filed to phase-c, with their rows:
- The 11 removed env vars are ignored without a warning, and some docs and
  scripts/overnight-batch.sh still name them. Four registry rows (`SOVEREIGN_BIND`,
  `SOVEREIGN_DB_PATH`, `SOVEREIGN_SERVER_PATH`, `SOVEREIGN_SUFFICIENCY_CHUNKS`) have no reader;
  `CW_RAILS_DIR` and `SOVEREIGN_DAEMON_BIN` are unregistered (pc-removed-env-warn).
- Bare 404s with no named absence: `/v1/apps*`, `/v1/mesh/measurements`, the `/internal/*` routes the
  daemon gave up on :9742, and a peer's `/internal/corpus/partition_evict` wipe-after-pull, which is
  fire-and-forget (pc-bare-404s).
- RUNBOOK §2 still says to keep cw-rails across reboots with a unit of your own, and the docs that
  describe the pre-flip daemon need correcting (pc-docs-after-cut). CLI_REFERENCE.md still calls
  `mesh create` "Promote the solo mesh", and `svrn mesh status --help` still says it reads the
  daemon on 9741.
- Split-deployment reporting: admin reload's success without checking serve, svrn's stale
  self-report after serve restarts, `/v1/mesh/status` not telling "serve down" from "serve slow",
  the RPC direct-IP probe's missing identity check (pc-rpc-probe-identity, pc-split-deploy-honesty),
  and `svrn mesh fetch-model`'s dead peer discovery (pc-fetch-model-peer-discovery).
- Some refusals point to the wrong place: on on-prem, `/v1/projects*` and its siblings name
  `svrn code mcp`, which on-prem does not ship (nginx 404s them first), and `GET /v1/mcp/servers`
  reports MCP as mounted while `/mcp` answers 503 (pc-onprem-followups).

## Unverified

- Whether conversations stored by the old sovereign-server are visible under `{sub}:{id}` scoping.
- What doctor's `mesh_member` check suggests on an upgraded member that has not run `svrn mesh up`.
- Whether `/v1/mesh/status` (now cw-rails' `RailsMeshStatus`) and `/status` keep the response shapes
  main's clients parse.
- Whether on-prem's `GET /v1/conversations/search` covers the old `POST /v1/search`, which was a
  corpus search. 46226ec78 says it does.
- Whether `svrn mesh create` always founds an encrypted mesh. The CLI still parses `--encrypt`. The
  claim comes from 1c120f23d's body and phase-b-36.
