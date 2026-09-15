# Lazy inference on the rail

*Work order, 2026-09-13. The cheapest change that puts inference capability on
the ring instead of on a timed HTTP probe. Written to be picked up on another
machine, so every claim carries the file:line it came from — none of it needs
re-deriving.*

The thesis is that distributed inference is an application on the rail rather
than a part of it, by the rail's own test (`docs/internal/RING_APPLICATIONS.md`:
"does it need to know what an act MEANS? If yes it is an application"). This
order proves one slice of that for about thirty lines of code, and deliberately
proves nothing else.

## The seam

`INFERENCE_APP_ID` already exists and is already on the ring.

- `sovereign/crates/sovereign-serving/src/store_adapter.rs:31` —
  `pub const INFERENCE_APP_ID: &str = "inference"`. `InferenceStateStore` is a
  thin `MeshStore` wrapper over `get(APP_ID, key)` / `set(APP_ID, key, bytes,
  node_id)`, with a `node_id_hex` helper already there for per-node keys
  (`:44-46`) and `set_plan`/`get_plan` as the shape to copy (`:63-77`).
- `sovereign/crates/sovereign-mesh/src/ring_roster.rs:257` — the namespace is in
  `DAEMON_OWN_NAMESPACES`, so its roster derives from mesh membership. No
  `roster.json`, nobody runs `svrn ring roster add`.
- `sovereign/crates/sovereign-mesh/src/rail_kv_pump.rs:121` —
  `RAIL_KV_PUMP_INTERVAL` is 2s. The pump drains `MeshStore::outbox_take` and
  signs each queued write onto the namespace's journal; `project_namespace`
  turns admitted ops back into store rows on the receiving side.
- Same file, module doc `:19-35` — daemon-own namespaces seal and compact
  automatically, at a 2000-op threshold, precisely because nobody will run
  `svrn ring seal inference` by hand.

So a `MeshStore::set` under that app id already converges across the ring:
signed, ordered, deduped, self-compacting. The rail needs no work at all.

## What is there today, and why it is the thing to replace

Cross-node routing decides in `sovereign-scheduler/src/scheduler_core.rs:339`
(`rank`), scoring each peer's `ProviderManifest` through
`oicp_types::scoring::best_claim_for_request` (`oicp-types/src/scoring.rs:481`,
per-claim at `:585-609`).

A manifest reaches the ranker by being **pulled**: `get_peer_manifest` in
`sovereign-mesh/src/peer_inference.rs` fetches `GET /oicp/v1/capabilities` from
each peer with `MANIFEST_FETCH_TIMEOUT = 800ms` (`:230`), cached for
`MANIFEST_TTL = 60s` (`:85`). Manifests are built by `synthesize_default_claims`
(`sovereign-api/src/routes_oicp.rs:40-97`) and served at `:234-273`.

That is a timeout-bounded network probe on the routing path, repeated by every
node against every other node once a minute, to learn facts that change only
when a model is loaded or unloaded.

## The change

1. Two methods on `InferenceStateStore`, beside `set_plan`/`get_plan`:
   `set_manifest(&ProviderManifest)` keyed `manifest/{node_id_hex}`, and
   `manifests() -> Vec<(NodeId, ProviderManifest)>`.
2. Call the setter where the manifest is already synthesized for
   `GET /oicp/v1/capabilities`. **On change, not on a timer** — see gotcha 2.
3. In `get_peer_manifest`'s caller, try the converged row first, fall back to
   the existing HTTP pull on a miss.

`rank()` and `best_claim_for_request` do not move. Only where the manifest came
from changes.

## The two tests

**Golden equivalence.** Feed `rank()` the same manifest via the pull and via the
rail row; assert identical ordering. This is ARCH principle 8's own prescription
for two implementations of one decision ("share the body and add a golden
equivalence test") and it is what stops the second source becoming a second
answer.

**The capability the pull cannot have.** Make a peer's `/oicp/v1/capabilities`
unreachable and assert the ranker still names it as a candidate.

Two daemons on one host reproduces both, the way `cw-lift` D2 did
(`quality/campaigns/closed/cw-lift.toml:27-40`). Say so in the commit body: a
single-host reading is not a cross-machine one, and that rung was explicit about
the same limit.

## Gotchas, in the order they will bite

**1. The rail fails open; the pull failed closed.** A peer that is off returns
nothing to a pull. A converged row persists, so it keeps saying "this node holds
a 122B" long after the laptop closed. Liveness must still come from the member
table — keep the existing `is_queryable`-shaped gate (`sovereign-api/src/
routes_knowledge.rs:585-590` is the analogous one on the corpus path) and let
the rail supply only what a node *can* serve. Skip this and the demo routes
every turn to someone asleep.

**2. Write on change only.** A manifest written on a timer is a heartbeat on the
journal, and journal write volume is the one thing that does not scale here —
`WorkProjection`-style folds re-walk history, and the 2000-op seal threshold is
sized for low-write namespaces.

**3. Do not claim a latency win yet.** `research/scale-analysis/
MESH_SCALE_100_USERS_1000_CORPORA.md` says manifest fetches are serial at
P×800ms; `peer_inference.rs:242` has `MANIFEST_FETCH_CONCURRENCY = 8`. One of
those is stale and nobody has checked which. The honest framing needs no number:
discovery stops being a timeout-bounded network call inside TTFT.

**4. Payload cap.** `rail_kv::to_payload` refuses rather than truncates at
`MAX_PAYLOAD_BYTES = 64 KiB` (`commonwealth-rail-core/src/payload.rs:61`). A
manifest for a node with a handful of models is a few KB. Worth one assertion,
not worth a design.

## Explicitly not in scope

The decode turn (`sovereign/deploy/mesh/WORK_PLANE.md:107-123` — "a decode turn
is not a job and must never become one"). `OriginKind` and any new ALPN.
`process:v1`, the sandbox and the container story. The 64 KiB result problem for
batch work. Terminal binding failover. The corpus fan-out's missing relevance
prefilter. Each is real and each is a separate order.

## Naming, to settle before a second one of these lands

There are already two unrelated things called offer — `OriginKind::Offer`
(`oicp-types/src/origin.rs:44-68`, a catalogue of what you have to sell or lend)
and `WorkAct::Offer` (`[compute.work_offer]`, compute donation on the `work`
rail). An inference capability offer would be a third. Pick different words now;
it is free today and expensive later.
