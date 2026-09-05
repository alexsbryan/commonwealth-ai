# Mesh cloud — ground truth

Verified facts underpinning `PLAN.md` (same directory), checked against source on 2026-08-04
(ARCH_PRINCIPLES §11.1 — cite, don't recall). Sections marked *(staff pass)* were verified via
targeted source reads the same day. Items marked *(doc)* cite repository markdown whose underlying
runs were not re-executed. When the plan or this file disagrees with the code, the code wins and
both files owe a fix in the same commit (§1.1).

---

## The mesh has no trust model

- Auth boundary is one equality check, gossip route only — `routes_internal/gossip.rs:28-45`. The doc
  comment names itself "the auth boundary".
- `/internal/scheduling/intent` returns `granted: true` unconditionally — `:112-114`.
- `Mesh` is flat: `id`, `name`, `join_key_hash`, `require_encryption`, `members`, `peers` —
  `commonwealth-core/src/mesh.rs:12-31`. No org, group, tenant, or role.
- `require_encryption` is **monotonic under merge**, stricter-wins, explicitly so no stale or hostile
  peer can downgrade — `mesh.rs:20-28`. **The template for clearance policy.**
- `internal_bind` defaults to `"0.0.0.0"` — `setup_config.rs:1043-1046`. The client port defaults to
  `127.0.0.1` (`:975`); do not conflate them.
- Two surviving stale mTLS claims: `routes_app_internal.rs:44`, `setup_config.rs:797`. The 2026-07-27
  correction is recorded at `routes_internal/mod.rs:8,17` and fixed that file only.

## The ranker is where inference-placement policy belongs

- `rank()` is pure — no I/O, no clock, no interior mutability — `scheduler_core.rs:317`.
- Typed exclusion reasons: `Quarantined` `:431`, `ManifestUnavailable` `:439`, `NoForcedChoice` `:464`,
  `NoClaimMatch` `:474`; paired tests `:901`, `:924`, `:1020`.
- `NodeCapabilities` rebuilt and gossiped every 10s — `capabilities.rs:64`.
- `benchmark` permanently `None` (`:223`) with guard test `gossip_never_advertises_a_benchmark`
  (`:450-454`). Heterogeneity is a constant in scoring today.

## Residency and custody are real

- `docs/TWO_NODE_QUICKSTART.md:1` — "a cited answer from a corpus that never leaves its machine";
  custody ledger at `:76`, glassbox verification at `:89`.
- Ephemeral grants: `grantable` enforced in exactly one place; revoke revokes the capability, fails a
  concurrent collaborate closed, retires the queue, and drops the gossiped handoff blob; idempotent;
  never mutates on-disk `CorpusMeta` — `corpus_grant.rs:1-21`, `:139-174`.
- Pull-mode queue is `corpus_next_unit` / `corpus_heartbeat` / `corpus_complete_unit` —
  `corpus_queue.rs:1-16`.
- `partition_evict` wipes the peer's working dir and logs the teardown — `corpus_queue.rs:694-722`.

## Tenancy exists and is inert

- Readers of `CorpusVisibility::Private { owner }`: `tenant.rs:44`, `context.rs:70`,
  `sovereign-contracts/src/types/mod.rs:904`.
- Only production writer: `sovereign-server/src/corpus_upload.rs:220`, behind `dev-routes`.
- Filter-after-LIMIT: `routes.rs:343` + `:347`, and `:420-421`.
- `approve_task` carries no `TenantId` — `routes.rs:375-384`.

## The app platform is a seam

- `AppProcess` zero callers; `AppPortMap::set` never called ⇒ proxy always 503 (`routes_apps.rs:115-124`);
  `/internal/app/registry` receiver with no sender; app mDNS trio dead; `install` is a HashMap insert
  (`routes_apps.rs:62-67`); registry unpersisted with lexicographic version compare (`registry.rs:53`).
- `MeshStore`: 7-day TTL, UTF-8 mangling on replication, same-second cross-node divergence
  (`store.rs:329-337`).
- Worker pods: single-tenant, one job; the only production runner POSTs HTTP to a child Sovereign
  daemon; the only `WorkerProvider` impls are Vast.ai — a rent-a-GPU controller, not on-prem.

## Measured performance *(doc)*

- 122B, 2 nodes, 36/12 blocks, head home: **17.3 / 17.9 / 17.8 tok/s vs 14.8 solo, ~20% better** —
  `DISTRIBUTED_PILOT_READINESS.md:543-544`.
- 4B: tunnel 39.6–41.0 vs direct 40.0–41.0 tok/s — tunnel tax invisible — `:533`.
- Relay floor **5.5–7.1 tok/s** (`QWEN122B_DISTRIBUTED_HANDOFF.md:33`); restated 5.8–7.1 from bench
  (`DISTRIBUTED_PILOT_READINESS.md:482`, `:536`). Ranges disagree slightly; treat 5.5 as the floor.
- LAN round trip 10.9–13.3 ms per 16 KB — `:507`. 600 KB logits return caps decode at ~12 tok/s even
  on LAN, which is why the output head stays home — `:494`.
- Worker `kill -9` under containment: 2m35s, one respawn, zero re-warms — `:52-66`.
- **Everything is two-node** — `:4`, `:273`. Retracted figure kept as the reporting standard —
  `:570-586`.

## The layer contract that decides where `TenantId` lives

- `TenantId(pub String)` is currently at `sovereign-server/src/auth.rs:92`.
- `[[forbid]] from = "commonwealth-core", to = "sovereign-*"` — **no `except`** —
  `quality/ARCH_LAYERS.toml:125-128`, reason *"mesh foundation must stay consumable without the agent
  runtime"*. `commonwealth-api` (`:169-173`) and `commonwealth-daemon` (`:160-164`) each carry
  `except = ["sovereign-contracts"]`; `commonwealth-core` does not.
- The forbid is a **name glob**, so it also rules out `sovereign-time` — the zero-dep contract-layer
  leaf that would otherwise be the obvious precedent for a new shared primitive crate.
- Layer order: `contract` (`oicp-types`, `sovereign-contracts`, `oicp-client`, `arch-layers`,
  `sovereign-time`) is below `mesh-foundation` (`commonwealth-core`, …), so `sovereign-contracts`
  cannot reach up either. The two crates that need the type cannot see each other in either
  direction.
- `oicp-types` is a serde-only leaf (`oicp-types/Cargo.toml:9-11`) already depended on by
  `sovereign-contracts` (`:14`) **and** `commonwealth-core` (`:9`). Zero new edges.
- `sovereign-server` depends on both `sovereign-contracts` (`:66`) and `commonwealth-core` (`:78`),
  so it can bridge whatever the lower layers cannot.

## Governance primitives (for H3 / T6)

- `CharterSections`, `parse_charter_sections`, `changed_sections`, `serialize_charter_sections` —
  `sovereign-cli-dev/src/amend.rs:59, 75, 128, 451`. Amendments bump `charter_version` (`:231`).
- `DecisionRecorder` trait with `NoteStoreDecisionWriter` impl — `sovereign-cli-dev/src/found.rs:305,
  338`. Non-private notes gossip mesh-wide keyed by `content_hash`.
- **Charter drift detection** — recompute the on-disk `CHARTER.md` hash against the recorded one,
  four outcomes (`none` / `differs` / `unknown` / `n/a`) — `sovereign-cli-dev/src/project_cmd/audit/mod.rs:110-130`.
- Charter skeleton + amendment flow — `sovereign-cli-dev/src/project_cmd/charter_amend.rs:36, 180`.
- Adjudication analogue — `sovereign/docs/GOVERN_A_CORPUS.md` ("surface tensions, resolve them into
  common law"): establish a governed baseline, see tensions, adjudicate, ask what the law is.
- **Scope caveat:** all of the above governs *software projects* and lives in `sovereign-cli-dev`, a
  high-layer CLI crate. The mechanism transfers to consortium governance; the schema and crate
  location do not.

## The three planes' deciders *(staff pass)*

- Retrieval fan-out selects peers by liveness + advertised corpora only, all candidates in parallel,
  3s per-peer timeout — `routes_knowledge.rs:127-163`, `:577-582`, `:284-303`, `:39`. It never calls
  the scheduler and structurally cannot: `rank()` is `pub(crate)` (`scheduler_core.rs:317`) and
  `sovereign-mesh` depends on `commonwealth-api`, not the reverse.
- The serving handler (`routes_internal/knowledge.rs:103`) checks: engine present, corpus installed,
  fan-out cap, unsealed-size cap, admission (a *resource* gate). It checks **no** sharing flag,
  visibility, membership, or caller identity — it maps `IndexInfo` down to
  `(corpus_id, chunk_count)` (`knowledge.rs:143-144`). `query_sharing` gates only advertisement
  (`capabilities.rs:264`).
- The only caller identity on the path is the `X-Node-Id` header — a bare hex parse, no signature,
  no roster check (`headers.rs:20-34`); it exists for ledger stamping. Absent → served, with zero
  ledger rows (`knowledge.rs:231`; pinned by `knowledge_served_e2e.rs:245`).
- The fan-out request has four fields — embedding, text, corpora, limit — and no identity of any
  kind (`oicp-types/src/knowledge.rs:17-34`).
- `cannot_know_from_here` is decided by the **local** grounding gate
  (`sovereign-core/src/runtime/epistemic.rs:206-212`); the coverage probe enumerates local indexes
  only (`:526`). The fan-out client used to discard `corpora_unavailable` and return transport
  failure as an empty vec, which made peers-refused and peers-empty indistinguishable at verdict
  time; since 2026-08-14 (`serve50-answer-honesty`) `MeshKnowledgeSource::search` returns
  `MeshSearchOutcome` and every failure path names the corpora it was asked for
  (`knowledge_client.rs:31-43`, lane `knowledge_client_unavailability.rs`). `corpora_searched` is
  still discarded. Retrieval has **no decision record**; the replayable
  `DecisionEvent` machinery (`decision_log.rs:918`) is inference-only.
- Custody today is one event, `KnowledgeQueryServed`
  (`commonwealth-core/src/contributions.rs:78-85`): serving-side, counted post-truncation, zero-hit
  serves and refusals unrecorded. The symmetric received-side pattern exists on the inference plane
  (`contributions.rs:70-77`).

## The ingest queue, in fact *(staff pass)*

- `WorkUnit = HfFile(usize) | JsonlShard(usize) | JsonlRange{start,end}`
  (`commonwealth-core/src/knowledge.rs:326-339`) — an index, not a payload. The peer re-acquires
  source itself (`ingest.rs:2209-2237`; step 1 is acquire, `:587-589`). No internal route serves
  source bytes (`server.rs:224-336`); `LocalFile` corpora cannot be peer-ingested.
- The peer runs acquire → extract → filter → chunk → **embed on its own embed slot** (embeds never
  cross the wire, `peer_inference.rs:2848-2867`); index build is deferred to the merge leader
  (`ingest.rs:1636-1640`). `complete_unit` carries outcome only (`corpus_queue.rs:659-667`); shards
  return as tarballs (`shard_manager.rs:506-554`); the owner alone merges and builds indexes
  (`sharding.rs:1405`), fetching shards **serially per peer** (`shard_manager.rs:382-462`).
- Leases: 5 min (`knowledge.rs:432`); reaper every 30s re-queues up to 3 attempts
  (`work_queue.rs:388-432`); heartbeat reclaim → 410 → peer-side cancel (`corpus_queue.rs:364-377`);
  merge dedups replays by content hash (`sharding.rs:588-593`). After 3 attempts a unit is dropped:
  "the merge leader proceeds without this unit; the corpus will be missing its chunks"
  (`knowledge.rs:378-380`) — silently.
- Concurrency: one pull loop per handoff per node (`auto_ingest.rs:715-717`), one ingest per
  partition dir (`ingest.rs:575-583`) — each node is exactly one worker. Single-file corpora slice
  into ~32 units (`corpus_collaborate.rs:309-311`). The coordinator self-pulls
  (`auto_ingest.rs:685-690`), which is the 1-node baseline for free.
- Embed-model equality is checked at dispatch (`corpus_collaborate.rs:164-176`) and peer-side
  (`auto_ingest.rs:693-701`) — both advisory. `QueueError::EmbedModelMismatch`
  (`work_queue.rs:70-73`) is never constructed; merge treats an empty model stamp as a wildcard
  (`sharding.rs:632-641`); the re-embed sample check is report-only (`shard_manager.rs:749-792`).
- Queue discovery: the coordinator unicasts the handoff blob to embed-compatible candidates via
  `POST /internal/app/state` (`corpus_collaborate.rs:572-657`); peers also scan `mesh_store` on a
  30s tick (`auto_ingest.rs:653`). **Corrected 2026-09-04: the "sender half is missing" reason given
  here is false** — `gossip.rs:799` (Step 4) has been a periodic full-snapshot sender on the 10 s
  round. The unicast is a latency shortcut, not a substitute for an absent sender, and BOTH are
  senders of replicated state that cw-lift rung 2e is deleting. Incidental
  find: `recv_app_state`'s `base64_decode` is a stub that treats `value_b64` as raw UTF-8
  (`routes_app_internal.rs:101-105`).

## Identity dies at the server; the tensor port is open *(staff pass)*

- `TenantId` is an axum extension consumed only inside `sovereign-server` handlers
  (`routes.rs:176-178` and callers). The remote-backend hop sends one static per-backend bearer and
  nothing else — no tenant header, field, or param (`oicp-client/src/lib.rs:451-453`; request body
  built at `:234-330`). `grep -rni tenant` over `commonwealth/` and `sovereign-mesh/src`: zero
  functional hits — the daemon has no tenant concept to receive one.
- `RankInputs` carries no corpus, dataset, or tenant — `InferenceRequirements` is capability hint /
  latency class / token counts / one `sharding` bool (`scheduler_core.rs:242-268`,
  `oicp-types/src/requirements.rs:16-39`; `PrivacyRequirements` at `:119-122`). The corpus↔tenant
  seam that does exist is retrieval-side: `PrincipalResolver` / `SensitiveCorpusOracle`
  (`sovereign-contracts/src/traits.rs:75-111`, consumed at `corpus_search.rs:189`).
- ggml-RPC worker: `--rpc-worker` and `role = "anchor"` default the bind to `0.0.0.0:50052`
  (`bootstrap.rs:268`, `:362-367`); the listener is `ggml_backend_rpc_start_server` on the raw env
  string (`rpc_distribution.rs:2011-2075`). Plaintext, no auth (`docs/THREAT_MODEL.md:63`,
  `:126-129`). The iroh ALPN forward dials the same listener over loopback — an *additional* path,
  not containment (`iroh_access.rs:265-296`); the LAN fast path prefers direct TCP
  (`daemon.rs:2074-2087`). The loopback-only posture is documented at `docs/CLOUD_TENSOR_PEER.md:79`.

## The CI economy *(doc — evidence for the CI/CD workload)*

- On 2026-07-24 every hosted GitHub Actions job began failing in ~4s on a spending limit; the gate
  "stopped existing without announcing it" — `docs/CI_ECONOMY.md:1-33`.
- Audited July spend: 4,369 billed minutes; 843 of 1,199 job-runs never started; the burn is bursts
  (2,950 min in two days of CI iteration), not steady state — `CI_ECONOMY.md:36-79`.
- Releases already run local at $0 (`scripts/release-all.sh`); hosted is the fallback —
  `CI_ECONOMY.md:336-346`.

## Related docs

`sovereign/docs/ENTERPRISE_FLEET_DEPLOY.md` is the closest existing document to this framing and
states the unauthenticated-internal-port posture plainly at `:90-95`. `sovereign/docs/MESH_NETOPS.md`
is written for a security team approving a deployment and carries a §5 "open validation (not yet
proven — do not represent as tested)". `sovereign/docs/specs/SCHEDULER_QUALITY.md` is the scheduler
design reference. `docs/THREAT_MODEL.md` carries the known-gaps list the plan updates on landing.
`docs/TWO_NODE_QUICKSTART.md` is the working residency demo. `docs/CI_ECONOMY.md` is the CI-spend
audit and incident report.
