# SMB local-RAG adoption — multi-tenancy and BYO-model assessment

_2026-08-13 · RuggedFox · research status: assessment from code read (three Explore readers, all file:line cited), no new runs_

The operator's scenario: an SMB wants local RAG over company documents
for all employees, where individual employees may augment the central
sources with their own — and some may want to run their own models. This
document interrogates how close `sovereign/deploy/onprem/` (the hardened
on-prem kit) and the platform behind it are to that shape, focused on
multi-tenancy. Companion: `FIFTEEN_MINUTE_COMMON_CASE.md` — the
adoption-funnel question (a firm person going "ooo" to running in 15
minutes), which is the earlier stage of the same customer. Sources: the kit docs (`deploy/onprem/{README,PLAN,EGRESS}.md`,
both configs, nginx units), `sovereign-server/src/{auth,tenant,routes,corpus_upload,routes_documents,ws}.rs`,
`sovereign-core/src/{context.rs,runtime/turn.rs,runtime/retrieval/corpus_search.rs}`,
`sovereign-store/src/sqlite/conversation.rs`, `sovereign-contracts/src/setup_config.rs`,
`sovereign-tools/src/local_corpus/watched/*`, and the daemon's `slot_aliases`.

## 1. The verdict, up front

The engine and the security posture are a strong fit for this market —
grounded answers with citations, provable refusal-when-unsourced, and a
zero-egress box whose kernel refuses outbound traffic (`IPAddressDeny=any`,
`EGRESS.md`). The kit as shipped is a **single-tenant, API-only pilot**.
Multi-tenancy is closer to real than the kit's own framing admits: key→tenant
mapping, tenant-scoped conversations, per-tenant private corpora with a
tested retrieval ceiling, and per-tenant scheduler quotas all exist in code
today. Three bounded gaps stand between that code and "all employees,
central + personal": (1) no employee-facing client, (2) two SQL queries that
filter tenants after the LIMIT, plus a key-onboarding story that is
hand-edited config, (3) no transport by which an employee's documents become
their private corpus. BYO-model has two distinct shapes — per-tenant model
selection on the central box (absent; small work on existing seams) and
bring-your-own-node on the mesh (exists in the platform, compiled out of the
kit) — see §6.

## 2. What the kit is, in one breath

`nginx :443` (TLS, 14-route allowlist, bearer passthrough) →
`sovereign-server :8080` (grounded runtime, `--no-default-features`) →
daemon `:9741` (model weights; `:9742` internal, no auth, loopback-pinned).
The hardened build compiles out `dev-routes` (shell-reaching solve/BDD routes,
absolute-path upload routes, `/mcp`) and `net-tools` (the search-tool web
fallback, `web_fetch`, `wikipedia_fetch` — the three tools the 2026-08-03
egress audit caught firing unconditionally on ordinary turns). `install.sh`
issues **exactly one API key, hardcoded to tenant `"firm"`** (`install.sh:180-195`),
restores the prebuilt `us-code` corpus, and watches one read-only share as
corpus `firm-docs`. `acceptance.sh` (12 checks, four verdicts) proves the
product — including abstention on an in-domain-but-absent probe — on the
customer's box at install time.

Standing caveats that outlive any gap-closing: the kit has **never been
rehearsed on a clean VM** (`PLAN.md` step 4 + open question 4 — "under an
hour" is an estimate); zero automated tests cover `sovereign-server`'s routes
(`acceptance.sh` is the compensating control, disclosed in the README); and
`package.sh` runs on our side, so every customer deployment means us
re-packaging with their corpus, probes, and naming (`us-code`, `firm-docs`,
tenant `"firm"`, and the litigation acceptance probes are baked in).

## 3. Multi-tenancy — what exists today

**Key→tenant resolution is real and trivial.** `[auth.keys]` is
`{secret = tenant_id}` (`sovereign-server/src/config.rs:114-120`);
`resolve_tenant` is a pure map lookup (`auth.rs:36-38`); the middleware
inserts `TenantId` on hit, 401 on miss (`auth.rs:43-88`). The only hardcoded
tenant is the empty-keys fallback `"default"` (`auth.rs:50-56`). Two keys →
two tenants is a shipped test fixture, `two_tenant_auth()` at
`http_tests.rs:235-240`. Nothing in the code forces one tenant — the pilot's
"one shared tenant" is literally `install.sh` writing one key mapped to
`"firm"`.

**Conversations are tenant-scoped by id prefix.** `tenant.rs:24-26`
(`format!("{}:{}", tenant_id, conversation_id)`); all CRUD and streaming go
through the scoped id (`routes.rs:206-207, 260, 292, 366`, `ws.rs:84`).
One global SQLite table, logical scoping — fine at this scale, provided the
predicate moves into the SQL (see §4).

**Per-tenant private corpora exist, with a real security boundary — this is
the load-bearing finding.** Uploads stamp
`CorpusVisibility::Private { owner }` (`corpus_upload.rs:220-222`) under a
namespaced id `user:<owner>:<slug>` (`corpus_upload.rs:86-88`). Isolation is
enforced twice:

1. Read surface: `TenantRuntime::forbidden_corpora` — the set of `Private`
   corpora owned by anyone else — filters `list_corpora` and `read_chunk`
   (`tenant.rs:38-48`, `routes.rs:485-495, 564-572`, chunk reads 404 so the
   corpus isn't even revealed).
2. Retrieval: `build_context` filters installed corpora by principal and
   stamps `corpus_ceiling` (`sovereign-core/src/context.rs:63-84`), re-applied
   at every corpus-chunk search — the comment at `corpus_search.rs:248-250`
   names it the security boundary a forged per-conversation
   `enabled_corpora` cannot widen. The principal is derived from the scoped
   conversation id on every turn (`tenant.rs:125-131`, `turn.rs:184-194`).

Both are tested: `upload_becomes_private_corpus_isolated_from_other_tenants`
(`corpus_upload.rs:310-366`) and `search_does_not_leak_across_tenants`
(`http_tests.rs:249`). The SMB shape maps 1:1 — central company docs are the
shared tier, personal augmentation is `Private` per tenant.

**Also tenant-scoped:** document assets are owner-stamped and filtered
(`routes_documents.rs:177-179, 110-115`), and scheduler fairness keys by
tenant (`reciprocity.rs:70-73`, `scheduler.rs:41`) so one employee cannot
starve another's turn quota.

**Explicitly NOT tenant-scoped:** tools (global), inference backends (global,
§6), `knowledge_view` (global ingest of all conversations into a shared
corpus — `sovereign-tools/src/knowledge_view/recipes.rs:231-249` carries no
tenant predicate; off in the kit, correctly), and there is no positive
per-tenant allowlist on which shared corpora a tenant may use — the only
narrowing is the host-global `[retrieval] corpora` allowlist
(`main.rs:294-302`).

## 4. The multi-tenant gaps, in adoption order

**Gap 1 — LIMIT-after-filter in two routes (the silent breaker).** Both the
conversation list and message search apply the tenant predicate *after* the
SQL window: `routes.rs:343-347` filters `starts_with(prefix)` on rows already
cut by `ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2`
(`sovereign-store/src/sqlite/conversation.rs:125-136`), and
`routes.rs:416-421` does the same after `search_messages`'s internal
`LIMIT 50` (`sqlite/conversation.rs:183-191`). This is an **availability**
bug, not a leak — cross-tenant leakage is closed by the same prefix filter
and tested. The failure mode: with N active tenants, the SQL window is the
*global* most-recent set, so a tenant's own rows can fall outside it and the
list/search render empty. The kit README discloses exactly this
(`README.md:212-215`, "a busy colleague's afternoon would make your own
conversation list render empty"). Fix: push the tenant predicate into the
`WHERE` of both queries. Small, mechanical, and it is the thing that turns
"two tenants" from a demo into a deployment.

**Gap 2 — the personal-corpus transport is compiled out, and its
replacement does not exist.** The one per-tenant ingest route,
`POST /v1/corpora/upload`, is correctly removed by `--no-default-features`
(`PLAN.md` workstream 1): its request shape is a **server-side absolute
path** (`CorpusUploadRequest.file_path`, `corpus_upload.rs:46-52`), so in the
general build any tenant key can ingest any file the process can read —
including the config holding every other tenant's API key. The kit's
alternative, `svrn corpus watch`, has **no owner attribution**
(`WatchedFolderConfig` carries `sensitive`, `follow_symlinks`, sync knobs —
no owner; `sovereign-tools/src/local_corpus/watched/config.rs:79-171`), so
every watched corpus is world-readable to every query on the box
(`registry.rs:54` keyed by bare `corpus_id`). And there is no document-push
wire protocol anywhere: OICP's ingest extension installs a *recipe by id*,
never raw documents (`oicp-types/src/ingest.rs:22-70`). So today "employee
adds their own documents" means IT stages files on the server and registers
a shared watched folder.

The fix is a new route, not a re-enable: a **client-bytes upload** (multipart
or staged body) that lands the file in a server-side scratch dir, then ingests
it as `Private { owner: tenant }` under `user:<tenant>:<slug>` — reusing the
existing two-layer isolation unchanged. The hazard that got the old route
compiled out (caller-supplied arbitrary paths) does not exist for client
bytes, provided the staging root is fixed and the filename is never taken
from the client.

**Gap 3 — key onboarding is hand-edited config.** `install.sh` generates one
key; adding one is "add a line, restart the server" (`server-config.toml:87-88`).
For a dozen employees that is tolerable; for onboarding 50 it is not. No SSO,
no rotation UI. This is process/tooling (a small key-management surface or a
config-reload path), not missing enforcement — the enforcement is already in
`auth.rs`.

## 5. The client gap (named here because it is the #1 adoption blocker)

The kit exposes the custom `sovereign-server` JSON API
(conversations/messages/corpora/search/documents/tools) behind a bearer
token, and **nothing else**: no web UI, no static assets, and no
OpenAI-compatible route through nginx (`firm-rag.conf` allowlist of 14
patterns; the daemon's `/v1/chat/completions` is loopback-only and, more
importantly, a raw ungrounded passthrough — `PLAN.md` architecture note 1).
Queue position is only emitted on the WebSocket stream route; the REST path
gives a caller no signal while waiting (`server-config.toml:36-38`).

"All employees access an LLM interface" today means curl, scripts, or the
desktop app — which the pilot deliberately does not ship. This is the gap
most likely to embarrass a deployment, and the most bounded to close: an
OpenAI-compatible wrapper over the grounded runtime (the server already owns
that runtime; `sovereign-server/src/main.rs:248`) would let any existing
chat client work against the box, or a minimal web frontend on the same
allowlist. Roughly: wire change plus one small surface, not research.

## 6. BYO-model — what "users run their own models" means here

Two distinct shapes, with very different current states:

**Shape A — per-tenant model selection on the central box: ABSENT, but the
resolution seam exists.** Inference is server-global: `[[inference.backends]]`
in `ServerConfig` (`config.rs`) names backends and `model_id`s; `routes.rs`
carries no per-request model field (the only `model` mention is provenance
metadata at `routes.rs:48`). Scheduler fairness is per-tenant, but model
choice is not. The daemon side already has the machinery a `model` field
would ride: slot aliases — a request names an advertised alias
(`commonwealth/fast`), the daemon resolves it against resident slots
(`sovereign_mesh::slot_aliases::resolution_alias_keys`, `daemon_cmd/build/inference.rs:94,
293-305, 357-359`; the alias map exists because clients advertise stems and
the daemon must map them to the role slot actually resident). So Shape A is:
add `model` to the server's request types, a per-tenant allowed-backend set
(one more enforcement field, sitting next to the existing quota keys), and
pass-through to the remote backend. Bounded — but note the physics: one GPU
box holds one or two resident models (the quoted profile pins 35B primary +
4B fast + 0.6B embed), so "choice" on a central box is choice among what is
resident, not arbitrary models. More resident models means fewer concurrent
turns (`max_concurrent_turns` is sized to VRAM, `server-config.toml:33-38`).

**Shape B — bring-your-own-node on the mesh: EXISTS in the platform,
compiled out of the kit.** The platform's native answer to "I want to run my
own model" is the employee's machine being a mesh node with its own daemon:
local `[models] primary` plus the `[shared_model]` overlay, where a node is
an `anchor` (holds model memory) or a `consumer` (queries the shared
instance) with a quorum gate (`setup_config.rs:204-259`, `SharedModelRole`);
peer-inference admission is governed by `max_peer_inflight`
(`setup_config.rs:771-772`, default 1) and corpora move between nodes via
canonical pull / snapshot restore, gated by per-corpus
`query_sharing`/`mesh_sharing` flags that user-owned corpora deliberately
never set (`corpus_ingest.rs:207-218`). The kit severs all of this
deliberately: no mesh, `max_peer_inflight = 0`, `[shared_model]` absent,
`discovery = "none"` (`daemon-config.toml:109-111, 145-147`).

Trade-off, stated plainly: Shape B gives real per-user model freedom
(any employee with a GPU workstation can serve whatever they want), at the
cost of per-employee hardware, mesh trust and egress-posture work (the kit's
zero-egress proof does not survive a mesh), and a very different security
story — the mesh moves bytes peer-to-peer, and the kit's entire hardening
thesis is "one box, two loopback hops." Shape A keeps one box and one audit
story but constrains choice to resident models. For the stated SMB scenario
("maybe individual employees would like to..."), Shape A on the company box
plus optional Shape B for the one or two power users is the honest
recommendation; the wire work is the same `model` field either way.

## 7. Sequencing

1. **Client surface** — OpenAI-compatible wrapper over the grounded runtime
   or a minimal web UI. Highest adoption leverage per unit work.
2. **Push the tenant predicate into the two SQL queries** (Gap 1) — before
   multi-tenant is ever sold; this is the bug that reads as "the product is
   broken" to the 12th employee.
3. **Client-bytes upload → `Private { owner: tenant }` corpus** (Gap 2) —
   reuses the tested isolation layers unchanged.
4. **Key management** (Gap 3) — a small issuance surface or config reload;
   N keys → N tenants already works.
5. **Per-tenant model selection** (Shape A) — only if asked; small SMBs
   rarely ask.
6. Later, beyond this scenario: team-level sharing (no read-ACL layer exists
   anywhere — the shipped "grants" are ingest-compute lending,
   `commonwealth-knowledge/src/ingest_grant.rs`, not visibility), SSO,
   tenant-scoped `knowledge_view`.
7. Regardless of all of the above: **rehearse the kit on a clean VM**
   (`PLAN.md` step 4) before any customer claim — `package.sh`/`install.sh`
   end-to-end, `systemd-analyze verify`, `nginx -t`, OCR against a real scan
   have never been executed.

## 8. Open questions

- Does the host-global `[retrieval] corpora` allowlist
  (`with_corpus_allow_list`, `main.rs:294-302`) compose with a tenant's
  `Private` corpora, or would a private corpus also need to be named in
  `[retrieval] corpora` to be reachable? The two mechanisms (global
  allowlist on the CorpusEngine vs. per-principal ceiling in
  `build_context`) sit in different layers; their interaction is not pinned
  by any test found in this read. Verify before promising Gap-2's shape.
- The kit's VRAM assumption lives in the quoting layer, not the kit — what
  smaller primary profiles are defensible for a 4-8 employee SMB on a
  consumer GPU, and at what answer-quality cost? Untested territory.
- Per-tenant quotas exist; per-tenant *rate limits* for API abuse do not —
  worth confirming what a misbehaving key can do to the single model queue.
