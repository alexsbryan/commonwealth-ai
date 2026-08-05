# Training verifier-v0 on rented GPU

The Halo measured **477 s/it** on the 4B — 13.3 days/epoch, 2.7× worse than
`VERIFIER_V0.md §4` budgeted. Renting is both faster and cheaper than the
local electricity, so M3 moves to a rented box. This directory is how.

```bash
cloud/pod.sh up --gpu RTX_PRO_6000_WS   # search, rent, wait for ssh
cloud/pod.sh sync  <id>                 # push scripts + the 780 MB dataset
cloud/pod.sh provision <id>             # stack, model, machine preflight
cloud/pod.sh probe <id>                 # full preflight, then the 25-step arm
cloud/pod.sh fetch <id>                 # pull traces + adapter back
cloud/pod.sh down  <id>                 # destroy, close the ledger row
```

`cloud/pod.sh list` shows what is running and what it has cost. So does
`sovereign pipeline pod list` — rows go into the same
`~/.sovereign/pipeline-pods.json` in the same schema `sovereign-pipeline`'s
`ledger.rs` reads, so a training pod is never invisible to the accounting that
already exists.

## Why this is a shell script and not `sovereign pipeline pod up`

`pipeline pod up` builds an **ephemeral inference worker**: it mints a
bootstrap blob, boots our `sovereign-cuda` image whose entrypoint ends in
`daemon run --worker-mode` (`sovereign/container/entrypoint.sh:114`), and
drives a job protocol whose only reverse flow is `GET
/internal/worker/completed` returning **JSON unit results**
(`worker_controller.rs:616`).

A training pod needs none of that and one thing that does not exist there:

| need | inference pod today |
|---|---|
| Python + torch | runtime image installs `curl ca-certificates unzip libnccl2 libgomp1` — **no Python at all** (`Containerfile.cuda:334`) |
| arbitrary command | entrypoint hardcodes `exec sovereign-cli daemon run --worker-mode` |
| bring **files** back | **no mechanism** — the protocol returns JSON, never files |

What *is* reusable is reused: `pod.rs`'s offer query and ranking are mirrored
here, and `ledger.rs`'s file and schema are written directly. Phase 2 lifts
this script into a `pipeline pod --kind train` once the probe has proven the
shape — building the Rust surface first would mean guessing it.

## What crosses the wire

Measured Halo uplink: **4.2 MB/s**. That number decides the whole transport
design.

| artifact | size | how |
|---|---|---|
| `Qwen/Qwen3.5-4B` | 8.8 GB | **pod pulls from HF** (~1 min at datacenter bandwidth vs ~35 min pushed from here) |
| `data/orpo-76k/` | 780 MB | rsync up, ~3 min — neither in git nor on the Hub |
| adapter + traces | 90–180 MB | rsync down |
| `hf/checkpoint-*` | GBs | **not fetched** — reproducible from the adapter |

`sync` uses `rsync -azt`. The `-t` is load-bearing: `train_orpo_trl.py` caches
per-row token lengths under a key of `(train.jsonl size, mtime)`. Drop the
mtime and the pod re-tokenizes 74,674 rows — two minutes of paid time and an
unexplained pause before step 1 that reads as a hang.

## The preflight gate

`cloud/preflight.py` runs before anything paid and **exits non-zero on any
failure**. Every check it makes is for something that degrades a run without
stopping it:

- **no gcc** → Triton cannot JIT its launcher stub → `fla` silently falls back
  and the 18 gated-deltanet layers run eager-torch at ~1.3× the step time. The
  run *succeeds*. Its s/it is a lie about the hardware.
- **`fla` missing** → same outcome.
- **`causal_conv1d` present** → flips the deltanet path to CHUNKED: ~100 GB of
  intermediates at seq 4096 and a SIGKILL partway in.
- **arch not in torch's list** → kernels PTX-JIT or fail; the live risk on
  Blackwell.
- **VRAM below 52 GB** → the 4B peaked at 51.88 GB on the Halo under exactly
  this config.

It reports **PASS / FAIL / SKIP** and names the counts. A check that could not
run is never counted as a pass (§18.1).

The Triton check compiles and runs a real kernel **in a subprocess**, because
in-process it can take the interpreter down rather than raise — it SIGSEGV'd on
the Halo and killed preflight mid-checklist. Isolated, a segfault becomes a
verdict.

**The gate is validated in both directions** (§18.4). On the Halo without
`LD_PRELOAD=libhsa-runtime64.so.1` it reports
`triton.jit FAIL — probe died on SIGSEGV` → UNFIT; with the preload set, the
same command reports `triton.jit ok` → FIT. A named failing input and a named
passing input, on real hardware.

## Why the pins are the Halo's pins

`requirements-cu128.txt` pins `torch==2.10.0+cu128` against the Halo's
`torch 2.10.0+rocm7.0`, and every other package to the exact version the Halo
ran. **The only difference between a Halo run and a cloud run is the
accelerator** — which is what makes the s/it comparison a hardware measurement
rather than a stack comparison.

`cu128` specifically: CUDA 12.8 is the floor for `sm_120` (Blackwell / RTX PRO
6000). `cu126` would exclude it; `cu129`/`cu130` drift further from what the
Halo proved.

## One launcher, both platforms

`scripts/launch_arm.sh` runs on the Halo and on a CUDA pod. It takes
`REPO_DIR`, `TRAIN_ENV`, `PY`, `MODEL`, `OUT`; the Halo's values are the
defaults, so every historical invocation still means what it meant. The
amdgpu-only parts (HSA preload, launch-time GTT baseline) are gated on the
sysfs path existing, not on the library being absent — "no amdgpu here" and
"amdgpu here but the runtime is missing" are different failures and only the
second should stop a run.

Two launchers would be two deciders for one recipe (§10.6), and the
hyperparameters at the bottom of that file *are* the recipe.

## The memory tripwire crosses platforms too

`--gtt-limit-gb` read amdgpu sysfs and compared `box == box and box > limit`.
On CUDA that read returns NaN, the comparison short-circuits, and **the guard
never fires and never says so** — a run's log is indistinguishable from a
guarded one. We were about to take that onto a rented GPU for a ~20-hour paid
run.

It is now `--mem-limit-gb` (`--gtt-limit-gb` still works — one dest, two
spellings), the reading comes from `mem_reading()` which uses sysfs GTT on
amdgpu and `torch.cuda.mem_get_info()` on CUDA (device-wide, so co-tenants are
visible either way), and the limit is platform-derived: 112 GB on amdgpu, 92%
of device total on CUDA. If neither source answers, the trainer prints

```
memory tripwire NEVER-RAN: no memory source — tripwire cannot be armed.
This run is UNGUARDED against memory exhaustion
```

and continues. An unguarded run is a decision the operator may make; it can no
longer be *mistaken* for a guarded one.

### …and arming it exposed what it was measuring (2026-08-05)

Armed on CUDA for the first time, the guard killed two cheap-tier probes at
step 4 and let the A100 finish. Same recipe, same demand:

| card | total | **ours** | reserved | device-wide | outcome |
|---|---|---|---|---|---|
| A100 SXM4 80GB | 79.25 | 36.53 | 60.22 | 61.87 | completed 25 |
| RTX PRO 5000 | 47.27 | 35.89 | 44.36 | 44.97 | aborted step 4 |
| RTX A6000 | 44.43 | 35.89 | 40.49 | 40.96 | aborted step 4 |

**Demand is identical and fits every card.** What varies is `reserved` —
torch's caching allocator expands to fill whatever is present and never hands
back memory it has touched. The guard judged *device-wide* against 92% of
total, so on a card small enough for the cache to fill it, **it aborted on its
own cache**. No OOM was ever observed on any of the three.

The tell was printed on every abort: `this process holds nanGB of it`. A guard
that cannot apportion what it measures cannot tell "a co-tenant is eating the
card" from "we cached a lot" — and apportioning was its entire purpose.

So the decider is now three numbers instead of one (`MemReading`):

- **ours** — `max_memory_allocated`, windowed per step. The thing that OOMs.
- **reserved** — our allocator's pool. Cache. Watched, never judged.
- **unattributed** — device-wide minus our reserve. A co-tenant, plus our own
  ~0.5–1.7 GB GPU context.

`--mem-limit-gb` still bounds the *pool*; what bounds *us* is
`demand_ceiling(limit, unattributed)` — the limit less what someone else holds.
On amdgpu that is arithmetically the old predicate (`ours > 112 − cotenant` ≡
`box > 112`), so the Halo's hard-won 112 is untouched. On CUDA it is the fix:
the PRO 5000's ceiling lands at 42.9 GB against 35.9 GB of demand.

**Raising the 92% would have been the wrong fix** — it hides the attribution
bug and leaves a guard that still cannot say what it is measuring.

Tests: `scripts/test_mem_tripwire.py` (exit 0 = pass). It runs all three cards
through both the old and the new decider: the old one must reproduce the two
observed aborts, the new one must pass all three and still fire on a synthetic
30 GB co-tenant. A fix you have not watched reproduce the original failure is
not a fix you have watched work.

### The guard was necessary and not sufficient — the second fix is the allocator

Re-run on the same card with the guard fixed: **15 clean steps instead of 3, then
a GENUINE OOM at step 16.** Not a guard trip — the guard never fired.

```
Tried to allocate 7.50 GiB; 2.41 GiB free of 47.27 GiB
  allocated by PyTorch       29.03 GiB   <- real demand, far under the ceiling
  reserved but UNALLOCATED   15.23 GiB   <- fragmentation
```

**On a small card the binding constraint is fragmentation, not demand** — and a
demand-based guard cannot see it, by construction: fragmentation lives in the gap
between `reserved` and `allocated`. That blind spot is accepted deliberately. Do
not close it by re-adding a device-wide trip; that fires on healthy caching a
dozen steps before the real event, which is the bug this whole section documents.

The remedy is `PYTORCH_ALLOC_CONF=expandable_segments:True`, now set by default
for CUDA in `scripts/launch_arm.sh`. Paired runs, same pod, same seed,
bit-identical losses — the allocator is the only variable:

| step | without | with |
|---|---|---|
| 3 | 34.77s · reserved 44.36 | 34.60s · reserved 40.22 |
| 10 | 41.27s · reserved 44.36 | 41.14s · reserved 40.22 |
| 15 | 46.62s · reserved 44.36 | 46.37s · reserved 40.22 |
| 16 | **OOM** | cleared — reserve grows 40.22 → 41.17 |
| 25 | — | **completed, status ok** |

It costs nothing measurable in speed and lowers steady reserve ~4.1 GB at
identical demand. **Both fixes are required**: without the guard fix the run
aborts at step 4 regardless of the allocator.

**Result: the cheap tier is viable.** 25 steps at **38.51 s/it median** on a
$0.6681/hr PRO 5000, against the A100's 38.06 at ~$0.93–1.31/hr — the two cards
are equal in throughput to within 1.2%, and the saving is entirely the price.
Quote the 24-step median, never one step: s/it ranges 32.0–46.4 *within* a run
as the length-grouped sampler works through buckets.

## Picking the GPU

Do not carry a price in a plan. `vastai search` moved the A100 SXM4 from
$0.660 to $0.934 in a single day. Re-read at launch.

The 51.88 GB measured peak puts a hard floor under the cheap tier — RTX 4090
48GB, A6000, RTX 6000 Ada ($0.39–0.54) are all out at micro 1. That leaves the
A100 80GB and the RTX PRO 6000 WS 96GB, which is why the probe runs on **both**
rather than on an argument about which is faster.
