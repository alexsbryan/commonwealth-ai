# Ephemeral worker pods — replacing the full-mesh-pod model

## Status

**MVP implemented.** 2026-05-15. Wire-protocol seam and full
single-pod lifecycle are live and unit-tested (28 tests across
`sovereign-mesh::worker_*` + `sovereign-cli::worker_pod_provider`)
with a real-TLS end-to-end integration test
(`sovereign/crates/sovereign-mesh/tests/worker_e2e.rs`) covering:

- Owner ↔ pod handshake over reqwest-pinned TLS against the
  seed-derived self-signed cert.
- Upload SHA validation, manifest dispatch, cursor-based completed
  poll, DELETE shutdown.
- Impostor-owner rejection (wrong signing key can't drive a pinned pod).

Outstanding from the MVP plan:

- Desktop wizard Tauri commands (`worker_pod_create/status/destroy`).
  The Rust controller surface is ready; the Tauri glue is a thin
  follow-up.
- `pipeline pod dispatch <id>` + `pipeline pod poll <id>` commands.
  `pod up` boots the pod into "uploads ready" state; dispatch +
  polling are not yet exposed as CLI subcommands (the controller
  helpers are public — wiring the CLI is mechanical).

Landed since the original MVP plan:

- **Runner Phase 1** — `WorkerState::spawn_disk_dump_watcher` (in
  `worker_http`): on full upload completion, atomically dumps GGUFs
  to `$SOVEREIGN_MODELS_DIR` (default `/workspace/models`) and
  writes a child-daemon `config.toml` one level up. Idempotent,
  validated SHA, safe to call before/after dispatch.
- **Runner Phase 2** — `SubprocessRunner` (in
  `worker_subprocess_runner`): on first dispatch, awaits the
  disk-dump signal, then lazy-spawns `sovereign-cli daemon run
  --config <path>` as a child process. Per-unit dispatch tasks
  proxy `{url, body}` payloads to `http://127.0.0.1:9741<url>`
  (the child's client port — distinct from the worker daemon's
  `:9742`) and feed the JSON response into the
  `/internal/worker/completed` queue. Child uses `kill_on_drop` so
  ephemeral pod destroy reaps it cleanly. Selected by default;
  set `SOVEREIGN_WORKER_RUNNER=echo` to revert to the stub for
  wire-protocol validation.
- **`daemon run --config <path>` flag** — needed by `SubprocessRunner`
  to point the child daemon at the auto-generated pod-side config
  (canonical `~/.sovereign/config.toml` doesn't exist on a fresh
  pod). When set, bypasses the first-boot wizard short-circuit and
  loads via `SetupConfig::load_from(path)`.
- **Fetch-from-URL** — pod can pull large GGUFs directly from R2 /
  B2 / HTTP origins via `UploadEntry.fetch_url` instead of the
  owner's residential upload bandwidth. CLI ergonomics:
  `--upload-from-base-url <base>` rewrites every `--upload`
  filename into `<base>/<name>`.
- **Multi-pod coordinator** — `MultiPodCoordinator::launch(spec, N)`
  partitions a JobSpec's units round-robin across N pods (each
  with a unique seed-derived TLS cert + owner-signed token),
  fans out `create_and_run_with_blob` in parallel, returns a
  `PoolHandle`. `PoolHandle::poll_until_complete` is a fan-in
  drain with per-pod cursors and a stall timeout;
  `destroy_all` is a fan-out teardown. CLI:
  `sovereign pipeline pod pool --pods N --manifest <units.jsonl>
  [--output <results.jsonl>] [--keep-alive]`. Tested with 3 real
  TLS pods running EchoRunner.
- **`MultiOfferVastWorkerProvider`** — companion `WorkerProvider`
  for the pool path: dispenses one pre-picked Vast offer per
  `create()` call (top-N by reliability + price), preserving the
  no-cycle property that keeps `vastai` shell-outs out of
  `sovereign-mesh`.
- **Local-podman smoke harness** —
  `sovereign/container/Containerfile.local-test` +
  `scripts/pod` wrapper + `sovereign-mesh/tests/local_pod_smoke.rs`
  validate the full wire protocol against real containers before
  paying for Vast. Three `#[ignore]` tests: single-pod lifecycle
  (boot → /health → upload → dispatch → poll → destroy), impostor
  rejection at container boundary, three-pod pool drain. Run with:

  ```
  cargo test --package sovereign-mesh --test local_pod_smoke \
    -- --ignored --nocapture
  ```

  Local-test image notes: ABI-aligned with the
  `dev-toolbox` toolbox (Fedora-on-Fedora) so the host-built
  binary's deps resolve; bypasses production `entrypoint.sh`
  (which expects curl + a clock-skewed host); bind-mounts host
  `/lib64` for the binary's dynamic deps. Production CUDA/ROCm
  images build their own binary against the matching GPU stack
  and skip all of this fixup.

Successor to the legacy `pipeline pod up` flow that joined every Vast
pod to the mesh as a full peer. That approach was shoehorned: it
worked because Tailscale gave us symmetric reachability for free, but
it forced a single-owner ephemeral worker through the same
gossip/join/leave machinery a persistent peer uses.

This document defines the architecture for treating Vast pods as
**ephemeral workers** instead of **persistent peers** — a clean
category split that removes Tailscale from the user's setup surface,
deletes a class of networking failure modes, and unlocks a 5-minute
BYO onboarding path for non-technical users.

## Motivation

### Why full-mesh-pods was a category error

A Vast pod, in the workloads we've built, is:

- **Single-owner**: one user rented it, paid for it, will destroy it
  when their job ends. Nobody else on the mesh has any business
  reaching it.
- **Time-bounded**: lifecycle measured in hours, not weeks. No
  long-term presence to gossip.
- **Stateless across runs**: every restart begins from scratch; no
  durable corpus inventory worth advertising.
- **Asymmetric in reachability**: it has a public IP (Vast maps its
  internal port to a host port); the owner's laptop typically does
  not. Mesh-style "everyone reachable from everyone" was always
  paid for by Tailscale and only worked because of it.

Treating that as a "mesh peer" pulled in:

- A Tailscale install inside the container (userspace mode, host shim,
  authkey rotation — every one of these was a networking failure mode
  we hit in the SEP deployment of 2026-05-15).
- A gossip presence (`MemberRecord`, `capabilities`, `current_in_flight`)
  that no other peer needs to consult: the owner already knows the
  pod's state by virtue of owning it.
- A join handshake that issues a `NodeId`, persists `mesh.json`, and
  enrols the pod into the membership lifecycle — for a process that
  lives for two hours.
- Symmetric `peer_inference_endpoints()` exposure: the pod
  accidentally becomes a load-balancer candidate for unrelated mesh
  traffic the owner never authorised. That's not "the elegance of the
  mesh"; it's leakage.

### What we keep

The mesh data shapes — units, handoffs, chunk ranges, atom batches,
Lance fragments — are correct and load-bearing. They survive. Only
the *transport* and *membership* assumptions need to flip.

## The reframe

Two distinct node categories, with separate protocols:

| Trait | Persistent peer | Ephemeral worker |
|---|---|---|
| Examples | desktop, friend's laptop | Vast pod, rented GPU box |
| Lifecycle | months | hours |
| Gossip presence | yes (full `MemberRecord`) | none |
| Reachability | symmetric (Tailscale or future P2P) | one-way: owner→worker over public TLS |
| Mesh membership | first-class | owned by exactly one persistent peer |
| Inference fanout | participates in load balancer | not advertised; owner-only |
| Knowledge fanout | hosts/serves corpora | does not host long-term corpora |
| Identity | durable `NodeId`, persisted to `mesh.json` | session-scoped TLS cert thumbprint |

Vast pods become **owned workers** of the persistent peer that
created them. Other mesh peers never see them.

## Architecture

### Three traffic flows, all owner-initiated

The asymmetric reachability constraint dictates that every meaningful
transfer is initiated by the owner. No reverse tunnels, no STUN/TURN,
no third-party rendezvous.

1. **Bootstrap & job dispatch** (owner → worker)
   Owner pushes a job blob containing the work-queue manifest, model
   references, and corpus partitions.

2. **Model & corpus upload** (owner → worker, with optional
   pod-pulls-from-URL acceleration)
   One-time at job start. The owner has two transport options per
   file in the upload manifest:

   - **Stream from owner's disk** over the same TLS connection.
     Right for files that only exist locally (recipe configs,
     owner-built corpora). Throughput is bottlenecked by the owner's
     residential upload — fine for hundreds of MB, painful for tens
     of GB.
   - **Pod fetches from a presigned URL** (R2 / B2 / S3 / DO Spaces).
     The owner mints a short-TTL presigned URL and bakes it into
     the bootstrap blob's manifest. Pod fetches the bytes itself in
     the background — at data-center egress speeds (multi-Gbps)
     instead of residential upload (10-50 Mbps). SHA validation is
     owner-signed in both cases, so the URL is trusted transport,
     not trusted source.

   The two options can be mixed in a single manifest: e.g. R2 for the
   big GGUFs, direct stream for a recipe config that only lives on
   the owner's laptop.

3. **Result pull** (owner → worker, repeated)
   Worker stages completed units (atoms, fragments, ledger emissions)
   in a local SQLite. Owner polls
   `GET /internal/worker/completed?since=<cursor>` and ingests batches
   into its persistent corpus engine. Cursor-based, idempotent,
   resumable across owner restarts.

### Bootstrap flow

```
Owner (desktop)                          Worker (Vast pod)
─────────────────                       ──────────────────
1. wizard collects: vast_api_key, job   │
   target (corpus, recipe, budget)      │
                                        │
2. mint bootstrap blob:                 │
   - worker TLS keypair seed (32B)      │
   - signed worker token (job id,       │
     owner pubkey thumbprint, expiry)   │
   - expected pod address (filled in    │
     after vast create)                 │
                                        │
3. vastai create instance               │
   --onstart-cmd "SOVEREIGN_BOOTSTRAP=  │
                  <base64-blob> /run"   │
                                        │
4. poll vastai instances show <id>      │
   for public_ipaddr + mapped port      │
                                        │  pod boots:
                                        │  - decodes blob
                                        │  - derives TLS keypair from seed
                                        │  - generates self-signed cert
                                        │  - starts HTTPS on :9742
                                        │  - awaits owner first contact
                                        │
5. HTTPS connect → expected port        │
   - pin pod cert against thumbprint    │
     derived from seed (no TOFU)        │
                                        │
6. POST /internal/worker/upload         │  pod streams to /models/, /corpus/
   (multipart: GGUFs + corpus shards)   │  validates SHA, signals ready
                                        │
7. POST /internal/worker/job            │  pod stages work queue, starts
   (manifest of N units to process)     │  enrichment loop locally
                                        │
8. GET /internal/worker/completed       │  returns batch since cursor,
   ?since=<cursor>  (poll, ~30s)        │  advances internal pointer
                                        │
9. owner ingests batch into local       │
   corpus engine; advances cursor       │
                                        │
10. when all units done:                │
    DELETE /internal/worker/job →       │  pod gracefully shuts down
    vastai destroy <id>                 │
```

### Why TLS-pinned-from-seed (no TOFU)

A deterministic keypair-from-seed scheme (Ed25519 seed → Curve25519
keys via standard derivation, or just an X25519 seed directly) means
the owner knows the pod's expected public key thumbprint *before*
the pod boots. First connection rejects any cert that doesn't match.
This closes the man-in-the-middle window that pure trust-on-first-use
would leave open — relevant for legal-case workloads where a
malicious Vast host has financial incentive to intercept.

The seed is generated by the owner, embedded in the bootstrap blob,
and shipped via Vast's `onstart` env. Vast can read it (Vast can
read anything we put in the container), so a malicious Vast host
*could* impersonate the pod — that's not changing. The pin closes
the secondary "untrusted network operator" threat, not the
"untrusted Vast host" threat. Lawyers concerned about the latter
should encrypt-at-rest before upload; out of scope for this spec.

## Touch list

### New: owner-side controller
**`sovereign-mesh/src/worker_pod.rs`** (new module). Owns:

- `WorkerHandle` — typed wrapper around a pod's public address +
  pinned cert + job state.
- `WorkerController::create(vast_offer, job_spec) -> WorkerHandle` —
  mints bootstrap, calls `vastai create`, polls for address.
- `WorkerController::upload(handle, models, corpus_shards)` — pushes
  data over HTTPS.
- `WorkerController::dispatch_job(handle, manifest)` — assigns work.
- `WorkerController::poll_completed(handle, cursor) -> Batch` — pulls
  results, advances cursor.
- `WorkerController::destroy(handle)` — graceful shutdown + vastai
  destroy.

### New: worker-side endpoints
**`sovereign-mesh/src/worker_http.rs`** (new module). Pod-side
implementation of:

- `POST /internal/worker/upload` — accepts streamed model/corpus
  uploads, validates against bootstrap-blob's SHA manifest.
- `POST /internal/worker/job` — accepts the work-queue manifest,
  stages units locally, starts the enrichment loop.
- `GET /internal/worker/completed?since=<cursor>` — returns the next
  batch of completed units; cursor-based for idempotence.
- `DELETE /internal/worker/job` — cancellation + graceful shutdown.

These endpoints are exposed on the same `:9742` port (or a separate
worker port — see open question below) and require a valid signed
worker token in the `Authorization` header. The token's
owner-pubkey-thumbprint claim is checked against the pod's own
bootstrap pin; only the owner can dispatch.

### Modified: pod entrypoint
**`sovereign/container/entrypoint.sh`** — replaces today's Tailscale
+ rclone + mesh-join sequence with:

1. Decode `SOVEREIGN_BOOTSTRAP` from env.
2. Derive TLS keypair, write to `/tls/`.
3. Start daemon with `--worker-mode --bootstrap-blob $BOOTSTRAP`.
4. Daemon advertises only the worker endpoints; no gossip, no mesh
   join, no `/v1/chat/completions` exposure outside the owner's
   TLS pin.

The clock-sync HTTP-Date fallback stays (still useful for any
non-TLS time-sensitive operations the daemon does internally).
Everything else dies: tailscaled, rclone, R2 env vars, mesh join
token plumbing.

### Modified: pipeline_cmd.rs
**`sovereign/crates/sovereign-cli/src/pipeline_cmd.rs::pod_up`** —
becomes a thin wrapper over `WorkerController::create` for backward
compatibility with the existing `sovereign pipeline pod up` CLI.
Operators using `pipeline pod up` keep their workflow; under the
hood it's the new transport.

### Modified: desktop wizard
**`sovereign-desktop/src-tauri/src/`** — new Tauri commands:

- `worker_pod_create(job_spec)` — runs the BYO Vast wizard's tail
  end: search → create → bootstrap → upload → dispatch. Returns a
  `WorkerHandle` token the frontend uses to poll status.
- `worker_pod_status(handle_token)` — returns `{ units_done,
  units_total, eta_secs, estimated_cost_usd }`.
- `worker_pod_destroy(handle_token)` — owner-initiated shutdown.

Wizard's secret-collection surface drops from five fields to two:
**Vast API key** and **Stripe billing** (the latter handled by Vast
itself; we deep-link to their billing page). No Tailscale, no R2,
no mesh join key — all replaced by the bootstrap blob the desktop
mints in-process.

### Deleted (when full-mesh-pods go away)
- Tailscale install steps in `Containerfile.cuda`.
- `TAILSCALE_AUTHKEY` / `TS_AUTHKEY` env validation in
  `pipeline_cmd.rs`.
- `R2_*` env vars + rclone in `entrypoint.sh`.
- `MESH_JOIN_LINK` env var (replaced by signed worker token).
- The post-pod-join slot-alias push and load-balancer-candidate
  registration: pods aren't load-balancer candidates.

## Compatibility

This is additive: persistent peers keep their mesh (gossip,
symmetric Tailscale-or-equivalent transport, load-aware peer
inference). Existing `sovereign pipeline pod up` invocations route
to the new controller; old containers still on the join-the-mesh
pattern keep working until they're rebuilt. No wire-protocol
breakage for the persistent-peer mesh.

The work-queue protocol stays — both the data shapes and the
coordinator-state. The pull-direction inversion only applies to
ephemeral-worker pods; persistent peers participating in
distributed ingest continue to use the push-back path
(`POST /internal/handoff/...`) over their symmetric mesh transport.

## Multi-pod jobs (planned fast-follow)

Sharding one large job across N pods is **architecturally free**
once the single-pod primitives land. The controller is the only
piece that grows:

```rust
// Single-pod (MVP)
WorkerController::create(offer, job_spec) -> WorkerHandle

// Multi-pod (fast-follow)
WorkerController::create_pool(n, offers, job_spec) -> Vec<WorkerHandle>
WorkerController::poll_pool(&[WorkerHandle]) -> MergedBatch
```

Implementation shape:
- Owner splits the job's unit manifest into N partitions (chunk-id
  ranges, like today's `assign_knowledge_shards`). Each partition
  ships to one pod via the same `POST /internal/worker/job`.
- Result-pull poller becomes a fan-in: tokio `select!` across N
  pods' `GET /completed?since=cursor_i`, each pod's cursor
  independent.
- Lance fragment merge on the owner side is unchanged from
  today's distributed-ingest path; the corpus engine already
  knows how to ingest fragments from multiple sources into one
  index (see the wikipedia-collaborative-ingest flow that
  produced our current 49,783-title snapshot).
- Per-pod TLS pins live in `Vec<WorkerHandle>`; no shared trust
  fate. One pod compromised → blast radius is its partition,
  not the job.
- Cost ceiling enforcement composes: the controller tallies
  `pod.cost_per_hour × N × elapsed`, can destroy individual
  pods if their progress lags badly enough that finishing them
  costs more than re-running on a faster offer.

What makes this a "fast-follow" not a "rewrite": every primitive
in the MVP is pod-local. Adding more pods only adds a fan-out at
the controller level. The bootstrap blob format, TLS pinning,
upload/dispatch/pull endpoints, entrypoint script — all
unchanged. Probably 2-3 extra days on top of the MVP week.

The one piece worth getting right in the MVP so the fast-follow
doesn't backtrack: **make `WorkerHandle` cheaply cloneable and
pollable independently**. A single `Vec<WorkerHandle>` plus a
fan-in poll loop is the entire delta. No shared state, no
cross-pod coordination, no consensus.

## Provider connectivity audit (derisks the public-egress assumption)

Verified 2026-05-15. The bootstrap protocol depends on the owner
being able to open a TLS connection to the pod's externally-routable
address. This must work on the vast majority of offers, not just a
niche, or the design doesn't scale.

**Vast.ai** — direct port mapping. Empirical check:
- `vastai search offers 'rentable=True'` (broad sweep, 500 offers):
  **500/500 (100%) have `direct_port_count > 0`** and a populated
  `public_ipaddr` field on the offer record itself. Median
  `direct_port_count` was 12 ports per host — well above the 1-2
  ports we need.
- Filtered to single-GPU + ≥24 GB VRAM, reliability >0.95: same
  100% pass rate across 52 offers.
- 52% of single-GPU offers also have `static_ip=True` (IP
  unchanged across sessions; nice-to-have for resumability, not
  required for the MVP since we re-resolve on each create).

Conclusion: **public TLS reachability on Vast is the baseline,
not a niche feature.** Filter at search time (`direct_port_count>=2`
to be safe) and the design works on the entire rentable pool.

**RunPod** — proxy-based public TCP. From our existing
`docs/CLOUD_PEER_DEPLOY.md` and `runpodctl` semantics:
- Every pod template accepts `--ports "9741/tcp,9742/tcp"` (or
  similar). RunPod assigns a public hostname per exposed TCP
  port: `<pod_id>-<port>.proxy.runpod.net`. Pure TCP pass-through,
  not HTTP-layer-terminated — TLS pinning works end-to-end.
- Standard on every pod, both Community Cloud and Secure Cloud
  tiers. Not a paid upgrade, not a niche flag.

Conclusion: the design is **transport-agnostic between Vast direct
IP and RunPod proxy hostname** — owner-side connection logic
treats both as `(host, port, expected_tls_thumbprint)` triples.
The `WorkerController` picks up either provider by accepting a
small `provider` enum that maps to the right address-discovery
poll (Vast: `instances show`, RunPod: `runpodctl get pod`).

This also derisks lock-in: if Vast's marketplace dynamics change
(prices spike, region exits), the same protocol runs on RunPod
or any future provider with public TCP port exposure. No protocol
changes, just a new `Provider::Adapter` impl.

## Out of scope (explicitly deferred)

- **Pod sharing across mesh peers.** Owner-only is the starting
  point. A future "share my pod with mesh peers" flag opens the
  worker's `/v1/chat/completions` to mesh-pinned identities — but
  that re-introduces the load-balancer-candidate question and a
  multi-tenant authorization surface. Not now.
- **Hot owner handoff.** If the owner's laptop dies mid-job, the
  pod keeps processing and staging results; on owner reboot it
  resumes polling from its persisted cursor. Cross-owner handoff
  (friend takes over polling because original owner is offline) is
  a future spec.
- **Cost ceiling enforcement.** The Vast pod ledger
  (`~/.sovereign/pipeline-pods.json`) tracks spend but the
  controller doesn't auto-destroy on budget exceed. Adding a
  budget tripwire is straightforward but not in the MVP.
- **Encrypt-at-rest for the upload payload.** Lawyers concerned
  about Vast-host snooping should pre-encrypt corpus shards;
  baking this into the upload path is a follow-up that needs UX
  thought (key management).

## Open questions

- **Worker port choice.** Today's daemon serves on `9741` (client)
  and `9742` (internal). Should the worker endpoints share `9742`
  (cleaner) or use a third port (clearer attack surface)? Lean
  toward shared `9742` with strict bootstrap-token gating.
- **Bootstrap blob size limit.** Vast's `onstart_cmd` has a
  practical length cap. Blob is ~200 bytes encoded — well under
  any plausible cap, but worth verifying with a large-corpus job
  spec embedded.
- **Vast metadata polling cadence.** How fast does
  `vastai instances show <id>` return a public_ipaddr after
  `create`? Empirically ~10-60s in our SEP runs. The wizard
  should show progress, not block silently.

## Net assessment

Effort: **~1 week MVP + 2-3 days for the multi-pod fast-follow**.

MVP (single pod):
- `worker_pod.rs` controller: 1.5 days.
- `worker_http.rs` endpoints: 1.5 days.
- Entrypoint rewrite + container rebuild: 0.5 days.
- Desktop wizard + Tauri commands: 1.5 days.
- Integration test (full bootstrap → upload → job → poll → destroy
  against a real Vast pod): 1 day.
- Documentation + runbook update: 0.5 days.

Fast-follow (N pods):
- `WorkerController::create_pool` + fan-in poll loop: 1 day.
- Manifest partitioning helper (reuses existing
  `assign_knowledge_shards`): 0.5 days.
- Per-pod cost ceiling enforcement + lagging-pod replacement:
  1 day.
- Multi-pod integration test (2-3 pods, manifest fan-out, merge):
  0.5 days.

Designing the MVP with a `Vec<WorkerHandle>`-friendly shape from
day one (cheap clone, independent polling, no cross-pod state)
keeps the fast-follow from becoming a rewrite. That's a free
choice with no MVP-time cost.

Wins:
- The five networking failure modes hit in the SEP deployment all
  disappear. No Tailscale install in container, no userspace mode,
  no host shim, no authkey rotation, no 100.x-address discovery.
- BYO secret surface drops to 2 fields (Vast API key + Stripe).
- Pods can no longer leak into the mesh as accidental
  load-balancer candidates.
- The same pull-protocol primitives degrade gracefully to the
  "friend behind CGNAT" case for persistent peers, paying down a
  separate piece of future-debt.

Risks:
- Vast public-port mapping availability: filter at search time.
- TLS pin recovery if seed is lost: same as today's
  "lost-mesh-join-key" recovery — destroy and re-create the pod.
- Migration period where some operators are still using
  `pipeline pod up` against old containers: both paths coexist
  until containers are rebuilt; no flag day.

Loss vs. today's mesh-pod approach:
- A pod can't take inference work from arbitrary mesh peers under
  load. **This is a feature, not a regression** — that behaviour
  was unauthorised in the first place.
- A pod's gossip presence is gone. Nothing currently consumes it
  except the load balancer (which shouldn't have).
