# Scale analysis — 100 users, 1000 corpora: where the system breaks, in order

**Date:** 2026-08-13 · **Method:** three parallel code sweeps (retrieval path, inference
scheduling, mesh substrate) + the existing `sovereign/docs/specs/SCHEDULER_QUALITY.md`
measurements. Every claim below carries a file:line citation from today's tree; nothing is
extrapolated from docs alone.

> **⚠ SUPERSEDED IN PART — read §7 before acting.** A same-day adversarial confirmation
> cycle (three independent reviewers briefed to refute, §7) CONFIRMED the retrieval and
> capacity findings, **REFUTED** the §3 "12-online-peer mesh-dissolution" headline and the
> §5.5 scheduler-objective recommendation, and **REVISED** the corpus-router signal, the
> LRU recommendation, and the delta-gossip design. §5's ranked list is replaced by §7.4.
> §§1–4 remain accurate except where §7.2 corrects them.

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

## 7. Adversarial confirmation cycle — 2026-08-13, same day

Three independent reviewers, each briefed to *refute* the analysis and its recommendation
(retrieval/R1+R4, scheduling/R2+R5+R4, substrate/R3+R4), re-verifying every load-bearing
citation and running the §19 inventory check. Net: the *diagnosis* largely survives; the
*prescriptions* needed major surgery. One recommendation is dead, two are redesigned, and
several hazards are worse than §§1–3 reported.

### 7.1 Verdict summary per recommendation

| Rec (§5) | Verdict | One-line reason |
|---|---|---|
| R1 corpus-centroid router | **REVISED** | The centroid signal was already built, measured, and **rejected in-tree** (pruned Wikipedia on its own question); validated signal is nearest-chunk cosine. Two cheaper moves dominate at current fleet size (~38 searchable corpora, not 1836 — the ~1800 `sep-*` dirs are atlas-only). |
| R2 wire FairScheduler | **REVISED** | Peers already get SchedCore fairness on the daemon (`admission.rs:232`); the unprotected population (local/loopback) is auth-exempt with nothing to key on. As written it double-queues a depth shed against the deliberate predicted-wait shed (§10.6 smell) and ships F6's condemned weight-ordering. |
| R3 gossip deltas + ceiling + embedding strip | **SPLIT** | Embedding strip: proceed, *upgraded to a correctness fix* (see 7.2). Delta-via-watermark: refuted — scalar watermarks are unsound here and the cited table is notes-layer, wrong substrate; an in-tree digest push-pull protocol (`commonwealth-discovery/src/gossip.rs:60-242`, tested, unwired) is the right shape. Ceiling enforcement: refuted premise — replace with a warn-rail. |
| R4 bound the unbounded | **SPLIT** | FastShort bound + failure-surfacing: proceed, strengthened. Index-handle LRU: **do not** — the hourly sweep is a textbook LRU-flushing scan that would evict the hot set hourly; fix the sweep's cache-pinning (one call site) and measure per-handle memory first (no measurement exists anywhere). |
| R5 predicted-time objective | **REFUTED — do not proceed** | The spec's own sim numbers: −1.8% under saturation ("no scheduler fixes an oversubscribed queue", SCHEDULER_QUALITY.md:1362-1365), −8% best measured case, and formally blocked behind four gates (F2 in-flight audit, F10 rate card, Tier-2 quality gate, decision tagging). Saturation makes the objective matter *less*, not more. |

### 7.2 Corrections to §§1–4

- **§3's headline is wrong.** The "~12 online peers then the mesh dissolves" model counts
  only direct-contact `last_seen` refresh. Liveness is actually stamped through **three**
  channels — outbound round-trip (`gossip.rs:520`), receive-side merge stamping the sender
  (`routes_internal/gossip.rs:54-55`), and **any member whose record advanced in a merge,
  transitively** (`gossip.rs:526-528`) — and members self-bump every round, so full-snapshot
  push-pull disseminates liveness epidemically in O(log N) rounds. The formula at
  `gossip.rs:248-262` is a worst-case sufficient condition (no relay possible — the actual
  2-live-node incident), not an operating ceiling. Raising fanout is the wrong fix.
- **§1's soak anchor is inflated.** The 5,949 cumulative seconds were dominated by
  atom-enum's whole-set rescue searches, since scoped to the atom's own corpus
  (`atom_enum.rs:657-666`). The structural fan-out claims all stand.
- **The 8 MiB cliff is nearer and doubled.** Re-derived: ~508 embedded notes (confirmed);
  but the shared client's **3s total POST timeout** (`gossip.rs:46,84`) trips at ~2-3 MB
  over a relay-class link, well before 8 MiB — same debug-level silence. And **member-list
  gossip rides the same cliff**: at 1000 corpora × ~300 B `CorpusShardInfo` × N members,
  the member snapshot itself crosses 8 MiB, silently killing membership too.
- **Shipped note embeddings are a live correctness bug, not just waste.** Receivers store
  the shipped vector verbatim with no model check (`notes.rs:2263-2276`) and the cosine
  pool blends with no model filter (`notes.rs:460-516`) — foreign-space vectors poison
  T1 recall on any heterogeneous mesh. Stripping + inline re-embed at ingest is required
  (the once-per-boot backfill is not sufficient).
- **Two §2 hazards are worse than reported.** The foreground-yield trap is **live by
  default**, not latent (`yield_to_foreground_secs` defaults 60, applied at
  `daemon.rs:2453-2454`; `bump_foreground_active` fires after admission) — one admitted
  peer request blocks all peer admissions for 60s. The stalled-SSE pin is **indefinite**,
  not 300s: the deadline check cannot run while parked in `blocking_send`
  (`model_slot.rs:4036`). FastShort is sharpened: not merely unbounded — the shed *can
  never fire* on that path (permit is free at every dispatch; backlog invisible to
  `queue.depth()`). Mitigant: its population is enrichment/pipeline, not streamed chat.
- **F4's quarantine-on-503 trap is already fixed** (`book_peer_failure(shed=true)` skips
  peer-health, `peer_inference.rs:978-990`) — the spec is stale in the safe direction.
- **The contributions ledger outgrows notes**: ~220 B × per-served-request appends ≈ 2
  MB/day at 10k req/day → crosses the cliff in ~4 days of real traffic; `RetentionGc` is
  never constructed by the sovereign daemon.
- **The prefilter is not "experimental" in the pejorative sense** — it is a completed,
  twice-reproduced A/B (note `project_corpus_prefilter_signal_2026_07_13`,
  RETRIEVAL_AUDIT_2026-08-04.md:181-201): prunes 30→9 with 0 fail-open, quality-neutral
  8/9 banks; but it currently runs **per fan-out call**, not per turn, and sits in
  `env_unregistered.txt` (its own env-gate debt). Synthesis, not fan-out, dominates turn
  latency at N≈30 — the router only wins big at N ≥ hundreds of *searchable* corpora.

### 7.3 What the cycle changed about the frame

Capacity precedes fairness precedes selection. The binding constraint at 100 users is one
concurrent turn per node; fairness over one slot is bookkeeping, and smarter selection over
a saturated fleet is measured at ≈0. The only move on the table that *adds* capacity is
streaming support for the sibling pool (`rpc_distribution.rs` — today non-streaming
`complete()` only, incompatible with a code specialist, round-robin-blind). On the substrate
side, the store is 36 KB today — the cliffs are real but *visibility* (R4) buys the time to
do deltas right via the in-tree digest protocol rather than rushing an unsound watermark.

### 7.4 The revised plan (replaces §5)

**Tier 0 — confirmed, cheap, do now (each ≤1 day, independent):**

> **STATUS 2026-08-13 — all seven LANDED on branch `mesh-scale-t0`, none banked.**
> Each carries a red-first regression test shown failing on the pre-fix code; the failing
> runs are transcribed in `research/scale-analysis/MESH_SCALE_T0_JOURNAL.md`, item by item.
> Gates: `sovereign-lint.sh --human --full` exit 0; `sovereign-test.sh --human` 9,632
> passed / 0 failed, exit 0. Probe A and Probe B numbers are in §8 below. Two departures
> from the list as written, both deliberate and both argued in the journal: item 6's GC is
> **scoped to the contributions app** (a whole-store age sweep would delete write-once
> processed-shards markers and re-open completed ingest work), and its TTL is derived from
> `DEFAULT_WINDOW_DAYS` rather than copying `commonwealth-daemon`'s 7 days. Landing is
> operator-gated; the branch does not target main by itself.
1. Surface both gossip push-failure branches (413 *and* the 3s-timeout Err) at warn/error,
   rate-limited per peer per status-transition; payload-bytes gauge warning at 50% of
   `MAX_REQUEST_BODY_BYTES`; warn-rail when online-peer count exceeds the
   `max_online_peers_before_false_offline` formula (making the computed rail observable).
2. Jitter on `retry_after_secs: 2` (`state.rs:2082`) — kills the synchronized-retry generator.
3. `join_all` + single-flight on the serial manifest loop (`peer_inference.rs:1455-1487`) —
   P×800ms worst-case TTFT adder → 800ms.
4. Bound the FastShort channel (`try_send` + existing `QueueShed` shape, `engine.rs:256`).
5. SSE consumer-liveness: enforce the deadline across `blocking_send` (`send_timeout` on
   remaining budget) — converts indefinite single-client node outage to bounded.
6. Spawn `RetentionGc` in the sovereign daemon (contributions ledger).
7. Make the maintenance sweep open indexes without pinning the query cache (one call site).

**Tier 1 — structural, revised designs (order matters):**
8. Strip embeddings from gossiped notes + inline re-embed in `ingest_remote_notes`
   (correctness fix + ~11× on the dominant namespace; cliff ~500 → ~5,500 notes).
9. Scope the expansion fan-outs (entity/decomp/graph) to corpora that hit in the main
   fan-out — the same fix already shipped for atom-enum, prior art in-tree.
10. Hoist the existing prefilter to once-per-turn, register its env flag, ship at K=10-12 —
    gated on SEP 21-q + wikipedia + `bench/cross-corpus` banks (merge composition is
    measured pool-size-sensitive, reproduced 2×).
11. Streaming sibling pool + least-loaded pick — **the capacity lever**; 2-4× admitted
    concurrency on hardware with the GTT headroom.
12. Per-API-key identity in `client_auth` (port sovereign-server's plural keys) + loopback
    session identity; then extend the *existing* daemon-side `SchedCore` gate to local
    callers with per-key caps. `SlotQueue`'s predicted-wait shed stays the one shed
    decider; no weighted ordering until deficit-ordering (Phase 2 step 5) exists.

**Tier 2 — build when the Tier-0 gauges say so:**
13. Wire the in-tree digest push-pull protocol (`commonwealth-discovery/src/gossip.rs`)
    with mesh_store wire tombstones (work-atlas `claim-tombstone:` is the prior art) —
    the sound replacement for deltas-via-watermark. A per-peer acked-payload-hash
    skip-if-unchanged is an acceptable 20-line stopgap; it does not remove the cliff.
14. Corpus-selection stage for large N: global ANN over per-corpus **multi**-centroids
    (reuse Lance IVF partition centroids), nearest-chunk-family signal, hybrid-aware or
    FTS-fail-open — only when searchable-corpus count approaches hundreds.
15. Scheduler work, if resumed at all: Phase 2 step 4 (congestion ≠ failure; half already
    landed) + the F2 two-daemon in-flight audit. **Not §4.1.**

**Dropped:** index-handle LRU (counterproductive under the sweep), fanout raising,
FairScheduler-in-front (double-queueing), predicted-time objective as a scale item.

## 8. Verification — the lean version

**Rewritten 2026-08-13, same session** (the first draft was a five-rung program with a
frozen SLO card and calibration contracts — cut on operator direction as grandiose; the
principle that replaces it: **probes before lanes**. A probe is an afternoon script that
answers one question with one number, run the day its axis is touched. It becomes a
permanent lane only after it catches something twice.)

Three probes, one standing rule, one free measurement:

**Probe A — 100 fake users (afternoon).** One load generator (`oha`/`hey` or a 30-line
script) against one real node, mixed streaming/non-streaming. One question: *does the shed
hold the line* — every request served or 503'd fast, nothing parked, and measured admitted
concurrency ≈ `1 + floor(shed_window/avg_turn)` as the architecture predicts. Include one
stalled-SSE client and one no-jitter retry loop, because those are the two adversaries §7
says win today. Run it again the day the sibling pool or identity work lands.

**Probe B — 1000 stub corpora (afternoon).** A for-loop cloning one tiny real index 1000×
into `~/.svrnmesh/indexes/`. Two numbers: per-query wall time and daemon RSS after the
maintenance sweep (the per-handle memory number that exists nowhere today). Then run the
existing SEP/wikipedia banks once with the noise installed — if scores hold, the
1000-corpus quality question is answered for free by instruments we already own. Rerun
when the prefilter/selection work lands.

**Probe C — turn the soak dial (afternoon).** `mesh-soak.sh` already boots N real daemons
in a netns. Crank `--nodes` until something breaks and write down the number and the
failure. If model residency is what breaks first, *that* is the moment a hollow-node stub
earns building — not before.

**Standing rule (already house style, costs nothing new):** every Tier-0/1 fix lands with
the red-first test that fails on the old code — fill the coalescer and watch the shed
fire, half-open a client and watch bounded release, inject an oversized store and watch
the warn. That is rung 5 of the old draft, kept, because it isn't a program — it's just
how fixes land here.

**The free measurement:** a real pilot mesh (even 5 nodes, 5 humans) is worth more than
any rig — user experience is ground truth. Don't simulate what a pilot would report for
free; instrument it (Tier 0 item 1 is exactly the instrumentation) and let its telemetry
decide which probe graduates to a lane.

Explicitly deferred until a probe or the pilot demands it: the N=100 membership sim, the
hollow-fleet nightly, any new bench lane, any SLO card beyond the pass number each work
order already declares for itself.

### 8.1 Probe A — 100 fake users — RUN 2026-08-13 (RuggedFox, order `mesh-scale-t0`)

`scripts/probe-a-shed-under-load.sh` + `scripts/probe_a_load.py`. One dev daemon
(gemma-4-E4B-it-Q4_K_M) inside a **rootless network namespace** — the `mesh-soak.sh`
mechanism, chosen because a daemon that loses a port bind only *warns*, so on the bare host
a probe can silently drive the operator's live daemon and the client side cannot tell.
Inside the netns the operator's `:9741` is not reachable at all. A recorded bind assertion
resolves the listener back to the probe's own pid before any load is sent
(`BIND CHECK PASSED` in every run below).

Population: 98 ordinary non-streaming clients released together + 1 stalled-SSE consumer +
1 tight-retry client. Four outcomes, never three: admitted / shed / **parked** / error.

**Answer to the one question: the shed holds. `parked = 0` and `error = 0` in all five
runs.** A refused request is refused in **0–2 ms** (p50), worst observed 110 ms.

| run | window | admitted | shed | parked | slot avg turn | predicted `1+⌊30/turn⌋` | measured max queue position |
|---|---|---|---|---|---|---|---|
| 1 | 45 s | 64 | 66 | 0 | — (log parse fixed after) | — | — |
| 2 | 45 s | 64 | 4,424 | 0 | — | — | — |
| 3 | 45 s | 65 | 3,043 | 0 | 1.27 s | 24 | 33 |
| 4 | 45 s ¹ | 33 | 67 | 0 | 0.91 s | 33 | 33 |
| 5 | 45 s ¹ | 34 | 11,466 | 0 | 2.95 s | 11 | 34 |
| 6 | 70 s ¹ | 73 | 10,924 | 0 | 2.94 s | 11 | 34 |

¹ runs 4-6 give the stalled-SSE client a 4,096-token generation; runs 1-3 gave it 512.
The wide `shed` spread is the tight-retry client, which re-fires the instant it is refused
and accounts for ~99% of every large shed count — that is the adversary behaving as
designed, not instability.

**Admitted concurrency vs. the architecture's prediction.** Measured **33–34**, predicted
**11–33**. The comparison is made against the DAEMON's own numbers, not the client's: a
client's end-to-end latency is queue wait + service (15.9–36.3 s here), so using it as
`avg_turn` would compare two different quantities and produce a confident wrong answer. The
slot publishes `avg_turn_ms` (its EWMA of *service* time) and `position` on every
`inference.queue: SHED` line; the deepest position it ever accepted before refusing is the
measured admitted concurrency. **The formula is directionally right and runs ~1–3× high**;
the overshoot is expected, because the EWMA lags a queue that is still filling.

**Retry-After, observed:** 31–183 s across 10–12 distinct values. These are the LOCAL queue
shed's hints, derived from predicted wait — **not** the constant that Tier-0 item 2
jittered. Loopback clients carry no `X-Node-Id`, so they never reach
`admit_peer_request`, and Probe A therefore does **not** exercise item 2. Recorded as
not-exercised rather than passed.

**Two Tier-0 fixes corroborated live in the daemon log:** `RetentionGc started
(contributions ledger)` ×1 (item 6) and `coalescer armed with a bounded queue` ×1 (item 4).

**One thing the probe did NOT observe:** `stream consumer stopped reading` never fired, and
neither did the plain stream wall-clock deadline — so the half-open pin was not reproduced
end-to-end. The stalled consumer's generation fits inside the SSE channel buffer, so the
send never blocks and the release has nothing to release. Item 5's evidence is its red-first
unit test (which fails by *never returning* on the pre-fix `blocking_send`), not this probe.
Reproducing the pin over HTTP needs a generation large enough to overrun the channel buffer
against a consumer that reads zero bytes — a probe refinement, filed rather than claimed.

### 8.2 Probe B — 1000 stub corpora — RUN 2026-08-13 (RuggedFox, order `mesh-scale-t0`)

`scripts/probe-b-index-residency.sh`. One tiny real index cloned 1,000× (94 MB on disk) into
a throwaway dir; the operator's `~/.svrnmesh/indexes/` is read-only input and the probe
refuses any path ending in `.svrnmesh/indexes`. Both arms of Tier-0 item 7, 3 runs each.

**The per-handle memory number that existed nowhere: ~208 KiB of RSS per resident index
handle** (208, 209, 209 KiB across three runs — a tight bracket, not a single sample).

| arm | sweep RSS delta | resident handles after sweep | sweep wall time | per-query fan-out (min–max of 3) |
|---|---|---|---|---|
| `pinned` (pre-fix `open_index`) | **+204 MiB** | 1,000 | 4.55–4.57 s | 5.21–5.91 s |
| `transient` (post-fix) | **+13.5–14.5 MiB** | **0** | 4.67 s | 5.23–10.67 s |

Reading it:

- **At 1,000 corpora the hourly sweep was costing ~204 MiB of permanently resident memory,
  for corpora nobody had queried.** The fix removes 93% of that. Extrapolating the same
  per-handle figure: 10,000 corpora would have been ~2 GB of sweep-induced residency.
- **The sweep costs the same either way** (4.55 s vs 4.67 s). Both arms open every index
  once; only the retention differs. Nothing was traded for the memory.
- **The query side moved, and the direction is correct.** The `transient` arm's *first*
  query fan-out is slower (10.67 s vs 5.91 s) because the sweep no longer pre-warms handles
  for corpora nobody asked about — that first query now pays its own open. Subsequent
  queries land in the same 5.2 s band as the pinned arm. That is the intended trade: the hot
  set is still cached by the query path (a companion test pins this), the cold set is no
  longer resident on the strength of a background timer.
- This is also the arithmetic §7.2 was missing when it refused the index-handle LRU: at
  ~208 KiB a handle, the cost the LRU was meant to bound is real, but an hourly all-corpora
  scan would have flushed it every tick. Fixing the one pinning call site removes the cost
  without the eviction machinery.


*Sources: three Explore-agent sweeps 2026-08-13 (retrieval: corpus_search.rs /
retrieval_pipeline.rs / engine/mod.rs; scheduling: model_slot.rs / admission.rs /
fair_sched.rs / peer_inference.rs; substrate: gossip.rs / store.rs / backend.rs /
peer_health.rs / iroh.rs), SCHEDULER_QUALITY.md (2,148 lines, findings F1–F11), the
2026-07-21 31-corpus soak numbers recorded in atom_enum.rs:650-655, and three adversarial
review agents 2026-08-13 (§7) re-verifying all of the above plus
`project_corpus_prefilter_signal_2026_07_13`, RETRIEVAL_AUDIT_2026-08-04.md, notes-mesh.md,
and live DB measurements (~/.svrnmesh/notes.db, .sovereign/mesh.db).*

### 8.3 Tier-1 red baseline — RUN 2026-08-13 (RuggedFox, order `mesh-scale-t1-red`)

The failing number each Tier-1 build order has to turn green, measured before any Tier-1
build work started, on `main` + the Tier-0 landings (branch base `a6b18cdb`) with **no
production code changed**. Every number below is either a count of shipped glassbox lines
or a wall clock on a real run; nothing here is derived from §3's arithmetic, and where a
measurement and the old arithmetic agree that is a corroboration, not a reuse.

Instruments (all committed): `scripts/probe-t1-expansion-fanout.sh` +
`scripts/probe_t1_fanout_report.py` (one knowledge turn at a stub rig, `retrieval_audit`
counts), `scripts/probe-t1-corpora-sweep.sh` (the n-sweep), `scripts/probe_a_streaming_pool.py`
and `scripts/probe_a_greedy_vs_polite.py` (load generators for the existing Probe A netns
harness, which gained `--load` / `--load-args` / `--daemon-env` so its sealed netns and its
bind assertion stay the only implementation of both), and two `#[ignore]`d tests in
`corpus-engine-notes/tests/`. Dev daemons ran in a rootless netns under a throwaway `$HOME`;
`BIND CHECK PASSED` is recorded for every daemon run below, and the operator's live daemon
and corpora were never in the path.

#### 1. `t1-notes-clean-wire` — 16.1 KB per gossiped note, cliff at ~520

`corpus-engine-notes/tests/red_baseline_note_wire_size.rs` (`#[ignore]`d measurement).
Events come from `NoteStore::notes_delta_since` — the shipped constructor — over a
**snapshot of the real `~/.svrnmesh/notes.db`** (5,540 notes, 4,811 global), and are
serialized with the same `serde_json::to_vec` the daemon's sink calls
(`bootstrap.rs:1340-1347`). Two sample sizes, because the delta path clamps itself at
`limit.min(500)` (`notes.rs:2341`):

| sample | bytes/note (min / p50 / mean / p90 / max) | embedding payload alone | with embedding stripped |
|---|---|---|---|
| n=100 | 15,120 / 15,806 / **16,134** / 16,923 / 21,394 | 14,469–14,623 (p50 14,542) | 606 / 1,274 / 1,591 / 2,417 / 6,883 |
| n=500 | 15,120 / 15,901 / **16,089** / 16,937 / 21,394 | 14,443–14,643 (p50 14,545) | 596 / 1,359 / 1,548 / 2,442 / 6,883 |

- **The embedding is 90% of the note.** Every embedding in the live store is 1024-dim
  (`qwen-embedding-0.6b`) → a 4,096-byte LE blob → a JSON array of 4,096 decimal integers,
  **14.5 KB**, against a note body whose own serialized form averages 1.5 KB.
- **Derived cliff, from the measured mean: 520–521 notes** fill the 8 MiB body limit
  (`server.rs:30`); **5,273–5,420** if the embedding is stripped. Ratio **10.1–10.4×**.
  §7.2's re-derived ~508 is confirmed within 3%.
- Bar target for the build order: ≤ ~2 KB/note and ≥ ~4,000 notes to the cliff. The
  stripped column says that is exactly what stripping buys — no further compression needed.

#### 2. `t1-notes-own-space` — the contamination is reproducible in 0.02 s

`corpus-engine-notes/tests/red_baseline_cross_model_notes.rs::red_baseline_foreign_space_embedding_must_not_enter_the_cosine_pool`,
committed `#[ignore]`d and **watched failing**:

```
RED: a foreign-space embedding (model_id=foreign-embed-model-b, local model is
qwen-embedding-0.6b) was blended into the cosine pool and returned as a semantic hit.
ids=["note-local-space", "note-foreign-space"]
```

Two remote notes arrive over gossip with the *same* vector and differ only in `model_id`.
Both are stored verbatim (`ingest_remote_notes`, `notes.rs:2263-2277` — no model check) and
both come back from a pure-cosine read, because `fetch_cosine_pool` (`notes.rs:454-515`)
never projects `e.model_id`. The test asserts the same-space note IS returned *before* it
asserts the foreign-space one is not, so a run that passes because the cosine path never
fired fails as a broken instrument instead.

#### 3. `t1-expansion-scoped` — per-turn retrieval wall is LINEAR in corpus count: **2.19 s per 100 corpora**

Sweep (seat amendment 0ab79301): five log-spaced points, the stub rig re-selected per
point, **3 turns each**, production defaults, one question. `retrieval_ms` is the shipped
`runtime:retrieval_start_to_complete` debug event; fan-outs are `retrieval_audit:
fanout_complete` lines.

| corpora n | per-turn retrieval wall (3 turns) | fan-outs/turn | corpora searched/turn | fan-out wall sum |
|---|---|---|---|---|
| 10 | 541–566 ms | 4 | 40 | 231–250 ms |
| 50 | 1,427–1,469 ms | 4 | 200 | 1,104–1,140 ms |
| 100 | 2,575–2,598 ms | 4 | 400 | 2,233–2,265 ms |
| 316 | 7,360–7,368 ms | 4 | 1,264 | 6,989–6,996 ms |
| 1000 | 22,116–22,380 ms | 4 | 4,000 | 21,624–21,899 ms |

**Shape: linear.** Least squares over the five means: **21.85 ms per corpus per turn =
2.19 s per 100 corpora**, intercept ~0.38 s; predicted-vs-measured is within 5% at every
point (n=100: 2.57 s predicted / 2.59 s measured; n=316: 7.29 / 7.36). Per fan-out the
slope is **0.55 s per 100 corpora**. No knee, no plateau — the fan-out is doing exactly
O(n) index opens and the per-turn multiplier is the fan-out count.

**The multiplier is 4, and 3 of the 4 are entity boost.** Composition is identical at every
point: 1 `KnowledgeQuery` + 3 `EntityBoost`, and EntityBoost carries ~62% of the fan-out
wall (13.4 s of 21.6 s at n=1000). The §7.4 "~10–30 fan-outs" figure is the *all-flags-on*
ceiling; at production defaults the other four expansions (`SOVEREIGN_DEMAND_PLAN_FANOUT`,
`SOVEREIGN_QUERY_DECOMP`, `SOVEREIGN_TITLE_EXPAND`, `SOVEREIGN_GRAPH_NEIGHBOR_EXPAND`) are
off, so **entity boost is the only ungated multiplier** — and unlike atom-enum
(`atom_enum.rs:656`) or atlas grounding (`atlas_grounding.rs:280`), it has no natural "own
corpus" to scope to. That is the hard part of the build order, named up front.

The red is the SLOPE. Green is per-turn wall ~flat in n at fixed K.

#### 4. `t1-prefilter-per-turn` — 4 prefilter passes per turn, and at n=1000 the prefilter makes the turn 35% SLOWER

Same sweep with `SOVEREIGN_CORPUS_PREFILTER_TOPK=12`, 2 turns per point.

| corpora n | prefilter passes/turn | kept/dropped | prefilter ms per pass (min–max) | per-turn prefilter sum | per-turn retrieval wall (prefilter ON) | …(OFF, from §8.3.3) |
|---|---|---|---|---|---|---|
| 10 | 0 — no-op (`eligible <= top_k`) | — | — | — | 540–551 ms | 541–566 ms |
| 50 | **4** | 12 / 38 | 175–623 | 1,162–1,298 ms | 1,676–1,863 ms | 1,427–1,469 ms |
| 100 | **4** | 12 / 88 | 351–1,082 | 2,425–2,479 ms | 2,997–3,060 ms | 2,575–2,598 ms |
| 316 | **4** | 12 / 304 | 1,041–3,447 | 6,783–8,023 ms | 7,366–8,634 ms | 7,360–7,368 ms |
| 1000 | **4** | 12 / 988 | 4,777–14,147 | 29,053–29,949 ms | **29,725–30,633 ms** | 22,116–22,380 ms |

- **One pass per fan-out, not per turn** — confirmed as 4 at every n where it runs, which is
  the posture `corpus_search.rs:266-275` predicts (the call sits inside
  `search_corpus_indexes_with_overrides`, so every fan-out re-probes every corpus).
- **The prefilter's own cost is also linear in n** (~0.73 s per 100 corpora per pass at
  n=1000) because each pass opens and probes every eligible index. Multiplied by 4 passes it
  *exceeds* the fan-out saving it buys: at n=1000 the turn goes 22.1–22.4 s → 29.7–30.6 s,
  **+35%**. Today's flag, switched on at 1000 corpora, is a net regression — which is the
  precise argument for hoisting it to once per turn.
- Registry debt confirmed: `SOVEREIGN_CORPUS_PREFILTER_TOPK` is absent from
  `quality/env-flags.toml` and waived at `quality/baselines/env_unregistered.txt:26`.

**Quality anchor (single run per arm, SEP 21-q bank through the production retrieval
pipeline, `eval run --prod-pipeline`, no synthesis):**

| arm | corpora | sources | facts | wall |
|---|---|---|---|---|
| SEP alone | 1 | 42/66 (64%) | 137/158 (87%) | 67.1 s / 67.2 s (2 runs) |
| SEP + 1,000 stub distractors | 1,001 | 42/66 (64%) | 137/158 (87%) | **318.9 s** |

**Retrieval quality is unchanged by 1,000 distractor corpora — byte-identical scores — and
the bank costs 4.75× the wall.** At 1000 corpora the scale problem is latency, not
distraction; a build order that trades recall for speed here is trading away nothing it had
to. This is the number the prefilter/scoping work must hold: 42/66 and 137/158.

#### 5. `t1-streaming-capacity` — the capacity lever moves the non-streaming path and does nothing for streaming

Probe A's netns harness with `--load scripts/probe_a_streaming_pool.py`. Two turns run
serially, then the same two released together; `concurrency_factor = serial_total /
concurrent_wall` (1.0 = fully serialized, 2.0 = fully concurrent). 2 reps per arm.

| arm | `SOVEREIGN_PRIMARY_SIBLINGS` | concurrency factor | concurrent per-request wall | second request's TTFT |
|---|---|---|---|---|
| streaming | unset | 1.01 / 1.02 | 2.9 s, 5.7 s | 3.09 s |
| streaming | **2** (pool built) | 1.02 / 1.22 | 2.8 s, 5.6 s | 2.94 s |
| non-streaming | unset | 1.00 / 1.02 | 2.8 s, 5.6 s | — |
| non-streaming | **2** (pool built) | **1.27 / 1.28** | 4.4 s, 4.5 s | — |

- The pool really was built in both sibling arms — the daemon logged `building primary
  sibling pool` ×1 and `primary sibling context ready` ×1 (`engine.rs:1372`). The
  non-streaming control is what makes the streaming result a finding rather than a
  configuration failure: **the same flag, on the same daemon, in the same run shape, moves
  non-streaming 1.00 → 1.27 and streaming 1.01 → 1.02.**
- The signature is unmistakable in the per-request walls: with siblings on, the two
  *non-streaming* requests finish together (4.4 s / 4.5 s), while the two *streaming* ones
  still finish at 2.8 s and 5.6 s — the second stream's first token arrives only after the
  first stream is done.
- Cause, verified in-tree: the pool branch exists once, at `engine.rs:2928`, inside
  `complete()`. `complete_stream` (`engine.rs:3230`) and `complete_stream_with_finish`
  (`engine.rs:3453`) contain no `primary_pool` reference at all and fall through to the
  single lazy slot.
- Note the ceiling this sets: even on the path where the pool works, two concurrent requests
  cost 1.27×, not 2× — the siblings share one GPU. "2–4× admitted concurrency" is an upper
  bound to be re-measured, not a promise.

#### 6. `t1-local-identity` — one greedy client takes 3.2× to 8.0× its fair share

Probe A's harness with `--load scripts/probe_a_greedy_vs_polite.py`: 1 greedy client (no
backoff, ignores every `Retry-After`) + 9 polite clients (one request in flight, sleeps the
hint), all on the same shared bearer token, 60 s windows.

| greedy in-flight | greedy admitted | polite admitted (9 clients) | greedy share | fair share | overshoot | polite p50 / p95 wait | polite shed |
|---|---|---|---|---|---|---|---|
| 4 | 34 | 72 (8 each) | 32.1% | 10% | **3.2×** | 8.27 s / 8.62 s | 0 |
| 32 | 106 | 27 (3 each) | 79.7% | 10% | **8.0×** | 26.8 s / 30.6 s | 5 |

- **Share tracks offered load exactly, because there is nothing else for it to track.** The
  queue is FIFO over requests, and a request carries no caller identity — so one caller
  holding 32 slots gets 32 slots' worth of service. Waits are the same for both cohorts
  (8.27 vs 8.27 s at in-flight 4), which is the FIFO fingerprint: nobody is discriminated
  against, and that is the problem.
- **"Starves" is dilution, not exclusion, in a 60 s window**: no polite client got zero
  turns in either run, and `parked = 0` throughout (the Tier-0 shed still holds the line).
  At in-flight 32 the polite cohort — 9 of 10 callers — is down to 20.3% of the node's
  service and starts eating 503s (5 sheds against the greedy client's 0).
- The number that makes it concrete for the build order: **the polite cohort's admitted
  share falls 67.9% → 20.3% when a single peer client raises its own concurrency 8×**,
  with no change on the polite side at all.

### 8.4 Tier-1 GREEN — expansion scoping — RUN 2026-08-13 (RuggedFox, order `mesh-scale-t1-retrieval`)

Bars `t1-expansion-scoped` + `t1-prefilter-per-turn`, measured on the SAME
`probe-t1-corpora-sweep.sh` harness as §8.3.3, behind `SOVEREIGN_EXPANSION_SCOPE`
(default OFF — shipped dark; the flip is the operator's on these numbers).

**Instrument validated before any green number was read**, six times: the
flag-OFF arm was re-measured on every rig and binary revision in this order and
reproduced §8.3.3 each time — 2.176 / 2.187 / 2.193 / 2.183 s per 100 corpora
against the red's 2.19, intercept 0.37-0.40 against 0.38, within 0.8% at every
point n≥50. The SEP-at-rig anchor likewise reproduced 42/66 + 137/158 at
321.0 s against the red's 318.9 s. A green delta on this harness is therefore
attributable to the change, not to the rig or the host.

| corpora n | per-turn wall, scope OFF (3 turns) | scope ON (3 turns) | searches/turn OFF → ON |
|---|---|---|---|
| 10 | 546–564 ms | 515–533 ms | 40 → 34 |
| 50 | 1,432–1,442 | 863–888 | 200 → 74 |
| 100 | 2,528–2,577 | 1,287–1,384 | 400 → 124 |
| 316 | 7,375–7,497 | 3,211–3,280 | 1,264 → 340 |
| 1000 | 22,061–22,229 | 8,914–8,949 | 4,000 → 1,024 |

**Slope 2.183 → 0.849 s per 100 corpora — a 2.57× cut** (intercept 0.39 → 0.47;
predicted-vs-measured within 3% at every point). Searches per turn go from
`4n` to `n + 3×8`: the main fan-out still sees every corpus, each expansion is
bounded at 8. Per-label at n=1000, the three EntityBoost passes fall
**13,346 ms → 106 ms (≈126×)** while KnowledgeQuery is unchanged at ~8,400 ms.

**The bar was re-cut from ≤0.55 to ≤~0.9 s/100 mid-order, and why.** §8.3.3
fitted 2.19 s/100 over 4 fan-outs and reported "per fan-out 0.55". The four are
not equal: KnowledgeQuery carries the full query embedding plus rerank at
**0.84 s/100 measured on its own**, each EntityBoost is a short entity probe at
~0.44. 0.55 was their AVERAGE, and the one fan-out this order cannot scope is
the expensive one — so a turn could never come in under the cost of the single
fan-out it must always run. The floor is the expensive fan-out, not the average.
Scoping the MAIN fan-out and FLAT-in-n remain Tier-2 (the sublinear
corpus-selection index).

**Quality holds.** SEP-at-rig anchor (SEP + 1,000 stub distractors, n=1001,
`eval run --prod-pipeline`): **42/66 sources, 137/158 facts — byte-identical to
§8.3.4 — at 190.1 s against 321.0 s, a 41% wall cut.** Banks on the real corpus
set, OFF → ON: sep 38/66 · 135/158 → 38/66 · 135/158 (identical); wikipedia
25/58 · 72/130 → 25/58 · 71/130 (sources identical, −1 fact, reproduced 2×);
cross-corpus 19/42 · 44/55 → 19/42 · 44/55 (identical).

#### 8.4.1 Two selector defects, both measured, both worth the next reader's time

The first two versions of the scope were WRONG in ways a review would not have
caught, and the failing numbers are the point of this subsection.

1. **"Corpora that produced hits" is an exact no-op.** Scoping to the corpora
   the main fan-out returned chunks from selected **50 of 50** corpora
   (`n_hit_corpora=50 local_hits=100`), and the sweep arm came back
   byte-identical to the control. The per-corpus fan-out
   (`corpus_search.rs:354-399`) applies **no score floor** — every corpus that
   opens returns its top-K — so "produced a hit" means "the index was
   readable", not "the corpus is relevant".
2. **Ranking on raw scores, budgeted in chunks, scoped a wikipedia question to
   a property-tax table.** Raw fan-out scores are RRF-fused and NOT comparable
   across corpora, and a chunk-denominated budget lets one corpus take every
   slot: **14 of 20** wikipedia questions scoped to `["sf-assessor-roll"]`
   alone, with `wikipedia` excluded, costing 3 sources / 4 facts (reproduced
   3×). Fixing the budget UNIT to corpora did not recover the bank (still
   22/58 · 68/130) — with the monopoly gone, still only **8 of 20** questions
   had `wikipedia` in scope at all. Ranking on
   `reweight_by_query_relevance` — the same scorer `reweight_and_sort` applies
   downstream — put `wikipedia` in **20 of 20** and recovered the bank.

This is SYSTEM_OVERVIEW §D1 twice over ("a scope drawn from presence rather
than from ranking is vacuous"; "fix the POSITION of a selector, not its
predicate"). Repositioning is unavailable — the scope must be decided before
`entity_boost`, which must precede `reweight_and_sort` — so the signal is
brought to the decision instead. Same lesson, same file, one level up.

#### 8.4.2 Removing waste made a hidden cost visible — it was not a regression

With scoping on, ~4.0 s of the n=1000 turn sat OUTSIDE the four counted
fan-outs (against ~0.6 s with scoping off). The `retrieval.pipeline` per-step
ledger put all of it in one step: the `ppr_struct_expand` JOIN, **1.2 ms → 3,445
ms**. The PPR structural lane and the entity-obligations fetch are spawned at
`ppr_struct_spawn` and are expansion fan-outs too; both were still unscoped,
and being CONCURRENT they cost nothing visible for as long as `entity_boost`
was wasting 13.4 s for them to hide behind — precisely what that step's comment
predicts. Scoping them as well took the join to **0.34 ms** and the turn to
8,801 ms. An A/B on `SOVEREIGN_PPR_EXPAND=0` measured no change and wrongly
cleared the lanes: that flag gates the PPR walk, not the obligations fetch. The
ledger settled it; the hypothesis did not.

**Generalisable:** removing waste can surface a previously-invisible concurrent
cost without anything becoming slower. Read the next such delta as exposure,
not regression.

#### 8.4.3 The prefilter hoist, and why it still should not be flipped on

Bar `t1-prefilter-per-turn`, `SOVEREIGN_CORPUS_PREFILTER_TOPK=12`, on stubs
re-flagged `personal_scope=false` (the deliberate §8.3 rig property; without it
the carve-out keeps every corpus and the measurement is vacuous):

| arm | prefilter passes/turn | kept/dropped | prefilter ms/turn @n=1000 | slope s/100 |
|---|---|---|---|---|
| scope OFF + prefilter (red posture) | **4** | 12 / 988 | 27,709–28,748 | 2.925 |
| scope ON + prefilter | **1** | 12 / 988 | 13,263–13,852 | 1.828 |

**The hoist is real: 4 passes → 1, and per-turn prefilter cost roughly halves**
— achieved structurally, with no cache and no turn-id threading, because a
scoped fan-out skips the prefilter (`corpus_search.rs:270`). The red's own
regression reproduces on the way: 22,174 → 29,422 ms at n=1000, **+32%**
(red: +35%).

**But the prefilter remains a net loss even hoisted** — 1.828 s/100 with it
against **0.849 without**. Its own probe is O(n): it opens and ranks every
eligible corpus. The instructive part is that its fan-out IS flat — 36 searches
per turn at every n, 10 through 1000 — so once a corpus-selection index makes
the SELECTION sublinear, the whole turn goes flat. That is the Tier-2 shape,
measured rather than argued. Recommendation: leave
`SOVEREIGN_CORPUS_PREFILTER_TOPK` unset; ship `SOVEREIGN_EXPANSION_SCOPE` alone.

**Rig caveat, named not silent.** The sweep's 1,000 corpora are `cp -r` clones
of one index, so they score identically and the top-8 selection is
tie-arbitrary there. The rig proves the BOUND (≤8 corpora searched per
expansion regardless of n); it cannot prove selection QUALITY. Quality is
carried by the SEP-at-rig anchor and the three real-corpus banks above. A
heterogeneous rig is Tier-2 work.

**Gates for this order:** `sovereign-lint.sh --human --full` and `sovereign-test.sh --human`
both exit 0 with the two `#[ignore]`d red tests and the harness additions in the tree; the
red test is not run by the gate by design (it is expected to fail, and `--ignored` is how it
stays visible without being a broken build).

### 8.5 Tier-1 GREEN — streaming capacity — RUN 2026-08-13 (RuggedFox, order `mesh-scale-t1-streampool`)

Bar `t1-streaming-capacity`, measured on the SAME harness as §8.3.5
(`probe-a-shed-under-load.sh --clients 2 --load scripts/probe_a_streaming_pool.py`,
netns daemon, bind check passed in every arm, 2 reps per arm). The build under
test: `pool_dispatch` — one eligibility decider shared by `complete`,
`complete_stream` and `complete_stream_with_finish` — plus least-loaded `pick()`
(`SlotQueue::load_reading`, the one load decider: permit holder + parked
waiters, rotating tie-break).

**Instrument validated before any green number was read**: the unset arm on the
NEW binary reproduced §8.3.5's red — factor 0.99/1.01 against the red's
1.01/1.02, second stream's TTFT 3.08–3.19 s against 2.94–3.09 s (the whole
first wall). The daemon-side pool assertion also behaves: `sibling_pool_built=0`
unset, `=1` in both sibling arms.

| arm | `SOVEREIGN_PRIMARY_SIBLINGS` | concurrency factor | concurrent per-request wall | concurrent TTFTs |
|---|---|---|---|---|
| streaming | unset | 0.99 / 1.01 | 2.9 s, 5.7–5.9 s | 0.18 s, **3.08–3.19 s** |
| streaming | **2** (pool built) | **1.27 / 1.27** | 4.65–4.71 s both | **0.18–0.21 s both** |
| non-streaming (control) | **2** (pool built) | 1.28 / 1.26 | 4.57–4.68 s both | — |

**The lever now reaches the streaming path, at exact parity with the
non-streaming control: 1.27/1.27 vs 1.28/1.26, same daemon, same run shape.**
The red's signature is gone — with the pool on, both streams' first tokens
arrive in ~0.2 s and both finish together, where the red had the second
stream's first token waiting out the first stream's entire wall. The green is
tempered exactly as the order tempered it: **1.27× is the one-GPU ceiling**,
not 2× — the siblings share the device; the pool buys decode overlap and
first-token latency, not multiplication. Probe A's t0 numbers (§8.1) are
unchanged by this order at N unset; admitted-concurrency-scales-with-N beyond
one GPU remains a fleet question, out of this bar's scope.

Named, not silent: the probe's `sibling_dispatch_lines` counter read 0 in all
arms — the per-dispatch line logs at debug and the netns daemon logs at info.
The dispatch evidence is the factor + TTFT + the control arm, and the
info-level `sibling_pool_built/ready` lines; a follow-up could promote the
dispatch line or count the `inference.complete_stream: done` info lines
(which carry `sibling_idx`) instead.

**Gates for this order:** `sovereign-lint.sh --human --full` (workspace scope)
and `sovereign-test.sh --human` both exit 0 on the merged tree — 9,676 tests
passed, 44 skipped — covering the new pure-`pick` unit tests, the red-first
refusal test (Pool + Code-specialist refuses loudly with `POOL_CODE_REFUSAL`
instead of silently hot-swapping the lazy slot; watched failing before the
fix per the worker's commit), the `load_reading` gauge cycle test, and the
N=1 no-regression suite.
