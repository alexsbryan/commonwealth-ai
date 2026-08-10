# Cloud peer deployment

> **Superseded.** This documents the original "pod joins your mesh as a peer" design (Tailscale + R2 + mesh-join). As of 2026-05-15 the container boots the ephemeral-worker model instead — a pod owned by one peer for one job, driven over a pinned transport, that never gossip-joins the mesh ([EPHEMERAL_WORKER_PODS.md](EPHEMERAL_WORKER_PODS.md)). The flow below won't boot as written against the current image (its `entrypoint.sh` now expects a `SOVEREIGN_BOOTSTRAP` blob), so treat it as the architecture-and-cost reference until the ephemeral-worker CLI lands and this is rewritten.

Spin up a transient cloud GPU as a sovereign-mesh worker. The remote
pod joins your tailnet, advertises its slots over OICP, and your
laptop's existing `sovereign enrich ...` flow routes Phase 1 chat
calls to it automatically. When you're done, you stop the pod.
There's no synced state — the local resolve step writes atlases to
`~/.svrnmesh/indexes/` as usual.

This is the cloud analog to [`TOOLBOX_SETUP.md`](TOOLBOX_SETUP.md):
TOOLBOX_SETUP describes running the daemon on a local Strix Halo
toolbox; this doc describes running it on a rented MI300X / H100 /
A100 / L40S box for the duration of an ad-hoc batch.

## When to use this

A cloud peer is the right fit when:

- A single ingest run would tie up your laptop GPU for >12 h.
- You want to run several ingests concurrently and your laptop
  already has the workload it can sustain (its primary slot).
- You want one-shot access to a class of GPU you don't own (H100,
  MI300X) without committing to a monthly bill.

It is not the right fit for:

- Steady-state daily ingests — the per-hour rate dominates.
- Sub-30-minute jobs — the cold-start cost (image pull + GGUF sync +
  slot load, ~5 min) eats into the speedup.

Per ad-hoc ingest of the Tier-2 SEP set (~150 articles) the cost
shape is in the [Cost](#cost) section below.

## Architecture

```
  laptop (mesh founder)              cloud pod (mesh peer, transient)
 ┌─────────────────────────┐        ┌──────────────────────────────────┐
 │ sovereign-cli daemon    │        │ entrypoint.sh                    │
 │  ├── primary slot       │        │  ├── 1. tailscale up             │
 │  ├── fast slot          │        │  ├── 2. rclone sync r2:models/   │
 │  ├── embed slot         │        │  ├── 3. write config.toml        │
 │  └── OICP server :9741  │◄──────►│  └── 4. exec sovereign-cli       │
 │                         │ tailnet│        daemon run                │
 │ enrich extract sep-X    │  9742  │   ├── primary slot(s)            │
 │  → routes Phase 1 to    │        │   └── OICP server :9741          │
 │    whichever peer has   │        │                                  │
 │    capacity             │        │   advertised via mesh gossip     │
 └─────────────────────────┘        └──────────────────────────────────┘
                                       │
                                       └── GPU (MI300X / H100 / etc.)

   model storage:                      ┌──────────────────────────────────┐
                                       │ Cloudflare R2 (S3-compatible)    │
                                       │   FINAL-Bench_Darwin-36B-Opus-Q6 │
                                       │   Darwin-9B-Opus.Q8_0.gguf       │
                                       │   Qwen3-Embedding-0.6B-Q8_0.gguf │
                                       └──────────────────────────────────┘
```

The pod has no persistent volume. Every cold start re-pulls GGUFs
from R2; every teardown drops the pod's local copy. R2's free egress
makes this practical (~$0 to re-pull 50 GB).

The mesh treats the cloud pod as just another peer. Routing,
capability advertisement, and request dispatch are unchanged from a
two-laptop mesh — the only thing different is that the second peer
happens to live in someone else's data center for an hour.

## Image flavors

Two Containerfiles, picked by which AMD/NVIDIA hardware your provider
has capacity for that day:

| flavor | dockerfile | base image | target hardware |
|---|---|---|---|
| ROCm | `sovereign/container/Containerfile`        | `rocm/dev-ubuntu-22.04:7.2`              | AMD MI300X (gfx942), MI250X (gfx90a), MI100 (gfx908) |
| CUDA | `sovereign/container/Containerfile.cuda`   | `nvidia/cuda:12.1.1-devel-ubuntu22.04`   | NVIDIA H100/H200 (sm_90), A100 (sm_80), L40S/RTX 4090 (sm_89), RTX 30xx (sm_86) |

Both produce the same operator-facing surface — same env vars, same
entrypoint, same mesh shape. The only difference is the inference
backend baked in.

For Containerfile-internal details (which apt packages each stage
needs and why), see [`sovereign/container/README.md`](../container/README.md).

## Provisioning checklist (one-time)

The whole rclone-config + GGUF-upload + post-upload sanity-check flow
is wrapped in
[`scripts/cloud-peer-provision.sh`](../scripts/cloud-peer-provision.sh).
After you've created the R2 bucket + token in the Cloudflare
dashboard, you can run:

```bash
export R2_ENDPOINT=https://<account>.r2.cloudflarestorage.com
export R2_ACCESS_KEY=...
export R2_SECRET_KEY=...
./scripts/cloud-peer-provision.sh
```

The script is idempotent (re-runs skip already-uploaded GGUFs) and
prints the env-var block ready to paste into RunPod at the end.
If you'd rather do it by hand, the manual steps are:

### 1. Stage GGUFs in R2

Cloudflare R2 is the recommended object store: free egress means
re-pulling the 50 GB GGUF set on every pod cold-start costs nothing.
Any S3-compatible store works — AWS S3, Backblaze B2, MinIO, etc.

```bash
# Configure rclone with an R2 remote (interactive, one-time)
rclone config   # choose "s3" → provider "Other"; paste R2 endpoint + key

# Upload (~50 GB; takes 5-15 min depending on uplink)
rclone copy ~/dev/commonwealth-ai/sovereign/models/FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf r2:sovereign-models/
rclone copy ~/dev/commonwealth-ai/sovereign/models/Darwin-9B-Opus.Q8_0.gguf r2:sovereign-models/
rclone copy ~/dev/commonwealth-ai/models/qwen-embedding-0.6b.gguf/Qwen3-Embedding-0.6B-Q8_0.gguf r2:sovereign-models/

# Verify
rclone size r2:sovereign-models   # ≈ 50 GB
```

Capture the R2 endpoint URL + an Object-API access key + secret. Save
them — they go into RunPod env vars.

### 2. Tailscale auth key

[https://login.tailscale.com/admin/settings/keys](https://login.tailscale.com/admin/settings/keys)
→ "Generate auth key":

- **Reusable**: yes (you'll spin up many transient pods).
- **Ephemeral**: yes (auto-cleans the device list when a pod tears
  down — otherwise dead nodes accumulate).
- **Tags**: `tag:sovereign-worker` (optional; useful for ACLs that
  let cloud peers reach `:9742` on the founder but nothing else).

Save the `tskey-...` string.

### 3. Capture your laptop's tailnet IP

```bash
tailscale ip -4
# e.g. 100.64.0.2
```

This becomes `MESH_SEED_ADDR` for the pod (with `:9742` appended).
Cloud pods bootstrap their mesh by gossipping to this address.

### 4. Container registry

GitHub Container Registry (`ghcr.io`) is free for public images and
fine for private ones with a PAT. Docker Hub also works, as does
RunPod's own registry. Examples below assume `ghcr.io/<your-user>/`.

## Build + push

ROCm:
```bash
cd ~/dev/commonwealth-ai
podman build -t ghcr.io/<you>/sovereign-rocm:latest \
             -f sovereign/container/Containerfile .
```

CUDA:
```bash
cd ~/dev/commonwealth-ai
podman build -t ghcr.io/<you>/sovereign-cuda:latest \
             -f sovereign/container/Containerfile.cuda .

# (optional) slim build for one GPU class — smaller image, faster cold-start:
podman build --build-arg CUDA_ARCHITECTURES=90 \
             -t ghcr.io/<you>/sovereign-cuda-h100:latest \
             -f sovereign/container/Containerfile.cuda .
```

Common arch aliases:

| `--build-arg CUDA_ARCHITECTURES=` | targets |
|---|---|
| `90` | H100, H200 |
| `89` | L40S, RTX 4090 |
| `80` | A100 |
| `86` | RTX 30xx |
| `80;86;89;90` *(default)* | everything Ampere → Hopper |

Authenticate + push:
```bash
echo "$GHCR_PAT" | podman login ghcr.io -u <you> --password-stdin
podman push ghcr.io/<you>/sovereign-rocm:latest    # or sovereign-cuda
```

First build per flavor is ~30-45 min (compiles llama.cpp +
sovereign-cli inside the image). Layer-cached rebuilds where only
sovereign-cli source changed are 1-3 min.

`docker build -f Containerfile[.cuda]` works the same — both engines
emit OCI images RunPod accepts.

## Deploy on RunPod

Pod template:

| field | value |
|---|---|
| **Container Image** | `ghcr.io/<you>/sovereign-rocm:latest` *or* `:sovereign-cuda:latest` |
| **Container Registry Auth** | ghcr token if private |
| **GPU** (Secure Cloud) | pick whichever is in stock today: |
|   | ROCm image → `MI300X` (192 GB), `MI250X` (128 GB) |
|   | CUDA image → `H100 PCIe 80GB`, `H100 SXM 80GB`, `A100 80GB`, `L40S 48GB` |
| **GPU Count** | 1 |
| **Container Disk** | 60 GB (GGUFs are ~38 GB; smaller disks ENOSPC mid rclone-sync) |
| **Volume Disk** | 0 (no persistent volume; we use S3-on-start) |
| **Expose HTTP Ports** | `9741` (only if you want to bypass Tailscale; usually unnecessary) |
| **Expose TCP Ports** | none |

Environment variables (RunPod "Secrets"):

| key | example | required |
|---|---|---|
| `TS_AUTHKEY`     | `tskey-auth-...`                              | yes |
| `MESH_SEED_ADDR` | `100.64.0.2:9742`                          | yes |
| `R2_ENDPOINT`    | `https://<account>.r2.cloudflarestorage.com`  | yes |
| `R2_ACCESS_KEY`  | r2 access key id                              | yes |
| `R2_SECRET_KEY`  | r2 secret access key                          | yes |
| `MESH_JOIN_LINK` | `sovereign://join/cwth-...`                   | optional — see [Mesh routing](#mesh-routing) |
| `R2_BUCKET`      | `sovereign-models`                            | no — defaults |
| `PRIMARY_COPIES` | `1` (single primary), `6` on MI300X 192GB     | no — defaults to 1 |
| `CONTEXT_SIZE`   | `32768`                                       | no — defaults |
| `PRIMARY_GGUF`   | `FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf`       | no — defaults |
| `FAST_GGUF`      | `Darwin-9B-Opus.Q8_0.gguf`                    | no — defaults |
| `EMBED_GGUF`     | `Qwen3-Embedding-0.6B-Q8_0.gguf`              | no — defaults |
| `NODE_ROLE`      | `ephemeral-worker`                            | no — metadata only |

Start the pod via the [`scripts/cloud-peer-deploy.sh`](../scripts/cloud-peer-deploy.sh)
wrapper (recommended), the RunPod web UI, or a hand-rolled
`runpodctl` invocation.

The wrapper reads creds from your shell env, picks sane defaults
(L40S 48GB, ~$0.79/hr for first smoke), and prints the cold-start
timeline + tear-down command:

```bash
# Required env: TS_AUTHKEY, MESH_SEED_ADDR, R2_*
./scripts/cloud-peer-deploy.sh up
# → prints pod ID + next-step commands

# From there:
./scripts/cloud-peer-deploy.sh ls              # list pods
./scripts/cloud-peer-deploy.sh get <pod-id>    # one pod's details
./scripts/cloud-peer-deploy.sh down <pod-id>   # stop billing + remove

# (No 'logs' subcommand: runpodctl doesn't ship one. Watch container
# output in the RunPod web UI's Logs tab or via Connect → Web Terminal.)
```

Override the GPU class or flavor by exporting before `up`:

```bash
FLAVOR=rocm GPU_TYPE='AMD Instinct MI300X' PRIMARY_COPIES=4 \
    ./scripts/cloud-peer-deploy.sh up
```

Or hand-rolled:

```bash
runpodctl create pods \
  --name sovereign-cuda-l40s \
  --imageName ghcr.io/<you>/sovereign-cuda:latest \
  --gpuType "NVIDIA L40S" \
  --gpuCount 1 \
  --containerDiskInGb 25 \
  --volumeInGb 0 \
  --ports "9741/tcp,9742/tcp" \
  --env "TS_AUTHKEY=tskey-..." \
  --env "MESH_SEED_ADDR=100.x.y.z:9742" \
  --env "R2_ENDPOINT=..." --env "R2_ACCESS_KEY=..." --env "R2_SECRET_KEY=..." \
  --env "R2_BUCKET=sovereign-models" \
  --env "PRIMARY_COPIES=1" \
  --env "CONTEXT_SIZE=32768"
```

Cold-start timeline:

| t       | event |
|---------|---|
| 0-30s   | pod scheduling, image pull |
| 30s-2m  | tailscale up + rclone sync from R2 (~50 GB at ~500 MB/s) |
| 2m-4m   | slot loads (`PRIMARY_COPIES * ~28 GB` weights → GPU) |
| 4m-5m   | daemon advertising via mesh gossip; ready |

## Mesh routing

Without `MESH_JOIN_LINK`, the pod boots into a **solo mesh**. It's reachable
over the tailnet for direct OICP calls, but the founder's scheduler doesn't
discover its slots via gossip — so multi-peer load-balancing requires
pinning each per-article `~/.svrnmesh/enrichment/<corpus>/config.json` to
the pod's URL via `base_url`.

With `MESH_JOIN_LINK`, the pod's `entrypoint.sh` runs `sovereign mesh join
<link>` after the daemon comes up. The pod becomes a real mesh peer; the
founder's scheduler discovers it via gossip and routes Phase 1 calls
according to slot availability across all peers (laptop included). This is
the right shape when you want the laptop to also pitch in on workload.

Generating the link on the founder (run once, or after `mesh rotate`):

```bash
# If you've never created a joinable mesh:
sovereign mesh create
# → prints: sovereign://join/cwth-a1b2-c3d4-e5f6 (paste into MESH_JOIN_LINK)

# If a joinable mesh already exists and you need a fresh key:
sovereign mesh rotate
# → invalidates the old key for *future* joins; existing members stay connected.
# Restart the daemon afterward so it picks up the new key.
```

Pass the link through your shell env when you `up` the pod:

```bash
export MESH_JOIN_LINK='sovereign://join/cwth-a1b2-c3d4-e5f6'
./scripts/cloud-peer-deploy.sh up
```

The same link can be reused across multiple concurrent pods — every peer
joins the same mesh.

## Verify mesh connectivity

From your laptop:

```bash
# 1. Tailscale should show the worker
tailscale status | grep sovereign

# 2. The mesh advertises the new peer's slots; check via OICP /v1/models
curl -s http://localhost:9741/v1/models | jq '.data[].id'
# Expect: primary, fast, embed appearing alongside locally-loaded slots.
# With PRIMARY_COPIES > 1: primary_0, primary_1, ... primary_N appear.
```

If `tailscale status` shows the worker but `/v1/models` doesn't list
the cloud slots, the mesh gossip isn't reaching the founder — check
`MESH_SEED_ADDR` against your current `tailscale ip -4`.

## Run the workload

From your laptop, no config change needed — the existing extract path
discovers the new mesh peer automatically and routes Phase 1 over OICP:

```bash
# Single article (smoke test)
sovereign enrich extract sep-hegel --full
sovereign enrich resolve sep-hegel --phase all

# Parallel batch — fan out as wide as your peer's primary_copies allows
cat /tmp/sep_tier2_remaining.txt | xargs -P 6 -I {} sh -c '
  sovereign enrich extract sep-{} --full && \
  sovereign enrich resolve sep-{} --phase all
'
```

The Phase 1 chat calls go to the cloud peer; the resolve step
(deterministic, CPU-bound) runs locally. Atlases land in
`~/.svrnmesh/indexes/sep-{slug}/atlas/` — no sync-back required.

## Tear down

```bash
runpodctl stop ${POD_ID}     # stops billing
runpodctl rm   ${POD_ID}     # removes pod
# Tailscale auto-cleans the ephemeral peer.
# Mesh detects offline within the gossip interval (~30s) and falls
# back to local slots automatically.
```

If you forget to teardown, the pod keeps billing. Set a reminder, or
tear down via RunPod's "auto-stop after N hours" template option.

## Cost

| component | cost |
|---|---|
| R2 storage         | ~$0.75/month for 50 GB GGUFs (free egress) |
| RunPod MI300X      | ~$2.49/hr while running (secure cloud) |
| RunPod H100 PCIe   | ~$2.39/hr |
| RunPod A100 80GB   | ~$1.49/hr |
| RunPod L40S 48GB   | ~$0.79/hr |
| Tailscale          | free (personal tier) |

Per ad-hoc ingest of ~150 SEP articles at the laptop's enrich rate of
~10 min/article, a single MI300X cuts wall-time roughly proportional
to the GPU's bandwidth advantage. Budget $5-15 per ingest run.
Standby month with no runs: ~$1 (R2 storage only).

## Troubleshooting

| symptom | cause | fix |
|---|---|---|
| Pod boots but `/v1/models` doesn't show remote slots | Tailscale didn't connect, or seed addr wrong | Check pod logs for `tailnet IP:` line; verify `MESH_SEED_ADDR` matches `tailscale ip -4` on laptop |
| `rclone sync` 403 | R2 key permissions | Ensure key has `Object Read & Write` on the bucket |
| Slot loads but daemon hangs | Out of VRAM | Lower `PRIMARY_COPIES` (1 ≈ 28 GB; 6 ≈ 168 GB) |
| Local extract still hits local Darwin | Mesh scheduler not preferring remote | Check `oicp/v1/capabilities` from laptop side; remote peer's capacity should be visible |
| `cargo build` fails on `ncclXxx` undefined symbols (CUDA) | NCCL not installed | Rebuild with the latest `Containerfile.cuda` (it adds `libnccl-dev` + `RUSTFLAGS=-C link-arg=-lnccl`) |
| `cmake` fails on `find_package(hipblas)` (ROCm) | Math libs not in base image | Rebuild with the latest `Containerfile` (it adds `hipblas-dev` + `rocblas-dev` + `<pkg>_ROOT` env vars) |

## Alternative: Tailscale-served GGUFs (no R2)

If you'd rather not run an object store, the image also ships an
alternate entrypoint at `/entrypoint-tailscale.sh` that fetches GGUFs
from the laptop over the tailnet on cold-start instead of from R2.

When this is the right call:

- You're self-host-oriented and want one less external service.
- Your laptop's upstream is fast enough that ~50 GB pull on cold-start
  doesn't hurt (≥500 Mbps up = ~14 min; 100 Mbps = ~70 min).

When it's not:

- Residential 30-100 Mbps up — every cold pod boot eats a long
  download window. R2 saturates the pod's downlink (~500 MB/s) and
  cold-starts in ~2 min.
- Multiple concurrent cloud peers — they'd serialize through one
  laptop uplink.

### Laptop side

In a separate shell on the laptop (not inside the toolbox — the
script reads `tailscale ip -4` from the host):

```bash
./scripts/cloud-peer-serve-models.sh
# ── sovereign cloud-peer model server ──
#   bind:        100.x.y.z:9743
#   serve dir:   /tmp/sovereign-models-serve.XXXX
#   contents:
#     FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf
#     Darwin-9B-Opus.Q8_0.gguf
#     Qwen3-Embedding-0.6B-Q8_0.gguf
```

The script symlinks the three local GGUFs into a temp staging dir
and serves it via `python3 -m http.server`. No auth — Tailscale ACLs
are the access boundary, so set them up if you haven't:

```
# Tailscale ACL example
{
  "tagOwners": {
    "tag:laptop":      ["autogroup:admin"],
    "tag:cloud-peer":  ["autogroup:admin"]
  },
  "acls": [
    { "action": "accept", "src": ["tag:cloud-peer"], "dst": ["tag:laptop:9742,9743"] }
  ]
}
```

### Pod side

Override the entrypoint at pod creation:

| RunPod field | value |
|---|---|
| Container Start Command | `/entrypoint-tailscale.sh` |

Drop the `R2_*` env vars; replace with (or rely on the defaults for):

| key | example | required |
|---|---|---|
| `TS_AUTHKEY`        | `tskey-auth-...`     | yes |
| `MESH_SEED_ADDR`    | `100.64.0.2:9742` | yes |
| `MODEL_SERVE_HOST`  | `100.64.0.2`      | no — defaults to host part of `MESH_SEED_ADDR` |
| `MODEL_SERVE_PORT`  | `9743`               | no — matches `cloud-peer-serve-models.sh` default |
| `PRIMARY_GGUF`      | …                    | no — same defaults as R2 path |

The pod uses the same skip-if-present logic as the R2 path (HEAD
remote, compare `Content-Length` to local size, re-fetch on
mismatch), so re-runs on a warm container disk are free.

## Notes

- `PRIMARY_COPIES > 1` requires the multi-primary-slot daemon code
  (the `[models.primary_pool]` config table). The image sets up
  `primary_pool` automatically via the entrypoint when
  `PRIMARY_COPIES > 1`. Single-slot pods (`PRIMARY_COPIES=1`) work
  on every daemon build.
- The image sets `primary_idle_secs = 0` so the primary slot loads
  eagerly on cold-start and stays resident. This trades a small bit
  of wall-time on every cold pod boot for instant chat-call latency
  once the daemon is up — the right call for ad-hoc burst workloads.
- `node_role = "ephemeral-worker"` is metadata only — the mesh
  doesn't currently branch on it. It's there so future role-aware
  scheduling has somewhere to read from.
