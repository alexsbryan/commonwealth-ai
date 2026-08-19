# Four peers, four places — mesh topology design for N=4

**Status:** written 2026-08-05; milestone ledger current to 2026-08-06.
**M1 shipped + live-PASSED · M3 done · M6 done · M4 WITHDRAWN (not runnable on
this fleet) · M2 half-withdrawn (B blocked, A open and cheap) · M5 measured, its
bound not yet built and now REQUIRED.**

**§7's exit clause is SETTLED (operator, 2026-08-06): the target is BOTH peer
fleets AND hub-plus-thin-clients, so the clause does not fire and the harder
shape governs.** What remains is therefore work, not deliberation: M5's bound
(on predicted WAIT, not depth), M2 Experiment A, and M6's peer-residency
instrument gap. Per-milestone status lines below are authoritative over this
summary.
The milestones in §7 are experiments, and each records whether it has been run —
`NEVER-RAN` is a status, not a blank.

> **Revision 2026-08-06.** A survey against the running system found several of
> this document's claims about the code to be wrong, and each is corrected
> inline below rather than silently edited, so the record shows what was
> believed and why it was not so. The headline: §4.2's claim that the manifest
> reads `resident_slots` was false, **and that omission was a live bug** —
> every node advertised every model as loaded regardless of residency. Fixed
> and verified the same day (§4.2). Two further themes recur in the
> corrections and are worth holding while reading the rest: **more already
> exists than this document assumed** (admission control, capacity verdicts,
> fragility, capability flags), and **the gaps that remain are liveness and
> instrumentation rather than absent machinery**.
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

**Measured 2026-08-06 — the request-hop half of this rule is now grounded, and
it holds with a wide margin.** The daemon times every peer manifest fetch
(`GET /oicp/v1/capabilities` over the real transport), which is exactly one
request-plane round trip. Over n=13 samples: min 5 ms, **p50 11 ms**, p90
154 ms, max 521 ms.

So a request hop costs ~11 ms typically — "roughly one RTT, tens of
milliseconds, once", as this section claimed. Against one tensor boundary at
21.2 ms *per token* (10.6 s over a 500-token answer), the request hop is
cheaper by roughly three orders of magnitude, and **stays cheaper by ~20× even
at its 521 ms tail**. Design rule 1 is not marginal; it is robust to the worst
sample observed.

Two caveats worth recording. n=13 is a sample, not a distribution (§18.5). And
the transport is not what §2 assumed: peer manifests resolve over ephemeral
local iroh tunnel ports, not LAN addresses, so "one RTT" here includes relay
and tunnel overhead rather than a bare LAN hop — which makes the margin above
conservative, not optimistic.

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

Applied to the 122B, one request in flight. **The tok/s column originally here
was RETRACTED 2026-08-06** — it derived throughput as `1000 / itl_p50`, which
treats a median as a mean, and measurement put it 65% high (see the block
below). What replaces it is a *projection*, labelled as one:

| nodes | median ITL (as measured) | ~tok/s, projected from the measured mean cost | retracted claim |
|---|---|---|---|
| 1 | 51.7 ms | 19.3 | 19.3 |
| 2 | 72.9 ms | 10.7 | ~~13.7~~ |
| 3 | 94.1 ms | 7.4 | ~~10.6~~ |
| 4 | 115.3 ms | **5.6** | ~~**8.7**~~ |

**Basis, and its limits.** The projected column prices each boundary at the
**+42.1 ms/token mean** cost measured below, not the +21.2 ms median the ITL
column carries. That measurement is one boundary, on the 4B, at this LAN's
~11 ms floor; applying it to the 122B and to three boundaries is extrapolation.
Treat N=1 as measured, N=2 as one measured boundary rescaled, and N=3/4 as
projections that have never been run. The ITL column is retained because a
median ITL is a sound latency claim — it is only unsound as a throughput term.

Spreading a model that already fits on one node across four costs you **~71%**
of your throughput — not the 55% first claimed — and buys nothing.
`RUN_GLM_5_2_ON_THE_MESH.md:45` already warns the tensor split needs shared IP
locality; this is the number behind that warning, and the corrected number makes
the warning stronger.

> **MEASURED 2026-08-06 — the direction is confirmed and UNDERSTATED; the
> arithmetic above is wrong by 65%.**
>
> Run on `Qwen3.5-4B.Q6_K` (chosen because it is **non-MTP**, so `itl_p50`
> means what this section assumes it means, and because it fits solo — the
> 122B did not, on that day, see M4-A). Solo (`32 local`) and 2-node
> (`27 local + 5 @BeefyMac`, pinned via `SOVEREIGN_RPC_BLOCK_SPLIT=5,27`),
> same session, `ping` sampled concurrently against the address the tensor
> plane actually dials.
>
> | | solo | 2-node |
> |---|---|---|
> | valid runs | **5 / 5** | **6 / 12** |
> | `decode_tok_s` | 44.48 (spread 0.97%) | 15.49 (spread 3.68%) |
> | `itl_p50` median | 22.5 ms | 39.1 ms |
> | mean ÷ median per-token | **1.00** | **1.65** |
>
> **A boundary does not add a constant. It adds a skewed distribution.** Solo's
> mean per-token time equals its median exactly (22.5 ms both ways). At 2 nodes
> the median is 39.1 ms but the mean — the one throughput is made of — is
> 64.6 ms. So one boundary costs **+16.6 ms/token at the median and
> +42.1 ms/token at the mean**, a factor of 2.5 apart.
>
> **That is the flaw in the table above.** It derives tok/s as `1000 / itl_p50`,
> i.e. it treats a median as a mean. Applied to this measured 2-node
> configuration the method predicts 25.6 tok/s; the measured value is
> **15.49 tok/s — a 65% overestimate.** Every tok/s figure in the N=2/3/4 rows
> is optimistic by roughly that factor, and the 21.2 ms per-boundary constant
> is a median masquerading as a throughput term.
>
> Design rule 1 survives comfortably — it survives *harder*, since splitting is
> more expensive than claimed, not less. What does not survive is quoting these
> tok/s numbers as predictions.
>
> **The 2-node configuration is also not steady.** It tripped `mesh bench`'s
> 25% inter-trial spread guard on **half** of its runs (rejections at 26, 26,
> 29 and 117%), while solo passed 5 for 5. A configuration that cannot hold a
> steady state across five trials cannot support a ±10% acceptance band.
>
> **One hypothesis tested and NOT confirmed.** Pearson r between 2-node decode
> and concurrent *mean* ping was **−0.09** (n=6) — no detectable relationship.
> That does not clear link jitter, because a mean-vs-mean correlation at n=6
> cannot see tail events, which is the mechanism actually suspected. Record it
> as **could-not-judge**, not as an exoneration of the link.

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
`InferenceProvider` (`oicp_synthesis.rs`), and a node lending its GPU has *no
slot* for the split model, because the weights live in ggml RPC buffers owned by
the **host's** llama context. So the worker advertises nothing about it and the
host advertises it exactly like a local model. The tensor plane is invisible to
the request plane by construction, which is what makes §2's two-plane split
hold: OICP genuinely does not need to know a model is distributed.

> **Correction, 2026-08-06.** This paragraph originally said the manifest was
> synthesized from "`model_id_for` **plus `resident_slots`**". It was not —
> `resident_slots()` had *no call site* in `oicp_synthesis.rs`, and **that
> omission was a live bug**, not a documentation slip. All five push sites wrote
> `available: true, loaded: true` as literals, so a node advertised every model
> as warm regardless of what was resident. Reproduced on RuggedFox the same day:
> `/status` reported `resident: false` for the idle-unloaded 30 GB primary while
> `/oicp/v1/capabilities` advertised `loaded: true` for it *and* both `primary`
> aliases. This is the steady state of a lazy slot, not an edge case.
>
> Fixed the same day: `status_for()` reads `resident_slots()`, absence and
> `transitioning` both report cold (§18.3). `available` deliberately stays
> `true` — a lazy slot is genuinely servable, and conflating "can serve" with
> "is warm" was the original error. Candidacy is unaffected:
> `best_claim_for_request` filters on `status.available`
> (`oicp-types/src/scoring.rs:486`), while `loaded` feeds only `LoadDebt`
> (`predicted_time.rs:143`) — it *prices* a candidate rather than excluding it.
>
> **The cold-start term is still zero, and that is now the honest open item.**
> `LoadDebt::pending_ms` charges `estimated_load_ms` when cold, and every push
> site advertises `estimated_load_time_sec: None` because nothing records a
> load time — `MeasurementRecord.cold_load_s` (`mesh_measurements.rs:843`) is
> declared but never populated. Measured on RuggedFox 2026-08-06: a cold
> `primary` request returned in **17.3 s**. That is the magnitude currently
> priced at 0 for every peer in the mesh. Advertising it needs a recorded
> figure, not a guess.

**Accounting is not solved. OICP models capability, not commitment.** A machine
with 30 of its 56 GB and most of its GPU time committed to someone else's split
still advertises full VRAM, and on the desktop topology publishes no in-flight
at all (§4.6). To the chat scheduler it is a **full, idle candidate**.

> **Correction, 2026-08-06.** This said "no capability flag anywhere." There are
> several, and they are gossiped: `AnchorProfile` (`can_anchor`, `vram_gb`,
> `model_resident` — `commonwealth-core/src/capabilities.rs:105-116`, built at
> `sovereign-mesh/src/capabilities.rs:164-170` from `SOVEREIGN_RPC_SERVE`,
> consumed at `daemon.rs:1864`) and `AvailableResources` (`free_vram_gb`,
> `free_ram_gb`, `gpu_utilization` — `capabilities.rs:154-162`).
>
> **The real gap is liveness, which is worse than absence.** `can_anchor` is a
> config flag, `vram_gb` a static sum, and `free_vram_gb` is read live only on
> NVIDIA — `capabilities.rs:341-349` falls back to the static
> `hw.gpus[0].vram_gb` on Metal and ROCm. So a fully-lent Mac advertises its
> device *total* as *free* **by construction, not by staleness**, and no
> refresh interval can fix it. The fleet's Intel Mac (`LittleMac`, reporting
> `metal`/24 GB on a machine with no unified memory) is the clearest example:
> the number is published, plausible, and not a measurement.
>
> Likewise, "no field can say *this model's availability depends on a machine
> that is not me*" was wrong: `SlotPlacement`
> (`sovereign-contracts/src/traits.rs:256-278`) carries `mode`
> (`local|distributed|stream-split|forming`), `total_blocks`, `local_blocks` and
> `workers`, hung off `ResidentSlot.placement` and produced by
> `summarize_placement` (`rpc_distribution.rs:1627`). It is simply not in the
> OICP manifest. Publishing fragility is plumbing, not a new concept.

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

The other path was closed to them until 2026-08-13. No model name and no
envelope meant `has_routing_signal` was false and the request was gated
`no_routing_signal` — served locally or not at all.

> **Design rule 3. A consumer must name the model.**
> ~~"Give me something good" reaches only whatever the entry node happens to
> hold, and never the mesh.~~
>
> **RETIRED 2026-08-13, by measurement.** The rule described the code, and the
> code was wrong. `MESH_SCALE_100_USERS_1000_CORPORA.md` §9.1.1 drove 100 turns
> from plain OpenAI clients at a census-verified 2-node mesh: every one was
> gated `no_routing_signal`, zero reached the peer, and admitted concurrency at
> N=2 sat *inside* the N=1 bracket. Meanwhile the named path had been routing
> envelope-less requests to peers all along — the two surfaces answered the same
> question ("may an unstated envelope cross to a peer?") differently, which is
> §10.6. `has_routing_signal` is removed; `oicp_select::offload_verdict_opt` is
> now the one asker, and it says what the named path already said: absence
> states nothing, and stating nothing is not a refusal. "Give me something good"
> now reaches the mesh.
>
> What did NOT change: a *present* envelope is judged exactly as before, so
> `local_only` and `Fast` work still never leaves the node.

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
  which a budget of one forbids.

  > **Correction, 2026-08-06.** This originally said "the existing cascade
  > retries a peer's several addresses but never a different peer." That is
  > false on the **scored** path: `ranked_route_plan` (`peer_inference.rs:1939`,
  > doc `:1925-1932`) emits one Soft `Peer` step *per peer that beats local*
  > followed by `LocalFallback`, and the cascade continues to the next peer.
  > It is true only on the **named/Hard** path (`:1872-1879`) — one step, no
  > sibling peer, no local fallback — which is precisely the consumer path, so
  > the concern survives, just in a narrower place. The fix is to port the
  > existing ranked failover to the Hard path rather than invent one.
  >
  > Also imprecise: `decremented_for_forward`
  > (`oicp-types/src/requirements.rs:182`) returns a *new* envelope and never
  > mutates `request.oicp`, so nothing is lost or double-spent across retries.
  > Retry-vs-forward is a **missing distinction on the sender's side**, not a
  > corrupted counter.
- **Session affinity, which does not exist.** Nothing in the scheduler is
  sticky. `stable_prefix_len` is handed to the local engine only
  (`inference_adapter.rs:404`) and is never a routing input. An agentic coding
  loop makes many sequential calls sharing a long prefix; consecutive calls can
  land on different holders and re-prefill from scratch each time. On these
  models that is not a rounding error — DSv4's measured TTFT was 12.6 s, and the
  122B prefills at 12–14 tok/s distributed, so discarding an 8k-token prefix
  costs seconds *per call* in a loop that makes dozens.
- **A `consumer` role that binds to an entry node.** A thin client is currently
  a full daemon, which is the wrong shape for a phone.

  > **Correction, 2026-08-06.** This originally said `SharedModelRole::Consumer`
  > "exists today only in containment classification (`containment.rs:242`); it
  > changes nothing about routing, gossip, or manifests." That line is inside a
  > `#[cfg(test)]` block, and the claim is wrong on all three axes. The role's
  > production effect is `apply_shared_model_role_to_env`
  > (`sovereign-cli-daemon/src/daemon_cmd/bootstrap.rs:338-393`): a Consumer
  > gets `SOVEREIGN_SHARED_MODEL_ID` (`:345-352`) but **not**
  > `SOVEREIGN_RPC_SERVE`/`_DISCOVER` (`:354`, `:361`), so it gossips
  > `anchor: None` (`capabilities.rs:164`), is excluded from the RPC split
  > (`daemon.rs:1942`), and routes via `shared_primary_id`
  > (`peer_inference.rs:1741`). What is genuinely missing is only the
  > entry-node binding — a smaller, separate design question.

### 4.6 Real admission control

Mesh inference carries no `X-Node-Id`, so it is classified as local traffic and
skips the admission gate entirely (`admission.rs:125-128`). Concurrent requests
then queue **silently** inside `Semaphore::new(1)` on the slot
(`model_slot.rs:1327`) until the client's 1800 s timeout
(`oicp-client/src/lib.rs:73`).

> **Correction, 2026-08-06 — most of this section was already built.** The
> ceiling does **not** default to `usize::MAX` in practice: `state.rs:370` is a
> pre-configuration constant, and its own doc (`:362-369`) says the daemon
> always applies a finite ceiling at boot, which it does
> (`sovereign-mesh/src/daemon.rs:2441-2447`, default **1** at
> `setup_config.rs:1069`). Confirmed live on RuggedFox:
> `/v1/mesh/status` reports `peer_inflight_ceiling: 1`.
>
> `503` + `Retry-After` also already ships and is mounted
> (`admission.rs:71-77`, `:186-192`; `commonwealth-api/src/server.rs:46,178`
> — line refs refreshed 2026-08-07 when the renderer was extracted to
> `shed_response`/`local_queue_shed_response` at `:93-115`),
> as does a bounded queue that reports position —
> `commonwealth-core/src/fair_sched.rs` (`QueueStatus { position,
> estimated_wait_ms }`, `TryGrant::WouldQueue { position }`).
>
> **So the defect is narrower and more specific than "build admission control."**
> Two things: (a) no inference client stamps `X-Node-Id` — `oicp-client` sets
> only `Authorization`, and the sole stamper in the repo is
> `routes_knowledge.rs:457` — so the gate never sees mesh inference; and
> (b) `AppState::admit_peer_request` (`state.rs:1757-1763`) collapses
> `WouldQueue { position }` and `Shed` into one `CeilingExceeded` with a
> hardcoded `retry_after_secs: 2`, **discarding the position it was just
> handed**. Note also that `record_turn` is never called on the peer path, so
> any `estimated_wait_ms` today would report the 3000 ms seed — a fabricated
> number (§18.3), which must be fixed in the same change rather than surfaced.
>
> Sequencing matters: stamping `X-Node-Id` arms a gate whose ceiling is 1, so it
> must land *after* the position fix and after local-first routing, or it reads
> as a regression — and would be one.

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
**Status: code SHIPPED 2026-08-05 · unit experiment PASSED · live experiment PASSED
2026-08-06 (one-hop decider form — see the note under Experiment B)**

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

> **Correction, 2026-08-06 — there were THREE dispatch paths, and the third was
> ungated. The lesson above was written and then repeated. Now FIXED.**
>
> `MeshInferenceProvider::complete()` (the **non-streaming** path) carries its
> own inline named-model branch at `peer_inference.rs:2242-2339` —
> `explicit_model_id` → `locate_named_model` → `Local`/`Peer`/`Unknown` — and it
> **returns before `select_route` is ever called** (`select_route` is invoked at
> `:2487` and `:2692`, both later in the file). M1's named-path fix lives inside
> `select_route` at `:1791-1814`, so the non-streaming path never reaches it.
> It forwards to a peer at `:2303-2304` with no `may_forward` check anywhere in
> the branch. The comment at `:2246` ("the routing decision above already saw
> the alias and chose Local") describes `select_route` and is simply wrong here:
> there is no decision above.
>
> The budget is *spent* but never *read*. `build_request`
> (`oicp-client/src/lib.rs:317,335,356`) decrements on every forward including
> this one, so the counter does reach 0 — but nothing on this path reads it, so
> a node receiving a spent request decrements a saturated zero and forwards
> again. **The ping-pong M1 exists to close is still reachable via
> non-streaming named requests.** Not yet reproduced end to end (that needs two
> mutually-stale manifests), so this half is derived from the code rather than
> observed — **M6 Experiment B must target the non-streaming path specifically**.
>
> **The same path is also invisible.** Measured live: an identical peer-routed
> request emits **2** decision-log records when `"stream": true` and **0** when
> `"stream": false`. Local and alias-resolved non-streaming requests likewise
> emit nothing. Since most programmatic OpenAI clients and plain `curl` default
> to non-streaming, the unobservable path is the common one in automation — and
> it is the *consumer* path, because design rule 3 tells thin clients to name
> the model. Any experiment reading the decision log must force `"stream": true`
> or it will see an empty file and misread it as "no gate fired".
>
> §10.6: two implementations of "where does a named request go" — one gated and
> observable, one neither.
>
> **Fixed the same day.** The bound was written into a *call site* instead of
> into the decider, so the fix was to make the decider exist:
> `resolve_named_dispatch` (`peer_inference.rs`) now owns name resolution, the
> hop bound and the decision record, and **both** `select_route` and
> `complete()` call it. `complete()`'s named arms additionally close the
> decision→outcome join, including on refusal — a `NamedUnknown` produced by
> the hop bound is exactly the event an operator chasing a ping-pong needs to
> see, and it previously left no trace at all.
>
> Two regression tests, both watched red against the pre-fix shape first
> (§18.1): `non_streaming_named_dispatch_emits_a_joined_decision_and_outcome`
> and `non_streaming_named_dispatch_refuses_to_forward_an_exhausted_request`.
> The mock peer in `scheduler_decision_records.rs` had only ever spoken SSE,
> which is *why* no test had driven this path — it now content-negotiates on
> the request's own `stream` flag, like a real peer.
>
> Verified live: the identical non-streaming request that produced **0**
> decision-log records now produces **2** (`named_local` joined to a `local`
> outcome). Gates green — lint 0 errors workspace-wide, 9190 tests pass.
>
> M1 now reads **shipped on all three dispatch paths.** The ping-pong itself
> is still un-reproduced end to end, so M6 Experiment B remains the test of
> record — but it is now testing a fix rather than an absence.

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

> **Experiment B — RUN 2026-08-06. PASSED on BOTH dispatch paths.**
>
> **Run in one-hop decider form, not as the three-node chain, and the
> difference is stated so the verdict is not over-read.** The chain as
> specified needs two mutually-stale manifests to induce a second hop; on a
> healthy full mesh every node sees every other directly, so A routes to C and
> no chain forms. What the chain exists to prove is a rule that is testable
> directly — `requirements.rs:47`, "a node holding zero must serve the request
> itself or refuse". So: send RuggedFox a request naming `Qwen3-4B-Q4_K_M` (held
> by LittleMac, not by RuggedFox) carrying `forward_budget: 0`.
>
> | arm | `forward_budget` | outcome |
> |---|---|---|
> | **control**, streaming | absent | `200`, `served_by peer:LittleMac`, verdict `named_peer`, 9,893 ms |
> | **test**, streaming | `0` | `503` in **3 ms**, verdict `named_unknown`, nothing forwarded |
> | **control**, non-streaming | absent | `200`, `served_by peer:LittleMac`, verdict `named_peer`, 5,371 ms |
> | **test**, non-streaming | `0` | `503` in **3 ms**, verdict `named_unknown`, nothing forwarded |
>
> The refusal names its own cause in the response body — "this request has
> already been forwarded once and its mesh hop budget is spent — a further
> forward could bounce…" — which is the operator-facing half §18.3 asks for.
> Three milliseconds versus five-to-ten seconds is the tell that the refusal
> happens at the decider, before any peer is dialled.
>
> **This run also confirms the observability fix.** The non-streaming named
> path emitted **2** decision records per request. Pre-fix it emitted **0**,
> which is the specific defect recorded in the Correction above, and the reason
> that path had never been driven by a test.
>
> **The control was load-bearing and nearly wasn't there.** The first run of
> this harness failed its own control: `InferenceRequirements` defaults to
> `sharding: local_only` (`requirements.rs:203-208`), so BOTH arms were refused
> — the control on *privacy* grounds, the test on *budget* grounds. Had the
> harness asserted only "the spent request was refused", it would have reported
> PASSED while the peer path was entirely untested. Both arms now set
> `mesh_allowed`. This is §18.2 in miniature: a refusal for the wrong reason
> reads identically to a refusal for the right one.
>
> **Still not proven end to end:** an actual A→B→C ping-pong. This experiment
> shows the bound is read and enforced at a node that holds a spent request; it
> does not stage the multi-hop topology that would arise from stale manifests.
> Harness: `m1_live.py` (session scratchpad).

---

### M2 — Residency classes replace scoring for feasibility
**Status 2026-08-06: Experiment B WITHDRAWN (blocked on parked work) ·
Experiment A OPEN and cheap, with one hazard named below. The milestone is NOT
wholly closable, and this section says why rather than pretending otherwise.**

> **Closing review, 2026-08-06.** M2 was slated for withdrawal alongside M4. It
> does not withdraw cleanly, and recording that is the point — the two halves
> are in genuinely different states.
>
> **Experiment B — WITHDRAWN.** It requires lending BeefyMac's GPU to a DSv4
> tensor split and then scoring BeefyMac for ordinary chat while the split is
> resident. The split is downstream of the ggml-RPC-over-iroh work, which is
> parked with an open hang. Its own text calls it "the cheapest real experiment
> in this document"; that was true when a working split was assumed, and is not
> true now. Reopen with the distributed work.
>
> **Experiment A — OPEN, and cheaper than it looks.** Routing happens BEFORE the
> model loads, so a request naming the 122B produces its decision record without
> paying for 85.6 GiB of weights. Evidence: the decision log already holds a 122B
> attempt that got as far as the **local-fit gate** and failed there
> ("needs ~89171 MiB … but only ~74708 MiB usable after the host reserve"),
> which means candidate scoring had already run and been recorded. So the
> question M2 asks — *is BeefyMac EXCLUDED, or merely OUT-SCORED?* — is one
> request and one decision-log read away.
>
> **Hazard, which is why this was not simply run.** That local-fit refusal was
> measured under the DEFAULT host reserve. The daemon used for the 2026-08-06
> measurements runs with `SOVEREIGN_LOCAL_FIT_RESERVE_GB=4`, which raises usable
> memory and may let the gate PASS — at which point the request proceeds to load
> an 85.6 GiB model, evicting whatever is resident and pinning the box for
> minutes. Run Experiment A with the default reserve, or with the daemon
> otherwise idle, and expect the local-fit refusal as the *cheap* outcome.
>
> **What settles this milestone** is not "did the request succeed" — it is
> whether BeefyMac appears in `scored` at all. Merely losing on score is the
> refutation (a busy RuggedFox would then let a request land where it
> physically cannot run, and the operator reads a capacity error as a routing
> error).

**Original status: NEVER-RAN**

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
> **Correction, 2026-08-06 (operator) — "the 122B does not fit this box solo" is
> WRONG as a standing premise, and it was load-bearing in the wrong direction.**
> The C5 note above recorded a fit-gate refusal at 89 GB needed vs 73 GB usable.
> That was **one quant on one day with two rust-analyzers holding 12.1 GB** — an
> environmental reading, not a property of the machine or of the 122B class. On
> disk: the MTP variant (`Qwen3.5-122B-A10B-UD-Q5_K_S`, 3 shards) is **83 GB**
> and the Q5_K_XL merge is 86 GB, against **125 GB** of host RAM. The MTP 122B
> fits solo.
>
> **This removes the last standing justification for multi-node on this fleet
> rather than adding one.** "Models too big for one box" was the one argument
> that survived C5 (splitting costs ~71% of throughput) and M3 (nothing ever
> moves; local-first already holds) — and it currently has **no instance here**.
> The exit clause in this document's own framing is therefore closer, not
> further. What would still justify the topology: a model that genuinely exceeds
> one host, or several large models required resident at once. Neither is on this
> fleet today, so neither should be assumed.

**Status: A PASSED (both paths) · B RUN — 2026-08-06. RECOMMENDATION WITHDRAWN
— see the framing correction immediately below. HOLD M2/M3; do not drop them on
the strength of A.**

> **Framing correction, 2026-08-06 (operator): THIS FLEET IS THE INSTRUMENT, NOT
> THE AUDIENCE.** The recommendation first written here was "drop M3 and M2",
> reasoning from A's `count = 0` plus B's mild ×2.13. That reasoning generalized
> a measurement of *our three machines* into a claim about the product, and the
> product targets a whole range of deployments we do not have on the bench.
>
> **The specific error: A tested the least representative configuration.** M3-A
> requires "four nodes, each `solo` for the same model" — i.e. *every node is a
> holder*. In that shape local-first is nearly tautological, and `count = 0` is
> unsurprising. But §4.5's consumer story — the shape the product is actually
> for — is **thin clients that hold no model at all**. There, every request is
> cross-node by construction: the herd is not an edge case to be dissolved, it is
> the steady state. A's zero says nothing about it.
>
> **So B is the load-bearing experiment, and it was run at toy scale.** Three
> originators, six requests, a 0.8B, a ~27-token prompt. The product's shape is
> many thin originators against one holder, with real prompts and a large model —
> where prefill dominates and the ×2.13 has no reason to hold. That measurement
> is the one that decides M2, M3 and M5, and it has not been taken.
>
> **What survives as a product-level claim, because it is a statement about a
> mechanism rather than about our hardware:** C5's finding that crossing a
> boundary costs ~71% of throughput at these link speeds — so tensor-splitting is
> a last resort, which deprioritizes M4 on any fleet with a box big enough. And
> the absence of prefix reuse, which every deployment pays and thin clients pay
> hardest, since they re-send context they cannot cache.
>
> **A note on this document's exit clause.** "Not worth continuing if measurement
> shows one box serves *the realistic fleet*" invites exactly the error above, by
> naming a fleet rather than a target deployment. It should be re-scoped to the
> product's intended deployments before it is used to stop anything — an
> objective-level edit, and the operator's call, not this document's.

> **RE-SCOPED 2026-08-06 (operator): the target is BOTH shapes — peer fleets AND
> hub-plus-thin-clients. The exit clause is therefore settled, and it does NOT
> fire.**
>
> The clause was never answerable as written, because "is one box enough" has a
> different answer per deployment, and the two the product targets disagree:
>
> | | peer fleet — every node holds models | **hub + thin clients (§4.5)** |
> |---|---|---|
> | evidence | M3-A `count = 0`; C5's ~71% boundary cost; the 122B fits solo | M5's 100 s at N=9 distinct contexts; M6-C's single holder |
> | herd | dissolves on its own | **is the steady state** — every request is cross-node by construction |
> | verdict | one box suffices | one box does not |
>
> Every "one box is enough" datapoint was measured where **every node holds the
> model**, which is the least representative configuration for §4.5 and the one
> the framing correction above already flags. Targeting both shapes means the
> harder one governs, since a peer fleet is a strict subset of a consumer
> deployment's requirements.
>
> **Consequences, which are now requirements rather than proposals:**
> - **M5's bound is REQUIRED.** The 100 s case IS the consumer deployment, and
>   thin clients pay it hardest — they re-send context they cannot cache, so
>   prefix reuse never rescues them.
> - **M2 Experiment A matters.** A consumer must never be routed to a box that
>   physically cannot run the model; "excluded" and "out-scored" are different
>   operator problems.
> - **M6's peer-residency instrument gap is worth fixing** (note 30f49807).
>   Nothing reports which models a peer holds, which is the question every
>   consumer routing decision implicitly answers.
>
> The decision rested on cost asymmetry, not on certainty: choosing the peer
> fleet and being wrong ships a system that stalls at 2–9 concurrent consumers
> with no signal; choosing both and being wrong builds one defensive bound that
> never fires.

> **A — the design's own claim. PASSED, count = 0, on BOTH routing paths.**
> All three online nodes advertise the `fast` alias, i.e. each is `solo` for the
> same id. Fired simultaneously at each node's own client API — peers reached
> through their iroh bridge forward ports, which land on their own `:9741`, so
> each peer originates as *itself* rather than being proxied.
>
> | originator | served as | cross-node? |
> |---|---|---|
> | RuggedFox | `Qwen3.5-0.8B-UD-Q6_K_XL` | no |
> | BeefyMac | `Qwen3.5-2B.Q6_K` | no |
> | LittleMac | `Qwen3-0.6B-Q4_K_M` | no |
>
> **The named path passes trivially** — `locate_named_model` returns Local
> whenever `self_manifest` carries the id — so it was re-run on the **scored**
> path (no pinned model, `mesh_allowed` + `latency_class: normal`, which is
> offload-eligible), because that is where a scorer could send everyone to the
> strongest node. Also zero: RuggedFox→0.8B, BeefyMac→2B, LittleMac→its 4B.
> **No herd forms, and no part of M2 or M3 is shipped.** §4.3's ordering is
> already the observed behaviour.
>
> Incidental but load-bearing for thin clients: `fast` resolved to a **different
> model on every node**. The alias is node-relative by design, so a mesh-wide
> client cannot treat `fast`/`primary` as naming one thing — worth stating
> wherever those aliases are documented.

> **B — quantify the herd. RUN; no pass/fail by design (§18.4).** The inverse
> case with *genuinely distinct* originators: `Qwen3.5-0.8B-UD-Q6_K_XL` is held
> only by RuggedFox (checked against all three manifests), so a request for it
> from BeefyMac or LittleMac must forward there. Two requests per node, six
> concurrent, against a 981 ms solo baseline taken first on a warm slot.
>
> | originator | latency | served |
> |---|---|---|
> | RuggedFox #0 / #1 | **980 / 1001 ms** | local |
> | LittleMac #0 / #1 | 2089 / 2089 ms | `@ peer RuggedFox` |
> | BeefyMac #0 / #1 | 2367 / 2391 ms | `@ peer RuggedFox` |
>
> p50 2089 ms = **×2.13** of solo; worst ×2.44; 6/6 served, nothing shed.
>
> **The finding is who pays.** The local originator's two requests ran at
> **exactly the solo baseline** (980, 1001 vs 981 ms) while four remote requests
> were in flight. Contention is not shared — it is borne entirely by the remote
> originators. And the penalty is far below serialization: six requests through a
> serializing slot would put the last at ~5.9 s, not 2.4 s, so the ~1.1–1.4 s
> remote delta is mostly transport and forward overhead rather than queueing.
>
> **Scope limit, stated so this number is not over-read:** measured on the 0.8B
> with a ~27-token prompt, where prefill is negligible. Contention on the 35B
> primary with realistic prompts would be dominated by prefill and could
> serialize hard — that case is unmeasured, and it is the one worth running
> before any admission work (M5).

> **B AT PRODUCT SCALE — RUN 2026-08-06. THE ×2.13 DOES NOT HOLD. THE 35B
> SERIALIZES, AND M5 IS NOW JUSTIFIED BY MEASUREMENT RATHER THAN ARGUMENT.**
>
> `Qwen3.6-35B-A3B-MTP-UD-Q6_K` (RuggedFox-only; BeefyMac holds the *different*
> IQ4_NL 35B), ~4,568-token prompt — 169× the toy's 27 — **streaming**, because
> that is the product's chat path *and* it has no prefix reuse, so a cache hit
> cannot masquerade as low queueing. Every request carries a unique marker line
> so no two share a token prefix, with sizes held constant. Solo baseline 8,061 ms
> (of which **7,983 ms is time-to-first-token — the unit of work here is ~99%
> prefill**).
>
> | N | p50 | vs solo | worst | vs solo | local p50 | remote p50 |
> |---|---|---|---|---|---|---|
> | 1 | 8061 | ×1.00 | — | — | 8061 | — |
> | 3 | 18046 | ×2.24 | 27776 | ×3.45 | 8985 | 22911 |
> | 6 | 28919 | ×3.59 | 49017 | ×6.08 | 12371 | 36940 |
> | 9 | 40574 | **×5.03** | 74303 | **×9.22** | 16107 | 53301 |
>
> **It is textbook serialization, and the ladder is what proves it** — a single N
> could not have. At N=9 the nine latencies land on clean multiples of the 8 s
> baseline: 8.0, 16.1, 24.2, 32.4, 40.6, 48.7, 57.9, 65.0, 74.3 s. Nine requests,
> nine ~8 s slots, each waiting for every one ahead of it. The toy run's six
> concurrent requests did *not* do this (worst 2.4 s against a 981 ms baseline,
> where serialization would have predicted 5.9 s) — so the mild number was an
> artifact of a prompt too small to occupy the slot.
>
> **Remote originators are systematically served last, and the stratification is
> perfect.** At N=9: local took slots 1–3, BeefyMac 4–6, LittleMac 7–9. No
> priority rule produces this — remote requests simply *arrive* later, having paid
> the forward first, so they queue behind. But the effect is what matters to a
> product: **a thin client is served after every request the holder originated
> itself.** Local is no longer free either (×2.0 at N=9), just first.
>
> **Nothing shed. 9/9 served, the last after 74 seconds of silence.** There is no
> admission ceiling, no queue bound and no backpressure — the client simply
> hangs. That is M5's case, and it is no longer speculative: at nine concurrent
> users of one holder, the ninth waits over a minute with no signal that anything
> is wrong. The difference between "slow" and "appears broken" is exactly what M5
> would buy.
>
> **THE QUEUE IS MADE OF PREFILL, WHICH IS WHY PREFIX REUSE OUTRANKS EVERYTHING
> ELSE HERE.** 99% of each 8 s slot is time-to-first-token. Prefix reuse does not
> merely save a turn — it shrinks the *unit of serialization*, so it divides the
> whole queue rather than subtracting from it. The two findings compound
> multiplicatively. Caveat kept honest: the 13.1× was measured on the 0.8B, and
> the equivalent gain on this 35B is **unmeasured** — the mechanism (whole-state
> restore) should carry over in kind, but the factor should not be assumed.
>
> **What this does NOT settle, and it cuts against M3 rather than for it.** The
> herd measured here is not a routing-choice herd — it is a *queue at the only
> holder*. Local-first routing cannot dissolve it, because for a model one node
> holds there is nowhere else to send anything. M3's mechanism still does not
> address the product's problem; the earlier reasoning for that conclusion was
> wrong, but the conclusion survives on this different and better ground.
>
> **AFTER PREFIX REUSE — re-run 2026-08-06, same harness, same ladder. The
> absolute numbers fall ~10×; the SHAPE does not change, and that distinction is
> the whole point.** The chat streaming path now shares one prefill body with
> `generate_sync` (`ModelSlot::prefill_reusing_prefix`), so it reuses a pinned
> prefix instead of re-prefilling every token.
>
> | | before | after | |
> |---|---|---|---|
> | solo baseline | 8061 ms | **756 ms** | ×10.7 |
> | N=9 p50 | 40574 ms | **4129 ms** | ×9.8 |
> | N=9 worst | 74303 ms | **7671 ms** | ×9.7 |
> | N=9 p50 ÷ baseline | ×5.03 | ×5.46 | *worse* |
> | N=9 worst ÷ baseline | ×9.22 | ×10.14 | *worse* |
>
> **The queue is smaller, not gone.** The ratios to baseline got slightly worse,
> because fixed per-request overhead and state-file I/O are now a larger share of
> a much shorter slot. So prefix reuse divided the *unit* of serialization by ten
> and left the serialization itself intact — **M5 is still required**, and can now
> be sized against cached prefill, which was the reason to sequence it second.
>
> **Scope of the win, which must travel with the number:** this is the
> SHARED-CONTEXT shape — N clients on the same document or repo, asking different
> questions. Clients with genuinely distinct contexts get no cross-client
> benefit; each still gains on its own repeat turns. The original run above used a
> *unique* prefix per request precisely to isolate contention, so it shows no
> reuse and is not the comparison for these numbers.

> **Instrument limit, stated plainly:** the nine originators were 3 machines × 3
> connections, not 9 distinct thin clients. Queueing at the holder is
> indifferent to that — the slot cannot tell the difference — so the
> serialization result generalizes. The *stratification* by class does depend on
> having two peers at different distances, and per-client network variance is
> absent. Real N≥9 thin clients need more nodes or a deliberate simulation.

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
**Status: WITHDRAWN 2026-08-06 — not runnable on this fleet, and its premise is
downstream of parked work. Not refuted; closed.**

> **Why this is being closed rather than carried.** A NEVER-RAN that cannot be
> run is not a backlog item, it is a permanent placeholder, and carrying one
> across frames is how a lineage turns into tweaking. Three independent
> blockers, each already documented in this section:
>
> 1. **Experiment A needs a third GPU-class machine and there isn't one.** This
>    section already says so: "the only candidate is an Intel Mac whose own
>    compute would *become* the measured delta."
> 2. **Running the runnable half would destroy the evidence it examines.**
>    `MAX_RUNS_PER_KEY = 8` (`mesh_measurements.rs:177`) and the 122B 2-node key
>    already holds 7 rows — the 72.9–100.7 ms spread that motivated restating
>    the criterion. Five new runs evict all but two of them.
> 3. **Experiment B tests unbuilt code.** It asserts a cross-domain tensor split
>    is *refused* before weights move, but M4's Change (domain-restricted
>    tensor-plane participation) was never built, so B has nothing to refuse
>    with. It also sits downstream of the ggml-RPC-over-iroh work, which is
>    parked.
>
> **What was genuinely earned here and should outlive the milestone**, because
> it is method rather than result:
> - The original pass band (84.7–103.5 ms) sat *inside* the existing null
>   distribution, so the experiment could not fail. The restated
>   band-over-≥5-interleaved-runs form is the correct shape and is worth reusing
>   for any future throughput claim (§18.5).
> - `decode_tok_s`, never `itl_p50_ms`: with MTP draft acceptance on, the same
>   model at the same placement reads `itl_p50` **0.1 ms** at 69.9 tok/s,
>   because accepted drafts arrive in bursts. Same key, +58% decode, nothing in
>   the record saying which regime produced it.
> - The negative control (5 solo benches with concurrent `ping`): solo
>   throughput varied 0.77% while the link varied 39%, r = +0.21 at n=5. Solo
>   throughput is ~50× less link-sensitive than the link is to itself, so any
>   link-dependence in a 2-node run is attributable to the boundary. That
>   control is reusable and its absence would have made a 2-node correlation
>   unreadable.
>
> **Reopen if** a third GPU-class node joins the fleet, or if distributed
> tensor-split work comes off the park. Back up
> `~/.sovereign/mesh-measurements.json` first — see blocker 2.

**Original status: NEVER-RAN**

**Change.** Tensor-plane participation restricted to one domain (§4.1),
refusal not penalty.

**Experiment A — justify the threshold before enforcing it.** Add a third node
to a 122B tensor split and measure ITL.

> **Correction, 2026-08-06 — the criterion below cannot fail as written, and
> the constant it rests on is unearned.** The stated band, 94.1 ms ±10%, is
> 84.7–103.5 ms. The measurement store already holds **six valid 2-node runs of
> this exact configuration spanning 72.9–100.7 ms** (decode 7.746–11.083 tok/s).
> The pass band sits *inside* the existing null distribution, so a 3-node run
> landing at 95 ms would "pass" while being indistinguishable from a 2-node run.
>
> Worse, the 21.2 ms per boundary is `72.9 − 51.7`: the **best** of those six
> 2-node runs minus a **single** solo run, measured on a different day.
> ARCH_PRINCIPLES §18.5 names this exact smell ("a single run is not a
> measurement"; "establish the noise floor at the sample size you are using,
> then decide what counts as a delta"). Two independent cross-checks on the 4B
> in the same store give +26.4 ms and +22.4 ms for one boundary, so the true
> figure is plausible but its spread is wide and unrecorded.
>
> **Restated so it can fail.** Let `B_solo`, `B2`, `B3` be the p10–p90 bands of
> `itl_p50_ms` over **≥5 valid runs each, interleaved in one session**
> (solo, N2, N3, solo, …) with `ping` sampled concurrently. Interleaving is not
> optional: the existing 51.7-vs-72.9 delta compares runs from different days.
> - **Passes if** `p10(B3) > p90(B2)` and `median(B3) − median(B2)` is within
>   ±50% of `median(B2) − median(B_solo)`.
> - **Refuted if** `median(B3) ≤ median(B2)` — throughput did not fall.
> - **Could-not-judge if** `B2` and `B3` overlap. On this link that is a likely
>   outcome and must be reported as such, never as a pass.
>
> **Also worth 15 minutes:** correlate concurrent ping RTT against
> `itl_p50_ms`. One boundary is one blocking round trip per token, and this
> link's 16 KiB variance is large enough to explain the store's entire 2-node
> spread on its own. If ITL tracks concurrent RTT, every 2-node number on
> record is a Wi-Fi sample rather than a topology sample.
>
> **Status on this fleet: NEVER-RAN, and not currently runnable.** A third
> node requires a third GPU-class machine; the only candidate is an Intel Mac
> whose own compute would *become* the measured delta. The runnable half is to
> re-earn the per-boundary constant as a band on the existing pair.
>
> **Instrument validated 2026-08-06 — the negative control this experiment
> needed.** Five solo benches with `ping` sampled concurrently:
>
> | | spread across 5 runs |
> |---|---|
> | concurrent ping (avg) | 21.5 → 32.9 ms — **39%** |
> | solo `decode_tok_s` | 69.64 → 70.18 — **0.77%** |
>
> Pearson r = +0.21 at n=5, i.e. noise. **Solo throughput is ~50× less
> sensitive to the link than the link is to itself**, which is what a boundary-
> free configuration must show. So the instrument is sound and any
> link-dependence observed in a 2-node run is attributable to the boundary
> rather than to measurement noise. Without this control, a 2-node correlation
> would have been unattributable.
>
> **Use `decode_tok_s`, not `itl_p50_ms`.** This section's 21.2 ms constant and
> the N=3/N=4 table above are built on `itl_p50`, and that metric is not sound
> across model shapes. Measured the same day on one model at one placement:
> with speculative decoding off, `itl_p50` is exactly `1000/decode`
> (22.7 ms ↔ 44.1 tok/s); with MTP draft acceptance on, the same model at the
> same placement reads `itl_p50` **0.1 ms** at 69.9 tok/s, because accepted
> drafts arrive in bursts and the real step cost moves to p95 (~57 ms). Same
> key, +58% decode, and nothing in the record says which regime produced it.
> `decode_tok_s` is `(frames-1)/(last-first)` with TTFT excluded by
> construction (`mesh_bench.rs:255`) and is sound for both shapes.
>
> **Operational hazard before running the 2-node half: `MAX_RUNS_PER_KEY = 8`**
> (`mesh_measurements.rs:177`). The 122B 2-node key already holds 7 rows — the
> 72.9–100.7 ms spread that motivates this whole restatement. Five new runs
> would evict all but two of them, so **running the experiment would delete the
> baseline the experiment exists to re-examine.** Back up
> `~/.sovereign/mesh-measurements.json` first. (Learned the hard way: eight
> solo benches evicted the 35B's entire prior history the same day.)

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
**Status: ALL THREE PIECES SHIPPED. Bound 2026-08-06 (default 30 s,
`SOVEREIGN_MAX_QUEUE_WAIT_SECS`). Piece 1 (make the wait visible) SHIPPED ·
piece 2 (bound it) SHIPPED · piece 3 (stamp `X-Node-Id`) SHIPPED in `a45905e9`
(`peer_inference.rs:1199`). The `Retry-After` HEADER gap (note bef03728) is
CLOSED 2026-08-07 — see "How a shed reaches the client" below.**

> **The bound, as built.** `SlotQueue` (`sovereign-inference::embedded::model_slot`)
> now owns the permit, the depth gauge, the turn-duration EWMA and the shed
> threshold together, because the shed decision is a function of all four and
> splitting them is what let the queue go unmeasured. Two accessors over one
> body: `ModelSlot::acquire_inflight` (fast/extras) and
> `EmbeddedLlamaCpp::acquire_lazy` (the primary — the one that actually
> serialises).
>
> **It bounds PREDICTED WAIT, not depth**, which is the whole lesson of the
> measurement above: the same depth of 8 cost 6.2 s shared-prefix and 90.7 s
> distinct-context. A depth bound cannot express a rule that is fine in one
> shape and catastrophic in the other; a wait bound can, because the EWMA
> tracks whichever shape the host is in. The estimator is
> `commonwealth_core::fair_sched::EtaEwma`, **extracted from `SchedCore` so the
> chat scheduler and the inference gate share ONE implementation** of "how long
> will this caller wait" (§10.6) rather than growing a second.
>
> Shedding happens BEFORE parking — a caller that pays the wait and is refused
> anyway is the worst of both worlds — and returns
> `Error::QueueShed { position, predicted_wait_ms, retry_after_secs }`,
> structured so the HTTP boundary and a peer load balancer can branch rather
> than parse prose (§18.3).
>
> **The park itself is also bounded** (added 2026-08-18, order
> daemon-empty-candidates-error). The pre-park shed above is an optimisation
> over the EWMA of COMPLETED turns (`SlotPermit::drop`); a turn that never
> completes never folds in, so the prediction can stay under the bound while
> the actual wait grows without limit — the wedge that hung two wl-judge
> calls 62 min with a `candidates=[]` decision and no outcome. `acquire_owned`
> now runs under `tokio::time::timeout(max_wait_ms)` and returns the SAME
> named `Error::QueueShed` (one decider, one threshold, one retry hint) when
> the bound hits — the queue's own `=0` escape hatch is preserved verbatim.
>
> **How a shed reaches the client** (closed 2026-08-07). Being structured
> *inside* the process was not enough: the trait between the engine and the
> HTTP layer returned `Result<_, String>`, so `routes_inference` flattened
> every shed into `{"type":"backend_error"}` with the retry hint buried in
> prose and **no `Retry-After` header at all**. A client could not tell
> backpressure from a crash. Measured on the live fleet, before and after:
>
> ```
> BEFORE  503 {"error":{"message":"local inference failed: host busy:
>               ~34746 ms predicted wait at queue position 6;
>               retry after 35s","type":"backend_error"}}     ← no Retry-After
> AFTER   503 {"error":"host busy: ~38520 ms predicted wait at position 4",
>               "reason":"local_queue_full","retry_after_secs":39}
>               + Retry-After: 39
> ```
>
> The carrier is `commonwealth_api::state::LocalInferenceError` — a
> two-member closed set (`Shed{..}` / `Other(String)`) on the two chat
> methods only, since nothing else can shed. `Display` + `From<String>`
> keep every existing `%e` call site unchanged. One translation
> (`inference_adapter::map_provider_error`) and **one renderer**
> (`admission::local_queue_shed_response` → `admission::shed_response`,
> which the peer-admission middleware now also calls instead of building
> its own response) — §10.6, so a shed cannot render as backpressure on
> one path and as a crash on another.
>
> **What this did NOT fix.** A shed still *fails* the caller when the peer
> has also declined; only its legibility changed. `mesh-live-probe` still
> reports `routing.no_hard_failure_when_servable` and
> `shed.never_fails_a_servable_caller` as violated. Trying the NEXT online
> peer is the fix for that, and it is deliberately unbuilt: over 15,728
> outcome records in `decisions-EXP.jsonl` spanning 37 h, the local queue
> shed fired **zero** times in organic traffic, and every historical
> double-shed failure predates `64c286bc`. The only occurrences are the
> probe's own 8-concurrent synthetic load. Re-check once the bound has days
> of real exposure. Open question for the operator: the probe counts any
> `503` as a hard failure, which now conflates an opaque crash with a
> well-formed refusal — revising that assertion would turn the probe green
> without the next-peer fix, and must be a deliberate decision, not a
> quiet one.
>
> **30 seconds is an operator decision, not a derived constant**, and the
> tradeoff it encodes is written at the constant: shedding gives a hub
> deployment with no alternative holder *nothing* instead of a slow answer.
> `=0` restores the pre-M5 unbounded wait.
>
> **Gates.** lint `--full` 0 errors workspace-wide; 312 + 21 + 15 in
> sovereign-inference, 161 commonwealth-core, 159 sovereign-contracts, 68
> sovereign-server. Seven queue tests, and the shed gate was **watched fail**
> by disabling the bound: 2 red in 3 s. The first version of those assertions
> HUNG instead of failing (the caller parks behind a permit the test never
> releases), so they are bounded by an `expect_shed` helper — a gate that hangs
> on the failure it exists to catch is worse than no gate (§18.1). M3-B at product scale (the 35B, ~4.5k-token prompts) showed the
holder serializing: nine concurrent originators produced nine ~8 s slots, the
ninth answering after **74 s of silence**, with nothing shed and no backpressure.
There is currently no ceiling, no queue bound and no signal to the client — the
difference between "slow" and "appears broken" is precisely what this milestone
buys. Note the ordering dependency: ~99% of each slot is prefill, so **prefix
reuse shrinks the unit this queue is made of** and should land first — an
admission ceiling sized against un-cached prefill would be sized against a
number we are about to change.

**Change.** A finite peer ceiling (today `usize::MAX`, `state.rs:370`), a
bounded queue reporting position, and `503` + `Retry-After` past it — replacing
today's silent block inside `Semaphore::new(1)` up to the 1800 s client timeout.

**Experiment. RUN 2026-08-06 — the MECHANISM PASSES, the WIRING FAILS.**
Drive one peer past the ceiling with concurrent requests.
- **Passes if** surplus requests receive a `503` with `Retry-After` within
  ~1 s.
- **Refuted if** they block silently. Note the current behaviour would *also*
  eventually return an answer, so a naive "did it work" check passes today —
  the experiment must assert on **time-to-rejection**, not on the final result.

> **Result.** Run as two arms against RuggedFox, four concurrent requests each.
> The only difference between them is one HTTP header.
>
> | arm | outcome |
> |---|---|
> | **no `X-Node-Id`** — the shape all real mesh inference has | all four `200`; `peer_inflight_current` stayed **0** throughout; completions serialized at 2.00 / 3.02 / 4.78 / **6.41 s** |
> | **with `X-Node-Id`**, local user active | `503` + `Retry-After: 34` + `{"reason":"yielded_to_local"}` in **0.01 s** |
> | **with `X-Node-Id`**, node quiet | `200` in 0.30 s — admits correctly, so it is not a blanket refusal |
>
> So the machinery meets M5's criterion outright — sub-second rejection, a
> concrete `Retry-After`, and a *named* reason — and it is simply never
> consulted, because nothing stamps the header. The refutation condition
> ("they block silently") is what actually happens to every peer request: the
> fourth waits **6.4 s** with no signal, bounded only by the 1800 s client
> timeout. Serialization comes from `Semaphore::new(1)` on the slot, not from
> the ceiling — the ceiling never sees the traffic.
>
> **This makes the remaining work smaller and the decision sharper than "build
> admission control".** Stamping `X-Node-Id` is a few lines, but it is a
> *policy* change, not a mechanical one: it immediately arms `yielded_to_local`,
> so a host with an active local user starts refusing peer work it currently
> accepts. That is presumably the intent of reciprocity, but it should be an
> operator decision made deliberately, not a side effect of a plumbing fix.

> **Correction, 2026-08-06 — M5 IS A WIRING MILESTONE, NOT A BUILD.** The
> "Change" line above asks for a finite ceiling, a bounded queue reporting
> position, and `503 + Retry-After` past it. **All three already exist and are
> shipped**, in `commonwealth-core/src/fair_sched.rs`: `SchedCore<K>` is a pure
> weighted-fair policy with `max_queue_depth`, 1-based live `position`, an EWMA
> `eta`, a per-origin anti-hog `cap`, runtime `set_slots`, and a `Shed` carrying
> `would_be_position`. `sovereign-server/src/scheduler.rs` is the async shell
> over it (`FairScheduler`, `Notify`-per-waiter), already emitting
> `ServerEvent::QueuePosition` on the WS chat path and shedding with a position
> hint on REST. This is the same discovery shape as M6's "Experiments A and B
> need NO CODE".
>
> **The gap is that the queue which actually serialises is not the queue the
> scheduler manages.** Three separate bounds sit on one GPU:
>
> | bound | where | consulted by peer inference? |
> |---|---|---|
> | `FairScheduler` (bounded, reports position) | `sovereign-server`, `/v1/conversations/*` | no — different route |
> | peer ceiling (default **1**, not `usize::MAX`) | `commonwealth-api/admission.rs`, gated on `X-Node-Id` | no — header never stamped on inference |
> | `Semaphore::new(1)` per slot (**unbounded wait, no signal**) | `sovereign-inference` `ModelSlot::inflight` | serialises the *fast/extras* slots only |
> | `Semaphore::new(1)` engine-wide (**unbounded wait, no signal**) | `sovereign-inference` `EmbeddedLlamaCpp::lazy_inflight` | **this is the one that serialises the PRIMARY model** |
>
> Note the ceiling correction: `state.rs:370`'s `usize::MAX` is the
> *pre-configuration* constant only. `sovereign-mesh::daemon` applies
> `DaemonSection.max_peer_inflight` at boot (`daemon.rs:2442`), whose default is
> **1** (`setup_config.rs:1069`). So the ceiling is finite in production — it is
> simply never reached, because peer inference arrives with no `X-Node-Id`.
> `peer_inference.rs:1056` stamps the header on the **manifest capabilities
> fetch** only, never on the forwarded chat completion.
>
> So M5 decomposes into three pieces of very different character, and only the
> first is policy-free:
>
> 1. **Make the wait visible** (DONE, this commit). There are TWO such gates and
>    **thirteen** bare `.acquire_owned().await` sites between them, none with any
>    timing, depth or tracing event — so the single most load-bearing wait in the
>    system was the one thing `tracing=debug` could not show. Now two accessors
>    over one shared body (`acquire_with_queue_gauge`), reporting `ahead` and
>    `waited_ms` at `info` on the crate target the daemon filter already admits:
>    `ModelSlot::acquire_inflight` (8 sites, per-slot) and
>    `EmbeddedLlamaCpp::acquire_lazy` (5 sites, engine-wide).
>
>    **Instrumenting only the per-slot gate would have measured nothing.** The
>    first pass did exactly that, and the N=9 ladder came back with ZERO
>    contention events against a client-side trace showing textbook
>    serialization. The configured primary model is served from the *lazy* slot
>    (`slot="primary"`, phase `complete_stream_with_finish/lazy`), so every
>    big-model chat turn queues on `lazy_inflight` and none of them touch
>    `ModelSlot::inflight`. Had the analyzer reported "no contention" as a
>    finding rather than as a could-not-judge, the conclusion would have been
>    confidently backwards (§18.2).
> 2. **Bound it** — needs a measured depth distribution first, which (1) is the
>    prerequisite for. The bound belongs at that single accessor.
> 3. **Stamp `X-Node-Id`** — the policy flip. Arms `yielded_to_local` and the
>    ceiling of 1 against real peer traffic. Operator's call, per the note above.
>
>    **DONE, and the operator made the call: stamp it.** The header now goes on
>    every forwarded completion, streaming and non-streaming, from
>    `peer_inference.rs::provider_for_peer` — the one constructor through which
>    all four routing paths reach a peer, so a fifth path inherits the stamp
>    rather than forgetting it. It rides in `RemoteApiProvider` as an opt-in
>    `node_id` applied by a single `stamped()` body shared by all seven
>    outbound methods; the provider also serves OpenAI, Ollama and bench
>    endpoints, which must NOT be told a node identity.
>
>    **The stamp shipped with a second change, and shipping it alone would have
>    been a regression.** Before it, no peer inference could ever be shed, so
>    nothing had to decide what a shed MEANS. After it, a `503 yielded_to_local`
>    arrives on the ordinary transport-failure path, where
>    `PeerHealthTracker::record_failure` quarantines a peer for 60 s after
>    `FAILURE_THRESHOLD = 3` consecutive failures — and a quarantined peer is
>    dropped from the candidate set *before* its manifest is read. With the
>    ceiling at its default of **1**, three concurrent turns is all it takes.
>    The mesh would have benched its healthiest, busiest neighbours precisely
>    when they were most in demand.
>
>    So `book_peer_failure` now exempts sheds from peer HEALTH, while leaving
>    the load-balance bookkeeping untouched: the in-flight counter still
>    decrements (skipping it leaks the count and permanently mis-ranks the
>    peer), and the failure EMA still nudges the scorer away from a peer that
>    just said it was full. Backing off is correct; declaring it broken is not.
>    `decision_log::looks_shed` is the one decider, already the source of the
>    `shed` flag on `FailoverAttempt` (§10.6).
>
>    **Gates (§18.1, watched fail).** Three new e2e tests in
>    `chat_completion_e2e.rs` and three wire tests in `oicp-client`. Disarming
>    both mechanisms turned exactly two tests red —
>    `a_peer_routed_turn_identifies_this_node_to_the_peer` and
>    `repeated_sheds_never_quarantine_a_healthy_peer` — while
>    `repeated_faults_still_quarantine_a_broken_peer` stayed green, which is
>    what distinguishes "sheds are exempt" from "health no longer works". The
>    absent-header case is asserted at the wire, because an unstamped request
>    still succeeds and is invisible in every other observable.
>
>    **VERIFIED LIVE, 2026-08-06, and the run found one more thing.** BeefyMac
>    answered a forwarded turn with `503 {"reason":"yielded_to_local"}`. That
>    gate is unreachable when `is_peer == false` (`admission.rs:125-128`), so a
>    real peer naming that reason is positive proof the header left this node —
>    the one thing no amount of local logging could establish. The exemption
>    held too: four consecutive sheds, and the balancer still picked BeefyMac
>    for the 4th and 5th attempts, where booking would have quarantined it on
>    the 3rd.
>
>    Two numbers worth carrying forward. The **foreground-yield window is ~60 s
>    wide**, so one 1.9 s local turn makes a node non-quiet for a minute — on a
>    machine whose operator is working, peer inference is now refused nearly
>    always. And the **M5 bound never fired**: predicted waits of 4.7/7.1/5.4/6.7 s
>    against the 30 s threshold. Correctly quiet, but the bound remains
>    unexercised under real pressure — a could-not-judge, not a pass.
>
>    **THE REGRESSION THE STAMP EXPOSED, now fixed.** Four of five load-balanced
>    turns for `primary` came back to the client as a hard `503`, while
>    `primary` was loaded here and answering in 1.57 s. `locate_named_model`
>    was answering two different questions with one shape: *"the peer is the
>    only holder"* and *"we both hold it and the peer looked less busy"*. Only
>    the first makes a peer failure terminal — falling back there would serve a
>    different model under the caller's name, which is the substitution §18.3
>    forbids. In the second, falling back serves **exactly what was asked for**.
>
>    That distinction was already written down, in `locate_named_model`'s own
>    contract: *"when a caller names a specific model the daemon MUST honour
>    that name … but when multiple nodes advertise the same id, the choice
>    between them is a load-balancing decision, not a name-resolution
>    decision."* The code simply had no type for it. `LocalAlternative`
>    (`LocalHasIt` / `SoleHolder`) is that sentence made structural. The
>    streaming path expresses the fallback as an extra cascade step
>    (`Peer{Soft}` → `LocalNamed`), the non-streaming path as a branch into a
>    now-shared `complete_named_locally`.
>
>    Gated by four tests — two behavioural, two sole-holder controls that keep
>    the fix from becoming a licence to substitute. Reverting both halves turned
>    exactly the two behavioural tests red, one per routing path, while both
>    controls stayed green.
>
>    **The fourth instance — and it was acted on the same day.** This fix had
>    to be written twice, because `complete()` resolved its own route inline
>    while both streaming entry points shared `select_route`. That is the
>    fourth feature to land on one surface and need porting to the other,
>    after the forward budget, the privacy gate and the outcome join. The rule
>    agreed at the third was to act on a fourth.
>
>    **DONE: `complete()` now consumes the same `RoutePlan`.** The routing
>    decision has exactly one implementation; what stays per-method is only
>    how a step's terminus is built, which is genuinely different (a response
>    here, a stream there). `select_peer` — a thin "take the first peer" view
>    that existed solely for `complete()` — is deleted.
>
>    It is **not** a behaviour-preserving refactor and is not labelled as one
>    (§10.1). Three observable changes, each pinned by a test:
>
>    1. **Non-streaming ranked routing now walks the whole ranking.** It used
>       to try one peer and collapse to local; the streaming paths always
>       walked the cascade. Two declining peers now produce two failover
>       attempts, not one. Watched: the new test fails against the previous
>       body with `left: 1, right: 2`.
>    2. **Peer in-flight is booked on every peer route.** The named branch did
>       this and the ranked branch did not, so concurrent ranked callers all
>       read `peer_inflight = 0` and piled onto one peer. Uniform now.
>    3. **A named route pins the model it resolved onto the wire, on BOTH
>       surfaces.** This closes the gap `a_shared_primary_reaches_the_peer_but_does_not_yet_pin_its_target`
>       had pinned since it was written — that test asked to be updated
>       deliberately if the wire ever carried the id, and it now does. This
>       one is not cosmetic: a peer that resolves models strictly REFUSES a
>       request naming nothing, so the unpinned turn was answered with a
>       refusal and the cascade then served the caller this node's model
>       instead of the one they named. The unification surfaced it because
>       moving the pin onto the step is what made it a property of the
>       decision rather than of one hand-written body.
>
>    **Deliberately NOT changed:** the contribution ledger still emits only
>    from the streaming wrapper's lifecycle. Turning it on for non-streaming
>    would start booking contribution for traffic that has never been booked —
>    ledger-visible, and it belongs in its own commit.
>
>    **Coverage owed before the move, and landed first (§10.4):** the ranked
>    non-streaming path had no outcome assertions at all — the one test that
>    drove it discarded its result with `let _ =`. Three characterization
>    tests now pin peer-served, no-worthy-peer, and shed-then-local, and all
>    three passed *before* the refactor as well as after.
>
>    **THE REFACTOR SHIPPED A REGRESSION, AND THE SUITE WAS GREEN.** Caught on
>    a deliberate re-check after the commit, not by any gate. Pinning the
>    resolved model was applied to the `Peer` step but NOT to `LocalNamed`,
>    where `complete_named_locally` rewrote the request only when the id was a
>    slot *alias*. For an explicitly-named request that is harmless — the
>    caller already put the name on the request. **A shared primary is not
>    named by the caller; it is resolved by this node.** So a shared primary
>    that resolved locally reached the provider with `model_id: None`, and its
>    slot picker fell back to choosing by SPEED — the caller asked for the
>    shared model and would silently get whatever this node felt like serving.
>    The pre-unification body had guaranteed this by rewriting the request up
>    front, and that guarantee was dropped with the body.
>
>    Verified as a genuine regression rather than a pre-existing gap by running
>    the new assertion against the previous body: it PASSES there and FAILED
>    here. Fixed by pinning the effective id — the alias's target when the id
>    is an alias, the id itself otherwise — through the same `pinned_request`
>    helper the peer step uses, so "what model reaches the server" now has one
>    implementation for both destinations.
>
>    Note what this says about the two green runs that preceded it: **the same
>    blind spot produced the same false confidence twice in one change.** The
>    coverage audit named the shared-primary rewrite as untested on this
>    surface; both times the fix was to write the assertion, and both times it
>    went red immediately. A path an audit has just told you is unasserted is
>    not covered by a green suite — it is invisible to it (§18.1).
>
> **M5's justification, re-measured 2026-08-06 — IT IS STRONGER, NOT WEAKER,
> AND PREFIX REUSE DID NOT WEAKEN IT.** The tempting read after 674c228d was
> that the "74 s of silence ≈ appears broken" case had evaporated, since the
> same N=9 ladder now worst-cases at 7.7 s. That read is WRONG, and piece (1)'s
> instrument is what showed it. Prefix reuse did not shrink the queue; it shrank
> the queue's UNIT, and only for clients that share a prefix.
>
> Two N=9 ladders against the 35B, identical in every respect except whether the
> nine originators share one document. Host-side depth from
> `inference.queue`; both ran with a ~4.5k-token prompt, streaming, 9/9 served:
>
> | | shared prefix | **distinct contexts** |
> |---|---|---|
> | queue depth (`ahead`) | 1→8 | 1→8 |
> | wait once parked, p50 | 2.3 s | **60.8 s** |
> | wait once parked, max | 6.2 s | **90.7 s** |
> | client total, worst | 7.0 s | **100.2 s** |
> | unit of serialization | ~0.77 s | ~10.5 s |
>
> **Same depth. A 15× difference in what that depth costs.** The solo baseline
> is ~9.9 s either way, so the distinct-context unit is simply the uncached
> prefill — untouched by 674c228d, which can only help a client that follows
> someone else's prefix.
>
> This is the shape the previous session flagged as unmeasured ("distinct-context
> clients get no cross-client benefit, only their own repeat turns"), and it is
> the shape the product actually has whenever N users work on N different
> documents. At N=9 that is **100 seconds of silence with no signal, no
> position, and no shed** — comfortably "appears broken", and worse than the
> 74 s that justified this milestone in the first place.
>
> So piece (2) is justified, and the bound is now sizeable against a measured
> number rather than a guess: depth tracks N−1 exactly, and wait ≈ depth ×
> unit, where unit is ~0.77 s shared / ~10.5 s distinct. A depth bound alone
> cannot express that — **the bound wants to be on predicted WAIT, not on
> depth**, because the same depth is fine in one shape and catastrophic in the
> other. `SchedCore` already carries the EWMA `avg_turn_ms` needed to compute it.

---

### M6 — Consumers can reach a model they do not hold
**Status: A PASSED · B PASSED · C RUN — all three settled 2026-08-06, and all
four findings FIXED and verified live: B1 (refusal named the wrong cause), B2
(named path ignored `sharding`), C1 (cause identified — a stale peer daemon, not
load), C2 (streaming refusal emitted no outcome record).** A's `resp.model_id` sub-check remains could-not-judge — the
peer-residency instrument gap below, re-confirmed during C: neither
`svrn mesh status` nor `/v1/mesh/status` will tell you which models a peer holds,
so C had to discover it by firing concurrent calls and watching where they landed.

**C's verdict changes the milestone's scope: DROP session affinity.** Zero node
changes in 13 served calls, for structural reasons (local wins its own ties; a
peer-only model has exactly one holder here). The work C surfaced instead is
prefix reuse — affinity would have protected a cache that does not exist — and
a second holder, per finding C1.

The milestone that matters most for real use, because a consumer is where a
person actually touches this system (§4.5). It is listed last and should be
scheduled first: M2–M5 improve a topology that already works; M6 is about
whether the product works at all from a laptop.

> **Correction, 2026-08-06 — Experiments A and B need NO CODE.** §7's framing
> implies M6 is the largest build in this document. It is the smallest: both
> experiments run against already-shipped behaviour (`explicit_model_id` is
> checked first at `peer_inference.rs:1749`, `locate_named_model` at `:1503`,
> and M1's named-path downgrade at `:1791-1814`). They are the cheapest
> confirmations here and should be run before anything in M2–M5 is built.
>
> Two prerequisites for reading their results, both instrument problems rather
> than code problems. First, **gated decisions are dark at the shipped log
> level** — they log at DEBUG (`decision_log.rs:700-708`) while the daemon
> filter is `mesh.decision=info` (`sovereign-cli-daemon/src/lib.rs:73`), so
> every gate name these experiments look for is invisible by default. Set
> `SOVEREIGN_DECISION_LOG=<path>`; the JSONL sink writes regardless of level.
> Second, the named-path `forward_budget_exhausted` (`peer_inference.rs:1800`)
> carries **no `target:`**, so it rides `sovereign_mesh::peer_inference` and
> never reaches that JSONL — it needs `RUST_LOG=sovereign_mesh=debug`, and
> `RUST_LOG` *replaces* the daemon filter rather than extending it
> (`tracing_init.rs:27-28`). An empty log after a served request means the
> instrument is dead: the verdict is **could-not-judge**, never "no gate fired,
> therefore passed" (§18.2).

**Change.** A `consumer` participant class that binds to an entry node rather
than running a scheduler; session affinity so an agentic loop keeps its prefix;
and the retry-vs-forward distinction the current `saturating_sub` cannot make.

**Experiment A — the contract holds. RUN 2026-08-06 — PASSED.** From a machine
holding no model, issue a plain OpenAI request naming a model that only a peer
holds.
- **Passes if** the answer comes back and the served model id equals the name
  requested — no substitution.
- **Refuted if** it 503s, or if a different model answers. The latter is the
  worse outcome and the one to look for: it is a silent substitution, and a
  user would experience it as "the model got dumber", not as an error.

> **Result.** Non-streaming `POST /v1/chat/completions` on RuggedFox for
> `Qwen3-4B-Q4_K_M` — a model with no file in `sovereign/models/` on this host —
> returned `Qwen3-4B-Q4_K_M @ peer LittleMac` with a correct answer in 8.1 s.
> No substitution, no 503. The streaming form of the same request produced a
> decision record with verdict `named_peer{peer: LittleMac}`.
>
> **One sub-check is could-not-judge, and it is the one that matters most.**
> `resp.model_id` is assigned from *our* candidate view (`peer_inference.rs:2306`),
> not echoed from the peer, so the served id proves the routing decision but not
> which weights actually ran — the §18.1 "assert on something the subject cannot
> author" problem. Closing it needs the peer's `/status` during the window, and
> there is no surface for that: peer daemons bind `:9741` to loopback only, and
> the mesh exposes no peer-residency read. **Instrument gap to close before M6
> is called settled.**

**Experiment B — the loop is actually closed. RUN 2026-08-06 — PASSED.** Two
nodes, both with stale manifests naming the *other* as the holder. Issue a named
request.
- **Passes if** it terminates with `forward_budget_exhausted` in the trace.
- **Refuted if** it ping-pongs to a client timeout. This is the failure the
  named-path half of M1 was written for and it has never been reproduced —
  neither before the fix (to confirm the bug) nor after (to confirm the fix).
  **Reproduce it first**: a fix for a failure nobody has watched happen is a
  guess with a test attached (§18.1).

> **Result — the named non-streaming path is hop-bounded, and `forward_budget`
> is the sole decider.** Five arms on RuggedFox, all non-streaming
> `POST /v1/chat/completions` for `Qwen3-4B-Q4_K_M` (held only by LittleMac;
> RuggedFox advertises six ids and this is not one of them), same 60 s manifest
> cache window, differing only in the envelope:
>
> | arm | `forward_budget` | `sharding` | verdict | HTTP |
> |---|---|---|---|---|
> | 1 | absent (→1) | *no envelope* | `named_peer{LittleMac}` | 200, served |
> | 5 | 1 | LocalOnly | `named_peer{LittleMac}` | 200, served |
> | 4 | 1 | MeshAllowed | `named_peer{LittleMac}` | 200, served |
> | 2 | 0 | LocalOnly | `named_unknown` | 503, refused in 3.4 ms |
> | 3 | 0 | MeshAllowed | `named_unknown` | 503, refused in 1.7 ms |
>
> Budget 0 refuses, budget 1 serves, and privacy changes nothing in either
> direction. Arms 1 and 2 are 20 ms apart against the same cache, so the
> LittleMac candidate provably *was* present when arm 2 refused: the refusal is
> the hop bound, not a resolution failure. Every arm emitted a paired
> decision+outcome record on the **non-streaming** surface — which is the
> independent confirmation that the `resolve_named_dispatch` extraction closed
> the 2-records-streaming/0-records-non-streaming split.
>
> **Named substitution (§18.3): the mutual-stale manifest was NOT choreographed.**
> Making two live daemons each advertise a model neither holds requires lying to
> a peer's manifest, and the only non-invasive lever (the 60 s cache) would need
> both stale windows to overlap. Instead the *arriving* request shape was
> synthesized directly — a budget-0 envelope is byte-identical to what
> `oicp-client/src/lib.rs:356` stamps on a forwarded named request. Arms 2/3
> therefore measure termination-on-arrival, which is *sufficient* for
> boundedness: the bounce cannot outlive its first receiver. What they do not
> measure is the end-to-end two-hop bounce.
>
> **Caveat the doc comment already names:** the bound holds only between nodes
> whose build carries `forward_budget`. A forwarder on an older build sends
> `None`, which a receiver reads as a full budget (`requirements.rs:57-60`).
> Verified only on RuggedFox's build.

> **Finding B1 — FIXED 2026-08-06, verified live. The refusal message named the
> wrong cause and sent the operator to a dead end.** Arms 2/3 returned `no node in this mesh advertises
> model 'Qwen3-4B-Q4_K_M' — check /v1/models for available names`
> (`peer_inference.rs:1956-1959`). That is false: LittleMac advertises it and
> served it 20 ms earlier. An operator who follows the message's own instruction
> finds the model listed and has nowhere to go. `NamedModelLocation::Unknown`
> collapses two distinct causes — nobody has it, versus the hop budget is spent
> — into one string, and only the second is reachable by a forwarded request.
> The honest cause *was* in the debug trace (`gate = "forward_budget_exhausted"`),
> so this was a §1/glassbox defect at the user-facing edge, not a missing
> decision.
>
> **The fix.** `NamedModelLocation::Unknown` now carries a
> `NamedUnknownReason` — a closed set, so an enum (§2) — with three arms, and
> `NamedUnknownReason::refusal()` is the single renderer both refusal sites call.
> There were **two** copies of the old message (the streaming and non-streaming
> sites); that duplication is why B1 stayed invisible, and it is the same §10.6
> shape as the bug M1 was written for, so the message got the same treatment the
> budget did: one decider, one name.
>
> A third cause surfaced while fixing it and was equally misreported:
> `SOVEREIGN_DISABLE_PEER_INFERENCE` also produced "no node advertises", when a
> peer may well advertise it and the operator's own env var is the refusal.
>
> **Verified live** on the rebuilt daemon, three cases: hop-exhausted names the
> hop budget and what to do about it; a genuinely absent id still reports
> absence (the inverse error would be just as misleading); the happy path still
> serves `Qwen3-4B-Q4_K_M @ peer LittleMac`. Pinned by two tests in
> `scheduler_decision_records.rs` — the pre-existing budget-0 test asserted only
> that the refusal named the *model*, so it passed with the wrong message; it
> now asserts the cause, and a new companion test asserts the contrast, because
> a one-sided assertion is satisfied by a message that blames the hop budget
> unconditionally. Operator-facing row added to
> `commonwealth/docs/routing-field-guide.md §8`.

> **Finding B2 — FIXED 2026-08-06, verified live. The named path never
> consulted `sharding`, so a `LocalOnly` envelope crossed the trust boundary.** Arm 5 stated `sharding == LocalOnly` and was served by peer
> LittleMac. That contradicts this module's own rule 1 —
> "No OICP on the request, or `sharding == LocalOnly` → local"
> (`peer_inference.rs:18-19`) — and the forwarding-boundary gate written to stop
> exactly this (`routes_inference.rs:242-268`, "LocalOnly requests must NOT
> cross the trust boundary"). Neither fires, for one structural reason: the
> privacy check lives in `offload_verdict`, and named dispatch deliberately
> never reaches it (`peer_inference.rs:1795`); the `routes_inference` gate sits
> at Priority 1, *after* Priority-0 `local_inference` — which is the
> mesh-routing provider that forwards. So the named path inherited the hop bound
> when it was hand-added, but not the privacy half of what it bypassed. **This
> is the same shape as the bug M1 was written for: a gate written into one call
> site instead of into the decider (§10.6).**
>
> **Magnitude when found: latent, not live.** Exposure needed a caller that set
> `LocalOnly` *and* pinned a model name. A census of every non-test
> `CompletionRequest` construction across `sovereign/crates`,
> `commonwealth/crates` and `corpus-engine` found **zero** doing both — internal
> callers pin a name with no envelope (CLI/bench/gliner) or attach an envelope
> with no pinned name (the grounding judges, via
> `Workload::Judge.requirements(posture)`). So nothing leaked, and the fix could
> not regress an internal caller. But the defence was coincidence rather than
> structure, which is what §7 forbids.
>
> **The fix, and the trap in it.** The gate now lives in
> `resolve_named_dispatch` beside the hop bound — the one place both routing
> surfaces call — because a privacy check written into a single call site is how
> this happened in the first place (§10.6). The rule is:
>
> | envelope | `sharding` | may cross to a peer? |
> |---|---|---|
> | absent | — | **yes** |
> | present | absent → LocalOnly | no |
> | present | `local_only` | no |
> | present | `mesh_allowed` | yes |
>
> **An absent envelope had to stay permissive, and this module's rule 1 said
> otherwise.** Rule 1 read "No OICP on the request, *or* `sharding ==
> LocalOnly` → local"; implemented literally it would refuse every thin-client
> request for a peer-only model — the exact case M6-A proved works and the
> reason the mesh is useful from a laptop. The rule was stale for the named
> path and has been corrected in the module header. A present envelope that
> withholds `mesh_allowed` *is* an opt-out, because OICP §3.1 makes LocalOnly
> the default deliberately.
>
> **Ordering matters, and a test caught it being wrong.** The privacy arm was
> written first, ahead of the hop bound — but a forwarded request carries the
> budget-only envelope `oicp-client` stamps, whose privacy field is absent and
> so reads as LocalOnly. Privacy-first therefore reported "you asked for
> local_only" at a request whose real story was "someone already forwarded
> this" — B1's misattribution, one gate over. The budget arm now runs first: a
> spent budget is a fact about the request's history, privacy-by-default is an
> absence, and the fact wins.
>
> **Verified live**, four cases: `local_only` + full budget now refuses with the
> privacy reason (it was served by a peer before the fix); `mesh_allowed`
> serves; **no envelope still serves** (the M6-A regression guard); budget-0
> still reports the budget, not privacy. Pinned by four tests in
> `scheduler_decision_records.rs`, including the no-envelope guard and a
> `mesh_allowed` arm — without the latter, a gate that refused *every*
> envelope-bearing request would have passed.

**Experiment C — quantify the affinity gap before building affinity.** Run one
agentic coding loop (many sequential calls, long shared prefix) against a
two-holder mesh and record which node served each call and the TTFT of each.
- **No pass/fail.** It measures how often consecutive calls change node and what
  each change costs. If calls happen to stay put, affinity is a solution to a
  problem this fleet does not have and M6 should drop it — which is exactly why
  this runs *before* the work, not after (§18.4).

> **RUN 2026-08-06. The pre-registered exit condition fired: calls stay put, so
> DROP AFFINITY.** Zero node changes in 13 served calls across both arms — and
> not by luck, for two structural reasons that no amount of load will change.
>
> One agentic-shaped loop per arm: 8 sequential streaming calls sharing an
> identical 12,611-char (~3,150-token) prefix — a real source file, the way an
> agentic loop re-sends the file it is editing — varying only a one-line tail.
> TTFT measured client-side off the SSE stream, not read back from the server's
> own record (§18.1: don't assert on a number the subject authors).
>
> | arm | model | served | node sequence | switches | TTFT ms |
> |---|---|---|---|---|---|
> | local | `Qwen3.5-0.8B-UD-Q6_K_XL` (held here) | 8/8 | local ×8 | **0 / 7** | first 1931, median 1163, range 1014–1931 |
> | peer | `Qwen3-4B-Q4_K_M` (peer-only) | 5/8 | LittleMac ×5, then the holder vanished | **0 / 4** | 111755, 143817, 156603, 124229 (n=4 valid) |
>
> **Why "stays put" is structural, not incidental.**
> 1. **A model this node holds can never leave.** `locate_named_model` makes
>    local a candidate whenever `self_manifest` carries the id and breaks ties in
>    local's favour, so a named request on a holder is pinned local on every
>    call. Affinity has nothing to decide.
> 2. **A peer-only model has exactly one holder on this fleet.** Verified
>    directly rather than assumed: four *concurrent* named calls for
>    `Qwen3-4B-Q4_K_M` all went to LittleMac and queued there (~550 ms apart). If
>    a second node advertised it, `min_by_key(inflight)` would have split them.
>    With one holder there is nowhere else to go.
>
>    > **That probe had a hole, and it was closed by a stronger test.**
>    > `gather_peer_candidates` *skips* a peer whose manifest fetch fails, and
>    > BeefyMac's fetch over the iroh bridge was observed failing intermittently
>    > during these runs — so "all four went to LittleMac" was also consistent
>    > with BeefyMac holding the model and being invisible at that moment. The
>    > clean test became available once LittleMac dropped: with the sole known
>    > holder *offline*, a named request for the id resolved `named_unknown`
>    > ("no node in this mesh advertises…"). If BeefyMac held it, that request
>    > had every reason to land there. It did not. One holder, confirmed by a
>    > direct negative rather than by an absence of splitting.
>
> **And the thing affinity would have protected does not exist.** No call after
> the first was materially faster in either arm — the peer arm's TTFT rose before
> it fell (111.8 → 143.8 → 156.6 → 124.2 s) and the local arm's spread
> (1014–1931 ms) is wider than its first-call delta. A working prefix cache
> produces the opposite signature. So affinity would have been keeping requests
> loyal to a cache that is not there; **prefix reuse is the prerequisite, and it
> is the work worth scoping instead.**
>
> **The confound is now resolved — there is no prefix cache at all. Measured
> 2026-08-06, same day, locally, where nothing is failing.** The peer arm alone
> could not separate "no cache" from "failing peer", so the question was re-asked
> with an A/B/A design on the local 0.8B: three calls sharing a long prefix A, a
> control call on a *different* prefix B of similar length, then back to A. A
> working cache has exactly one signature — the repeats far below the cold call,
> the novel prefix back up at cold price. That is not what happens:
>
> | leg | prefix | TTFT | prefill tok/s |
> |---|---|---|---|
> | A1 (cold) | A, 8,423 tok | 4000 ms | 2106 |
> | A2, A3 (repeat A) | A | 3233, 2977 ms | 2605, 2830 |
> | B1 (**novel** B) | B, 10,214 tok | 3822 ms | 2672 |
> | A4 (back to A) | A | 3805 ms | 2213 |
>
> Normalised for length, every leg sits in 2100–2830 tok/s, and the two readings
> that settle it are these: a **novel** prefix is processed *faster per token*
> (B1, 2672) than a **repeated identical** one (A4, 2213), and returning to
> prefix A after B pays full cold price. The ~20% gap between A2/A3 and A1 is
> warm-up plus noise, which is precisely why the B leg is in the design — without
> it, "the second call was quicker" reads as a cache and is not one.
>
> So the peer arm's flat TTFT was never about that peer dying; it was the same
> absence, paid at ~130 s instead of ~3 s. **An agentic loop re-sending an
> 8.4k-token file pays full prefill on every single turn, everywhere.** That is
> the quantified case for prefix reuse, and it is a bigger number than anything
> session affinity could have returned.

> **Finding C1 — one agentic loop against a thin peer ended with the peer gone,
> and the originator had no fallback.** LittleMac stopped answering mid-call-4
> after four calls that each prefilled ~3,150 tokens at ~130 s. Timeline from
> RuggedFox: last gossip contact 11:28:06, peer-manifest transport errors from
> 11:28:56, call 4's stream ending with no content after 75 s, `gossip: peer
> marked Offline` at 11:29:31 on 74 s staleness. Calls 5–7 then returned a
> correct hard 503 (`named_unknown` — the sole holder genuinely was gone).
>
> **CAUSE IDENTIFIED, AND IT IS NOT LOAD — corrected 2026-08-06.** The
> reproduction below is real and the pattern held twice, but the operator
> supplied the mechanism RuggedFox could not see: **LittleMac was running an old
> daemon carrying a known Metal bug, and crashed on that.** So the reproducible
> "dies at call 4" is a *stale-binary* signature, not a capacity limit.
>
> **What this retracts.** The claim that "one agentic loop takes a thin peer off
> the mesh", and the operational rule that four ~3,150-token prefills is a thin
> peer's ceiling. Neither is supported: load was the trigger that exposed an
> already-broken build, not the cause. A reproduction identifies a trigger; it
> does not identify a mechanism, and this is a clean example of the difference.
>
> **What survives untouched**, because it never depended on why the holder died:
> a named request carries `soft=false`, so when its sole holder disappears the
> originator refuses rather than degrading — see the product consequence below.
> The TTFT series also stand as measurements of that build.
>
> The two runs, kept as the record of the trigger:
>
> | | run 1 | run 2 |
> |---|---|---|
> | calls 0–3 TTFT | 111.8, 143.8, 156.6, 124.2 s | 108.2, 103.5, 108.0, 110.9 s |
> | call 4 | stream ends with no content, 76.7 s | stream ends with no content, 50.1 s |
> | calls 5–7 | refused | refused |
> | peer marked Offline | 11:29:31 (staleness 74 s) | 11:48:59 (staleness 67 s) |
>
Four calls of ~3,150-token prefill, then it stops answering — twice, at the
> same call, ~20 minutes apart. Consistent enough to look causal from here, which
> is exactly why the peer-side mechanism mattered and why guessing at it (OOM?
> thermal?) would have been wrong. The correction above supplies it.
>
> **Method note worth keeping: a reproducible trigger is not a mechanism.** Two
> matched runs failing at the same call index is strong evidence that the load
> *reaches* a fault, and no evidence at all about what the fault is. The
> originator's view — manifest transport errors, then `gossip: peer marked
> Offline` on a 60 s staleness threshold — is identical whether the far side
> OOM'd, overheated, or hit a Metal bug in a stale build. **Any peer-side failure
> attributed from originator-side signals alone is a could-not-judge (§18.1),
> however clean the reproduction looks.** The re-run should also have been priced
> against this: it cost ~20 minutes and two operator interventions to establish a
> trigger that a glance at the peer's own log resolved immediately.
>
> **Run 2 also cleans up a second result.** Its first four TTFTs are flat —
> 108.2, 103.5, 108.0, 110.9 s, ~7% spread — where run 1's rose. So run 1's rise
> *was* the peer already degrading, and the flat series is the honest baseline:
> ~107.6 s per call, with an identical prefix, improving not at all. That is the
> prefix-cache absence confirmed on the peer path by direct measurement rather
> than by inference from the local probe.
>
> The product consequence is independent of the cause, and it is the finding:
> **a named request has `soft=false`, so when its one holder disappears the
> originator refuses rather than degrading.** For a model the entry node cannot
> serve there is no second holder, no local fallback, and no retry — the
> agentic loop simply stops. That is the M6 §4.5 consumer story failing on a
> two-holder-in-name-only fleet, and it is a better argument for a *second
> holder* than for session affinity.

> **Finding C2 — FIXED 2026-08-06. A streaming refusal emitted a decision with
> no outcome to join to.** The arm now emits `ServedBy::Failed` with the refusal
> text before returning `Err`, pinned by
> `a_streaming_refusal_still_joins_an_outcome_to_its_decision`. Note this was
> never a missing design: `NamedDispatch`'s own doc already stated that "every
> named resolution — including `Unknown` — is a decision that still needs an
> outcome to join back to", and it carries the two ids for exactly that purpose.
> The streaming surface simply never honoured it, which is what makes this the
> third of three.
>
> Original finding: `m6c-peer-05/06/07` each produced a `named_unknown` decision record and
> **no** outcome record. The non-streaming path fixed exactly this — its Unknown
> arm calls `outcome_ctx(...).failed(msg)` under the comment "A refusal is a
> verdict, not a gap in the record" — but the streaming `select_route` Unknown
> arm returns `Err` bare. This is the **third** instance today of one routing
> surface getting a fix the other did not (the forward budget, the privacy gate,
> now the outcome join), which is the §10.6 argument for the two surfaces sharing
> a decider rather than being kept in sync by hand.

---

## 8. Standing measurements, not attached to a milestone

These settle questions this design rests on but does not change.

- **S1 — the link is latency-bound, not bandwidth-bound.** A 16 KiB payload
  will not measurably beat a 64 B payload on round-trip time on this LAN.
  Measured 2026-08-05 and consistent with §5; re-run if the fleet's networking
  changes. A refutation makes faster interconnect worth revisiting and §3.3
  wrong.

  > **Re-run 2026-08-06, n=30 per cell — S1 holds at the floor, but it is
  > answering the wrong question.**
  >
  > | host | 64 B min / avg / mdev | 16 KiB min / avg / mdev |
  > |---|---|---|
  > | BeefyMac | 2.2 / 21.0 / **37.0** | 5.7 / 74.3 / **86.1** |
  > | LittleMac | 3.8 / 20.4 / **25.4** | 6.9 / 46.5 / **51.2** |
  >
  > At the floor a 16 KiB payload costs only ~3 ms more, so §3.3's conclusion
  > (faster interconnect buys nothing) survives. But the mean is 2.3–3.5×
  > the 64 B mean and **mdev is 25–86 ms on a LAN** — the dominant term on
  > this link is neither payload size nor base latency, it is **jitter**.
  >
  > That matters more than S1 itself. §2 prices one tensor boundary at a fixed
  > 21.2 ms *per token*, and §3.1 extrapolates a table from it. A per-token
  > blocking round trip inherits this variance directly, which is the obvious
  > candidate explanation for the 72.9–100.7 ms spread across the six recorded
  > 2-node runs. **A per-boundary constant cannot be quoted without a spread
  > until that correlation is measured** (see M4 Experiment A, restated).
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
