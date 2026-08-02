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

```bash
# 1. code
cd ~/dev/commonwealth-ai && git push          # then on the Mac: git pull

# 2. data (2.2 GB). --update so a partial earlier copy resumes cleanly.
rsync -avh --progress --update \
  ~/dev/commonwealth-ai/research/verifier-v0/data/ \
  beefymac-ops:dev/commonwealth-ai/research/verifier-v0/data/
```

Do **not** sync `runs/` — the Halo run dirs are evidence that belongs to this box,
and `findings/M0_PROBE_HALO.md` already carries their conclusions.

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
