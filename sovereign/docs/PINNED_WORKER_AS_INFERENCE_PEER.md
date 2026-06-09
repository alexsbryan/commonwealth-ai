# Pinned worker pods as inference peers

**Status.** Shipped (2026-05-16). Owner-side wiring landed in
`sovereign-mesh/src/{pinned_transport,pinned_worker_source,pinned_pod_snapshot}.rs`,
pod-side proxy in `sovereign-mesh/src/worker_inference_proxy.rs`, CLI
glue in `sovereign-cli/src/pipeline_cmd.rs` (`pod up` writes snapshot,
`pod down` deletes it) and `sovereign-cli/src/daemon_cmd.rs` (daemon
loads snapshots and builds the composite source at startup). 27 new
unit tests cover transport / source / snapshot / proxy round-trips;
the existing `tests/worker_e2e.rs` still validates TLS pinning
end-to-end. See SYSTEM_OVERVIEW.md §5.12 for the system-level map.

Deferred from v1 (still open):

- Hot-reload of the pinned source — today the daemon picks up new
  pods only on restart.
- HTTP register/unregister endpoint for runtime pod attach.
- Live-Vast E2E in CI; unit + proxy tests cover the plumbing.

The spec below is preserved as the design record.

**Goal.** Let `sovereign pipeline run` route inference work to an
ephemeral worker pod (e.g. a Vast L40S rented via `pod up`) the
same way it routes to a mesh peer today (e.g. mac-peer). No new
shell-out plumbing in the recipe driver, no result-upload protocol
on the pod side — the pod is treated as one more `/v1/chat/completions`
backend, scored and selected by the same load balancer.

**Why now.** With the ephemeral-worker pod stack landed
(`worker_pod`, `worker_http`, `worker_daemon`, `worker_subprocess_runner`,
`worker_controller`, `multi_pod_coordinator`, `pipeline pod up/pool`),
the pod's child daemon already exposes `/v1/chat/completions` on its
`:9741` and proxies for it via the worker daemon's `:9742` TLS-pinned
channel. The remaining gap is owner-side: nothing in
`MeshInferenceProvider` knows to include the pod in its
`select_peer` pool.

## Background — how peer selection works today

The local daemon's `MeshInferenceProvider` (in `peer_inference.rs`)
calls `PeerEndpointSource::peer_inference_endpoints()` to learn which
remote `/v1/chat/completions` URLs it can route to.

```rust
pub trait PeerEndpointSource: Send + Sync {
    async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint>;
    // … local_node_id, ledger_emission_for
}

pub struct PeerInferenceEndpoint {
    pub node_id: NodeId,
    pub name: String,
    pub base_urls: Vec<String>,    // try-order: `http://<ip>:9741/v1`
    pub system_ram_gb: u32,
    pub benchmark: Option<BenchmarkResult>,
    pub current_in_flight: Option<u32>,
}
```

`EmbeddedDaemon`'s impl walks online mesh members from `MeshStore`
and yields one endpoint per non-self peer.

`select_peer(request)` then:

1. Filters out non-routable requests (LocalOnly OICP, Fast/Medium speed).
2. Fetches each peer's `/oicp/v1/manifest` to score capability match.
3. Picks the best peer (capability + throughput + load).
4. Returns the chosen `PeerInferenceEndpoint`; caller does the HTTP.

**Two structural assumptions that break for pinned worker pods:**

- **Plain HTTP on `:9741`.** Pods expose TLS on `:9742` with a
  seed-derived self-signed cert that only the owner's pinned
  `reqwest::Certificate::from_der(...)` client accepts. No plain HTTP.
- **Bearer = mesh's gossiped token.** Pods accept only the
  owner-signed `WorkerToken` minted into the bootstrap blob. The
  mesh's gossip-issued token won't verify.

## The shape of the integration

### 1. Extend `PeerInferenceEndpoint` with an opaque transport handle

```rust
pub struct PeerInferenceEndpoint {
    pub node_id: NodeId,
    pub name: String,
    pub base_urls: Vec<String>,
    pub system_ram_gb: u32,
    pub benchmark: Option<BenchmarkResult>,
    pub current_in_flight: Option<u32>,
    /// NEW: how to actually open a connection to this endpoint.
    /// `None` means the default (plain HTTP to base_urls,
    /// mesh-token bearer). `Some(handle)` means use the pinned
    /// client + worker token recorded in the handle.
    pub transport: Option<PinnedTransport>,
}

#[derive(Clone)]
pub struct PinnedTransport {
    /// Pre-built reqwest::Client whose only trust root is the
    /// pod's seed-derived cert. Cheap to clone.
    pub client: reqwest::Client,
    /// Bearer to set on every outbound call. The owner-signed
    /// WorkerToken from the bootstrap blob — same one the
    /// WorkerHandle holds.
    pub bearer: String,
    /// Display label for tracing; not load-bearing.
    pub label: String,
}
```

`PinnedTransport` lives in `sovereign-mesh` because both
`MeshInferenceProvider` and the worker controller live there. No
new crate; no new public dep.

### 2. New `PeerEndpointSource` impl that yields the pinned pods

```rust
// New in worker_controller or a sibling module.
pub struct PinnedWorkerEndpointSource {
    inner: Arc<RwLock<Vec<PinnedPod>>>,
}

struct PinnedPod {
    handle: WorkerHandle,
    blob: BootstrapBlob,
    client: reqwest::Client,
    /// Stamped capabilities (RAM, benchmark) the operator sets
    /// when registering the pod. Defaults derived from the
    /// Vast offer (gpu_name → RAM/benchmark presets).
    capabilities: PodCapabilities,
}
```

Implements `peer_inference_endpoints()` by mapping each `PinnedPod`
to a `PeerInferenceEndpoint` with:

- `node_id` = a synthetic id derived from the blob seed (so
  ledger / throughput-tracking keys are stable across re-runs)
- `base_urls` = `vec![format!("https://{}:{}/v1", handle.host(), handle.port())]`
  — but the worker daemon's `:9742` doesn't serve `/v1/chat/completions`!
  See §3.
- `transport` = `Some(PinnedTransport { client, bearer, label })`

### 3. Pod-side proxy route — `POST /v1/chat/completions` on `:9742`

Today the worker daemon serves four routes on `:9742`
(upload / dispatch / completed / shutdown / health) + the
`SubprocessRunner` proxies a `{url, body}` payload to the child's
`:9741` per dispatched WorkUnit.

For inference-peer integration, we need a **synchronous** proxy:
`POST :9742/v1/chat/completions` on the owner side, response
streams back over the TLS-pinned connection.

Add to `worker_http::worker_router`:

```rust
.route("/v1/chat/completions", post(chat_completions_proxy))
.route("/v1/models", get(models_proxy))
.route("/v1/embeddings", post(embeddings_proxy))
```

Implementation: read the request body, forward to the child
daemon's local `http://127.0.0.1:9741`, stream the response back.
SSE / chunked transfer is preserved because reqwest's `bytes_stream()`
+ axum's `Body::from_stream` compose cleanly.

The auth middleware that already gates `/internal/worker/*` covers
these too — same owner-signed bearer requirement. No new auth path.

**Pre-flight gate:** these routes 503 until the child daemon is
ready (use the same `child_ready` atomic the `SubprocessRunner`
maintains). Otherwise the owner sees confusing connection-refused
errors during the ~90s model warmup.

### 4. Composite `PeerEndpointSource` for the daemon

```rust
pub struct CompositeEndpointSource {
    mesh: Arc<dyn PeerEndpointSource>,        // existing — EmbeddedDaemon
    pinned: Arc<PinnedWorkerEndpointSource>,  // new
}

#[async_trait]
impl PeerEndpointSource for CompositeEndpointSource {
    async fn peer_inference_endpoints(&self) -> Vec<PeerInferenceEndpoint> {
        let mut out = self.mesh.peer_inference_endpoints().await;
        out.extend(self.pinned.endpoints().await);
        out
    }
    // local_node_id + ledger_emission_for delegate to mesh
}
```

`MeshInferenceProvider::new` accepts an `Arc<dyn PeerEndpointSource>`
already; pass the composite instead of the bare `EmbeddedDaemon`.

### 5. Outbound HTTP wrapper that honours `PinnedTransport`

Currently `MeshInferenceProvider` builds a `reqwest::Client` per
request when calling out to a peer. With the new `transport` field,
the call site becomes:

```rust
let (client, bearer_override) = match &endpoint.transport {
    Some(t) => (t.client.clone(), Some(t.bearer.clone())),
    None => (default_mesh_client(), None),
};
let mut req = client.post(format!("{}/chat/completions", base_url));
if let Some(b) = bearer_override {
    req = req.bearer_auth(b);
}
// existing send / streaming logic unchanged
```

That's the *only* hot-path change. The scoring, manifest fetch,
throughput tracking, fan-out fallback — all unchanged.

### 6. CLI surface — `pipeline run --extra-worker <pod-handle>`

```bash
sovereign pipeline run sovereign-recipes/sep/pipelines/sep-core-v1.toml \
  --concurrency 3 \
  --extra-worker pod://<vast-id>
```

Implementation:

1. The flag takes one or more `pod://<vast-id>` handles.
2. For each, the CLI loads a serialized `PinnedPod` snapshot from
   `~/.sovereign/worker-pods/<vast-id>.json`. The snapshot has
   the `WorkerHandle` (cert is re-derived from the seed in the
   blob), the bootstrap blob (so the cert + bearer are
   reconstructable), and the public address.
3. Snapshots are written by `pod up` — adds a serialization step
   at the end of the successful boot path. Already prints the
   token + thumbprint; saving the same info to disk is a one-line
   addition.
4. `pod down <vast-id>` removes the snapshot. `pod list` reads
   from the same directory.
5. `pipeline run` builds the `PinnedWorkerEndpointSource` from
   the loaded snapshots, hands it to the composite source, runs
   the recipe normally.

### 7. OICP manifest — pinned pods need to advertise capabilities

The peer-selection scoring path fetches each peer's
`/oicp/v1/manifest` to score capability match. Today the worker
daemon's `:9742` doesn't serve OICP. Two choices:

- **A.** Add an OICP manifest route to `:9742`. The manifest is
  static for the pod's lifetime (one model, one GPU); we can
  pre-compute it at boot from the child daemon's view and serve
  it from `:9742` without proxying.

- **B.** Skip OICP scoring for pinned pods — assume they pass
  capability checks, fall back to throughput + load. Faster to
  ship; less accurate scoring.

Recommendation: **A**. The pod knows its own model (from the
disk-dump + child config) and its own GPU class (set when the
Vast offer was picked, persisted in the snapshot). Constructing
a minimal manifest is mechanical.

### 8. Throughput + ledger emission

`MeshInferenceProvider` records per-peer throughput observations
under `peer_observations[peer_name]`. The pinned pod's synthetic
`node_id` + `name` (e.g. `pod-<vast-id>`) just becomes another key
in that map. No new code.

Ledger emission for pinned pods is a question: do we count it as
"contribution received" (yes, the owner paid for compute they got)?
For MVP, default `ledger_emission_for` to `None` — pods don't
participate in the mesh contribution accounting because they're
the owner's own paid compute, not a peer's gifted compute.
Revisit in Phase 2 if commodity-flow accounting needs it.

## Hard parts (the things that will surprise you)

1. **Cert re-derivation must be deterministic across runs.**
   The CLI loads a snapshot, re-derives the cert from the blob's
   seed, builds a `reqwest::Client`. `self_signed_cert(seed)`
   must be byte-identical to what the pod is serving — already
   true (rcgen with a fixed seed produces a fixed cert), but the
   test suite should pin this with a regression test.

2. **`:9742` HTTPS for OICP probes confuses tracing.** The mesh
   load balancer logs all peer-routing decisions; the pod's URL
   in those logs will be `https://...:9742/v1/...` instead of
   `http://...:9741/v1/...`. Operator confusion is the failure
   mode. Mitigation: log entry should include the endpoint's
   `name` (e.g. `pod-3686xxxx`) prominently so the `:9742`
   appears as an attribute of "this is a pinned pod" not "the
   mesh broke".

3. **Multi-pod pool + load-balancer interaction.** If the operator
   registers 4 pods via `--extra-worker pod://A pod://B pod://C
   pod://D`, the load balancer sees them as 4 distinct peers and
   distributes across them. Good. But the **mesh's local-affinity
   bias** (peers preferring their own users) doesn't apply to
   pinned pods — they have no "users" beyond the owner. The
   scoring path should treat the affinity field as 1.0 (neutral)
   when `transport.is_some()`. One-line carve-out in `oicp_select`.

4. **Pod failure mid-run.** If a pod crashes (child daemon SEGV,
   Vast host dies), the load balancer sees connection failures
   and naturally falls back to other peers (mesh's existing
   fan-out retry). But the pod's snapshot file is still on disk
   pointing at a dead instance. Add a stale-pod sweep: when the
   CLI loads snapshots, optionally probe `/health` and skip
   unhealthy ones with a warning.

5. **Stream chunk pacing.** The chat-completions proxy on
   `:9742` is the only synchronous-stream route. Make sure the
   axum response builder uses `Body::from_stream(bytes_stream)`
   rather than buffering — a buffered proxy turns a streaming
   200-token completion into a 6-second wait, which screws up
   any UI driven by SSE.

## Test plan

### Unit (sovereign-mesh)
- `PinnedTransport` clones cheaply (just an `Arc`-shared client)
- Composite endpoint source: mesh + 0 pinned = mesh only;
  mesh + N pinned = mesh + N
- Synthetic `node_id` derivation from blob seed is deterministic

### Integration (sovereign-mesh/tests)
- New `tests/pinned_worker_routing.rs`:
  - Stand up a fake child daemon (axum) on a random port that
    serves `/v1/chat/completions` with a known response.
  - Wrap in the worker daemon's `:9742` listener (real TLS).
  - Build a `MeshInferenceProvider` with the pinned source.
  - Call `chat_completions` end-to-end; assert (a) the request
    reaches the fake child, (b) the response makes it back
    through the TLS pin, (c) throughput stats are recorded
    under the synthetic `pod-*` name.
- Multi-pod variant: 2 pinned pods, fire 4 requests, confirm
  both pods see traffic.

### Local podman
- Extend `tests/local_pod_smoke.rs` with one more `#[ignore]`d
  test that runs the actual sovereign-cli binary in a container,
  fires a real `/v1/chat/completions` against `:9742` (using
  `SOVEREIGN_WORKER_RUNNER=subprocess` so the child daemon
  actually responds), confirms the inference proxy chain works.
  Same trick as the existing smoke tests; new wrinkle is that
  `subprocess` needs a tiny model in the container — use a
  Qwen3.5-2B GGUF or smaller for fast cold-load.

### Real Vast
- `pipeline pod up` to provision a pod, then
  `pipeline run sep-core-v1 --concurrency 3 --extra-worker pod://<id>`.
- Smoke 5 SEP slugs and confirm the load balancer routes some
  to the pod, some to mac-peer, some local.
- Cost ceiling: track `cost_per_hour × wall-clock` per pod; the
  pinned-worker snapshot file can hold the start time.

## What this is NOT

- **Not a result-upload protocol.** SEP's enrich path writes atoms
  to the OWNER's disk. The pod is acting purely as an inference
  backend; the owner-side `sovereign enrich build` writes
  `~/.sovereign/indexes/sep-<slug>/atlas/atoms.json` as it does
  today. The pod never touches that file.

- **Not a corpus-upload protocol.** The pod doesn't need the SEP
  parquet. All it gets is `/v1/chat/completions` requests with
  prompts the owner constructed.

- **Not multi-tenant.** The pod is owned by one CLI session
  (the bootstrap blob's owner signature). Concurrent runs by the
  same owner are fine; another machine using the same pod is
  blocked by the bearer.

- **Not a permanent mesh peer.** The pod never gossips, never
  has its own users, never accepts work outside the owner's
  dispatch.

## Scope estimate

- `PeerInferenceEndpoint::transport` field + wrapper plumbing:
  ~100 lines mesh, ~40 lines tests
- `PinnedWorkerEndpointSource` + composite: ~150 lines mesh
- `:9742` `/v1/chat/completions` proxy + OICP manifest: ~200
  lines `worker_http` + tests
- Snapshot file format + CLI `--extra-worker` flag: ~120 lines CLI
- Real-Vast integration test + runbook update: ~100 lines

Total: ~700 lines + tests. **2-3 days** if the design holds.
**3-5 days** if `:9742` streaming proxy surfaces axum / rustls
quirks (most likely failure mode).

## Open questions

1. Should snapshots survive process restarts, or be ephemeral
   per CLI invocation? **Lean: ephemeral**, written + deleted by
   `pod up` / `pod down`, with a `pod list` for visibility.
   Alternative: a TOML-backed registry like
   `~/.sovereign/pipeline-pods.json` (which already exists for
   the cost ledger).

2. Token rotation. The `WorkerToken` has an `expires_unix` claim;
   a 12h pipeline run will outlive a short-TTL token. Either:
   (a) raise the default TTL when the pod is created via this
   flag, or (b) re-mint and POST the new token to the pod via a
   new `/internal/worker/rotate-token` route. **Lean: (a)** for
   v1; (b) for v2 once we know what TTLs feel right in practice.

3. Failure semantics when a pinned pod errors mid-stream. Today
   peer-routing errors fall back to local on retry. For pinned
   pods, "local" is the owner's machine, which may not have the
   model loaded. Probably fine — the pipeline driver retries
   with backoff per recipe — but worth a regression test.
