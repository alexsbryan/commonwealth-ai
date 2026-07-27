# Cloud tensor peer — renting a GPU pod into the mesh

Validated 2026-07-27 (see `DISTRIBUTED_PILOT_READINESS.md` validation log): a
$0.055/hr Vast RTX 3060 Ti joined the production mesh as a ggml-RPC tensor
worker over iroh and served a shard of the Qwen3.5-4B primary at
**~7.2 t/s median WAN decode** (vs 17.35 t/s same-day LAN forced-tunnel
baseline — the delta ≈ one WAN round-trip per token for the pipeline
crossing). Total experiment cost ≈ $0.04.

The pod is a **tensor worker only**: it holds a shard of the host's primary
and computes layers on demand. All WAN tensor traffic rides iroh
(`cwth/rpc/0` ALPN); the raw — unencrypted — ggml-RPC socket binds loopback
on the pod and is bridged by the iroh acceptor (THREAT_MODEL.md).

## Prerequisites

- Image `ghcr.io/alexsbryan/sovereign-cuda:<tag>` built from the SAME HEAD as
  the host (`Containerfile.cuda`). ggml-RPC is wire-version-sensitive:
  host↔worker llama.cpp must be same-tree. The daemon logs its build SHA at
  boot (`SOVEREIGN_GIT_SHA` build-arg) — verify it matches host HEAD.
- Vast account with credit (**read the `credit` field, not `balance`**) and an
  SSH key registered (`vastai show ssh-keys`; register with
  `vastai create ssh-key "$(cat ~/.ssh/id_ed25519.pub)"`).
- GPU arch must be in the image's CUDA arch list (80;86;89;90): Ampere/Ada
  (RTX 30xx/40xx, A-series). **Avoid** Blackwell (RTX 50xx — SM120 vs the
  image's CUDA 12.1) and Turing/Pascal (RTX 20xx/GTX 10xx).

## Host (once per experiment)

```bash
# 1. Daemon with worker discovery + forced iroh tunnel. The allowlist pins the
#    measured plan to the pod — without it, ANY mesh peer still advertising an
#    RPC port (e.g. from an earlier experiment) takes a shard too.
SOVEREIGN_RPC_DISCOVER=1 \
SOVEREIGN_RPC_TUNNEL=always \
SOVEREIGN_SHARED_MODEL_HOST_NODE_ID=<host node id> \
SOVEREIGN_RPC_WORKER_ALLOWLIST=<pod node id, once known> \
  target/debug/sovereign-cli-daemon daemon run
# (Start without the allowlist, read the pod's node id from mesh status after
#  it joins, then restart with it — eligibility re-settles in 300 s.)

# 2. The invite: use the join_link from /v1/mesh/status — it carries the
#    iroh= dial info the pod needs. Do NOT use `svrn mesh rotate` output for
#    this: the CLI persists to the XDG root while the daemon reads
#    ~/.svrnmesh, so CLI-rotated keys never go live (split-brain, noted
#    2026-07-27). The daemon's live key is authoritative:
curl -s localhost:9741/v1/mesh/status | python3 -c \
  "import json,sys; print(json.load(sys.stdin)['join_link'])"
```

## Pod

```bash
# 1. Provision (inert onstart bypasses the entrypoint's bootstrap contract):
vastai search offers 'rentable=true reliability>=0.95 num_gpus=1 gpu_ram>=8 \
  dph_total<=0.12 inet_down>=500 direct_port_count>=2 compute_cap>=800 compute_cap<=900' \
  -o dph_total --raw
vastai create instance <offer-id> --image ghcr.io/alexsbryan/sovereign-cuda:latest \
  --disk 30 --ssh --direct --onstart-cmd 'sleep infinity' --raw
# If SSH is refused: attach your key to the running instance
#   vastai attach ssh <instance-id> "$(cat ~/.ssh/id_ed25519.pub)"

# 2. Launch the worker (scp + run; script is self-documenting):
scp -P <port> scripts/cloud-rpc-peer-launch.sh root@<ssh-host>:/workspace/
ssh -p <port> root@<ssh-host> \
  "DIAL_LINK='<join_link>' nohup bash /workspace/cloud-rpc-peer-launch.sh \
   > /workspace/worker.log 2>&1 &"
```

The launch script handles the traps found on the first run:

- `[models].embed` is **required** by the config schema — the stub GGUF
  doubles for it (the worker never embeds; the embed-probe WARN is expected
  and only opts the pod out of collaborative ingestion).
- `/root/.local/share/svrnmesh` is symlinked to `/root/.svrnmesh` **before**
  `mesh join` — the CLI persists mesh membership via the XDG root while the
  daemon reads the HOME root; without the symlink the join is invisible to
  the daemon.
- `SOVEREIGN_RPC_SERVE=127.0.0.1:50052` — loopback ONLY (raw ggml-RPC is
  plaintext; iroh is the only WAN path).

## Gates

- **G1 — discovery.** Host log: `discovered mesh RPC worker peer=<pod> …
  via=iroh-bridge`. Pod `/status`: `rpc_worker {port, iroh:true}`.
  `svrn mesh transport`: expect `mixed`/`direct` — relay-only is a finding.
  Worker eligibility needs 300 s continuous presence (do not shorten the
  settle; see DISTRIBUTED_GDN_CRASH_STATUS.md §8.5).
- **G2 — measured decode.** After the discovery loop auto-reloads the primary
  (`mode=distributed` in `/status`):
  `PEER=<pod name> scripts/measure-distributed-decode.sh` — six guards; only
  a `VALID` verdict counts. Results land in
  `target/distributed-decode/distributed-decode.json`.

Expected shape on a small-share split (pod got 1/32 blocks — its free VRAM
advertises minus the stub + CUDA overhead, floored by the 4 GiB quantize
bucket): decode ≈ local rate degraded by ~1 WAN RTT per token; TTFT well
under 2 s once warm. The pod's shard warms via `byte_ranges` (its slice only
— 47 s for ~86 MB on the validated run).

## Teardown

```bash
vastai destroy instance <id>
# Rotate the join key the pod saw — via the DAEMON endpoint (takes effect
# live); the CLI rotate writes where the daemon never reads (see above):
curl -s -X POST localhost:9741/v1/mesh/rotate
# Verify: join_key in /v1/mesh/status changed.
# Restart the host daemon without the experiment env vars.
```

## Known limits / follow-ups

- `svrn pipeline pod up` is NOT this path (hardwires `--worker-mode` +
  bootstrap entrypoint; no mesh join, invisible to `discover_rpc_workers`).
  Productizing pod-as-tensor-peer through the pipeline tool is a named
  follow-up.
- The pod's tiny VRAM share is structural for small models: the host's VRAM
  dominates the byte-mass split. For a real capacity win, this path wants the
  can't-fit-one-box case (122B) — see QWEN122B_DISTRIBUTED_HANDOFF.md.
- Member records for destroyed pods linger as `offline` until pruned by
  mesh policy; rotation prevents rejoin.
