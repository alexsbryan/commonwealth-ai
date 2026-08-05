#!/usr/bin/env python3
"""ORPO LoRA trainer on TRL — the Strix Halo (ROCm) half of VERIFIER_V0 §7 M0.

The M2 Max half of the M0 probe ran under `mlx_lm_lora`. This is the
counterpart so the wall-clock table in findings/M0_PROBE.md can name BOTH
boxes instead of extrapolating everything from the Mac.

Defaults mirror the Mac probe exactly (M0_PROBE.md:9):
    Qwen/Qwen3.5-0.8B, ORPO, LoRA r=32/alpha=64, lr 1e-4,
    batch 4 x grad-accum 8 (effective 32), seq 4096, 100 iters, data/orpo-probe.

Glassbox: every optimizer step emits wall-clock, loss, host RSS and GPU memory
to a JSONL sidecar, and the run ends by writing a summary JSON with the s/it
distribution. A timing claim you cannot re-derive from the log is not a
measurement.

Usage:
    train_orpo_trl.py --out runs/probe-halo-trl
    train_orpo_trl.py --out runs/probe-halo-unsloth --unsloth
"""

from __future__ import annotations

import argparse
import inspect
import json
import os
import platform
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import NamedTuple

REPO = Path(__file__).resolve().parents[1]  # research/verifier-v0
DEFAULT_DATA = REPO / "data" / "orpo-probe"

# The B-matrix rule lives in exactly one place. check_adapter_trained.py is the
# standalone auditor for arbitrary adapter dirs; this trainer applies the SAME
# rule to its own output, so a run can never again look healthy while having
# learned nothing (HALO_HANDOFF_2026-08-02.md §1).
sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_adapter_trained import DIVERGED, TRAINED, report, scan  # noqa: E402


# --------------------------------------------------------------------------
# observability helpers
# --------------------------------------------------------------------------

def host_rss_gb() -> float:
    """Resident set of THIS process, in GB. /proc is authoritative; psutil is
    an optional dependency we do not want to require."""
    try:
        with open(f"/proc/{os.getpid()}/status") as fh:
            for line in fh:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1]) / (1024 * 1024)
    except OSError:
        pass
    return float("nan")


def host_gtt_gb() -> float:
    """GTT held by the WHOLE BOX, from sysfs.

    `torch.cuda.max_memory_allocated()` cannot see GTT reserved by the HIP
    runtime outside PyTorch's allocator. On the M0 probe it read 29.34 GB flat
    while the box held 103 GB and the trainer was SIGKILLed
    (findings/M0_PROBE_HALO.md:72). Recording only torch's counter is how a run
    ratchets to death while its own log reports a steady 29 GB.

    This number is the box's, NOT this process's — see proc_gtt_gb.
    """
    try:
        with open("/sys/class/drm/card1/device/mem_info_gtt_used") as fh:
            return int(fh.read().strip()) / 1024**3
    except OSError:
        return float("nan")


class MemReading(NamedTuple):
    """One memory sample with every number's MEANING kept separate.

    This shape exists because collapsing them into one number cost two paid
    cheap-tier probes on 2026-08-05. Measured, same recipe, three cards:

        card             total  ours  reserved  pool_used  outcome
        A100 SXM4 80GB   79.25  36.53   60.22     61.87    completed 25 steps
        RTX PRO 5000     47.27  35.89   44.36     44.97    ABORTED at step 4
        RTX A6000        44.43  35.89   40.49     40.96    ABORTED at step 4

    OUR DEMAND IS IDENTICAL ON ALL THREE and fits every card with room. What
    varies is RESERVED — torch's caching allocator expands to fill whatever is
    present and does not hand back memory it has touched. The tripwire judged
    `pool_used` against 92% of `total`, so on a small card it aborted on ITS
    OWN CACHE. No OOM was ever observed on any of the three. Both cheap-tier
    aborts were false positives, and the tell was printed every time: "this
    process holds nanGB of it" — the guard could not attribute what it judged.

    So: `ours` is what OOMs, `reserved` is cache and is NOT a leak, and
    `unattributed` (pool_used - reserved) is what SOMEONE ELSE holds — which
    was the guard's actual purpose. A guard that cannot say whose memory it is
    cannot tell "a co-tenant is eating the card" from "we cached a lot".
    """
    ours: float           # attributed to THIS process — the thing that OOMs
    reserved: float       # our allocator's pool, cache included. Not a leak.
    pool_used: float      # everything resident in the pool, all processes
    unattributed: float   # pool_used - reserved: co-tenants (+ our GPU context)
    total: float
    source: str


def mem_reading(torch) -> MemReading:
    """Sample the pool this trainer can actually exhaust, WITH attribution.

    ONE DECIDER, TWO PLATFORMS (ARCH_PRINCIPLES §10.6). The tripwire needs to
    know "how much of the memory that kills this process is ours, and how much
    is someone else's" — and both halves have a different home on each box:

    - amdgpu (the Halo): unified memory. `host_gtt_gb` is box-wide because a
      co-tenant's model load counts against the same ~125 GB pool that SIGKILLs
      us; `proc_gtt_gb` attributes our share via drm fdinfo. Here the guard is
      SURVIVAL: exhaustion is a SIGKILL that takes the adapter, the timings and
      twice now the graphical session.
    - CUDA (rented GPUs): discrete VRAM. `mem_get_info` reports the WHOLE
      DEVICE including other processes; `memory_allocated` is our live demand
      and `memory_reserved` our allocator's pool. Here the guard is a
      COURTESY — a discrete OOM raises a Python exception rather than killing
      the process — so it must not cost us a run it was never going to save.

    WHY THIS FUNCTION EXISTS AT ALL. Until 2026-08-04 the tripwire read the
    amdgpu sysfs path directly and compared `box > limit`. On CUDA that read
    raises OSError, returns NaN, and `box == box` is False — so the guard
    silently never fired. The run looked guarded in the log and was not: a gate
    nobody has watched fail (§18.1) resolving into a silent substitution
    (§18.3). Fixing that arming bug is what exposed the attribution bug above.

    Returns all-NaN/"unavailable" when neither source answers. The CALLER must
    treat that as could-not-judge and say so out loud — never as "under limit".
    """
    gtt = host_gtt_gb()
    if gtt == gtt:  # not NaN
        total = float("nan")
        try:
            with open("/sys/class/drm/card1/device/mem_info_gtt_total") as fh:
                total = int(fh.read().strip()) / 1024**3
        except OSError:
            pass
        ours = proc_gtt_gb()
        if ours != ours:
            # fdinfo did not answer. Charge the WHOLE BOX to us — conservative,
            # preserves the pre-2026-08-05 amdgpu behaviour exactly, and says
            # which of the two happened rather than quietly under-counting.
            return MemReading(gtt, float("nan"), gtt, 0.0, total,
                              "amdgpu-sysfs-gtt-unattributed")
        return MemReading(ours, float("nan"), gtt, max(0.0, gtt - ours), total,
                          "amdgpu-sysfs-gtt")
    if torch.cuda.is_available():
        try:
            free, total = torch.cuda.mem_get_info()
        except Exception:  # noqa: BLE001 - fall through to could-not-judge
            pass
        else:
            total_gb = total / 1024**3
            pool_used = (total - free) / 1024**3
            # PEAK since the last reset, not the instantaneous figure: sampled
            # at step end the live tensors are the TROUGH (activations already
            # freed), and what has to fit is the peak inside the step. The
            # caller owns the window — see the reset in the step callback.
            ours = torch.cuda.max_memory_allocated() / 1024**3
            reserved = torch.cuda.memory_reserved() / 1024**3
            # Never negative: on a quiet card this is our own CUDA context
            # (~0.5-1.7 GB measured across the three cards above), and a real
            # co-tenant shows up as GB on top of that.
            return MemReading(ours, reserved, pool_used,
                              max(0.0, pool_used - reserved), total_gb,
                              "cuda-mem-get-info")
    nan = float("nan")
    return MemReading(nan, nan, nan, nan, nan, "unavailable")


def resolve_mem_limit(explicit: float | None, total: float, source: str) -> tuple[float, str]:
    """(limit_gb, rationale). Platform-appropriate default, never a silent one.

    The historic 112.0 default is an amdgpu number — 112 of the Halo's ~125 GB
    unified pool, set on 2026-08-03 after 95 killed arm A at step 118 by
    sitting BELOW this workload's ordinary ~95-100 GB transient. It is
    meaningless on an 80 GB card, where it would sit ABOVE the device total and
    could never fire.

    On CUDA the limit is derived from the device instead: 92% of total. The
    headroom is deliberately thinner than the Halo's because discrete VRAM has
    no host-memory spillover — there is no gradual pressure, the allocation
    just fails — so the guard's job is to stop cleanly a step or two before
    that, not to leave a large reserve.

    THIS IS A LIMIT ON THE POOL, NOT ON US. What the guard compares against our
    demand is `demand_ceiling(this, unattributed)` — see there for why the
    difference is the whole 2026-08-05 bug.
    """
    if explicit is not None:
        return (explicit, f"explicit --mem-limit-gb {explicit}")
    if source.startswith("amdgpu-sysfs-gtt"):
        return (112.0, "amdgpu default 112 GB of ~125 GB unified (see M2 arm A)")
    if source == "cuda-mem-get-info" and total == total:
        return (round(0.92 * total, 1), f"92% of {total:.1f} GB device total")
    return (float("nan"), "no memory source — tripwire cannot be armed")


def over_limit(used: float, limit: float) -> bool:
    """Is the pool over the limit? NaN on EITHER side is False — and that is
    exactly why this is a named function with tests rather than an inline
    comparison.

    The bug it replaces read `box == box and box > args.gtt_limit_gb`: a NaN
    reading (every non-amdgpu box) short-circuited to False, which is correct
    for "do not abort" and catastrophic as "the guard is working". The NaN case
    is could-not-judge, and the CALLER distinguishes it — see the ARMED /
    NEVER-RAN lines in the step callback. Keeping the predicate honest here and
    the verdict-reporting there is the split that makes both testable.
    """
    return used == used and limit == limit and used > limit


def demand_ceiling(pool_limit: float, unattributed: float) -> float:
    """How much may OUR process demand before the guard fires.

    The pool limit belongs to the POOL; what is left for us is that limit minus
    whatever somebody else is already holding. Judge our demand against this
    and the guard answers the only question that predicts an OOM: is there
    still room for what we need?

    ON amdgpu THIS IS ARITHMETICALLY THE OLD BEHAVIOUR AND THAT IS DELIBERATE.
    ours > 112 - cotenant is the same predicate as box > 112, so the Halo's
    hard-won 112 GB setting (95 killed arm A at step 118 by sitting BELOW this
    workload's ordinary transient) keeps its exact meaning. Nothing about the
    unified-memory box changes here.

    ON CUDA IT IS THE FIX. The old guard judged pool_used — our demand PLUS our
    allocator's cache — against 92% of the device, so a card small enough for
    the cache to fill it aborted a run that fit. Reserved is not a leak and
    must not be treated as one. Worked on the three measured cards: the ceiling
    lands at 42.9 GB (PRO 5000) and 40.4 GB (A6000) against a real demand of
    35.9 GB, so both pass, while a genuine 30 GB co-tenant drops the PRO 5000's
    ceiling to 13.2 GB and aborts before the allocator ever fails.

    NaN propagates — over_limit() reads that as could-not-judge, never as safe.
    """
    return pool_limit - unattributed


def proc_gtt_gb(pid: int | None = None) -> float:
    """GTT resident for THIS process alone, attributed via drm fdinfo.

    WHY THIS EXISTS. The sysfs counter above is box-wide, and this box has GPU
    co-tenants: the sovereign daemon and its `--compute-child` hold renderD128
    open and were measured at 26.9 GB resident while a training probe ran.
    Attributing that to the trainer would turn a co-tenant's model load into a
    phantom "leak", and — worse — a real leak into a shrug about the daemon.
    M0's attribution was done by hand once (`/proc/*/fd`, one pid holding GPU
    fds); this makes it automatic and per-step.

    Dedupes on drm-client-id: a process opens the render node many times and
    every fd reports the SAME totals, so summing over fds inflates by the fd
    count.
    """
    pid = pid or os.getpid()
    seen: dict[str, int] = {}
    fddir = f"/proc/{pid}/fd"
    try:
        fds = os.listdir(fddir)
    except OSError:
        return float("nan")
    for fd in fds:
        try:
            if "renderD" not in os.readlink(f"{fddir}/{fd}"):
                continue
            client = None
            kib = 0
            with open(f"/proc/{pid}/fdinfo/{fd}") as fh:
                for line in fh:
                    if line.startswith("drm-client-id:"):
                        client = line.split()[-1]
                    elif line.startswith("drm-resident-gtt:"):
                        kib = int(line.split()[1])
            if client is not None:
                seen[client] = max(seen.get(client, 0), kib)
        except (OSError, ValueError, IndexError):
            continue
    if not seen:
        return float("nan")
    return sum(seen.values()) / 1024**2


def gpu_mem_gb(torch) -> tuple[float, float, float]:
    """(allocated, max_allocated, RESERVED) in GB.

    RESERVED is the one that matters and the one this trainer spent two
    sessions not logging. `memory_allocated` is what the tensors need and it
    sits FLAT while the process dies: arm A's trace pinned `gpu_peak_gb` at
    32.56 from step 2 to 118 while box GTT climbed past 100 GB. The growth is
    torch's allocator RESERVE, which is what actually occupies unified memory,
    and it grows when the allocator is handed a new sequence shape every step
    (probe: 3.48 -> 82.88 GB over 60 varying-shape iters, FLAT at 37.91 for 60
    fixed-shape iters, identical alloc_peak). Without this column the ratchet
    was misdiagnosed twice -- as co-tenancy, then as a leak.

    ON DISCRETE CUDA THE OPPOSITE HOLDS, and the guard now says so: reserve
    growing to fill a rented card is the allocator doing its job, not a
    ratchet, because torch frees cached blocks and retries before it ever
    OOMs. Reserved is the column to WATCH on both platforms and the column to
    JUDGE on neither -- see MemReading and demand_ceiling.

    NOTE the middle value is a PER-STEP peak: the step callback resets the
    counter after each sample, and maintains the run-wide max itself.
    """
    if not torch.cuda.is_available():
        return (float("nan"), float("nan"), float("nan"))
    return (
        torch.cuda.memory_allocated() / 1024**3,
        torch.cuda.max_memory_allocated() / 1024**3,
        torch.cuda.memory_reserved() / 1024**3,
    )


def _decile(series: list[float], agg) -> list[float] | None:
    """`agg` applied to each tenth of `series`, in order.

    Ten numbers instead of one slope. Reading them left to right answers the
    only memory question that has ever mattered on this box: is the process
    holding more than it did, or is it briefly touching more?
    """
    if not series:
        return None
    n = len(series)
    k = max(1, n // 10)
    return [round(agg(series[i:i + k]), 2) for i in range(0, n, k)][:10]


def _batch_fingerprint(inputs) -> str:
    """A stable identity for one batch, for answering 'did resume re-feed?'.

    Hashes the token ids rather than reporting a row index because the index
    is exactly what we do not have: by the time a batch reaches training_step
    it has been through the sampler and the collator and carries no row id.
    The ids ARE the row, so two legs printing the same digest are training on
    the same examples -- which after a resume means the dataloader skip did
    not happen.
    """
    import hashlib

    keys = sorted(k for k, v in inputs.items()
                  if k.endswith("input_ids") and hasattr(v, "detach"))
    if not keys:
        return f"UNFINGERPRINTABLE (no *input_ids in {sorted(inputs)})"
    h = hashlib.blake2b(digest_size=8)
    shapes = []
    for k in keys:
        t = inputs[k].detach().to("cpu")
        h.update(k.encode())
        h.update(t.numpy().tobytes())
        shapes.append(f"{k}{tuple(t.shape)}")
    return f"{h.hexdigest()}  {' '.join(shapes)}"


VISION_HINTS = ("visual", "vision", "image", "vit", "patch_embed", "merger")


def language_linear_modules(model) -> tuple[list[str], dict[str, int]]:
    """Every nn.Linear on the LANGUAGE side, by full module name.

    Why not `target_modules="all-linear"`: Qwen3.5-0.8B is a
    `Qwen3_5ForConditionalGeneration` — it carries a vision tower and an MTP
    head that the Mac probe did not adapt (M0_PROBE.md counted 186 adapted
    modules across linear_attn + self_attn, language side only). "all-linear"
    would silently adapt the vision tower too, making the two probes different
    experiments wearing the same config.

    Full names are returned rather than leaf names because peft matches by
    suffix: leaf names like `q_proj` would re-admit the vision tower through
    the back door.
    """
    import torch.nn as nn

    names: list[str] = []
    for name, mod in model.named_modules():
        if not isinstance(mod, nn.Linear):
            continue
        low = name.lower()
        if any(h in low for h in VISION_HINTS):
            continue
        # lm_head is the output projection; MTP is a separate prediction head.
        # Neither is a decoder projection, and neither was adapted on the Mac.
        if low.endswith("lm_head") or ".mtp" in low or low.startswith("mtp"):
            continue
        names.append(name)

    breakdown: dict[str, int] = {}
    for n in names:
        leaf = n.split(".")[-1]
        breakdown[leaf] = breakdown.get(leaf, 0) + 1
    return names, breakdown


def load_base_model(args, torch, dtype):
    """Resolve the model class from the checkpoint rather than assuming one.

    `AutoModelForCausalLM` raises on a `*ForConditionalGeneration` config in
    some transformers versions; when it does, honour the architecture the
    checkpoint itself declares.
    """
    from transformers import AutoConfig, AutoModelForCausalLM

    cfg = AutoConfig.from_pretrained(args.model)
    arch = (getattr(cfg, "architectures", None) or ["<unstated>"])[0]
    print(f"checkpoint architecture: {arch} (model_type={cfg.model_type})")
    kwargs = dict(dtype=dtype, attn_implementation=args.attn)
    try:
        return AutoModelForCausalLM.from_pretrained(args.model, **kwargs), arch
    except (ValueError, KeyError) as exc:
        print(f"  AutoModelForCausalLM refused it ({type(exc).__name__}); "
              f"falling back to {arch} directly")
        import transformers as tf
        cls = getattr(tf, arch, None)
        if cls is None:
            raise RuntimeError(
                f"transformers {tf.__version__} does not export {arch}. "
                f"The installed transformers cannot load this checkpoint, so a "
                f"Halo number here would not be the same experiment as the Mac's."
            ) from exc
        return cls.from_pretrained(args.model, **kwargs), arch


def describe_environment(torch) -> dict:
    """Everything a reader needs to decide whether a later run is comparable."""
    env = {
        "host": platform.node(),
        "platform": platform.platform(),
        "python": sys.version.split()[0],
        "torch": torch.__version__,
        "torch_hip": getattr(torch.version, "hip", None),
        "torch_cuda": getattr(torch.version, "cuda", None),
        "container": None,
        "gpu": None,
        "gpu_count": torch.cuda.device_count() if torch.cuda.is_available() else 0,
        "hsa_override": os.environ.get("HSA_OVERRIDE_GFX_VERSION"),
    }
    try:
        with open("/run/.containerenv") as fh:
            for line in fh:
                if line.startswith("name="):
                    env["container"] = line.split("=", 1)[1].strip().strip('"')
    except OSError:
        pass
    if torch.cuda.is_available():
        env["gpu"] = torch.cuda.get_device_name(0)
        try:
            props = torch.cuda.get_device_properties(0)
            env["gpu_arch"] = getattr(props, "gcnArchName", None)
            env["gpu_total_mem_gb"] = round(props.total_memory / 1024**3, 2)
        except Exception:  # noqa: BLE001 - purely informational
            pass
    for mod in ("trl", "peft", "transformers", "datasets", "accelerate", "unsloth"):
        try:
            env[mod] = __import__(mod).__version__
        except Exception:  # noqa: BLE001
            env[mod] = None

    # Which gated-deltanet memory path will this run take? On gfx1151 that
    # single fact decides whether the run survives, and until now NOTHING
    # recorded it: the only trace in train.log was a transformers warning
    # string, so reconstructing a past run's path meant grepping for prose.
    #
    # transformers gates on `is_fast_path_available = all(...)` over four
    # symbols. With fla only, three of four resolve -> SEQUENTIAL path,
    # ~25 GB GTT, survives. Add causal-conv1d and all four resolve ->
    # CHUNKED path, ~100 GB GTT, killed by the host OOM killer.
    # See note e643e089. find_spec, not import: no side effects, no CUDA
    # extension load, no cost to a run that is about to be timed.
    import importlib.util as _ilu

    for mod in ("fla", "causal_conv1d", "triton", "liger_kernel"):
        try:
            env[mod] = "present" if _ilu.find_spec(mod) else None
        except Exception:  # noqa: BLE001
            env[mod] = None
    env["deltanet_path"] = (
        "chunked (INFEASIBLE on gfx1151 -- expect ~100 GB GTT and SIGKILL)"
        if env.get("causal_conv1d") and env.get("fla")
        else "sequential" if env.get("fla")
        else "eager-torch (no fla -- ~1.3x slower)"
    )
    return env


# --------------------------------------------------------------------------
# main
# --------------------------------------------------------------------------

def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--model", default="Qwen/Qwen3.5-0.8B")
    ap.add_argument("--data", type=Path, default=DEFAULT_DATA,
                    help="directory holding train.jsonl / valid.jsonl")
    ap.add_argument("--out", type=Path, required=True, help="run directory")
    ap.add_argument("--iters", type=int, default=100, help="optimizer steps")
    ap.add_argument("--batch-size", type=int, default=4)
    ap.add_argument("--grad-accum", type=int, default=8)
    ap.add_argument("--seq-len", type=int, default=4096)
    ap.add_argument("--lr", type=float, default=1e-4)
    ap.add_argument("--lora-r", type=int, default=32)
    ap.add_argument("--lora-alpha", type=int, default=64)
    ap.add_argument("--beta", type=float, default=0.1, help="ORPO lambda/beta")
    ap.add_argument("--unsloth", action="store_true",
                    help="load through unsloth.FastLanguageModel instead of plain HF")
    ap.add_argument("--grad-checkpointing", action="store_true",
                    help="trade speed for memory; OFF by default so the s/it is "
                         "comparable to the Mac probe, which did not use it")
    ap.add_argument("--attn", default="sdpa", choices=["sdpa", "eager", "flash_attention_2"])
    ap.add_argument("--dtype", default="bfloat16", choices=["bfloat16", "float16", "float32"])
    ap.add_argument("--seed", type=int, default=17, help="matches the data seed")
    ap.add_argument("--empty-cache-every", type=int, default=0, metavar="N",
                    help="call torch.cuda.empty_cache() every N optimizer steps "
                         "(0 = never). The untested GTT-ratchet mitigation from "
                         "M0_PROBE_HALO.md:79 — measure s/it against a baseline "
                         "run before adopting it, empty_cache is not free.")
    # ONE THRESHOLD, TWO SPELLINGS (§10.6). `--gtt-limit-gb` is the historic
    # name and is what launch_arm.sh and every archived run command pass; it
    # names an AMD concept and lies on a CUDA box. `--mem-limit-gb` is the
    # platform-neutral name new callers should use. Both write the SAME dest,
    # so there is still exactly one decider — what changed is that it now has
    # an honest name on both platforms rather than two implementations.
    ap.add_argument("--mem-limit-gb", "--gtt-limit-gb", dest="mem_limit_gb",
                    type=float, default=None,
                    help="the memory pool's ceiling. THIS PROCESS's OWN demand "
                         "may use it less whatever a co-tenant already holds, "
                         "and the run aborts when it exceeds that for "
                         "--mem-limit-consecutive samples in a row. Our "
                         "allocator's CACHE is not demand and does not count "
                         "against it — judging the pool's total was a false "
                         "positive that killed two paid probes on 2026-08-05. "
                         "Default is PLATFORM-DERIVED, not a constant: 112 GB "
                         "on amdgpu (of ~125 GB unified), 92%% of device total "
                         "on CUDA. "
                         "The old hardcoded 112 was an amdgpu number that sits "
                         "above an 80 GB card's total and could never fire. "
                         "History on the amdgpu value: M0 was SIGKILLed at "
                         "100.7 GB and took the desktop session with it; the "
                         "limit was RAISED from 95 on 2026-08-03 because this "
                         "workload's ordinary transient is ~95-100 GB (about 3x "
                         "alloc_peak) and 95 killed arm A at step 118. Keep any "
                         "explicit value ABOVE ~3x your measured alloc_peak and "
                         "below the device total.")
    ap.add_argument("--save-total-limit", type=int, default=2, metavar="N",
                    help="how many checkpoints to KEEP; older ones are deleted "
                         "as new ones are written. Default 2 is sized for "
                         "resume-only use. RAISE IT for any run whose "
                         "intermediate checkpoints you intend to SCORE — at the "
                         "default, a 5,856-step ladder saving every 500 steps "
                         "arrives with only its last two rungs and the "
                         "deletions never appear in the log. A 4B checkpoint is "
                         "~763MB (496 optimizer + 248 adapter).")
    ap.add_argument("--save-every", type=int, default=25, metavar="N",
                    help="write a resumable checkpoint every N optimizer steps "
                         "(default 25 ~= 71 min at the measured 171 s/step). "
                         "Checkpoints land in <out>/hf/checkpoint-<step> and "
                         "carry optimizer, scheduler, RNG and step count.")
    ap.add_argument("--resume", action="store_true",
                    help="continue from the newest checkpoint under <out>/hf "
                         "instead of starting over. REFUSES if there is none — "
                         "silently restarting a 19-hour run from step 0 while "
                         "the operator believes it resumed is the failure this "
                         "flag exists to prevent.")
    ap.add_argument("--no-group-by-length", dest="group_by_length",
                    action="store_false", default=True,
                    help="disable length bucketing. ON by default: the allocator "
                         "reserve grows when it is handed a NEW sequence shape "
                         "every step, and that reserve — not the tensors — is "
                         "what fills unified memory. Measured on the synthetic "
                         "probe: 37 varying shapes reserved 3.48 -> 82.88 GB over "
                         "60 iters, ONE fixed shape reserved 37.91 GB and stayed "
                         "flat for 60, at identical alloc_peak. Bucketing puts "
                         "similar lengths next to each other so the allocator can "
                         "reuse blocks. Turn it off only to reproduce a pre-"
                         "2026-08-03 run.")
    ap.add_argument("--stop-at-step", type=int, default=0, metavar="N",
                    help="stop cleanly once step N completes (0 = run to "
                         "--iters). The LR SCHEDULE IS UNAFFECTED: it still "
                         "spans --iters, so a run stopped at N sits at exactly "
                         "the schedule position a full run passes through at N. "
                         "That is what makes two arms comparable when neither "
                         "reaches the horizon. Lowering --iters instead would "
                         "re-fit the decay to the shorter run and compare two "
                         "different schedules (ARCH_PRINCIPLES §10.6). Added "
                         "2026-08-03 to match arm AB to arm A, which the old "
                         "95 GB tripwire cut at step 117 of a 400-step "
                         "schedule; Ctrl-C cannot land on a named step and the "
                         "GTT tripwire fires wherever the data happens to "
                         "spike.")
    ap.add_argument("--mem-limit-consecutive", "--gtt-limit-consecutive",
                    dest="mem_limit_consecutive", type=int, default=3, metavar="N",
                    help="how many CONSECUTIVE over-limit samples abort the run "
                         "(default 3). One instantaneous sample is not a "
                         "measurement (§18.5) — a transient spike and a runaway "
                         "look identical for exactly one reading, and the run "
                         "that died at step 118 died on a single one. The streak "
                         "is logged per step as gtt_over_consecutive.")
    ap.add_argument("--liger", action="store_true",
                    help="fuse the LM head with the ORPO loss via liger-kernel "
                         "so the 248,320-token vocabulary's logits are never "
                         "materialised. REQUIRED on discrete GPUs: one fp32 "
                         "logits tensor at seq 4096 over chosen+rejected is "
                         "8.1 GB and the loss holds several, which OOM'd an "
                         "80 GB A100 at step 0 on 2026-08-04 while the peak "
                         "counter still read 51.85 GB. The Halo never hit it "
                         "because 124 GB of unified memory absorbed it. "
                         "REFUSES if liger_kernel is missing rather than "
                         "training unfused.")
    ap.add_argument("--rss-limit-gb", type=float, default=40.0,
                    help="abort if this process exceeds this RSS (the Mac run's "
                         "tripwire; peak trainer RSS there was ~22 GB)")
    args = ap.parse_args()

    args.out.mkdir(parents=True, exist_ok=True)
    steps_path = args.out / "steps.jsonl"
    summary_path = args.out / "summary.json"

    # -- imports are deferred so --help works without the stack installed ----
    import torch
    from datasets import load_dataset
    from peft import LoraConfig
    from transformers import AutoModelForCausalLM, AutoTokenizer, TrainerCallback
    # TRL moved ORPO under `trl.experimental` in 1.x (verified on trl 1.9.2,
    # where a top-level `from trl import ORPOConfig` raises ImportError and
    # helpfully suggests GRPOConfig instead). Try the classic path first so
    # this still runs against pre-1.0 installs.
    os.environ.setdefault("TRL_EXPERIMENTAL_SILENCE", "1")
    try:
        from trl import ORPOConfig, ORPOTrainer
        trl_orpo_path = "trl"
    except ImportError:
        from trl.experimental.orpo import ORPOConfig, ORPOTrainer
        trl_orpo_path = "trl.experimental.orpo"

    env = describe_environment(torch)
    env["trl_orpo_import_path"] = trl_orpo_path
    env["ld_preload"] = os.environ.get("LD_PRELOAD")
    print("=== environment ===")
    for k, v in env.items():
        print(f"  {k:18s} {v}")

    # gfx1151 guard. torch's bundled ROCm 7.0 HSA runtime segfaults on the
    # first GPU copy/kernel under ROCm 7.2.4 -- AFTER reporting the device
    # perfectly. Catch it here, where the message can name the fix, rather than
    # 40 seconds into training as an unexplained core dump.
    HSA_FIX = "/opt/rocm/lib/libhsa-runtime64.so.1"
    if (env.get("gpu_arch") == "gfx1151"
            and HSA_FIX not in (os.environ.get("LD_PRELOAD") or "")
            and os.path.exists(HSA_FIX)):
        print(f"\nFATAL: gfx1151 without the system HSA runtime preloaded.\n"
              f"Every GPU op will SIGSEGV once training starts, even though "
              f"torch reports the device correctly above.\n\n"
              f"    export LD_PRELOAD={HSA_FIX}\n\n"
              f"(HSA_OVERRIDE_GFX_VERSION and LD_LIBRARY_PATH do NOT fix this "
              f"-- see note b18dacf9.)", file=sys.stderr)
        return 2

    if not torch.cuda.is_available():
        print("\nFATAL: no GPU visible to torch. A CPU s/it number answers no "
              "question this probe was created to answer.\n"
              "Try: export HSA_OVERRIDE_GFX_VERSION=11.0.0", file=sys.stderr)
        return 2

    torch.manual_seed(args.seed)
    dtype = getattr(torch, args.dtype)

    # -- data ---------------------------------------------------------------
    train_file = args.data / "train.jsonl"
    valid_file = args.data / "valid.jsonl"
    if not train_file.exists():
        print(f"FATAL: no train.jsonl under {args.data}", file=sys.stderr)
        return 2
    ds = load_dataset("json", data_files={"train": str(train_file)})["train"]
    missing = {"prompt", "chosen", "rejected"} - set(ds.column_names)
    if missing:
        print(f"FATAL: dataset missing columns {missing}", file=sys.stderr)
        return 2
    print(f"\ntrain rows: {len(ds)}  ({train_file})")

    # -- model --------------------------------------------------------------
    t_load = time.monotonic()
    peft_config = None
    arch = None
    targets: list[str] = []
    breakdown: dict[str, int] = {}

    if args.unsloth:
        from unsloth import FastLanguageModel
        model, tokenizer = FastLanguageModel.from_pretrained(
            model_name=args.model,
            max_seq_length=args.seq_len,
            dtype=dtype,
            load_in_4bit=False,
        )
        model = FastLanguageModel.get_peft_model(
            model,
            r=args.lora_r,
            lora_alpha=args.lora_alpha,
            lora_dropout=0.0,
            bias="none",
            use_gradient_checkpointing="unsloth" if args.grad_checkpointing else False,
            random_state=args.seed,
            target_modules=["q_proj", "k_proj", "v_proj", "o_proj",
                            "gate_proj", "up_proj", "down_proj"],
        )
        peft_config = None  # unsloth already wrapped it
    else:
        tokenizer = AutoTokenizer.from_pretrained(args.model)
        model, arch = load_base_model(args, torch, dtype)
        model.to("cuda")

        targets, breakdown = language_linear_modules(model)
        if not targets:
            print("FATAL: found no language-side nn.Linear modules to adapt",
                  file=sys.stderr)
            return 2
        print(f"adapting {len(targets)} language-side Linear modules "
              f"(Mac probe: 186). By leaf name:")
        for leaf, count in sorted(breakdown.items(), key=lambda kv: -kv[1]):
            print(f"    {leaf:24s} {count}")
        peft_config = LoraConfig(
            r=args.lora_r,
            lora_alpha=args.lora_alpha,
            lora_dropout=0.0,
            bias="none",
            task_type="CAUSAL_LM",
            target_modules=targets,
        )

    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token
    load_s = time.monotonic() - t_load
    print(f"model loaded in {load_s:.1f}s")

    # -- length bucketing ---------------------------------------------------
    # HF's LengthGroupedSampler needs a length column it can READ. It cannot
    # derive one from ORPO's prompt/chosen/rejected text columns, and when it
    # cannot find lengths it falls back to a random sampler with a log line and
    # NO error — the exact silent-substitution shape §18.3 forbids. So compute
    # the column here, and verify below that the setting survived.
    LENGTH_COL = "length"
    dropped_rows = 0  # rows removed for exceeding --seq-len; see the filter below
    if args.group_by_length:
        t_len = time.monotonic()
        # CACHED BY HAND. `datasets.map` does NOT reuse its cache here (measured
        # 2026-08-03: 118.7s on the first run, 118.7s on the second), and a
        # 2-minute tax on every arm is 2 minutes nobody chose to spend. The key
        # is the data file's identity, so editing the data invalidates it.
        st = train_file.stat()
        cache = train_file.parent / f".lengths-{st.st_size}-{int(st.st_mtime)}.json"
        lens = None
        if cache.exists():
            try:
                lens = json.loads(cache.read_text())
                if len(lens) != len(ds):
                    print(f"NOTE: length cache has {len(lens)} rows, dataset has "
                          f"{len(ds)} — recomputing.")
                    lens = None
            except (OSError, ValueError):
                lens = None

        if lens is None:
            def _lengths(batch):
                # prompt+chosen is the sequence that sets the step's shape; the
                # rejected branch is the same order of magnitude and the sampler
                # only needs a sort key, not an exact count.
                return {LENGTH_COL: [
                    len(tokenizer(p + c, add_special_tokens=False)["input_ids"])
                    for p, c in zip(batch["prompt"], batch["chosen"])
                ]}

            ds = ds.map(_lengths, batched=True, batch_size=256,
                        desc="measuring sequence lengths")
            lens = list(ds[LENGTH_COL])
            try:
                cache.write_text(json.dumps(lens))
            except OSError as e:
                print(f"NOTE: could not write length cache ({e}); "
                      f"the next run pays the tokenisation again.")
        else:
            ds = ds.add_column(LENGTH_COL, lens)

        print(f"length bucketing on: {len(lens)} rows in "
              f"{time.monotonic() - t_len:.1f}s ({cache.name})  "
              f"min={min(lens)} p50={sorted(lens)[len(lens) // 2]} max={max(lens)}",
              flush=True)

        # DROP ROWS THIS TRL CANNOT BOUND. Measured 2026-08-04, and the reason
        # is a TRL bug, not a config mistake:
        #
        #   longer_response_length = max(len(chosen), len(rejected))
        #   for answer_tokens in [chosen_tokens, rejected_tokens]:
        #       if len(prompt_input_ids) + longer_response_length > max_length:
        #           answer_tokens[k] = answer_tokens[k][: max_length - longer_response_length]
        #
        # It truncates only the RESPONSE. Its own docstring claims "first we
        # truncate the prompt", but prompt truncation does not exist in this
        # version (`truncation_mode`, the old prompt-side knob, has 0 mentions).
        # With prompt 5010 and response 1400 against max_length 4096 the slice
        # is `[:2696]` on a 1400-token response — A NO-OP — and the row reaches
        # the model at 6410 tokens. No ORPOConfig setting fixes this.
        #
        # WHY DROP AND NOT TRUNCATE: cutting a grounding prompt means choosing
        # between the instruction (head), the document (middle) and the claim
        # (tail). Losing the claim or the instruction silently changes the task
        # into a different one; there is no safe blind policy. Dropping is
        # honest, and the cost is negligible — see the count printed below.
        #
        # WHY IT MATTERS AT 0.017%: `truncation_report.json` measured only
        # 0.017% of Stream A over 4096 and it was filed NO ACTION. But
        # LengthGroupedSampler SORTS LONGEST FIRST, so those ~13 rows are the
        # FIRST batches every run. A 0.017% data problem is a 100% crash.
        margin = 16  # BOS/EOS and the rejected branch, which `lens` does not measure
        budget = args.seq_len - margin
        over = [i for i, n in enumerate(lens) if n > budget]
        if over:
            pct = 100.0 * len(over) / len(lens)
            print(f"  DROPPING {len(over)} of {len(lens)} rows ({pct:.3f}%) over "
                  f"{budget} tokens — this TRL truncates responses only and "
                  f"cannot bound them; longest kept is "
                  f"{max(n for n in lens if n <= budget)}", flush=True)
            keep = [i for i, n in enumerate(lens) if n <= budget]
            ds = ds.select(keep)
            lens = [lens[i] for i in keep]
        else:
            print(f"  all rows within {budget} tokens", flush=True)
        dropped_rows = len(over)

    # -- config -------------------------------------------------------------
    cfg_kwargs = dict(
        output_dir=str(args.out / "hf"),
        max_steps=args.iters,
        per_device_train_batch_size=args.batch_size,
        gradient_accumulation_steps=args.grad_accum,
        learning_rate=args.lr,
        beta=args.beta,
        max_length=args.seq_len,
        # BOTH BOUNDS, OR NEITHER BINDS. TRL 1.9.2's ORPOTrainer truncates in
        # two places and needs two settings:
        #   responses: tokenizer(..., truncation=True, max_length=max_completion_length)
        #   prompt:    prompt[: max_length - longer_response_length]
        # `max_completion_length` unset means None means NO response bound, and
        # the prompt clamp then has nothing finite to subtract. Measured
        # consequence on 2026-08-04: a run configured --seq-len 4096 fed a
        # FIRST BATCH of chosen_input_ids(1, 6410) — 56% over — and OOM'd an
        # 80 GB A100 at step 0. Length bucketing sorts longest-first, so step 1
        # gets the dataset maximum (6409); there is no warm-up into it.
        #
        # `max_prompt_length` — which this file passed until 2026-08-04 — DOES
        # NOT EXIST in this TRL (0 mentions in ORPOTrainer source). The
        # signature filter dropped it to a one-line NOTE, so the config
        # expressed a truncation policy the library never implemented and the
        # only symptom was a log line nobody had reason to read as fatal.
        max_completion_length=args.seq_len // 2,
        logging_steps=1,
        # CHECKPOINT SO A RUN CAN BE PAUSED AND RESUMED. At the measured 171 s/step
        # a 400-step arm is ~19 HOURS; "one process, start to finish" is not a
        # posture that survives a box that is also someone's workstation. HF
        # checkpoints carry optimizer + scheduler + RNG + step count, so
        # --resume continues rather than restarts.
        save_strategy="steps",
        save_steps=args.save_every,
        # ROTATION SILENTLY DELETES A LEARNING CURVE. The default of 2 is right
        # for a run whose checkpoints exist only to resume from — but a run whose
        # PURPOSE is a ladder of intermediate checkpoints loses every early rung
        # long before it finishes, and nothing says so: the directory just has
        # two entries at the end and the deletions are invisible in the log.
        # Caught in the M3 resume rehearsal 2026-08-05, where checkpoint-10
        # vanished when checkpoint-30 was written.
        # A 4B checkpoint is ~763 MB (496 optimizer + 248 adapter), so 12 rungs
        # is ~9 GB against the pod's 120 GB disk. Raise it for any run you intend
        # to score midway; see --save-total-limit.
        save_total_limit=args.save_total_limit,
        report_to=[],
        disable_tqdm=True,  # the bars flood a captured log with 100+ KB of \r frames
        seed=args.seed,
        bf16=(args.dtype == "bfloat16"),
        fp16=(args.dtype == "float16"),
        gradient_checkpointing=args.grad_checkpointing,
        remove_unused_columns=False,
    )
    if args.group_by_length:
        # NOT `group_by_length` — transformers 5.14's ORPOConfig does not have
        # it, and setting it here only produces a dropped-kwarg NOTE. Bucketing
        # is applied via the sampler override at trainer construction.
        cfg_kwargs["length_column_name"] = LENGTH_COL
    if args.liger:
        # THE LOGIT PROBLEM. Qwen3.5's vocabulary is 248,320 tokens. ORPO
        # forwards chosen AND rejected, so one optimizer micro-step holds
        # 2 x seq_len x 248320 logits — at seq 4096 that is 8.1 GB for a SINGLE
        # fp32 copy, and the loss needs several live at once (logits,
        # log_softmax, gathered logps). Call it 25-30 GB spent on the loss head
        # before a single activation.
        #
        # On the Halo this was invisible: 124 GB of UNIFIED memory absorbed it,
        # and `gpu_peak_gb` read 51.88. The same config OOM'd an 80 GB A100 at
        # step 0 with `Triton Error [CUDA]: out of memory` while reporting a
        # 51.85 GB peak — the peak counter never sees what the allocator could
        # not get. THE HALO'S PEAK IS NOT A VRAM FLOOR FOR DISCRETE CARDS.
        #
        # Liger fuses the LM head with the loss so full logits are never
        # materialised. That is the fix; a bigger card is not.
        cfg_kwargs["use_liger_kernel"] = True
    # TRL has churned on this field name; keep both spellings working.
    sig = inspect.signature(ORPOConfig.__init__).parameters
    dropped = sorted(set(cfg_kwargs) - set(sig))
    cfg_kwargs = {k: v for k, v in cfg_kwargs.items() if k in sig}
    cfg = ORPOConfig(**cfg_kwargs)

    # THE SIGNATURE FILTER ABOVE IS A SILENT DROPPER. It exists so a TRL rename
    # does not crash the run, but that means a setting can vanish and the run
    # still exits 0 having trained something OTHER than what was asked for —
    # and the trace would look like the fix simply did not work. Name what was
    # dropped, and REFUSE if the one the operator explicitly asked for is gone.
    if dropped:
        print(f"NOTE: ORPOConfig does not accept {dropped} on this TRL — dropped.")
    # A DROPPED TRUNCATION BOUND IS FATAL, NOT A NOTE. Everything else in
    # cfg_kwargs degrades gracefully when a TRL rename eats it; these do not.
    # Losing a length bound does not make the run worse, it makes the run a
    # DIFFERENT run — unbounded sequences, quadratic memory, and (measured
    # 2026-08-04) an OOM at step 0 on an 80 GB card from a config that reads
    # `--seq-len 4096`. The NOTE above was printed, truthfully, for months and
    # nobody read it as "your truncation policy does not exist here".
    TRUNCATION_BOUNDS = {"max_length", "max_completion_length"}
    lost_bounds = TRUNCATION_BOUNDS.intersection(dropped)
    if lost_bounds:
        sys.exit(
            f"FATAL: this TRL's ORPOConfig has no {sorted(lost_bounds)}. Those "
            f"are TRUNCATION BOUNDS — without them sequences are unbounded and "
            f"--seq-len {args.seq_len} is decorative. Find this version's "
            f"equivalent field and set it; do not run unbounded. "
            f"(Fields it does have: "
            f"{sorted(x for x in sig if 'length' in x or 'trunc' in x)})")
    # REFUSE rather than silently train without it (§18.3). --liger is asked for
    # because the run does not FIT otherwise; silently dropping it turns "the
    # memory fix is not supported here" into "the memory fix did not work", and
    # the operator debugs the wrong thing. Two ways it can vanish — the package
    # is absent, or this TRL has no such field — and both are named.
    if args.liger:
        import importlib.util as _ilu

        if _ilu.find_spec("liger_kernel") is None:
            sys.exit("FATAL: --liger requested but liger_kernel is not installed. "
                     "pip install liger-kernel  (it is what keeps 248k-vocab "
                     "logits from being materialised; without it the 4B does "
                     "not fit 80 GB at seq 4096).")
        if "use_liger_kernel" not in sig:
            sys.exit("FATAL: --liger requested but this TRL's ORPOConfig has no "
                     f"`use_liger_kernel` field (trl fields checked: {len(sig)}). "
                     "Refusing to run unfused — it will OOM on a discrete card.")
    # Bucketing rides on LengthGroupedSampler. If that import is gone, training
    # would silently run UNBUCKETED and the trace would read as "the fix did not
    # work" rather than "the fix never ran" — four verdicts, not two (§18.1).
    if args.group_by_length:
        try:
            from transformers.trainer_pt_utils import LengthGroupedSampler  # noqa: F401
        except ImportError as e:
            print(f"FATAL: --group-by-length needs transformers' "
                  f"LengthGroupedSampler and it is unavailable ({e}). Re-run "
                  f"with --no-group-by-length to train unbucketed on purpose.",
                  file=sys.stderr)
            return 2

    # -- per-step instrumentation ------------------------------------------
    class StepTimer(TrainerCallback):
        def __init__(self) -> None:
            self.last = None
            self.durations: list[float] = []
            # APPEND when resuming. Opening "w" on a resumed leg silently
            # truncates the previous leg's trace, so a 19-hour run paused three
            # times would end with only its last leg measured — and the memory
            # trajectory the trace exists to show is exactly the thing that
            # spans legs.
            self.fh = open(steps_path, "a" if args.resume else "w")
            self.t0 = time.monotonic()
            self.gtt_over = 0  # consecutive over-limit samples; see the tripwire

            # ARM THE TRIPWIRE OUT LOUD, ONCE, BEFORE STEP 1 (§18.1: four
            # verdicts, not two). Three of the four states are reachable here
            # and each prints a distinguishable line, because the failure this
            # replaces was the fourth — NEVER-RAN, indistinguishable from
            # passing, discovered only by reading the source.
            r0 = mem_reading(torch)
            self.mem_source = r0.source
            self.run_peak = 0.0  # run-wide max of the per-step allocated peak
            self.mem_limit, why = resolve_mem_limit(
                args.mem_limit_gb, r0.total, self.mem_source)
            if self.mem_limit == self.mem_limit:
                print(f"  memory tripwire ARMED: pool limit {self.mem_limit:.1f}GB "
                      f"({why}); source={self.mem_source}; judges THIS PROCESS's "
                      f"demand against that limit minus whatever a co-tenant "
                      f"holds (unattributed at arming: {r0.unattributed:.1f}GB, "
                      f"ceiling {demand_ceiling(self.mem_limit, r0.unattributed):.1f}GB); "
                      f"aborts after {args.mem_limit_consecutive} consecutive "
                      f"samples over",
                      flush=True)
            else:
                # Refuse to imply safety we cannot provide. The run continues —
                # an unguarded run is a decision the operator may legitimately
                # make — but it can never again be MISTAKEN for a guarded one.
                print(f"  memory tripwire NEVER-RAN: {why}. This run is "
                      f"UNGUARDED against memory exhaustion; an OOM here loses "
                      f"the adapter, the timings and the log.",
                      file=sys.stderr, flush=True)

        def on_step_end(self, targs, state, control, **kw):  # noqa: ANN001
            now = time.monotonic()
            if self.last is not None:
                self.durations.append(now - self.last)
            self.last = now
            # The mitigation named as untested in M0_PROBE_HALO.md:79. Runs
            # BEFORE the memory sample so the sample reports the post-reclaim
            # figure — otherwise the trace measures the thing we just released.
            if args.empty_cache_every and state.global_step % args.empty_cache_every == 0:
                torch.cuda.empty_cache()

            alloc, peak, reserved = gpu_mem_gb(torch)
            rss = host_rss_gb()
            mem = mem_reading(torch)
            self.run_peak = max(self.run_peak, peak) if peak == peak else self.run_peak
            # What OUR process may demand: the pool's limit less whatever we
            # cannot attribute to ourselves. Recomputed EVERY step, not once at
            # arming, because a co-tenant landing on a rented card mid-run is
            # precisely the event this guard exists to catch.
            ceiling = demand_ceiling(self.mem_limit, mem.unattributed)
            # Counted BEFORE the record is written, so the streak is in
            # steps.jsonl. A guard whose state is invisible in the trace cannot
            # be debugged after the fact -- which is how arm A's abort came to
            # be read as a memory leak.
            #
            # See over_limit(): NaN on either side is could-not-judge, reported
            # once at arming time, never silently read as "under limit".
            over = over_limit(mem.ours, ceiling)
            self.gtt_over = self.gtt_over + 1 if over else 0
            rec = {
                "step": state.global_step,
                "elapsed_s": round(now - self.t0, 3),
                "step_s": round(self.durations[-1], 3) if self.durations else None,
                "loss": (state.log_history[-1].get("loss")
                         if state.log_history else None),
                "rss_gb": round(rss, 2),
                "gpu_alloc_gb": round(alloc, 2),
                # Run-wide max of allocated, unchanged in meaning from every
                # archived trace — but now accumulated in Python, because the
                # torch counter behind it is reset each step (below) to make
                # gpu_step_peak_gb a real per-step figure.
                "gpu_peak_gb": round(self.run_peak, 2),
                "gpu_step_peak_gb": round(peak, 2),
                "gpu_reserved_gb": round(reserved, 2),  # see gpu_mem_gb: THE column
                # Keeps the historic key name so every archived amdgpu trace and
                # the summary's _series("gtt_gb") stay byte-comparable. On CUDA
                # it carries device-wide used VRAM — same meaning ("how full is
                # the pool"), different pool. mem_source is what tells the two
                # apart; a trace that does not say which pool it measured is not
                # glassbox. NOTE this is NOT what the guard judges any more —
                # mem_ours_gb is. See MemReading.
                "gtt_gb": round(mem.pool_used, 2),       # whole device / whole box
                "mem_ours_gb": round(mem.ours, 2),       # THE judged number
                "mem_unattributed_gb": round(mem.unattributed, 2),  # co-tenants
                "mem_ceiling_gb": round(ceiling, 2) if ceiling == ceiling else None,
                "mem_total_gb": round(mem.total, 2),
                "mem_source": self.mem_source,
                "mem_limit_gb": self.mem_limit,
                "proc_gtt_gb": round(proc_gtt_gb(), 2),  # amdgpu only; NaN on CUDA
                "gtt_over_consecutive": self.gtt_over,
            }
            self.fh.write(json.dumps(rec) + "\n")
            self.fh.flush()
            # Close the measurement window so the NEXT sample's peak is that
            # step's, not the run's. Without this the judged number ratchets
            # monotonically and "N consecutive samples over" degrades into "N
            # steps after the first spike, ever" — which is the opposite of the
            # transient-vs-runaway distinction the streak exists to draw.
            if torch.cuda.is_available():
                torch.cuda.reset_peak_memory_stats()
            if state.global_step % 5 == 0 or state.global_step <= 3:
                s = rec["step_s"]
                print(f"  step {rec['step']:4d}  {s if s is None else f'{s:6.2f}'}s/it"
                      f"  loss={rec['loss']}  rss={rec['rss_gb']}GB"
                      f"  gpu={rec['gpu_alloc_gb']}/{rec['gpu_step_peak_gb']}GB"
                      f"  reserved={rec['gpu_reserved_gb']}GB"
                      f"  ours={rec['mem_ours_gb']}/{rec['mem_ceiling_gb']}GB",
                      flush=True)
            if rss == rss and rss > args.rss_limit_gb:
                print(f"\nABORT: RSS {rss:.1f}GB exceeded --rss-limit-gb "
                      f"{args.rss_limit_gb}. This tripwire exists because a "
                      f"four-job pileup locked the machine on 2026-07-29.",
                      file=sys.stderr)
                control.should_training_stop = True

            # THE TRIPWIRE THAT MATTERS ON THIS BOX. The RSS one above cannot
            # fire on the failure we actually have: at M0's death host RSS was
            # under 1 GB while GTT stood at 100.7 GB. Unified memory means GTT
            # pressure is what SIGKILLs the trainer and what has twice taken the
            # desktop compositor with it, so the guard has to watch GTT — and
            # BOX GTT, not this process's, because a co-tenant's model load
            # counts against the same 125 GB.
            # ONE SAMPLE IS NOT A MEASUREMENT (ARCH_PRINCIPLES §18.5). Arm A was
            # killed at step 118 by a SINGLE instantaneous reading, with the
            # limit set at 95 GB -- BELOW this workload's ordinary transient of
            # ~95-100 GB (roughly 3x alloc_peak). That abort was certain to fire
            # eventually; surviving to 118 was luck, and it cost the run. A guard
            # that trips on normal operation is not a safety net, it is a
            # scheduled failure. So require N CONSECUTIVE over-limit samples: a
            # real runaway stays over, a transient does not.
            if over and self.gtt_over < args.mem_limit_consecutive:
                # Visible BEFORE it fires -- the operator sees pressure building
                # rather than only learning about it from the abort.
                print(f"  WARN: our demand {mem.ours:.1f}GB over ceiling "
                      f"{ceiling:.1f}GB (--mem-limit-gb {self.mem_limit} less "
                      f"{mem.unattributed:.1f}GB unattributed) for "
                      f"{self.gtt_over} consecutive sample(s); aborting at "
                      f"{args.mem_limit_consecutive}.", flush=True)
            if self.gtt_over >= args.mem_limit_consecutive:
                # EVERY NUMBER HERE IS ATTRIBUTED. The message this replaces
                # ended "this process holds nanGB of it", and that nan was the
                # visible tell of the bug that cost two cheap-tier probes: the
                # guard was aborting on a total it could not apportion. If you
                # are reading this after an abort, the question to ask first is
                # which of the three numbers below is the surprising one.
                print(f"\nABORT: this process demanded {mem.ours:.1f}GB, over "
                      f"its {ceiling:.1f}GB ceiling, on {self.gtt_over} "
                      f"CONSECUTIVE samples. Ceiling = --mem-limit-gb "
                      f"{self.mem_limit} less {mem.unattributed:.1f}GB held by "
                      f"someone else on this {mem.total:.1f}GB pool "
                      f"({self.mem_source}); our allocator additionally has "
                      f"{mem.reserved:.1f}GB reserved, which is CACHE, not "
                      f"demand, and is not what this fired on. Stopping cleanly "
                      f"so the adapter and timings survive — an OOM SIGKILL "
                      f"would take both, and the graphical session with them.",
                      file=sys.stderr)
                control.should_training_stop = True

            # The planned stop, checked LAST so a tripwire abort on the same
            # step still reports its own reason. This is a normal end of run,
            # not a failure: the exit code stays 0 and the adapter is saved by
            # the same path a full run uses.
            if args.stop_at_step and state.global_step >= args.stop_at_step:
                print(f"\nSTOP-AT-STEP: reached step {state.global_step} of the "
                      f"{args.iters}-step schedule (--stop-at-step "
                      f"{args.stop_at_step}). Stopping cleanly. The scheduler "
                      f"horizon was NOT shortened, so this checkpoint sits at "
                      f"the same LR-schedule position a full {args.iters}-step "
                      f"run passes through here.", flush=True)
                control.should_training_stop = True
            return control

    timer = StepTimer()

    trainer_kwargs = dict(
        model=model,
        args=cfg,
        train_dataset=ds,
        callbacks=[timer],
    )
    if peft_config is not None:
        trainer_kwargs["peft_config"] = peft_config
    tsig = inspect.signature(ORPOTrainer.__init__).parameters
    trainer_kwargs["processing_class" if "processing_class" in tsig else "tokenizer"] = tokenizer

    # LENGTH BUCKETING GOES THROUGH THE SAMPLER, NOT THE CONFIG. transformers
    # 5.14's ORPOConfig has `length_column_name` but NO `group_by_length` (123
    # params, checked — not recalled), so setting it via the config is a no-op
    # the signature filter drops on the floor. Overriding the sampler hook does
    # the same job and does not depend on a flag TRL may or may not expose.
    #
    # WHY IT HELPS AT MICRO-BATCH 1, where there is no intra-batch padding to
    # save: LengthGroupedSampler shuffles, chunks into megabatches, and sorts
    # WITHIN each chunk, so CONSECUTIVE STEPS see similar sequence lengths. That
    # is what lets torch's caching allocator reuse blocks instead of reserving a
    # new segment per novel shape — the mechanism the probe isolated (varying
    # shapes reserved 82.88 GB, one fixed shape 37.91 GB and flat).
    trainer_cls = ORPOTrainer
    if args.group_by_length:
        from transformers.trainer_pt_utils import LengthGroupedSampler

        _lens = list(ds[LENGTH_COL])

        class LengthBucketedORPOTrainer(ORPOTrainer):
            def _get_train_sampler(self, train_dataset=None):  # noqa: ANN001
                # Announced, because a sampler that silently failed to engage
                # would look exactly like "bucketing did not help".
                print(f"  sampler: LengthGroupedSampler over {len(_lens)} rows "
                      f"(batch_size={cfg.per_device_train_batch_size})", flush=True)
                return LengthGroupedSampler(
                    batch_size=cfg.per_device_train_batch_size,
                    lengths=_lens,
                )

        trainer_cls = LengthBucketedORPOTrainer

    # THE INSTRUMENT FOR THE RESUME QUESTION, which was inferred and never
    # observed. transformers 5.14 DOES fast-forward the dataloader past
    # consumed batches -- trainer.py:1690 calls `skip_first_batches`, guarded
    # by `ignore_data_skip`, which defaults False (training_args.py:1227). But
    # it announces that through `logger.info` (trainer.py:1496), which is
    # invisible at our verbosity, so the line was looked for and not found and
    # the behaviour was written down as unverified.
    #
    # Reading the source settles the code path; it does NOT settle what this
    # dataset and sampler actually hand back. A leg that correctly skipped
    # 3,744 rows and one that silently re-fed them from row 0 produce identical
    # logs today. The fingerprint distinguishes them: if the resumed leg prints
    # the SAME value as the leg that started from step 0, the skip did not
    # happen and every pause is repeating data.
    class _FirstBatchFingerprint:
        _fp_printed = False

        def training_step(self, model, inputs, *a, **kw):  # noqa: ANN001
            if not self._fp_printed:
                self._fp_printed = True
                print(f"  first batch this leg: {_batch_fingerprint(inputs)}",
                      flush=True)
                # ASSERT THE BOUND, DO NOT JUST PRINT IT. This line printed
                # `chosen_input_ids(1, 6410)` on a run configured --seq-len
                # 4096 and was read as informational for the whole session; the
                # only other symptom was an OOM three minutes later that looked
                # like "the GPU is too small". Length bucketing sorts
                # longest-first, so THE FIRST BATCH IS THE DATASET MAXIMUM —
                # which makes this the cheapest possible place to catch an
                # unbounded sequence, before a single expensive forward.
                over = {k: tuple(v.shape) for k, v in inputs.items()
                        if k.endswith("input_ids") and hasattr(v, "shape")
                        and v.shape[-1] > args.seq_len}
                if over:
                    print(f"\nABORT: batch exceeds --seq-len {args.seq_len}: "
                          f"{over}. Truncation is NOT being applied — check that "
                          f"this TRL's length bounds are set (max_length and "
                          f"max_completion_length) and were not dropped by the "
                          f"signature filter. Training on longer sequences than "
                          f"configured silently changes the memory profile and "
                          f"the experiment.", file=sys.stderr, flush=True)
                    raise RuntimeError(
                        f"batch length {max(s[-1] for s in over.values())} "
                        f"exceeds configured --seq-len {args.seq_len}")
            return super().training_step(model, inputs, *a, **kw)

    trainer_cls = type("InstrumentedORPOTrainer",
                       (_FirstBatchFingerprint, trainer_cls), {})

    trainer = trainer_cls(**trainer_kwargs)

    adapted = [n for n, _ in trainer.model.named_parameters() if "lora_" in n]
    print(f"LoRA tensors: {len(adapted)}  "
          f"(Mac probe adapted 186 modules across linear_attn + self_attn)")

    # -- train --------------------------------------------------------------
    print(f"\n=== training: {args.iters} steps, effective batch "
          f"{args.batch_size * args.grad_accum}, seq {args.seq_len} ===", flush=True)
    t_train = time.monotonic()
    status = "ok"

    # RESUME IS EITHER REAL OR IT IS AN ERROR — never a silent restart (§18.3).
    # `resume_from_checkpoint=True` raises if none exists, but the operator
    # deserves the step number BEFORE a 19-hour run, not a stack trace.
    resume_from = None
    if args.resume:
        ckpt_root = args.out / "hf"
        ckpts = sorted(ckpt_root.glob("checkpoint-*"),
                       key=lambda p: int(p.name.split("-")[-1])) if ckpt_root.exists() else []
        if not ckpts:
            print(f"FATAL: --resume was given but no checkpoint exists under "
                  f"{ckpt_root}. Drop --resume to start from step 0 deliberately.",
                  file=sys.stderr)
            return 2
        resume_from = str(ckpts[-1])
        done = int(ckpts[-1].name.split("-")[-1])
        print(f"RESUMING from {ckpts[-1].name}: {done} of {args.iters} steps "
              f"already done, {args.iters - done} to go.", flush=True)

    try:
        trainer.train(resume_from_checkpoint=resume_from)
    except KeyboardInterrupt:
        # A pause is a first-class outcome, not a crash. The adapter save and
        # the gate below run regardless of `status`, so a Ctrl-C keeps the
        # weights AND leaves a checkpoint to resume from.
        status = "interrupted"
        print("\nPAUSED (KeyboardInterrupt). Resume with the same command "
              "plus --resume.", file=sys.stderr)
    except Exception as exc:  # noqa: BLE001 - we want the partial timings
        status = f"error: {type(exc).__name__}: {exc}"
        print(f"\nTRAINING FAILED: {status}", file=sys.stderr)
    train_s = time.monotonic() - t_train
    timer.fh.close()

    # -- adapter save + gradient gate ---------------------------------------
    # UNCONDITIONAL, and it runs even when `status != "ok"`: a run that died at
    # step 63 still answers "did gradients ever reach the weights?", and that is
    # the question four days of Mac runs never got asked. An adapter is ~80 MB;
    # the throughput cost is one save. See HALO_HANDOFF_2026-08-02.md §4.
    adapter_dir = args.out / "adapter"
    try:
        trainer.model.save_pretrained(str(adapter_dir))
        verdict = scan(adapter_dir)
    except Exception as exc:  # noqa: BLE001 - never lose the timings over a save
        verdict = {"saved": False, "verdict": "unusable", "trained": None,
                   "error": f"{type(exc).__name__}: {exc}"}
        print(f"\nADAPTER SAVE FAILED: {verdict['error']}", file=sys.stderr)

    # -- summary ------------------------------------------------------------
    step_recs = [json.loads(l) for l in steps_path.read_text().splitlines()]

    def _series(key: str) -> list[float]:
        return [v for v in (r.get(key) for r in step_recs)
                if v is not None and v == v]  # drop nulls and NaN

    # The attributed series — what WE held, on either platform. mem_ours_gb is
    # the current key; the two fallbacks read archived traces written before
    # 2026-08-05 (amdgpu-attributed, then unattributed) so a re-summarised old
    # run still reports the same series it always did.
    gtt_series = (_series("mem_ours_gb") or _series("proc_gtt_gb")
                  or _series("gtt_gb"))
    box_series = _series("gtt_gb")
    d = timer.durations
    # The first optimizer step carries compile/warmup cost; report both so the
    # steady-state number cannot be quietly confused with the overall one.
    steady = d[1:] if len(d) > 1 else d
    summary = {
        "status": status,
        "run": str(args.out),
        "backend": "unsloth" if args.unsloth else "trl-vanilla",
        "environment": env,
        "config": {
            "model": args.model,
            "data": str(args.data),
            "train_rows": len(ds),
            # Rows the length filter removed. Non-zero means the effective
            # dataset is NOT the one the manifest names, so it belongs in the
            # run manifest, not just in the log.
            "rows_dropped_over_seq_len": dropped_rows,
            "iters_requested": args.iters,
            # The schedule horizon and the stop are separate facts. Recording
            # only steps_timed leaves a reader unable to tell a planned stop
            # from a tripwire abort from a crash — and that distinction is the
            # whole basis of comparing two arms cut at the same step.
            "stop_at_step": args.stop_at_step or None,
            "group_by_length": args.group_by_length,
            # BOTH, because they differ by one and the difference has already
            # cost a wrong plan. `steps_timed` counts INTER-STEP DURATIONS, so
            # it is always one short: step 1 has no predecessor to time against.
            # Arm A reported steps_timed 117 and had trained 118 optimizer
            # steps, and "arm A was cut at 117" was carried into a session frame
            # and used to size a matching run. The step a checkpoint actually
            # sits at is the last one in the trace.
            "steps_completed": (step_recs[-1]["step"] if step_recs else 0),
            "steps_timed": len(d),
            "batch_size": args.batch_size,
            "grad_accum": args.grad_accum,
            "effective_batch": args.batch_size * args.grad_accum,
            "seq_len": args.seq_len,
            "lr": args.lr,
            "lora_r": args.lora_r,
            "lora_alpha": args.lora_alpha,
            "beta": args.beta,
            "dtype": args.dtype,
            "attn": args.attn,
            "grad_checkpointing": args.grad_checkpointing,
            "architecture": arch,
            "lora_tensors": len(adapted),
            "adapted_modules": len(targets),
            "adapted_by_leaf": breakdown,
        },
        "timing": {
            "model_load_s": round(load_s, 1),
            "train_wall_s": round(train_s, 1),
            "s_per_it_overall": round(train_s / max(len(d), 1), 2),
            "s_per_it_mean_steady": round(statistics.mean(steady), 2) if steady else None,
            "s_per_it_median_steady": round(statistics.median(steady), 2) if steady else None,
            "s_per_it_min": round(min(d), 2) if d else None,
            "s_per_it_max": round(max(d), 2) if d else None,
            "first_step_s": round(d[0], 2) if d else None,
        },
        "memory": {
            "host_rss_peak_gb": round(max(
                (r["rss_gb"] for r in step_recs), default=float("nan")), 2),
            # From the TRACE, not from torch's counter: that counter is reset
            # every step (see the step callback) so reading it here would report
            # the last step's peak as the run's.
            "gpu_peak_gb": round(max(_series("gpu_peak_gb"), default=float("nan")), 2),
            # What our allocator held vs what it actually needed. A large gap is
            # normal caching behaviour on a big card and is NOT a leak — the
            # guard aborted two paid probes for confusing the two (MemReading).
            "gpu_reserved_peak_gb": round(
                max(_series("gpu_reserved_gb"), default=float("nan")), 2),
            "mem_unattributed_peak_gb": round(
                max(_series("mem_unattributed_gb"), default=float("nan")), 2),
            # GTT first/last/peak/slope, because the FAILURE MODE on this box is
            # a SLOPE, not a level. A single peak cannot distinguish "held 25 GB
            # steadily" from "ratcheted 25 -> 103 and got SIGKILLed", and those
            # are the two outcomes we actually need to tell apart. These are
            # THIS PROCESS's GTT; box_gtt_peak_gb is the whole machine, and a
            # gap between them is a co-tenant, not a leak.
            "gtt_first_gb": gtt_series[0] if gtt_series else None,
            "gtt_last_gb": gtt_series[-1] if gtt_series else None,
            "gtt_peak_gb": max(gtt_series) if gtt_series else None,
            # FLOOR AND ENVELOPE, NOT A SINGLE SLOPE. Until 2026-08-03 this
            # reported `gtt_growth_mb_per_step` as (last - first)/steps, and on
            # arm A that printed 847.9 MB/step -- which reads as a steady leak
            # and is not what the series does. The run's LAST sample was its
            # single worst excursion (101.97 GB, the one the tripwire caught),
            # so the estimator was measuring one spike and dividing it across
            # 117 steps. The floor never moved: arm A's per-20-step minimum sat
            # between 4.4 and 6.3 GB from step 1 to step 118 while its maximum
            # climbed 37 -> 60 -> 80 -> 102. A leak raises the FLOOR; growing
            # transients raise only the ENVELOPE, and only the second one is
            # fixed by length bucketing. One number could not tell them apart,
            # so report both.
            "gtt_floor_by_decile_gb": _decile(gtt_series, min),
            "gtt_envelope_by_decile_gb": _decile(gtt_series, max),
            "box_gtt_peak_gb": max(box_series) if box_series else None,
            "empty_cache_every": args.empty_cache_every,
        },
        "adapter": verdict,
    }
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")

    print("\n=== summary ===")
    print(json.dumps(summary["timing"], indent=2))
    print(json.dumps(summary["memory"], indent=2))
    print(json.dumps(summary["adapter"], indent=2))
    print(f"\nwrote {summary_path}\nwrote {steps_path}")

    # The gate, stated where nobody can miss it. A timing number from a run that
    # did not train is not a slower measurement — it is not a measurement.
    if verdict.get("verdict") == TRAINED:
        print(f"\nGATE PASS -- adapter TRAINED, max|B| {verdict['max_abs_b']:.6e} "
              f"({verdict['b_nonzero']}/{verdict['lora_b_tensors']} B tensors nonzero)")
    else:
        print("\nGATE FAIL\n" + report(verdict), file=sys.stderr)
        # Distinct codes: a diverged run needs an LR change, a not-trained run
        # needs a framework fix. A caller must be able to tell them apart.
        return 4 if verdict.get("verdict") == DIVERGED else 3

    if status != "ok":
        return 1

    # Sizing extrapolation, stated in the same shape as M0_PROBE.md's table.
    sit = summary["timing"]["s_per_it_median_steady"] or summary["timing"]["s_per_it_overall"]
    eff = args.batch_size * args.grad_accum
    # Real epoch sizes, from the manifests — not estimates. Stream A train is
    # data/orpo-76k (74,674 after the 1k/1k valid/test split); A+B train is
    # data/orpo-ab (93,693 = 76,674 A + 19,019 B, 19.9% B, built 2026-08-01).
    for name, rows in (("Stream A (orpo-76k train)", 74674),
                       ("A+B (orpo-ab train)", 93693)):
        iters = rows / eff
        print(f"  {name}: {iters:,.0f} iters/epoch x {sit}s = "
              f"{iters * sit / 3600:.1f} h/epoch")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
