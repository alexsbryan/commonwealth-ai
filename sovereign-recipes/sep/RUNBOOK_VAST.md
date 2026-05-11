# SEP enrichment on a Vast.ai peer — runbook

Five commands from a clean checkout to a running ingest. Everything else
that used to be ad-hoc bash (xargs fan-out, restart loops, manual cost
math, peer health probes) is handled by the pipeline driver.

## Prereqs (one-time)

1. **Vast CLI installed and logged in**
   ```bash
   pip install vastai
   vastai set api-key $VAST_API_KEY   # or the CLI prompts on first use
   ```

2. **Container image pushed**

   Build the CUDA image from `sovereign/container/Containerfile.cuda`
   and push it somewhere Vast can pull from:
   ```bash
   podman build -t ghcr.io/<you>/sovereign-cuda:latest \
                -f sovereign/container/Containerfile.cuda .
   podman push ghcr.io/<you>/sovereign-cuda:latest
   export SOVEREIGN_VAST_IMAGE=ghcr.io/<you>/sovereign-cuda:latest
   ```

3. **Mesh founder running locally**

   The pod will mesh-join your laptop. Make sure the daemon is up:
   ```bash
   sovereign daemon status
   # If it's not running:
   sovereign daemon start
   ```

4. **Tailscale auth key**

   Generate a reusable, ephemeral auth key in the Tailscale admin
   console (Settings → Keys). Then:
   ```bash
   export TAILSCALE_AUTHKEY="tskey-auth-…"
   ```

5. **Mesh-join link**
   ```bash
   sovereign mesh status   # copy the cwth-… join link
   export MESH_JOIN_LINK="cwth-…"
   export SOVEREIGN_FOUNDER_ADDR="$(tailscale ip -4 | head -1)"
   ```

6. **SEP corpus on the laptop**
   ```bash
   sovereign corpus acquire sep   # ~1 GB parquet
   ```

## Run the campaign

```bash
# 1) Spin up a Vast pod.
sovereign pipeline pod up --gpu L40S --recipe-id sep-core-v1 --max-price 0.80
# Prints: vast id, hourly rate, image. Pod entrypoint joins the
# mesh automatically; no SSH required.

# 2) Verify the pod showed up.
sovereign mesh status
# Wait until you see the new peer with the SEP-suitable model loaded.
# Takes 3-8 min: tailscale-up + image pull + model fetch + daemon.

# 3) Start the ingest. Defaults to the whole SEP corpus, autopaced
#    via adaptive concurrency, retries failed slugs up to 3x.
sovereign pipeline run sovereign-recipes/sep/pipelines/sep-core-v1.toml
# Ctrl-C any time — the driver drains in-flight units, then exits.
# Re-running picks up exactly where you left off.
```

## During the run

```bash
# Live snapshot from another shell. Works whether the driver is alive
# or paused — reads straight from the worklist DB.
sovereign pipeline status sep-core-v1

# Live pod cost.
sovereign pipeline pod list

# Tail the driver itself for the per-tick status line + failure logs.
# (The driver emits to stderr; redirect to a file if running under tmux.)
```

What to watch for:

- **`rate_per_hr` climbing then flattening** — adaptive concurrency
  found a stable ceiling. Healthy.
- **`concurrency_eff < concurrency_max`** — backoff is active. Either
  the mesh is genuinely at capacity (fine — let it run) or peer
  health is degrading. Cross-check with `sovereign mesh status`.
- **`failure buckets: refused / timeout / vram_thrash` growing** —
  the mesh is overloaded. `pod up` a second peer to add capacity,
  or accept the lower throughput.
- **`failure buckets: mismatch / model_missing` growing** — data or
  config bug. Stop and triage; more capacity won't help.

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

| Symptom | Likely cause | Action |
|---|---|---|
| `vastai search returned no offers` | `--max-price` too low, GPU sku rare | raise `--max-price`, try `--gpu RTX_4090` |
| Pod up but `mesh status` doesn't show it | tailscale auth key expired or one-shot | regenerate as reusable, `vastai logs <id>` for clues |
| `rate_per_hr` stuck at zero | mesh sees the peer but model isn't loaded | check pod logs: model still downloading from R2, or VRAM-OOM on load |
| Driver exits saying "source command failed" | SEP parquet not acquired | `sovereign corpus acquire sep` then retry |
| Single slug fails 3× → `failed` | bad slug or recipe bug | `sovereign pipeline run sep-core-v1.toml --key <slug>` once locally to repro |

## Files touched by this workflow

| File | Purpose |
|---|---|
| `~/.sovereign/pipeline.db` | worklist state (per-recipe, persists across runs) |
| `~/.sovereign/pipeline-pods.json` | pod cost ledger |
| `~/.sovereign/mesh.json` | mesh membership (auto-managed) |
| `sovereign-recipes/sep/pipelines/sep-core-v1.toml` | recipe — edit this to tune retries/concurrency/schedule |
| `sovereign/container/Containerfile.cuda` | pod image source |
