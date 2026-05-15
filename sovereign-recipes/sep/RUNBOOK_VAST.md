# SEP enrichment on a Vast.ai peer — runbook

The pipeline driver runs locally; one or more Vast.ai pods join the
mesh as ephemeral worker peers. Each pod runs the same `sovereign-cli
daemon` as the laptop, but advertises a primary GGUF slot the driver
fans inference out to via the mesh load balancer.

Everything that used to be ad-hoc bash (xargs fan-out, restart loops,
manual cost math, peer health probes) is handled by `sovereign
pipeline`.

## Where to run these commands

Two environments matter — get the distinction right or things break
in confusing ways:

- **Toolbox** (`sovereign-vulkan` Distrobox/toolbx container). The
  daemon lives here, because llama.cpp's Vulkan backend can't find
  `libamdhip64` from the host systemd unit (see memory:
  [project_wiki_tier2_500_atlas]). The `sovereign` CLI you'll run
  for `pipeline run` / `pipeline status` / `pipeline pod up` is the
  one on `~/.local/bin/sovereign` inside this toolbox.
- **Host**. Where Tailscale runs (`tailscaled` is a host service),
  where `podman` / `docker` lives, and where `vastai` is installed.

The toolbox can reach the host's tailscale daemon through a bind
mount — that's the "tailscale-from-toolbox" trick used below.
Otherwise it's a clean separation: containers/Tailscale ops on host,
sovereign ops in toolbox.

## Prereqs (one-time)

### 1. Tooling

Inside the toolbox:
```bash
pip install vastai
vastai set api-key $VAST_API_KEY   # or the CLI prompts on first use
which rclone                       # required for the local R2 preflight
# If rclone is missing:
curl https://rclone.org/install.sh | sudo bash
```

### 2. Container image pushed to a registry

Build the CUDA image from `sovereign/container/Containerfile.cuda`
and push it somewhere Vast can pull from. **The image bundles
`entrypoint.sh` — every time you change either file you must rebuild
and re-push.**

Build runs on the **host** (no podman inside the toolbox):
```bash
# In a HOST shell, from the repo root:
podman build -t ghcr.io/<you>/sovereign-cuda:latest \
             -f sovereign/container/Containerfile.cuda .
podman push ghcr.io/<you>/sovereign-cuda:latest
```

Then back in the **toolbox**, point the CLI at it:
```bash
export SOVEREIGN_VAST_IMAGE=ghcr.io/<you>/sovereign-cuda:latest
```

Verify the pushed image is current before paying for a pod:
```bash
TOKEN=$(curl -s "https://ghcr.io/token?service=ghcr.io&scope=repository:<you>/sovereign-cuda:pull" | python3 -c 'import sys,json;print(json.load(sys.stdin)["token"])')
curl -sL -H "Authorization: Bearer $TOKEN" \
     -H "Accept: application/vnd.oci.image.manifest.v1+json" \
     https://ghcr.io/v2/<you>/sovereign-cuda/manifests/latest \
  | python3 -c 'import json,sys;m=json.load(sys.stdin);print("config:",m["config"]["digest"])'
# Then fetch the config blob — its `created` field is the push time.
```
If the push time is older than your last edit to `entrypoint.sh` or
`Containerfile.cuda`, **the pod will run a stale entrypoint**. Common
trap — symptoms range from "rclone sync fails because the sync code
path is the old one" to "daemon never starts because the config has
wrong slot names". Rebuild and re-push.

### 3. Mesh founder running locally

The pod will mesh-join your laptop. Inside the toolbox:
```bash
sovereign daemon status
# If it's not running:
sovereign daemon start
```

### 4. Env-var contract for `pod up`

`sovereign pipeline pod up` validates the following at start and
fails fast with the complete missing-set if anything's absent.
The CLI reads them from the shell env and re-exports them into
the pod's onstart command. The pod entrypoint
(`sovereign/container/entrypoint.sh:41-45`) re-checks them with
`: "${VAR:?…}"` — drop one and you waste ~60s of pod boot per
attempt.

| Var | Read by | Source |
|---|---|---|
| `TAILSCALE_AUTHKEY` or `TS_AUTHKEY` | CLI → exported to pod as both names | Tailscale admin → Settings → Keys → Generate (reusable, ephemeral, pre-authorized). The CLI prefers `TAILSCALE_AUTHKEY` and falls back to `TS_AUTHKEY` — so `export TS_AUTHKEY=…` in `~/.bashrc` is enough. |
| `R2_ENDPOINT` | CLI → pod | Cloudflare R2 bucket endpoint (`https://<account>.r2.cloudflarestorage.com`) |
| `R2_ACCESS_KEY` | CLI → pod | Scoped R2 token, Object Read on the bucket |
| `R2_SECRET_KEY` | CLI → pod | Paired with `R2_ACCESS_KEY` |
| `R2_BUCKET` | CLI → pod | Bucket name OR `bucket/prefix` if your GGUFs sit under a folder. Defaults to `sovereign-models` (no prefix). |
| `SOVEREIGN_VAST_IMAGE` | CLI directly | `ghcr.io/<you>/sovereign-cuda:latest` (or pass `--image`) |
| `MESH_JOIN_LINK` | CLI → pod | `cwth-…` bare key from daemon's `/v1/mesh/status` (or pass `--mesh-join-link`) |
| `SOVEREIGN_FOUNDER_ADDR` | CLI → pod as `MESH_SEED_ADDR` (port `:9742` auto-appended) | The HOST's tailnet IPv4 (or pass `--founder-addr`) |
| `PRIMARY_GGUF` (optional) | CLI → pod | Overrides the GGUF filename the pod loads as primary. Defaults to `FINAL-Bench_Darwin-36B-Opus-Q6_K.gguf`. The CLI hardcodes `SINGLE_MODEL=primary`, which fetches only primary + embed (skipping the fast slot — `ModelsSection::fast` is Optional and the primary slot subsumes the fast role; see `sovereign-core/src/setup_config.rs`). Right shape for L40S since the 3-slot loadout doesn't fit in 45 GB VRAM. |
| `EMBED_GGUF` (optional) | CLI → pod | Overrides the embed model filename. Defaults to `Qwen3-Embedding-0.6B-Q8_0.gguf` (~700 MB). Required slot — embedding is a different model class, can't be subsumed by primary. |
| `CONTEXT_SIZE` (optional) | CLI → pod | Defaults to `16384`. Safe on an L40S with Darwin-36B-Q6. Bump to `32768` on H100 / 80 GB cards. |

Quick collect commands (inside the toolbox):
```bash
# Already in ~/.bashrc? Skip this. Otherwise:
export TS_AUTHKEY="tskey-auth-…"          # CLI auto-falls-back from TAILSCALE_AUTHKEY → TS_AUTHKEY
# R2_* should already be in ~/.bashrc for daemon use. Verify with:
echo "$R2_BUCKET / $R2_ENDPOINT / akey=${#R2_ACCESS_KEY}b"

# Mesh join key + this node's tailnet address come straight from the
# daemon — `sovereign mesh status` reads /v1/mesh/status and exposes
# both via flags right-shaped for env capture.
export MESH_JOIN_LINK="$(sovereign mesh status --json | python3 -c 'import sys,json;print(json.load(sys.stdin)["join_key"])')"
export SOVEREIGN_FOUNDER_ADDR="$(sovereign mesh status --self --addr-only | cut -d: -f1)"
echo "founder addr = $SOVEREIGN_FOUNDER_ADDR"   # should be 100.x.y.z
```

`sovereign mesh status` (with no args) prints a human-readable view
showing every member's node_id, name, status, and advertised
addresses. `--addr-only` strips everything except the addresses
(one per line) for scripting. `--self` filters to the current node.
The HTTP endpoint `/v1/mesh/status` is the underlying source of
truth for both CLI and the desktop UI.

### 5. R2 layout — verify before the first pod

The R2 bucket can be:
- a flat layout: `r2:sovereign-models` containing `*.gguf` directly, set `R2_BUCKET=sovereign-models`.
- a nested layout: `r2:sovereign-models/sovereign-models/*.gguf`, set `R2_BUCKET=sovereign-models/sovereign-models`.

Verify with rclone before launching anything:
```bash
RCLONE_CONFIG_R2_TYPE=s3 RCLONE_CONFIG_R2_PROVIDER=Cloudflare \
RCLONE_CONFIG_R2_REGION=auto \
RCLONE_CONFIG_R2_ENDPOINT="$R2_ENDPOINT" \
RCLONE_CONFIG_R2_ACCESS_KEY_ID="$R2_ACCESS_KEY" \
RCLONE_CONFIG_R2_SECRET_ACCESS_KEY="$R2_SECRET_KEY" \
RCLONE_CONFIG_R2_ACL=private \
rclone lsf "r2:$R2_BUCKET"
```
You must see your `PRIMARY_GGUF` filename as a top-level entry of
this listing. `pod up` runs the same probe locally and refuses to
launch if it doesn't see the file — that's the cheapest check we
have.

### 6. SEP corpus on the laptop

```bash
sovereign corpus install sep   # ~1 GB parquet
```

## Run the campaign

```bash
# 1) Spin up a Vast pod. Defaults: L40S, $0.80/hr cap, sep-core-v1.
#    The CLI prints what it's about to do; pass --dry-run first if
#    you want to inspect the onstart command without paying.
sovereign pipeline pod up --gpu L40S --recipe-id sep-core-v1 --max-price 0.80

# 2) Capture the vast id from the output. IMMEDIATELY tail logs in
#    another shell — pod logs are NOT preserved after destruction:
VAST_ID=<from-step-1>
vastai logs $VAST_ID --tail 1000 > /tmp/pod-$VAST_ID.log &
# (Re-run vastai logs periodically; it's polled, not streamed.)

# 3) Verify the pod joined the mesh.
curl -s http://localhost:9741/v1/mesh/status | python3 -m json.tool
# Look for a NEW member with `status: online`. Takes 3-8 min:
#   ~30s tailscale-up + beacon
#   ~3-5 min model fetch (30 GB for Darwin-36B-Q6 on a 1 Gbps link)
#   ~30-60s daemon boot + slot load
#   ~5s mesh-join handshake
# /v1/models will also start listing the new peer's model.

# 4) Start the ingest. Defaults to the whole SEP corpus, autopaced
#    via adaptive concurrency, retries failed slugs up to 3x.
#
# Two shapes to pick from:
#
# 4a) Foreground — keep the terminal attached. Best for short runs
#     or while you're babysitting:
sovereign pipeline run sovereign-recipes/sep/pipelines/sep-core-v1.toml \
    --concurrency 3
# Ctrl-C drains in-flight units, then exits cleanly.
#
# 4b) Background under nohup — survives terminal disconnect. Best
#     for multi-hour campaigns. **Do NOT stop a backgrounded driver
#     with Ctrl-C** — once the launching terminal disconnects,
#     SIGINT no longer reaches the driver (observed orphan, 2026-05-15;
#     driver kept running for hours after operator believed it was
#     paused). Always stop via `sovereign pipeline pause` (next
#     section), which finds the PID via pgrep and SIGTERMs directly.
LOGFILE=~/.sovereign/logs/pipeline/sep-core-v1-$(date +%Y%m%d-%H%M%S).log
mkdir -p "$(dirname "$LOGFILE")"
nohup sovereign pipeline run sovereign-recipes/sep/pipelines/sep-core-v1.toml \
    --concurrency 3 > "$LOGFILE" 2>&1 &
echo "log: $LOGFILE"
# Re-running `pipeline run` picks up exactly where you left off.
```

### Stopping cleanly

```bash
# This is the right tool — sends SIGTERM to the driver(s) for this
# recipe (found via pgrep), waits for in-flight units to finish,
# then releases the worklist claims so the next run can pick them
# back up. Works regardless of how the driver was started or
# whether your terminal is still attached.
sovereign pipeline pause sep-core-v1

# If a driver is wedged or you don't care about in-flight work:
sovereign pipeline pause sep-core-v1 --force        # SIGKILL
```

`--concurrency N` controls how many enrich shell-outs run in parallel
locally. The daemon's mesh load balancer then routes each one across
all online peers. Set N ≈ (local primary slots) + (sum of remote
primary slots) for a good ceiling.

## During the run

```bash
# Live snapshot from another shell. Works whether the driver is alive
# or paused — reads straight from the worklist DB.
sovereign pipeline status sep-core-v1

# Live pod cost.
sovereign pipeline pod list

# Tail the driver itself for the per-tick status line + failure logs.
tail -f ~/.sovereign/logs/pipeline/sep-core-v1-*.log

# Snapshot the pod's stdout (vast destroys it on close — capture early
# and often). If the pod is on fire, this is the only post-mortem source.
vastai logs $VAST_ID --tail 2000 > /tmp/pod-$VAST_ID-$(date +%H%M).log
```

What to watch for:

- **`rate_per_hr` climbing then flattening** — adaptive concurrency
  found a stable ceiling. Healthy.
- **`concurrency_eff < concurrency_max`** — backoff is active. Either
  the mesh is genuinely at capacity (fine — let it run) or peer
  health is degrading. Cross-check with `/v1/mesh/status`.
- **`failure buckets: refused / timeout / vram_thrash` growing** —
  the mesh is overloaded. `pod up` a second peer to add capacity,
  or accept the lower throughput.
- **`failure buckets: mismatch / model_missing` growing** — data or
  config bug. Stop and triage; more capacity won't help.
- **`failure buckets: inference_json_parse` growing** — the primary
  model on a peer is producing non-conforming JSON. Verify the peer
  loaded the expected GGUF (`curl http://<peer-tailnet-ip>:9741/v1/models`);
  if it loaded a smaller/different model, fix `PRIMARY_GGUF` on its
  `pod up` and relaunch.

## Pausing overnight (or for the day)

The recipe ships with `[schedule]` commented out. To make the driver
auto-pause during the day:

```toml
[schedule]
active_hours = "22:00-06:00"   # local time
```

Or just `Ctrl-C` whenever you want. The same `pipeline run` command
resumes it.

## Done — destroy the pod

```bash
sovereign pipeline pod list           # find the vast id
sovereign pipeline pod down <id>      # destroys + closes ledger + prints final $
```

The pod is gone; the ingest's progress is preserved in the worklist DB
(`~/.sovereign/pipeline.db`). Re-launching a pod tomorrow and re-running
the same recipe picks up where it left off.

## Failure-mode triage

The pod prints its boot progress to stdout as numbered `[entrypoint]`
sections (0=GPU diag, 1=Tailscale, 2=R2 sync, 3=config, 4=daemon).
`vastai logs` shows everything up to ~5-10s ago. If the pod exited,
**the logs disappear** — keep a tail running.

| Symptom | Likely cause | Action |
|---|---|---|
| `pod up` complains about missing env vars before doing anything | Contract isn't met | Set them in the toolbox shell (see §4 above) — CLI prints the full missing set so one round-trip resolves it |
| `pod up` R2 pre-flight FAILED: PRIMARY_GGUF=… not in bucket | R2_BUCKET path mismatch with actual layout | Re-run the `rclone lsf` check from §5; either fix R2_BUCKET to include the prefix or upload the GGUF |
| `pod up`: `vastai search returned no offers` | `--max-price` too low, GPU sku rare | Raise `--max-price`, try `--gpu RTX_4090` or `--gpu H100_80GB` |
| Pod boots but never appears in `/v1/mesh/status` | Tailscale beacon failed (entrypoint exits at the 60s mark) | `vastai logs <id>` and look for "FATAL: cannot reach mesh seed". Causes: TS_AUTHKEY single-use already consumed, Tailscale ACL blocking `tag:cloud-peer → tag:laptop`, founder daemon down, MESH_SEED_ADDR stale (laptop's tailnet IP changed) |
| Pod boots but `/v1/models` doesn't include its model | Model still downloading from R2, OR the daemon crashed at slot load | Check log for `[entrypoint] FATAL` first. If sync completed, look for slot-load errors — often VRAM-OOM on small GPUs |
| Pod stays online but `rate_per_hr` stays at zero | Mesh routing skipping the new peer | `curl http://<peer-tailnet-ip>:9741/v1/models` — if it lists the expected primary, the load balancer is suppressing it. Restart the local driver |
| Driver exits saying "source command failed" | SEP parquet not acquired | `sovereign corpus install sep` then retry |
| Single slug fails 3× → `failed` | Bad slug, broken parquet row, or recipe bug | `sovereign enrich sep-ingest <slug> --force && sovereign enrich build sep-<slug>` to repro locally |
| Pushed image is current but pod still runs old code | Vast cached an older layer | Force `vastai destroy instance <old-id>` then `pod up` again — Vast pulls the manifest fresh each time |
| `R2 self-test FAILED: ... 403 Forbidden` with rclone "Time may be set wrong" NOTICE in the log just before | Host clock skewed >15 min from UTC — AWS SigV4 rejects | The entrypoint now sets the clock from `https://www.cloudflare.com`'s HTTP Date header before rclone runs (`[entrypoint] clock sync`). If that step says `WARNING: date -s rejected`, the container is missing `CAP_SYS_TIME` — try a different Vast offer (each host's runtime caps differ). |
| Pod boots, daemon parses config, then logs `fast/primary alias setup failed; primary will lazy-load` | `ModelSlot::from_existing_model` returned an error — rare, usually a context-size mismatch or backend init quirk | Non-fatal but doubles VRAM on first primary use. Check the warning's `error=` field; if it's recurring, file an issue with the GGUF + ctx_size from the pod's config |
| You "Ctrl-C'd" the pipeline driver hours ago but `sovereign pipeline status` shows work still being claimed; `ps` finds a live `sovereign pipeline run` PID | Driver was orphaned — started in a terminal that's since disconnected (script, Claude harness, lost SSH). SIGINT went to the terminal, not the driver. | `sovereign pipeline pause <recipe-id>` — pgrep-based, terminal-independent. Always prefer this over Ctrl-C for backgrounded/nohup'd drivers. |

## Files touched by this workflow

| File | Purpose |
|---|---|
| `~/.sovereign/pipeline.db` | Worklist state (per-recipe, persists across runs) |
| `~/.sovereign/pipeline-pods.json` | Pod cost ledger |
| `~/.sovereign/logs/pipeline/sep-core-v1-*.log` | Per-driver-invocation log (created by the `pipeline run` shell snippet above) |
| `~/.sovereign/mesh.json` | Mesh membership (auto-managed) |
| `sovereign-recipes/sep/pipelines/sep-core-v1.toml` | Recipe — edit to tune retries/concurrency/schedule |
| `sovereign/container/Containerfile.cuda` | Pod image source — rebuild + push after edits |
| `sovereign/container/entrypoint.sh` | Pod boot script — same: rebuild + push after edits |
| `sovereign/crates/sovereign-cli/src/pipeline_cmd.rs:cmd_pod_up` | The CLI side; env-var validation + R2 preflight + onstart synthesis |

## When you don't need a pod

Local-only run, three commands:

```bash
SOVEREIGN_DISABLE_PEER_INFERENCE=1 sovereign daemon start
sovereign pipeline run sovereign-recipes/sep/pipelines/sep-core-v1.toml
sovereign pipeline status sep-core-v1
```

At the laptop's primary-slot throughput this is ~4-9 slugs/hour →
4-7 days for the full SEP corpus. A single L40S pod adds another
~6-8 slugs/hour (~1.6× the laptop's rate), so the breakeven against
the $0.60/hr is hours, not days.
