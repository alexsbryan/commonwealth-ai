# Scale analysis — 100 users, 1000 corpora: where the system breaks, in order

**Date:** 2026-08-13 · **Method:** three parallel code sweeps (retrieval path, inference
scheduling, mesh substrate) + the existing `sovereign/docs/specs/SCHEDULER_QUALITY.md`
measurements. Every claim below carries a file:line citation from today's tree; nothing is
extrapolated from docs alone.

**BLUF:** the system as shipped is engineered — and in several places *measured* — for a
household: roughly 2–12 online nodes, ~30 corpora, 1–3 concurrent turns per node. Against the
hypothetical (100 users, 1000 installable corpora, any corpus can surface for any query), it
does not degrade — it crosses four distinct hard ceilings, each owned by a different subsystem,
and three of the four fail **silently** (latency and debug-level logs, not errors). The
ceilings are independent axes: fixing one does not move the others.

| Axis | Ceiling as implemented | What breaks first |
|---|---|---|
| Corpora per query | ~low hundreds | query latency: all-corpora fan-out ×10–30 per turn, no router |
| Concurrent users per node | ~2–3 | single-permit chat slot + 30s predicted-wait shed |
| Online mesh nodes | **~12** (derived in code, unenforced) | gossip false-offline flapping dissolves the mesh |
| Users as identities | **0** | no user/tenant type exists anywhere; identity is the node |

---

## 1. The 1000-corpora axis: retrieval has no router

**There is no corpus selection step.** The default is "search every installed corpus":
`corpus_search.rs:38-43` documents `None` = no filter; the only relevance-based pruning
(`corpus_relevance_prefilter`, `corpus_search.rs:489`) is experimental, gated on
`SOVEREIGN_CORPUS_PREFILTER_TOPK`, **off by default** — and when on, it is itself O(N) real
ANN probes (`nearest_vector_distance` per corpus, `corpus_search.rs:528-529`), so it trades
rerank cost for open cost rather than removing the O(N).

**The fan-out is not paid once per query.** Call-site census: main retrieval
(`retrieval_pipeline.rs:1850`), up to 4 entity boosts **sequentially**
(`retrieval_pipeline.rs:1665-1680`), up to 4 query-decomposition sub-queries sequentially
(`query_expansion.rs:1136`), graph-neighbor expansion per candidate title
(`query_expansion.rs:133`) — ~10 to ~30 full all-corpora fan-outs per turn, each also paying
its own `embed_query` (the query is embedded 1 + up to ~12 times, not once).

**Per corpus, per fan-out:** an `installed_indexes()` walk amortized across the call
(`engine/mod.rs:1415-1512` — full `read_dir`, 1–2 stats per entry, ~30-field `IndexInfo` clone
per corpus, all under one `std::sync::Mutex`), a LanceDB open (~5s cold for the 1.9M-chunk
Wikipedia index, cached in an **unbounded, never-evicted** `HashMap` at `engine/mod.rs:186`),
a hybrid IVF-PQ + Tantivy search overfetching **50** candidates (`index/search.rs:770`), and a
cross-encoder rerank of all 50 — through a **single `std::sync::Mutex<RerankSlotContext>`**
(`rerank_slot.rs:56-61`). Fan-out concurrency (`SOVEREIGN_KQ_FANOUT_CONCURRENCY`, default 4)
parallelizes only the LanceDB leg; the rerank leg is globally serial.

**Measured anchor, in-tree:** the 2026-07-21 soak at 31 corpora logged **2,647 cross-corpus
searches / 5,949 cumulative seconds in 90 minutes; 31 corpora touched per turn**
(`atom_enum.rs:650-655`). The in-code estimate is ~2s/corpus (`corpus_search.rs:284-285`).

**At N=1000, per query:** ~30,000 directory stats + 30,000 `IndexInfo` clones before any
vector math; 1000 × 50 = **50,000 cross-encoder scorings through one llama.cpp context**; a
merged pool of ~52,000 chunks (~100 MB of `ScoredChunk`, doubled transiently by the
full-content-cloning dedupe at `retrieval_pipeline.rs:2071`) — all discarded down to
`KQ_MERGED_LIMIT` = 20 chunks / 24,000 chars. Main fan-out wall time extrapolates to ~500s at
concurrency 4. **The work is linear in installed corpora; the useful output is a constant.**

**Resident memory converges to all-N.** The 60-minute maintenance sweep opens *every* index
(`corpus_maintenance.rs:124-139`), pinning all 1000 LanceDB handles into the no-eviction cache
within an hour of boot regardless of use. Atlas contexts/graphs are likewise load-and-never-
evict (`atlas_context_manager.rs:171,183`), with the whole atoms table resident per loaded
atlas (`store.rs:728-737`). An in-code comment already describes the ~1,800-index case as "a
btrfs metadata storm that thrashes even a 128 GB box" (`engine/mod.rs:190-194`).

**Mesh coupling:** the ~10s gossip tick rebuilds `hosted_corpora`
(`capabilities.rs:103,117-123`) — 1000 stats + clones per tick, and a 1000-entry list
serialized into every member record, multiplying the substrate traffic in §3. One asymmetry
worth noting: the *peer-serving* knowledge path caps at **16 corpora per peer**
(`routes_internal/knowledge.rs:24`) — a cap the local path entirely lacks.

## 2. The 100-users axis: inference capacity and the missing scheduler

**Effective chat capacity per node is one concurrent turn.** The primary/code slot is a
single-permit semaphore (`engine.rs:1345`, `model_slot.rs:864`); every Normal/Slow-class turn
on the node serializes through it. The queue sheds when predicted wait exceeds **30s**
(`model_slot.rs:803,989`) — with a 15s knowledge-turn EWMA the **third** concurrent caller
gets a 503. Steady-state admitted concurrency ≈ `1 + floor(30s / avg_turn)`. 100 concurrent
users on one node: ~2–3 served or queued, ~97 shed.

**There are three schedulers, and the fair one is not on the path.**
1. `peer_admission_layer` (`admission.rs:232`) — shed-only, applies **only** to requests
   carrying `X-Node-Id`; local requests bypass it entirely (`admission.rs:238-241`).
2. `SlotQueue` — the semaphore above: global FIFO, no identity, no per-origin anything.
3. `FairScheduler` (`sovereign-server/src/scheduler.rs:99`) — genuinely fair (per-user cap 1,
   weight + FIFO, depth 32, cancel-safe) but constructed **only in `sovereign-server`**, a
   binary the daemon/desktop OpenAI-compatible surface never touches.

Client auth on the daemon surface is one shared bearer token (`client_auth.rs:112-170`), so
100 users are 100 indistinguishable callers on one FIFO. One aggressive client starves the
other 99 and nothing can even name it.

**The mesh cannot absorb the overflow.** Per-node peer admission defaults to
`max_peer_inflight = 1` (`setup_config.rs:1110`, applied `daemon.rs:2473`). The routing
decision does **serial** per-peer manifest fetches at up to 800ms each with no cap and no
single-flight dedup (`peer_inference.rs:1455-1487`) — at P peers and C concurrent requests, a
60s-TTL expiry produces P×C fetches and up to P×800ms of added TTFT. Shed responses carry a
constant `Retry-After: 2` with no jitter (`state.rs:2079`), and peer selection is
deterministic argmax with no jitter or hysteresis (spec F5) — a synchronized-retry and herding
generator. A saturated peer that stalls rather than sheds (outbound timeout is **1800s**,
`oicp-client:93`) books real failures: 3 consecutive → 60s quarantine escalating to 600s
(`peer_health.rs:47-56`), shrinking fleet capacity exactly under load.

**Sharpest single-node hazards:** a stalled SSE consumer holds the node's only chat slot up
to the 300s deadline (`engine.rs:3123,3272`, `model_slot.rs:758`) — one half-dead client ≈
full node outage; the FastShort coalescer feeds from an **unbounded** channel upstream of the
shed check (`engine.rs:256`), the one true unbounded-growth point; and
`bump_foreground_active()` fires unconditionally *after* peer admission
(`routes_inference.rs:43`), so with a yield window configured, the first admitted peer request
arms the yield gate against all peer traffic for 60s.

**Spec drift, both directions:** SCHEDULER_QUALITY F4's "the hub never sheds — it queues
without bound" is now **stale** — `max_peer_inflight` defaults to 1 and the 30s predicted-wait
shed landed (`model_slot.rs:989`; its own doc-comment at `:1070` still says "deliberately
unbounded"). F9's local-load fix is in. Still open: whether `in_flight_publisher` counts
inbound-served work (all bump sites are joiner-side outbound; sim priced the bug at +126% to
+584% if real), and F2's availability term still equates one human at a keyboard with 80
queued requests.

## 3. The mesh-substrate axis: the ~12-node ceiling

**The gossip design has a derived ceiling of ~12 online peers, computed in code and enforced
nowhere.** Fanout 2 (`gossip.rs:42`), interval 10s (`gossip.rs:100`), offline threshold 60s
(`gossip.rs:96`): `max_online_peers_before_false_offline = fanout × threshold/interval = 12`
(`gossip.rs:248-262` — unit-tested, never called at runtime). At 30 online peers a reachable
peer waits ~150s between contacts against a 60s threshold — **every peer permanently flaps
Offline**, and since inference routing, knowledge fan-out, and `/v1/models` all filter on
`Online|Busy`, the mesh functionally dissolves while every process is healthy. The module
docstring records this class of incident at *four* members.

**mesh_store replication is O(N²·S) with a silent cliff at 8 MiB.** Every 10s round pushes the
*entire* non-excluded store to *every* online peer (`gossip.rs:593-676` — no fanout cap, no
delta, no watermark; a watermark table exists in schema and is unused). Global notes gossip
their f32 embeddings JSON-encoded as decimal byte arrays — ~16 KB on the wire per ~1 KB note
(`notes.rs:118-124,1949-1953`). At the 8 MiB body limit (`server.rs:30`) — roughly **500
embedded notes** — every `/internal/app/state` POST starts 413ing, the failure logs at
**debug** (`gossip.rs:658-664`), and replication of notes, work-atlas, model info, and the
contributions ledger all stop converging fleet-wide with no surfaced error. The contributions
ledger (one append-only row per served request, `contributions.rs:73`) has no GC in the
sovereign daemon (`RetentionGc` is spawned only by `commonwealth-daemon`), so the store only
grows toward that cliff. Deletions resurrect: `upsert_if_newer` re-inserts anything absent
locally (`backend.rs:67-99`), so GC'd rows churn back every round until every node expires
them independently.

**Other per-round costs that go quadratic-ish at fleet scale:** full fsync'd `mesh.json`
rewrite from inside the gossip receive handler under the global mesh write lock, per inbound
message (`routes_internal/gossip.rs:76-85` → `daemon.rs:2424`); work-atlas GC every 60s doing
O(sessions × (claims + observations)) full scans over the mesh-replicated set
(`gc.rs:124-155`); `AtlasObserver` broadcasting each debounced file-edit observation to every
online peer (`observer.rs:205-210`); knowledge fan-out spawning one uncapped task per peer per
query (`routes_knowledge.rs:276-293`); one iroh loopback bridge per (peer, ALPN) with a fresh
QUIC connection per accepted TCP connection (`iroh.rs:416-445,554,704`).

**Model advertisement breaks structurally at fleet scale:** the store key is
`model:<hash(path,role)>`, not namespaced by node (`daemon.rs:3402-3410`), so all nodes
holding the same GGUF at the same path collide into one LWW cell whose `origin` — the only
liveness signal — is whoever wrote last. A model hosted by 99 online nodes vanishes from
`/v1/models` because the 100th writer went offline. `available_on` is deliberately empty
(`daemon.rs:3413-3420`); there is no place to record multi-host residency.

## 4. The identity axis: there is no "user"

Grep across `commonwealth-*`, `sovereign-mesh`, `sovereign-work-atlas` finds **no user,
tenant, or account type** — two doc comments only. Mesh identity is the node (NodeId +
Ed25519); mesh auth is one shared join key; client auth is one shared bearer token per node
("not per-user tenancy", `routes_ollama.rs:36-37`). Admission, fairness, quarantine, the
contribution ledger, and foreground-yield ("the local user is at the keyboard") all key on
node and assume **node == user**. So "100 users" forks into two bad shapes: 100 users on 100
nodes hits every §3 hazard; 100 users on a few nodes get zero isolation — shared quota, shared
token, one FIFO, and a foreground-yield signal that thinks they are one person.

## 5. What the analysis implies — the moves that change asymptotics

Ordered by leverage, not effort. These are design directions, not an order.

1. **Corpus routing must become a first-class stage.** The single biggest cliff, and the one
   the architecture already has vocabulary for (§2.4 of ARCH_PRINCIPLES: open text → centroid).
   A corpus-level centroid/metadata index — embed the query once, one ANN over 1000 corpus
   centroids, take top-K — turns O(N-corpora) per fan-out into O(log N) + K. The experimental
   prefilter validates the idea but does it as N per-index probes; the routing decision needs
   its own small index, not N opens. The peer-side 16-corpus cap shows the system already
   believes a bounded fan-out answers questions.
2. **Wire the fairness that already exists.** `FairScheduler` + `SchedCore` are built,
   tested, cancel-safe — and guard the wrong binary. Per-user identity (even API-key-level)
   plus that scheduler in front of the slot queue converts "one aggressive client = outage"
   into bounded unfairness.
3. **Gossip needs deltas and an enforced ceiling.** The N²·S full-snapshot push is the
   substrate's death; a watermark table already exists in schema. The 12-peer formula exists
   and is unit-tested — enforce or scale it (fanout and threshold as functions of online
   count). Strip embeddings from gossiped notes (re-embed on receipt: prior art says
   cosine ≥0.92 across spaces; or ship binary, not decimal JSON).
4. **Bound every cache and queue that is currently unbounded:** index cache (LRU on handles),
   FastShort channel, knowledge fan-out semaphore — plus surface the 8 MiB replication
   failure at error level (a silent fleet-wide convergence stop is the definition of a gate
   that never ran).
5. **The scheduler objective work (spec §4.1, predicted-time) becomes load-bearing at scale.**
   At N=2 the product objective's blindness is masked; at 100 users the herding (F5),
   constant-Retry-After stampedes, and offload-eagerness (F7) compound. The spec's arm-first
   discipline already exists; scale is the argument for resuming it.

## 6. What is *right* about the current design at this scale

Worth naming, because it is the reuse surface: retrieval is already fully async with a
per-fan-out concurrency knob; the shed path (predicted-wait, 503 + Retry-After) is the correct
*shape* for admission and just needs identity + jitter; custody flags (`query_sharing` /
`mesh_sharing`) mean the 1000-corpora federation story does not require moving bytes; the
peer-side knowledge cap proves bounded fan-out is acceptable to the product; and the sim
(`mesh_sim`) + decision-replay instrument mean scheduler changes can be priced before they
ship — the measurement culture is the scarce asset here, and it transfers to scale work
unchanged.

---

*Sources: three Explore-agent sweeps 2026-08-13 (retrieval: corpus_search.rs /
retrieval_pipeline.rs / engine/mod.rs; scheduling: model_slot.rs / admission.rs /
fair_sched.rs / peer_inference.rs; substrate: gossip.rs / store.rs / backend.rs /
peer_health.rs / iroh.rs), SCHEDULER_QUALITY.md (2,148 lines, findings F1–F11), and the
2026-07-21 31-corpus soak numbers recorded in atom_enum.rs:650-655.*
