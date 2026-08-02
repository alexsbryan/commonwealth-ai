# Migrating the verifier-v0 training lane to the M2 Max

**Why:** M0 measured the Strix Halo at 176.71 s/it against the Mac's ~53 (3.33x
slower) *and* found it cannot sustain a run — OOM-killed at step 63 of 100 as GTT
ratcheted 25 → 103 GB. See `findings/M0_PROBE_HALO.md`. Roles are now: **Mac
trains, Halo serves.** This document is the move.

**This overturns VERIFIER_V0 §4**, which schedules M3 on the Halo (§4:245, :462).
§4 assumed 10–25 sustained TFLOPS on gfx1151; measured is 2.7 (Mac 9.0). The spec
has not been edited — that is the spec owner's call.

---

## 0. Read this first — the correctness trap

**Any `data/orpo-76k` or `data/orpo-ab` already on the Mac is STALE AND
CONTAMINATED.** The Mac's probe ran 2026-07-29. Stream A's contamination re-fix
landed 2026-07-31 (`findings/contamination_report_streamA_refixed.json`) and the
splits were rebuilt on the Halo 2026-08-01 11:04, now excluding 34 rows
(`manifest.json: contamination_excluded_rows: 34`).

**Overwrite, do not reuse.** After syncing, verify on the Mac:

```bash
python3 -c "import json;m=json.load(open('data/orpo-76k/manifest.json'));print(m['counts'],m['contamination_excluded_rows'],m['seed'])"
# expect: {'train': 74674, 'valid': 1000, 'test': 1000} 34 17
```

If `train` is not 74674, you are on the pre-re-fix build.

---

## 1. What moves, and how

Code is already in git (27 tracked files). **`data/` and `runs/` are gitignored**
(`.gitignore:3-4`) and must be copied. Total `data/` = 2.2 GB.

Only **Stream B is irreplaceable** — 19,019 ORPO pairs generated locally from the
35B model. Everything else is rebuildable from HF via `prepare_orpo_data.py`.
Copy it all anyway: it is minutes over Tailscale, and rebuilding risks divergence
in the contamination exclusions above.

**Run from the Fedora HOST, not the `sovereign-rocm-7.2.4` toolbox** — that
container has no `ssh`/`scp`/`rsync`/`tailscale` (only `curl`), and its
`sovereign` CLI is broken there too (`libvulkan.so.1` missing), so the mesh path
is unavailable from inside as well.

Code is easy:

```bash
cd ~/dev/commonwealth-ai && git push          # then on the Mac: git pull
```

Do **not** sync `runs/` — the Halo run dirs are evidence that belongs to this box,
and `findings/M0_PROBE_HALO.md` already carries their conclusions.

### BLOCKER: there is no file-transfer path to the Mac today

`rsync` over `beefymac-ops` **fails by design**, tried 2026-08-02:

```
ops-channel: verb not allowed: [rsync --server -vulogDtpre.iLsfxCIvu . dev/...]
allowed: ping status mesh-status transport http-status mesh-http logs [N]
         cache-size exe-info git-head daemon-start daemon-stop daemon-restart
         daemon-kill9 [dry]
```

`beefymac-ops` is a **sandboxed verb surface** (`docs/OPS_CHANNEL.md`) — port
2222, dedicated `svrn_ops_ed25519` key, `IdentitiesOnly yes`. The allowlist has no
file-transfer verb, and `~/.ssh/config` defines **no other route to the Mac**.
This is a deliberate posture, not a misconfiguration; do not work around it by
loosening the channel without the owner's say.

### The payload is far smaller than 2.2 GB — reframe before picking a route

Only **Stream B** cannot be rebuilt. `prepare_orpo_data.py` regenerates
`orpo-76k`, `orpo-probe` **and** `orpo-ab` from the HF source (manifest pins
`source_sha256` + `seed: 17`) given Stream B's pairs. So the file that actually
has to cross is:

```
data/stream_b/all/orpo_pairs.jsonl      19,019 rows, ~180 MB
```

**~180 MB, not 2.2 GB** — a 12x reduction that makes every option below viable.
Rebuild the rest on the Mac and verify against §5's manifest checks, which exist
precisely to catch a divergent rebuild.

### Candidate routes, in the order worth trying

1. ~~A normal sshd on port 22~~ — **RULED OUT** 2026-08-02:
   `ssh -p 22 alexsbryan@100.104.36.28 true` → `Connection refused`. The Mac runs
   no general sshd. Do not retry this.

2. **RECOMMENDED — manual pull over Tailscale. No sudo, no new SSH surface.**
   The ops channel restricts *automation*, not a human at the keyboard, and
   Tailscale is already the authenticated encrypted path. Serve on the Halo, pull
   on the Mac:

   ```bash
   # Halo host shell, in research/verifier-v0/data/stream_b/all/
   python3 -m http.server 8099

   # Mac Terminal, in the matching directory
   curl -O http://<halo-tailscale-ip>:8099/orpo_pairs.jsonl
   ```

   Stdlib only, zero sudo on both ends. Taildrop (`tailscale file cp`) also
   works but wants `sudo tailscale set --operator=$USER` once on Fedora to reach
   the daemon socket — so the HTTP route is the more sudo-free of the two.

3. **HF Hub as the transport.** Push Stream B as a private dataset repo and pull
   it on the Mac. Consistent with how prebuilt corpora already move in this repo.

4. **A `recv-blob` ops verb — only if this becomes recurring.** Not worth it for
   one 180 MB move; the trigger is the Mac becoming the training box and starting
   to ship checkpoints *back*.

   **Keeping it sudo-free is a design property, not a configuration trick:** sudo
   is only needed if you write outside the ops user's own tree, so don't. The
   forced command already runs as `alexsbryan` — write to `~/svrn-inbox/` and
   nothing privileged is involved (no system paths, no ports < 1024, no installs).

   ```sh
   recv-blob)                       # ssh beefymac-ops recv-blob NAME SHA256 < file
     name=$(basename -- "$2")
     case "$name" in ""|.*|*/*) exit 2;; esac
     case "$name" in *[!A-Za-z0-9._-]*) exit 2;; esac
     dest="$HOME/svrn-inbox/$name"
     head -c "$MAX_BYTES" > "$dest.part" || exit 3
     [ "$(shasum -a 256 "$dest.part" | cut -d' ' -f1)" = "$3" ] || { rm -f "$dest.part"; exit 4; }
     mv "$dest.part" "$dest"
     ;;
   ```

   **The client must never supply a destination path** — only a basename, which
   the server sanitizes; the server picks the directory. That is the exact failure
   mode already on record against `sovereign-server`, whose
   `/v1/documents/upload` took an absolute server-side path and let any tenant
   ingest another tenant's config. Same bug class; do not reintroduce it in a new
   surface. The byte cap bounds a hostile sender, and the sha256 check makes the
   verb idempotent and gives a real integrity signal rather than "bytes arrived."

---

## 2. Mac-side stack (already proven at 0.8B — do not re-derive)

Per `README.md:20-30`: mlx 0.32.0 / mlx-lm 0.31.3 / Metal, verified 2026-07-29,
Qwen3.5 (`qwen3_5`) natively supported.

```bash
uv venv .venv --python 3.13
uv pip install --python .venv/bin/python mlx-lm-lora mlx-lm datasets huggingface_hub
```

**The trainer is `mlx_lm_lora`, NOT `scripts/train_orpo_trl.py`.** That script is
PyTorch/TRL and exists for the Halo lane; it is not the Mac path and has never
been run on MPS.

### Traps that travel with it

- **`mlx_lm_lora.train -c config.yaml` only fills args whose argparse default is
  `None`** (`README.md:34`). Flags like `--train-mode` silently keep their CLI
  defaults over the YAML value. **Pass operative flags on the CLI**; use YAML only
  for `lora_parameters`, which has no flag. This one silently trains the wrong
  recipe.
- **`mlx_lm fuse` is broken for Qwen3.5 — use `scripts/fuse_lora_manual.py`.**
  Two independent defects (`findings/M0_PROBE.md`): it drops the MTP layer, and
  it corrupts the hybrid-attention merge outright. Already solved, in git.
- **`hf download lytang/LLM-AggreFact` fails** ("Unable to parse string as hex
  hash value"); direct `curl` with the bearer token works (`README.md:42`).

---

## 3. Sequence — M1 first, and it needs almost nothing

**M1 (0.8B on Stream A) is the next milestone, not M3.** It needs only
`data/orpo-76k` (781 MB) and the 0.8B base the Mac already has. That is the
fastest path to "running on the Mac."

| run | data needed | base model | measured/est. wall-clock |
|---|---|---|---|
| **M1** — 0.8B, Stream A | `orpo-76k` | already on Mac | **~34 h/epoch** (measured basis) |
| **M3** — 4B LoRA, A+B | `orpo-ab` | **Qwen3.5-4B, ~8 GB, must fetch** | ~7 days/epoch, ~2 weeks for 2 |

---

## 4. Do this before scheduling M3: a 4B memory probe (~30 min)

**The one thing gating the 2-week M3 run is unmeasured: does a 4B ORPO step fit
in 64 GB under MLX?** M0 established exactly how expensive it is to discover a
memory ceiling late — the Halo trained fine for 50 steps and then died.

Run a 3–5 step 4B probe on `orpo-probe` and watch peak RSS **before** committing
two weeks. Cheap, and it converts the last assumption in the plan into a number.

Reasons to expect it fits, none of them a substitute for measuring:

- 4B bf16 weights ≈ 8 GB, vs ~1.6 GB for the 0.8B. The 0.8B run peaked ~22 GB of
  64 GB, so the naive delta lands ~28–30 GB.
- **The ORPO memory driver does not scale with model size.** The logits tensor is
  `micro x 2 x seq x 248,320` — chosen *and* rejected against a 248k vocab. That
  term is identical at 0.8B and 4B, and it is what forced micro-batch 1 on the
  Halo.
- MLX is unified-memory native and does not have the ROCm allocator's
  `expandable_segments` gap that caused the Halo ratchet.

Also carry forward from M0, they are framework-independent:

- **Effective batch 32 / seq 4096** sets iters/epoch and therefore the whole
  wall-clock table. Hold both constant or the numbers stop comparing.
- **Bigger micro-batch was *slower*** on the Halo (313.3 vs 231.8 s/it) because
  sequences span ~2k–5k tokens and get padded to the longest in the batch.
- **Gradient checkpointing was free** in time and cut memory ~3x.
- Only **2 of 2000** probe rows hit the 4096 truncation, and `max_prompt_length`
  2048 truncates 7 of 2000 (0.35%). Re-check the latter on the real sets before
  M1 — for a grounding verifier a truncated document is a label the model cannot
  verify.

---

## 5. Verification — you have moved correctly when

1. `git log --oneline -1` matches on both boxes.
2. `data/orpo-76k/manifest.json` on the Mac reports `train: 74674`,
   `contamination_excluded_rows: 34`, `seed: 17`.
3. `data/orpo-ab/manifest.json` reports `train: 93693`, `stream_b_rows: 19019`,
   `stream_b_share: 0.1988`.
4. `du -sh data` on the Mac ≈ 2.2 G.
5. A 3-step 0.8B `mlx_lm_lora` run on `orpo-probe` reproduces ~53 s/it.
