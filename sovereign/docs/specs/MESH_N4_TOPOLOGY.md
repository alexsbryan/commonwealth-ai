# Four peers, four places — mesh topology design for N=4

**Status:** design. Written 2026-08-05. **M1 shipped** (§7); M2–M5 are proposals.
The milestones in §7 are experiments, and each records whether it has been run —
`NEVER-RAN` is a status, not a blank.
**Scope:** four roughly-identical nodes in four physical locations, any of which
can originate a request, sometimes concurrently — **plus consumers that hold no
model at all** (§4.5, M6). The realistic six-node fleet is four holders and two
thin clients, not six peers, and the consumer is where a person actually touches
this system. This is the topology the code
has never been designed for — every operational doc assumes one named host you
talk to, and `SCHEDULER_QUALITY.md` F1 says the quiet part out loud: *"the reason
the current stack 'just works' solo and on a pair, and why no existing test can
see the problem: **every test has one decider.**"*

Grounded throughout in the two models actually on disk, read from their GGUF
headers (`scripts/`-adjacent scratch parser, 2026-08-05) and from measurements
filed in `~/.svrnmesh/mesh-measurements.json`. No figure below is recalled.

---

## 1. The two facts that decide everything

Both models are sparse MoE, and both are overwhelmingly expert weight:

| | Qwen3.5-122B-A10B Q5_K_XL | DeepSeek-V4-Flash Q4_K_XL |
|---|---|---|
| blocks | 48 | 43 |
| `n_embd` | 3072 | 4096 |
| KV heads | 2 (GQA) | **1 (MQA)**, k/v len 512 |
| experts | 256, **8 used** | 256, **6 used** |
| total tensor bytes | 85.6 GiB | 144.4 GiB |
| **routed experts** | **79.4 GiB — 92.8%** | **137.1 GiB — 94.9%** |
| **dense backbone** | **6.2 GiB — 7.2%** | **7.3 GiB — 5.1%** |
| expert bytes touched/token | 2.48 GiB | 3.21 GiB |
| activation at a pipeline boundary | 12 KiB | 16 KiB |
| KV per token (f16) | 96 KiB | 86 KiB |

**Fact one: the dense backbone of a 144 GiB model is 7.3 GiB.** Attention,
norms, router, shared expert, embedding and output head together fit in the VRAM
of any node in the mesh, with room to spare. Everything that makes these models
large is expert weight that is *idle* for any given token.

**Fact two: 2.3–3.1% of the expert weight fires per token.** 8-of-256 and
6-of-256. So per token the machine reads ~2.5–3.2 GiB of weights out of 79–137
GiB resident.

Together these say: **for these models, distribution is a memory-capacity
problem, not a compute or bandwidth problem.** That is the same claim
`RUN_DEEPSEEK_V4_FLASH.md` makes, and it is the half of the thesis that is
earned. The design consequence is that you should split across *the fewest*
machines that make the model fit, never across all of them because they are
there.

*(KV formula cross-check: DSv4 at 1M context = 88,064 B/token × 1,048,576 =
**92.3 GB**, matching the 92 GB the doc states independently. The instrument
agrees with the doc, so the per-token figures above can be trusted.)*

---

## 2. The asymmetry that makes WAN routing safe and WAN splitting fatal

This is the load-bearing insight and everything in §4 follows from it.

**The request plane is pipelined. The tensor plane is synchronous.**

- **Request plane (OICP/HTTP).** Node A forwards a whole request to node B and
  reads back an SSE stream. The prompt crosses once; tokens stream. There is no
  per-token round trip. A WAN hop costs one RTT on the *request*, then nothing.
- **Tensor plane (ggml-RPC).** Per decode token the host issues `set_tensor` →
  `graph_recompute` → `get_tensor`, and **blocks** on the last one
  (`ggml-rpc.cpp:512-520`). Every async entry point in the RPC backend is
  `NULL` — `set_tensor_async`, `get_tensor_async`, `cpy_tensor_async`,
  `event_record`, `event_wait` (`ggml-rpc.cpp:744-761`) — and
  `ggml_backend_rpc_synchronize` is documented as a no-op. One machine boundary
  is one blocking round trip **per token**.

Measured cost of one tensor boundary, from records filed on the RuggedFox↔BeefyMac
pair: the 122B's solo ITL is **51.7 ms**, its 2-node ITL is **72.9 ms** — so a
boundary costs **21.2 ms per token** (of which the measured 16 KiB link floor is
10.8 ms; the rest is the second machine's own overhead).

For a 500-token answer, **one tensor boundary adds 10.6 seconds**. One WAN
request hop adds roughly one RTT — tens of milliseconds, once. The request hop
is two to three orders of magnitude cheaper.

> **Design rule 1. Prefer a remote request over a local split.**
> Routing a request to a peer that can serve it alone always beats splitting it
> across two peers that cannot, even when the peers are further away.

---

## 3. What the arithmetic rules out

### 3.1 Pipeline parallelism across four WAN nodes — rejected

Pipeline parallelism **does not reduce single-token latency**. The layers still
execute in sequence; they just execute on different machines. Its only benefit
is memory capacity, and it *costs* one blocking round trip per boundary per
token.

| nodes | boundaries | added @11 ms LAN floor | @25 ms WAN | @93 ms contended Wi-Fi |
|---|---|---|---|---|
| 1 | 0 | 0 ms | 0 ms | 0 ms |
| 2 | 1 | 11 ms | 25 ms | 93 ms |
| 3 | 2 | 22 ms | 50 ms | 186 ms |
| 4 | 3 | **33 ms** | **75 ms** | **279 ms** |

Applied to the 122B, one request in flight:

| nodes | ITL | tok/s |
|---|---|---|
| 1 | 51.7 ms | 19.3 |
| 2 | 72.9 ms | 13.7 |
| 3 | 94.1 ms | 10.6 |
| 4 | 115.3 ms | **8.7** |

Spreading a model that already fits on one node across four costs you 55% of
your throughput and buys nothing. `RUN_GLM_5_2_ON_THE_MESH.md:45` already warns
the tensor split needs shared IP locality; this is the number behind that
warning.

### 3.2 Expert sharding (MoE-parallel) — rejected, and it is worse than it looks

The tempting idea: every node holds the 7.3 GiB dense backbone (cheap) plus a
quarter of the routed experts (34 GiB each), so four nodes hold DSv4 with no
node holding more than ~42 GiB.

It fails on communication topology. Per layer the router selects 6 of 256
experts; with experts spread evenly over four nodes, on average **1.5 are local
and 4.5 are remote**. You then either ship activations to the expert holders and
gather results — an all-to-all — or fetch 12.75 MiB of expert weight per remote
expert. Either way it is **43 all-to-all rounds per token** instead of 3
point-to-point hops. Expert sharding trades a linear chain for a quadratic mesh
on a link whose costs are latency-dominated (§5).

### 3.3 Tensor parallelism — not available

Splitting individual matmuls needs an RDMA-class interconnect. Upstream ggml
does implement an RDMA transport (`ggml/src/ggml-rpc/transport.cpp`), but it is
**hard-disabled on Apple** (`ggml/src/ggml-rpc/CMakeLists.txt:11-16`), so a
Vulkan↔Metal pair silently negotiates back to TCP. Thunderbolt would help but
died at the cable on this fleet (`RUN_DEEPSEEK_V4_FLASH.md:806-810`) — and note
it would not help much anyway: §5 shows this link is latency-bound, not
bandwidth-bound.

---

## 4. The design

### 4.1 Two planes, one locality rule

Introduce an explicit **locality domain**: a set of nodes whose pairwise RTT
floor is below a threshold (proposal: 5 ms, which the measured 4.76 ms LAN floor
clears and the 11.4 ms overlay floor does not).

- **The tensor plane may only operate within one domain.** A ggml-RPC worker
  may never be chosen across domains. This is a hard refusal, not a score
  penalty — a scored preference silently degrades into the rejected topology of
  §3.1 the moment the LAN peer is busy.
- **The request plane may cross domains freely.** It is pipelined; that is what
  §2 establishes.

Today there is no domain concept at all: `peer_inference_endpoints`
(`daemon.rs:1765-1780`) filters only on not-self, Online|Busy, and dialable.

### 4.2 Residency classes replace scoring for the "can you serve this" question

Each node advertises, per model it knows about, exactly one of:

- **`solo`** — weights + planned KV fit in this node's memory alone.
- **`domain`** — fits only with tensor-split help from same-domain peers.
- **`none`**.

Grounded on the real fleet: RuggedFox (124 GB) is `solo` for the 122B (85.6 GiB
weights + 3.0 GiB KV per 32k sequence) and `domain` for DSv4 (144.4 GiB).
BeefyMac (56 GB) is `none` for both alone.

This is a capacity predicate, not a preference. It belongs in the manifest
alongside the existing `x:` feature gates (`scheduler_core.rs:450-466` is the
pattern to copy) so a peer that cannot serve a model is *excluded*, not
out-scored.

#### How OICP already sees a split model — and the one thing it cannot see

**Addressing is already solved, and elegantly.** A tensor-split model has
exactly one advertiser. The manifest is synthesized from the running
`InferenceProvider` — `model_id_for` plus `resident_slots`
(`oicp_synthesis.rs`) — and a node lending its GPU has *no slot* for the split
model, because the weights live in ggml RPC buffers owned by the **host's**
llama context. So the worker advertises nothing about it and the host advertises
it exactly like a local model. The tensor plane is invisible to the request
plane by construction, which is what makes §2's two-plane split hold: OICP
genuinely does not need to know a model is distributed.

**Accounting is not solved. OICP models capability, not commitment.** Nothing
marks a node as currently lending — there is no `rpc_serving`, no
`is_rpc_worker`, no capability flag anywhere. A machine with 30 of its 56 GB and
most of its GPU time committed to someone else's split still advertises full
VRAM, and on the desktop topology publishes no in-flight at all (§4.6). To the
chat scheduler it is a **full, idle candidate**.

Two failures follow, and both need more than two nodes to appear:

- **Two schedulers, one GPU, mutual blindness.** Routing chat to a lending
  worker steals GPU from a split that is already at 505 ms/token and blocks
  synchronously every token, with no backpressure to push back with. The
  blindness is symmetric: `worker_eligibility.rs` gates only which workers a
  host may distribute *to*, and never asks whether a worker is busy serving
  chat. With static roles at N=2 this never bites; where every node both lends
  and serves, it is a live double-booking.
- **Fragility is unadvertised.** A split-held model and a locally-held model
  present identically, but one has a second machine as a single point of
  failure and worker loss is an uncatchable `GGML_ABORT` unless containment is
  armed. No field can say *"this model's availability depends on a machine that
  is not me"*, so the scorer cannot break a tie toward the candidate with the
  smaller blast radius.

**The fix rides on this section rather than adding a subsystem.** Residency has
to be computed from free memory anyway; make it **commitment-aware** — a
lending node's usable capacity is its VRAM minus the lent shard, and its
availability must reflect that it sits on a latency-critical path. One number,
read by both `worker_eligibility` and the chat scorer: §10.6, one decider for
"how much of this GPU is already spoken for." Fragility wants a second, separate
thing — a flag on the *model* entry marking residency that spans machines.

### 4.3 Routing preference order

For a request for model M originating on node A:

1. **A itself, if A is `solo` for M.** Zero network.
2. **Any `solo` holder of M, nearest domain first.** One request hop.
3. **A domain that is collectively `domain`-capable for M** — only if no `solo`
   holder exists anywhere in the mesh.
4. Refuse, naming which of the three failed. (§18.3: absence is reported.)

> **Design rule 2. Local-first is the default, not a scored outcome.**

This is not only a latency policy — **it dissolves the herd**. `SCHEDULER_QUALITY.md`
F5 identifies deterministic argmax over a shared, stale signal as a herd
generator: four nodes reading one 20–30 s-old snapshot with no jitter and no
hysteresis all pick the same target. Under rule 2, four symmetric `solo` nodes
each serve themselves and never contend at all. The scorer runs only for the
genuinely-remote case, where the population of candidates is small and the
consequence of a tie is bounded.

Staleness for reference: gossip interval 10 s with `FANOUT = 2`
(`gossip.rs:42,100`) reaches a given peer within ~2 rounds at N=4, so ~20 s worst
case, plus a 60 s manifest cache (`peer_inference.rs:72`). The scorer currently
*prefers* the gossiped value over its own live observation
(`scheduler_core.rs:510-521`).

### 4.4 Request TTL — close the ping-pong  *(shipped, M1)*

A hop counter on the OICP envelope: decrement on forward, refuse at zero.
`forward_budget` in `oicp-types/src/requirements.rs`, spent by
`decremented_for_forward` (the only place a hop is spent), enforced by
`offload_verdict` in `oicp_select.rs`. Absent resolves to one hop, never to
zero — reading absence as "may not forward" would have disabled mesh routing
on upgrade.

This is not hypothetical. The desktop installs the **raw** provider specifically
so an inbound peer request cannot re-enter routing
(`sovereign-desktop/src-tauri/src/state.rs:941-953`). The CLI daemon installs
the *mesh* provider (`daemon_cmd/mod.rs:670-673`), and the envelope survives the
hop unchanged (`oicp-client/src/lib.rs:306-311`), so node B re-runs the full
scorer on node A's request and may forward it to C. The guard that used to keep
inbound requests single-hop was retired, and its replacement comment claims a
property it does not enforce (`inference_adapter.rs:355-358`).

At N=4 with four originators this is a live correctness bug, not a tidiness
issue.

### 4.5 Consumers — the participant class that holds nothing

**§4.1–4.6 model nodes that hold models. That is not the product.** The real
fleet is asymmetric: a few fat nodes holding weights, and some number of thin
clients — a laptop, a phone, an IDE extension — that hold nothing, originate
everything, and are where a user actually experiences this system. A six-node
mesh is realistically four holders and two consumers, not six peers.

Consumers are not a smaller version of a peer. They differ on every axis that
matters to routing:

| | holder (§4.2) | consumer |
|---|---|---|
| holds weights | yes | **never** |
| originates | sometimes | **always** |
| connectivity | steady | intermittent — and its mesh view is stalest exactly when it wakes |
| can score peers | yes (has manifests) | only by paying for gossip + N manifest fetches |
| useful residency class | `solo` / `domain` | **`none`, always** |

#### The path that already works, and the contract nobody wrote down

`select_route` checks `explicit_model_id` **first** (`peer_inference.rs:1749`),
before any envelope logic. A plain OpenAI request with a `model` field resolves
through `locate_named_model` to Local / Peer / Unknown with **no OICP envelope
required**. So an IDE extension, `curl`, or any third-party OpenAI client
reaches whichever node advertises the model, in one hop. The name is forwarded
verbatim (`oicp-client`'s `model_field` takes `request.model_id` when present),
so there is **no silent substitution** across the hop.

The other path is closed to them. No model name and no envelope means
`has_routing_signal` is false (`peer_inference.rs:916-924`) and the request is
gated `envelope_absent` — served locally or not at all.

> **Design rule 3. A consumer must name the model.**
> "Give me something good" reaches only whatever the entry node happens to
> hold, and never the mesh. This is the whole consumer contract and it is
> currently undocumented anywhere a client author would look.

#### The bound the named path was missing

The named path is *name resolution*, not the OICP scorer, so it never reached
`offload_verdict` — M1's forward budget bounded the scored path and left this
one open. That is backwards: the named path is precisely the consumer path.

Worse, it is the path where a loop is reachable. `locate_named_model` resolves
against a 60-second-cached manifest, so two nodes whose caches each say *"the
other one has it"* bounce a named request between them until a client timeout.

Closed 2026-08-05 (M1, second half): a named request that has already been
forwarded is not forwarded again — it is downgraded to `Local` if this node has
the model, else to `Unknown`, with the reason in the trace. Because a
thin-client request carries no envelope of its own, the forward now mints a
**budget-only** envelope: every routing field absent, so it is invisible to both
`has_routing_signal` and the daemon's Priority-1 gate
(`routes_inference.rs:276-279`) and cannot override the pinned model name it
travels with.

#### What consumers still need

- **An entry node, bound deliberately.** A consumer should not run a scheduler.
  It should bind to a home node that routes on its behalf — which also gives it
  a stable place for its KV to live. The cost is that the entry node is a single
  point of failure for that client, which is why the next item is not optional.
- **Retry is not forwarding, and the budget cannot currently tell them apart.**
  If an entry node's chosen holder is down, failing over is a *second* forward,
  which a budget of one forbids. Nothing breaks today — the existing cascade
  retries a peer's several addresses but never a different peer
  (`peer_inference.rs:2468`) — but peer failover cannot be added until
  `saturating_sub` stops conflating "onward" with "elsewhere".
- **Session affinity, which does not exist.** Nothing in the scheduler is
  sticky. `stable_prefix_len` is handed to the local engine only
  (`inference_adapter.rs:404`) and is never a routing input. An agentic coding
  loop makes many sequential calls sharing a long prefix; consecutive calls can
  land on different holders and re-prefill from scratch each time. On these
  models that is not a rounding error — DSv4's measured TTFT was 12.6 s, and the
  122B prefills at 12–14 tok/s distributed, so discarding an 8k-token prefix
  costs seconds *per call* in a loop that makes dozens.
- **A `consumer` role that means something at runtime.** `SharedModelRole::Consumer`
  exists today only in containment classification (`containment.rs:242`); it
  changes nothing about routing, gossip, or manifests. A thin client is
  currently a full daemon, which is the wrong shape for a phone.

### 4.6 Real admission control

Today: the peer ceiling defaults to `usize::MAX` (`state.rs:370`), and mesh
inference carries no `X-Node-Id` so it is classified as local traffic and skips
the gate entirely (`admission.rs:125`). The actual limiter is
`Semaphore::new(1)` on the slot (`model_slot.rs:1327`), so concurrent requests
queue **silently** inside the peer until the client's 1800 s timeout
(`oicp-client/src/lib.rs:73`).

For N=4 that must become: a finite ceiling, a bounded queue that reports
position, and a `503` with `Retry-After` past it. A caller that is going to wait
40 seconds should be told, not discover it.

### 4.7 Capacity planning

Per concurrent sequence, from §1: 122B costs 96 KiB/token of KV (3.0 GiB at 32k
context), DSv4 costs 86 KiB/token (2.7 GiB at 32k, 92.3 GB at its full 1M).

So a 124 GB node serving the 122B to four concurrent 32k sessions needs
85.6 + 4 × 3.0 = **97.6 GiB** — comfortable. Concurrency is affordable on these
models; it is *capacity for the weights* that is scarce. This reinforces §1:
plan for replication, not for splitting.

---

## 5. Why more network will not save the split

Measured 2026-08-05, ICMP to BeefyMac's LAN address, radio warmed:

```
             min     p10     p50     p90     p99     max    loss
64 B        4.76   22.90   94.90  194.00  237.00  248.00      0%
16 KB      10.80   29.40   92.70  224.00  386.00  576.00      0%
```

The p50 at 16 KB is indistinguishable from the p50 at 64 B, and at the floor
16 KB costs only 6 ms more. **The link is latency-bound, not bandwidth-bound**,
which is unsurprising given §1: the payload at a pipeline boundary is 12–16 KiB.
Faster pipes do not help; fewer round trips do. That is what §4.1 buys.

(The p50s here reflect a badly contended Wi-Fi on the measurement day, not run
conditions. The floor matches the 10.9 ms raw-TCP p50 measured 2026-07-18.)

---

## 6. The opportunity this design defers, and why

With four concurrent users and a four-node pipeline, you could keep four
requests in flight at different stages and get **~4× aggregate throughput at
unchanged per-token latency**. That is the classic reason pipeline parallelism
exists, and it is the one case where §3.1's verdict would flip.

It is structurally impossible today, for two named reasons:

1. ggml's RPC server calls `rpc_serve_client` **synchronously inside the accept
   loop** (`ggml-rpc.cpp:1831-1842`) with `listen(sockfd, 1)`
   (`transport.cpp:621`). One worker serves exactly one host, one request at a
   time.
2. The engine slot is `Semaphore::new(1)` plus a `Mutex<SlotContext>`
   (`model_slot.rs:1327`, `:486`). No micro-batching exists to fill a pipeline
   with.

Both are upstream/engine-level. Recorded here so the opportunity is not
rediscovered as a surprise, and explicitly **not** proposed as work.

---

## 7. Milestones

**Each milestone is an experiment, not a deliverable.** A milestone is complete
when its experiment has been *run* and reported — not when its code merges. The
distinction is deliberate: this design is a pile of claims about a topology
nobody here has operated, and shipping a change without running its experiment
would leave a claim looking settled because it compiled.

Every milestone therefore states what would **refute** it. A milestone whose
experiment cannot fail is not a milestone; it is an assertion in prose
(ARCH_PRINCIPLES §7.2), and the four verdicts apply — passed, failed,
**could-not-judge**, **never-ran** (§18.1, §18.2). "Never-ran" is the honest
status of most of what follows, and it is recorded as such rather than left
blank.

---

### M1 — Bound the forward chain
**Status: code SHIPPED 2026-08-05 · unit experiment PASSED · live experiment NEVER-RAN**

**Change.** `forward_budget` on the OICP envelope
(`oicp-types/src/requirements.rs`), spent in exactly one place
(`InferenceRequirements::decremented_for_forward`, called from
`oicp-client`'s `build_request`), enforced on **both** routing paths:

- *scored path* — `offload_verdict` (`oicp_select.rs`) reports
  `forward_budget_exhausted` as a gate name distinct from
  `not_offload_eligible`, because "stays home by policy" and "someone already
  forwarded this to me" are different operator problems.
- *named path* — `select_route` downgrades an already-forwarded
  `NamedModelLocation::Peer` to `Local` (if this node has the model) or
  `Unknown`. **This half was missed on the first pass and added the same day.**
  Named dispatch is name resolution, not the OICP scorer, so it never reaches
  `offload_verdict` — which left the bound covering the peer-to-peer path and
  not the *consumer* path (§4.5), where a stale-manifest loop is actually
  reachable. A thin-client request carries no envelope, so the forward mints a
  budget-only one; every routing field stays absent so it cannot override the
  pinned model name it travels with.

The generalisable mistake: the first implementation was placed where the
*architecture diagram* said requests are routed, not where they are **actually**
routed. Two dispatch paths existed and only one was gated.

**Experiment A (unit) — PASSED.** Eight tests, each confirmed to execute
individually rather than inferred from a suite total:
a forwarded envelope leaves with `Some(0)` written explicitly; the synthesized-
envelope branch spends too; the decrement saturates; an envelope from a build
without the field still loads and still routes; the other two gates keep their
old reported name.

**Experiment B (live) — NOT RUN.** Needs three nodes. Originate on A a request
for a model only C holds, where B is reachable from A and C from B. Read A's
and B's decision logs.
- **Passes if** B's log shows gate `forward_budget_exhausted` and C receives
  nothing.
- **Refuted if** the request reaches C. That would mean some path *rebuilds*
  the envelope rather than forwarding it, and the budget is being reset — the
  same class of bug as the retired single-hop comment at
  `inference_adapter.rs:353-360`.

**Known limit, not a defect to hide.** A peer on an older build forwards
`None`, which reads as a full budget. The bound holds between updated nodes and
degrades to today's unbounded behaviour when an old node is the forwarder.
Refuting Experiment B against a mixed-version mesh proves nothing.

---

### M2 — Residency classes replace scoring for feasibility
**Status: NEVER-RAN**

**Change.** Per-model `solo` / `domain` / `none` in the manifest (§4.2),
excluding rather than out-scoring, in the shape of the `x:forced_choice` gate
at `scheduler_core.rs:450-466`.

**Experiment.** On the real fleet, ask both nodes to serve the 122B: weights
85.6 GiB plus 3.0 GiB of KV per 32k session. RuggedFox (124 GB) is `solo`;
BeefyMac (56 GB) is `none`.
- **Passes if** BeefyMac never appears as a scored candidate and its exclusion
  is named in the decision log.
- **Refuted if** BeefyMac appears and merely loses on score. That is the
  failure this milestone exists to prevent: a busy RuggedFox would then let a
  request land where it physically cannot run, and the operator would read a
  capacity error as a routing error.

**Experiment B — commitment, not just capacity. RUNNABLE TODAY ON TWO NODES**,
which makes it the cheapest real experiment in this document and the one to run
first. Lend BeefyMac's GPU to a DSv4 split (the 2026-08-04 configuration: 30
local + 13 @BeefyMac), then, while the split is resident, ask the chat scheduler
to score BeefyMac for an ordinary request.
- **Passes if** BeefyMac's advertised capacity excludes the lent shard and its
  availability reflects that it is on a latency-critical path.
- **Refuted — and this is the expected outcome today — if** it is offered as a
  full-capacity idle candidate. Confirming that is the point: it converts "two
  schedulers cannot see each other" from a reading of the code into a
  reproduction, which is what §18.1 asks for before a fix is written.
- **Watch the split's ITL while the chat request runs.** The magnitude of the
  contention is the number that decides whether commitment-aware residency is
  worth building, and nothing currently measures it.

---

### M3 — Local-first routing dissolves the herd
**Status: NEVER-RAN**

**Change.** The preference order in §4.3, which depends on M2.

**Experiment A — the design's own claim.** Four nodes, each `solo` for the same
model. Fire one request on each simultaneously. Count cross-node inference
requests.
- **Passes if** the count is zero.
- **Refuted if** any peer traffic occurs — §4.3 is then not local-first in
  practice, whatever the ordering says.

**Experiment B — quantify the herd it claims to dissolve.** The inverse case:
exactly ONE node `solo`, four concurrent originators. Record the arrival times
and per-request latency distribution at the target.
- This one has **no pass/fail** — it is a measurement, and its purpose is to
  put a number on `SCHEDULER_QUALITY.md` F5 on real hardware rather than in the
  simulator. Run it *before* M3 ships, or the improvement has no baseline to be
  measured against (§18.4: validate the instrument before the result).

---

### M4 — Locality domains, with a hard cross-domain refusal
**Status: NEVER-RAN**

**Change.** Tensor-plane participation restricted to one domain (§4.1),
refusal not penalty.

**Experiment A — justify the threshold before enforcing it.** Add a third node
to a 122B tensor split and measure ITL.
- **Passes if** ITL lands near 94.1 ms (±10%) and throughput near 10.6 tok/s,
  as §3.1 extrapolates from the measured 21.2 ms per boundary.
- **Refuted if** throughput *rises*. §3.1 would then be wrong, pipelining is
  overlapping somewhere I could not find in the RPC backend, and M4's whole
  premise collapses. This experiment is worth running **first**, because a
  refutation here invalidates §3.1, §4.1 and M4 together.

**Experiment B.** Attempt a tensor split naming a worker whose RTT floor
exceeds the 5 ms domain threshold.
- **Passes if** it is refused with a named reason before any weight moves.
- **Refuted if** it proceeds, or if it fails later with a generic error — a
  refusal that arrives after a 40 GB shard fetch is not a refusal.

---

### M5 — Admission ceiling and a bounded queue
**Status: NEVER-RAN**

**Change.** A finite peer ceiling (today `usize::MAX`, `state.rs:370`), a
bounded queue reporting position, and `503` + `Retry-After` past it — replacing
today's silent block inside `Semaphore::new(1)` up to the 1800 s client timeout.

**Experiment.** Drive one peer past the ceiling with concurrent requests.
- **Passes if** surplus requests receive a `503` with `Retry-After` within
  ~1 s.
- **Refuted if** they block silently. Note the current behaviour would *also*
  eventually return an answer, so a naive "did it work" check passes today —
  the experiment must assert on **time-to-rejection**, not on the final result.

---

### M6 — Consumers can reach a model they do not hold
**Status: NEVER-RAN**

The milestone that matters most for real use, because a consumer is where a
person actually touches this system (§4.5). It is listed last and should be
scheduled first: M2–M5 improve a topology that already works; M6 is about
whether the product works at all from a laptop.

**Change.** A `consumer` participant class that binds to an entry node rather
than running a scheduler; session affinity so an agentic loop keeps its prefix;
and the retry-vs-forward distinction the current `saturating_sub` cannot make.

**Experiment A — the contract holds.** From a machine holding no model, issue a
plain OpenAI request naming a model that only a peer holds.
- **Passes if** the answer comes back and the served model id equals the name
  requested — no substitution.
- **Refuted if** it 503s, or if a different model answers. The latter is the
  worse outcome and the one to look for: it is a silent substitution, and a
  user would experience it as "the model got dumber", not as an error.

**Experiment B — the loop is actually closed.** Two nodes, both with stale
manifests naming the *other* as the holder. Issue a named request.
- **Passes if** it terminates with `forward_budget_exhausted` in the trace.
- **Refuted if** it ping-pongs to a client timeout. This is the failure the
  named-path half of M1 was written for and it has never been reproduced —
  neither before the fix (to confirm the bug) nor after (to confirm the fix).
  **Reproduce it first**: a fix for a failure nobody has watched happen is a
  guess with a test attached (§18.1).

**Experiment C — quantify the affinity gap before building affinity.** Run one
agentic coding loop (many sequential calls, long shared prefix) against a
two-holder mesh and record which node served each call and the TTFT of each.
- **No pass/fail.** It measures how often consecutive calls change node and what
  each change costs. If calls happen to stay put, affinity is a solution to a
  problem this fleet does not have and M6 should drop it — which is exactly why
  this runs *before* the work, not after (§18.4).

---

## 8. Standing measurements, not attached to a milestone

These settle questions this design rests on but does not change.

- **S1 — the link is latency-bound, not bandwidth-bound.** A 16 KiB payload
  will not measurably beat a 64 B payload on round-trip time on this LAN.
  Measured 2026-08-05 and consistent with §5; re-run if the fleet's networking
  changes. A refutation makes faster interconnect worth revisiting and §3.3
  wrong.
- **S2 — DSv4's deficit is kernels, not topology.** On the same pair at N=2 the
  122B sustains 10.484 tok/s × 2.48 GiB = **26.0 GiB/s** of expert reads; DSv4
  sustains 1.556 × 3.21 = **5.0 GiB/s**. Same hardware, same topology, same
  256-expert MoE shape. **Experiment:** run DSv4 single-box on BeefyMac at a
  quant that fits. Landing near 1.5–2 tok/s confirms the kernels; landing near
  8–10 refutes and puts the blame back on the link. Partially decomposable
  today with `scripts/rpc-timing-split.py`, which separates worker compute from
  everything else.

---

## 9. Related

- `sovereign/docs/specs/SCHEDULER_QUALITY.md` — F1 (dead time > service time),
  F2 (invisible inbound load), F5 (deterministic argmax as herd generator).
- `docs/RUN_DEEPSEEK_V4_FLASH.md` — the N=2 measurement, the link bound, and
  `scripts/rpc-timing-split.py`.
- `docs/RUN_GLM_5_2_ON_THE_MESH.md` — the hub-and-spoke framing this supersedes
  for multi-origin topologies.
- `sovereign/docs/MESH_LOAD_AWARENESS.md` — gossiped in-flight, and its own open
  TODO about the manifest path.
