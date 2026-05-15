# Cluster-wide load awareness for mesh inference

## Status

**Diagnosed, not yet implemented.** 2026-05-15. Surfaced during the SEP
pod deployment: a freshly-joined Vast.ai pod (idle) was bypassed by
the founder's load balancer in favor of a LAN peer (BeefyMac) that was
already busy serving local traffic.

This document captures the architectural gap, the math behind the
observed behavior, and the proposed fix so the next session can land
the implementation cleanly.

## The gap

`MeshInferenceProvider.peer_observations[name].in_flight` is updated
only when the founder *itself* dispatches a request to that peer
(`sovereign-mesh/src/peer_inference.rs:520`,
`record_dispatch`/`record_success`/`record_failure`). It reflects the
founder's outbound traffic to the peer — not the peer's *actual*
serving load.

Concretely: BeefyMac may be serving 10 requests from its own local
user (the operator's Claude desktop, local pipelines, anything hitting
`localhost:9741` on BeefyMac). The founder sees `in_flight = 0` for
BeefyMac because none of those 10 originated from the founder. So
`load_penalty(obs)` returns `1.0` regardless of BeefyMac's actual
load, and the load-balance scoring is structurally blind to peer-local
traffic.

## Why it matters: the math

`oicp-types/src/lib.rs:159` scores each candidate as

    score' = score
           × observation_mult     // claim affinity × (1 - failure)
           × load_penalty         // 1 / (1 + 0.05 × in_flight)
           × locality_bonus       // 1.15 Local, 1.05 Near, 1.00 Far
           × cold_start_weight    // 0.7 → 1.0 over 20 samples
           × throughput_factor

For a brand-new Far peer (Taiwan pod, samples=0, idle) vs a warm Near
peer with seemingly-zero load (BeefyMac, but actually busy):

| peer                                    | cold_start | load_penalty | locality | total mult |
|-----------------------------------------|------------|--------------|----------|------------|
| Taiwan pod (cold, idle, Far)            | 0.70       | 1.00         | 1.00     | **0.70**   |
| BeefyMac (warm, 0 observed, Near)       | 1.00       | 1.00         | 1.05     | **1.05**   |
| BeefyMac (warm, *actual* 10 in-flight)  | 1.00       | 0.67         | 1.05     | 0.70       |

The pod can't earn its way past cold-start because it's never given
the chance — the cold-start bootstrap fails when a warm peer wins on
phantom-idle.

## The fix shape (chosen direction)

Gossip the peer's own observed in-flight count via `NodeCapabilities`.
The scoring layer prefers the gossiped value over the founder's
local view. Wire-tolerant: serde default to `None`, older peers behave
as before.

### Touch list

#### 1. Schema
`commonwealth/crates/commonwealth-core/src/capabilities.rs`

Add to `NodeCapabilities`:

```rust
/// The peer's observed concurrent in-flight inference count at the
/// moment this capability snapshot was built. Gossiped so remote
/// schedulers can see the peer's full load — including requests it
/// served from its own local user, which the founder's
/// `peer_observations` is structurally blind to.
///
/// `None` for older peers and tests that don't wire a counter
/// through. Scoring falls back to the founder's local view of the
/// peer in that case (the legacy behavior).
#[serde(default, skip_serializing_if = "Option::is_none")]
pub current_in_flight: Option<u32>,
```

#### 2. Source of truth
`sovereign/crates/sovereign-mesh/src/peer_inference.rs`

Expose `MeshInferenceProvider`'s local in_flight count via a published
counter:

```rust
pub struct MeshInferenceProvider {
    ...
    local_observations: Arc<RwLock<NodeObservations>>,
    /// Mirror of `local_observations.in_flight` published for the
    /// gossip emitter without taking the RwLock on every tick.
    /// Updated alongside the RwLock in `record_dispatch` /
    /// `record_success` / `record_failure`. Lock-free `.load()`
    /// for readers.
    in_flight_publisher: Arc<AtomicU32>,
}
```

Update `record_dispatch` / `record_success` / `record_failure` to also
write `in_flight_publisher.store(new_count, Ordering::Relaxed)`
alongside the existing `local_observations.write().await.in_flight`
mutation.

#### 3. Plumbing into AppState
`sovereign/crates/sovereign-mesh/src/app_state.rs` (or wherever the
inner state lives — actually `AppStateInner`).

Add:

```rust
pub local_in_flight_counter: Arc<AtomicU32>,
```

Initialized as `Arc::new(AtomicU32::new(0))`. The daemon (sovereign-cli
side, where `MeshInferenceProvider::new` is called) constructs the
counter, passes a clone into both AppState and MeshInferenceProvider.
They share the same Arc, so when MIP updates, AppState reads the new
value lock-free.

#### 4. Gossip emitter
`sovereign/crates/sovereign-mesh/src/capabilities.rs::build_local_capabilities`

Add `current_in_flight: Option<u32>` parameter. Populate from
`app_state.inner.local_in_flight_counter.load(Ordering::Relaxed)` in
the gossip.rs caller. Set the new NodeCapabilities field.

For non-mesh callers (storage-only nodes, tests that don't wire a
counter): pass `None` — same behavior as before.

#### 5. Scoring
`sovereign/crates/sovereign-mesh/src/peer_inference.rs::select_peer`

Where the per-peer `obs` is assembled (around line 862):

```rust
let mut obs = peer_obs_snapshot
    .get(&peer.name)
    .cloned()
    .unwrap_or_default();
// Prefer the gossiped value over our local view of this peer.
// The peer's own count includes traffic the founder never sees
// (peer-local user requests). Stay with self-observed when the
// peer hasn't gossiped a value yet (older daemon, fresh join).
if let Some(gossiped) = manifest.current_in_flight {
    obs.in_flight = gossiped;
}
```

Note: `manifest` here is the peer's `NodeCapabilities` (the gossiped
form). Need to verify the manifest fetch path actually delivers
`current_in_flight` — check `get_peer_manifest` in peer_inference.rs.

#### 6. Tests

- **Unit**: in `oicp-types/tests/` or `oicp_select.rs` cfg(test) — show
  that two peers with identical static configs but different
  `current_in_flight` rank differently. Captures the multiplicative
  effect cleanly.
- **Integration**: spin up 3 mock daemons (founder + 2 peers). Send N
  requests directly to peer A so its `in_flight_publisher` climbs.
  Then have founder dispatch via mesh — expect peer B to win the
  routing despite A being more "warm" by sample count. Without this
  test, regressions are easy: the touch points are far apart.

## Out of scope (note for the next iteration)

- **Time-decay on samples.** A warm peer accumulates samples that
  never expire. After a long run, even an idle warm peer beats a fresh
  joiner forever. Consider adding `samples_at` and decaying older
  samples to bring sustained-mesh fairness in line with point-in-time
  load awareness.
- **Bootstrap exploration.** Even with gossip-in-flight, a cold peer
  may never get traffic if all warm peers stay just-busy-enough not to
  trip the load penalty. Add a small ε probability of routing to a
  cold-start peer regardless of score, to give it a chance to warm up.
- **Symmetric peer-to-peer load views.** Right now BeefyMac's view of
  Taiwan pod's load is also blind to founder-driven traffic to Taiwan.
  The gossip fix here is *outbound*-symmetric — every peer publishes
  its own load. So peer-to-peer scoring (not just founder-to-peer)
  benefits too, automatically.

## Observed instance: SEP pod deployment

- 2026-05-15 ~13:30 local
- Founder: toolbx (Strix Halo, 100.115.12.21)
- Peers online: BeefyMac (LAN, 192.168.1.14), Taiwan pod
  (100.102.113.85, joined ~13:20)
- 3 consecutive `commonwealth/primary` calls from the founder all
  routed to BeefyMac. Direct `curl` to the Taiwan pod's
  `/v1/chat/completions` confirms the pod is fully functional.
- Operator (running Claude desktop on BeefyMac) confirms BeefyMac is
  actively serving local-user inference. Founder's
  `peer_observations[BeefyMac].in_flight` does not reflect that load
  — peer remains "phantom idle" to the scheduler.
