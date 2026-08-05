#!/usr/bin/env python3
"""Gate a rented GPU pod BEFORE any paid training runs on it.

Run: python cloud/preflight.py [--data DIR] [--model DIR] [--json OUT]
Exit 0 = the pod is fit to train. Any other exit = do not start the run.

WHY THIS EXISTS AND WHY IT IS A HARD GATE. Every failure mode this checks for
is one that does NOT stop a training run — it degrades it, silently, and is
discovered hours later as a bad number that looks like a hardware result:

  - no gcc          -> Triton cannot JIT its launcher stub -> fla silently
                       falls back, transformers logs "Triton is not supported
                       on current platform", and the 18 gated-deltanet layers
                       run eager-torch at ~1.3x the step time. The run
                       SUCCEEDS. Its s/it is a lie about the hardware.
  - fla missing     -> same outcome, different cause.
  - causal_conv1d   -> flips the deltanet path to CHUNKED: ~100 GB of
      PRESENT      intermediates at seq 4096 and a SIGKILL partway in.
  - sm not in       -> torch runs but every kernel goes through PTX JIT or
      arch list        fails outright; on Blackwell this is the live risk.

Reporting a degraded run's throughput as "the A100 number" and sizing a
multi-day campaign from it is exactly the failure ARCH_PRINCIPLES §18.4 names:
validate the instrument before the result. The instrument here is the pod.

FOUR VERDICTS, NOT TWO (§18.1). Each check reports PASS, FAIL, or SKIP with a
reason. A check that could not run is never counted as a pass, and the summary
line names how many of each. `--allow-degraded` downgrades the fla/gcc checks
from FAIL to a loud warning, for the specific case of deliberately measuring
the eager-torch path — it is not a way to get a green light.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

PASS, FAIL, SKIP = "PASS", "FAIL", "SKIP"
results: list[dict] = []


def record(name: str, verdict: str, detail: str, **extra) -> None:
    results.append({"check": name, "verdict": verdict, "detail": detail, **extra})
    mark = {PASS: "  ok  ", FAIL: "  FAIL", SKIP: "  skip"}[verdict]
    print(f"{mark}  {name}: {detail}", flush=True)


# --------------------------------------------------------------------------
# checks
# --------------------------------------------------------------------------
def check_gpu(torch) -> dict:
    """Name, VRAM, compute capability — and whether this torch build can
    actually target it. `get_arch_list()` is the check that matters: a torch
    wheel compiled without sm_120 will still import, still report the GPU, and
    then fail or PTX-JIT every kernel."""
    info: dict = {}
    if not torch.cuda.is_available():
        record("gpu.available", FAIL, "torch.cuda.is_available() is False — "
               "no usable GPU; nothing below can be judged")
        return info
    props = torch.cuda.get_device_properties(0)
    cap = torch.cuda.get_device_capability(0)
    sm = f"sm_{cap[0]}{cap[1]}"
    arches = torch.cuda.get_arch_list()
    info = {
        "name": torch.cuda.get_device_name(0),
        "total_gb": round(props.total_memory / 1024**3, 2),
        "capability": sm,
        "arch_list": arches,
        "torch": torch.__version__,
        "cuda": torch.version.cuda,
        "device_count": torch.cuda.device_count(),
    }
    # Prefixed: `info` carries a "name" key and record()'s first parameter is
    # also `name`. Splatting it raw is a TypeError, not a wrong value — but
    # only because the collision happens to be with a positional.
    record("gpu.available", PASS,
           f"{info['name']} {info['total_gb']}GB {sm} "
           f"(torch {info['torch']} / CUDA {info['cuda']})",
           **{f"gpu_{k}": v for k, v in info.items()})

    # ROCm and CUDA name architectures differently and torch reports BOTH
    # through the same API: `get_device_capability` returns a CUDA-shaped
    # (major, minor) even on AMD, where the real identity is `gcnArchName`
    # (gfx1151). Comparing the CUDA-shaped name against a gfx arch list
    # produces a confident, wrong FAIL — which is what this check did on its
    # first run against the Halo. The arch identity has to come from the same
    # vocabulary as the list.
    is_rocm = getattr(torch.version, "hip", None) is not None
    arch_id = getattr(props, "gcnArchName", None) if is_rocm else sm
    info["arch_id"] = arch_id
    info["backend"] = "rocm" if is_rocm else "cuda"
    if arch_id and any(a.split(":")[0] == str(arch_id).split(":")[0] for a in arches):
        record("gpu.arch_supported", PASS,
               f"{arch_id} is in this torch's arch list ({info['backend']})")
    elif arch_id is None:
        record("gpu.arch_supported", SKIP,
               "could not read an architecture id from this device")
    else:
        # Not automatically fatal on CUDA: it can PTX-JIT forward from a lower
        # arch if the list contains a compatible one. But it is slow and it is
        # the single most likely way a Blackwell box quietly underperforms, so
        # it is reported as a failure and the operator decides.
        record("gpu.arch_supported", FAIL,
               f"{arch_id} NOT in torch arch list {arches} — kernels will JIT "
               f"or fail; any s/it measured here is not this GPU's number")
    return info


def check_vram_floor(info: dict, floor_gb: float) -> None:
    """A card below the floor OOMs after the model loads and minutes of paid time.

    THE FLOOR MOVED, AND WHY IT MOVED MATTERS (2026-08-05). It was 52 GB, from
    the Halo's 51.88 GB peak. That peak was measured on OVER-LENGTH sequences:
    TRL 1.9.2's ORPO cannot bound a long prompt, so rows reached the model at up
    to 6410 tokens while the config said 4096. Every Halo memory figure predates
    that fix and is therefore an overestimate of what seq 4096 actually costs.

    The first post-fix measurement, A100 SXM4 80GB, 25 steps, same recipe
    (`runs/probe-4b-cloud-m1`): **36.53 GB allocated peak**, 30% under the old
    floor. That reading is trustworthy in the direction we are using it —
    the run SUCCEEDED, so its allocated peak is a true lower bound on demand.
    Contrast the OOM'd run whose `gpu_peak_gb` read 51.85: a peak counter cannot
    see memory the allocator FAILED to get, so THAT number was never a floor and
    is why this docstring exists.

    THE FLOOR WAS 44, ONE PROBE APPEARED TO FALSIFY IT, AND THAT READING HAS
    SINCE BEEN WITHDRAWN. An RTX A6000 reading 44.43 GB passed at 44 and then
    ABORTED at step 4 of 25 — but the abort was OUR MEMORY TRIPWIRE FIRING ON
    ITS OWN ALLOCATOR CACHE, not an OOM. Device-wide reached 40.96 GB against a
    guard at 40.9 (92% of total) while ALLOCATED peak was 35.89 and torch's
    reserve was 40.49. No OOM was observed on that card or on the RTX PRO 5000
    that aborted the same way. Both were false positives; the guard now judges
    our own demand against the limit less any co-tenant (`demand_ceiling` in
    train_orpo_trl.py, and cloud/README.md for the three-card table).

    So the reasoning that set 46 — "demand is allocated PLUS the reserve PLUS
    the context, ~41 GB on that card" — IS WRONG. Reserve is elastic: it
    expands to fill a big card and torch frees cached blocks before it ever
    OOMs. Demand is 35.9-36.5 GB, measured identically on all three cards.

    47.27 GB IS NOW MEASURED, NOT PREDICTED. An RTX PRO 5000 completed all 25
    steps on 2026-08-05 (`runs/probe-4b-m1-rtx-pro-5000-46888730-expseg`,
    status ok, 38.51 s/it median, adapter gate PASS) — but ONLY with
    `PYTORCH_ALLOC_CONF=expandable_segments:True`. Without it the identical
    recipe on the identical pod OOM'd at step 16 on FRAGMENTATION, not demand:
    29.03 GB allocated, 15.23 GB reserved-but-unallocated, one 7.50 GB request
    it could not serve. `launch_arm.sh` now sets that on CUDA by default.

    SO THE BINDING CONSTRAINT ON A SMALL CARD IS FRAGMENTATION HEADROOM, NOT
    DEMAND, and this check cannot see it — it reads a device total. Steady
    demand is ~30 GB; the completed run peaked at 41.17 GB RESERVED against a
    47.27 GB card, so what a card actually needs is room for the allocator's
    working set, which expandable segments shrinks by ~4 GB.

    THE FLOOR STAYS 46 AND IS STILL PROBABLY TOO HIGH. Nothing below 47.27 has
    been measured under BOTH fixes. What moves it is one completed 25-step run
    on a 44-45 GB card with expandable segments on — the A6000 at 44.43 is the
    natural candidate, costs ~$0.40/hr, and its 2026-08-05 abort is no longer
    evidence against it (that was the guard's false positive). Until that run
    exists, lowering this substitutes an inference for a measurement (§18.4),
    which is the mistake that put 46 here in the first place.
    """
    total = info.get("total_gb")
    if total is None:
        record("gpu.vram_floor", SKIP, "no GPU to measure")
        return
    if total >= floor_gb:
        record("gpu.vram_floor", PASS,
               f"{total}GB >= {floor_gb}GB floor (A100 measured 36.53GB "
               f"ALLOCATED peak post-truncation-fix for the 4B at micro 1 x "
               f"accum 32 x seq 4096; floor carries headroom for fragmentation)")
    else:
        record("gpu.vram_floor", FAIL,
               f"{total}GB is BELOW the {floor_gb}GB floor — the 4B allocated "
               f"36.53GB at peak on an A100 under this exact config, and the "
               f"floor adds headroom because a smaller card's fragmentation is "
               f"not predictable from a run that had 79GB to play with")


def check_compiler() -> None:
    """Triton JITs a C launcher stub per kernel. No compiler, no fla."""
    cc = shutil.which("gcc") or shutil.which("cc")
    if cc:
        try:
            v = subprocess.run([cc, "--version"], capture_output=True,
                               text=True, timeout=20).stdout.splitlines()[0]
        except Exception:  # noqa: BLE001
            v = "(version unreadable)"
        record("triton.compiler", PASS, f"{cc} — {v}")
    else:
        record("triton.compiler", FAIL,
               "no gcc/cc on PATH — Triton cannot build launcher stubs, fla "
               "degrades to eager-torch and only WARNS about it")


_TRITON_PROBE = r"""
import sys, torch, triton, triton.language as tl

@triton.jit
def _add1(x_ptr, y_ptr, n, BLOCK: tl.constexpr):
    off = tl.program_id(0) * BLOCK + tl.arange(0, BLOCK)
    m = off < n
    tl.store(y_ptr + off, tl.load(x_ptr + off, mask=m) + 1.0, mask=m)

x = torch.zeros(256, device="cuda", dtype=torch.float32)
y = torch.empty_like(x)
_add1[(1,)](x, y, x.numel(), BLOCK=256)
torch.cuda.synchronize()
assert bool((y == 1.0).all().item()), "kernel ran but produced wrong values"
print(getattr(triton, "__version__", "?"))
"""


def check_triton_jit(torch) -> None:
    """Compile and run one real Triton kernel — IN A SUBPROCESS.

    An import check is not enough: `import triton` succeeds on a box with no
    compiler, and the JIT is where it actually fails. That failure is exactly
    what fla swallows into a warning.

    IN A SUBPROCESS because this probe can take the interpreter down, not just
    raise. Running it in-process SIGSEGV'd on the Halo (exit 139) and killed
    preflight partway through its own checklist — so the gate crashed instead
    of reporting, and the remaining checks never ran. A gate that dies on the
    thing it is probing is not a gate (§18.1). Isolated, a segfault becomes a
    FAIL verdict with the signal named, and preflight finishes its list.
    """
    if not torch.cuda.is_available():
        record("triton.jit", SKIP, "no GPU")
        return
    # A REAL FILE, not `python -c`. Triton's decorator reads the kernel's
    # source with inspect.getsource() and refuses a stdin/-c body outright:
    # "ValueError: @jit functions should be defined in a Python file". Probing
    # via -c therefore reports a Triton limitation as if it were a broken pod.
    import tempfile

    with tempfile.TemporaryDirectory() as td:
        probe = Path(td) / "triton_probe.py"
        probe.write_text(_TRITON_PROBE)
        try:
            p = subprocess.run([sys.executable, str(probe)],
                               capture_output=True, text=True, timeout=300)
        except subprocess.TimeoutExpired:
            record("triton.jit", FAIL, "kernel compile did not finish in 300s")
            return
    if p.returncode == 0:
        record("triton.jit", PASS,
               f"compiled and ran a kernel on {torch.cuda.get_device_name(0)} "
               f"(triton {p.stdout.strip().splitlines()[-1] if p.stdout.strip() else '?'})")
    elif p.returncode < 0:
        import signal
        sig = signal.Signals(-p.returncode).name
        record("triton.jit", FAIL,
               f"probe died on {sig} — the Triton/driver stack on this box "
               f"cannot compile a trivial kernel; fla will not work here")
    else:
        tail = (p.stderr.strip().splitlines() or ["(no stderr)"])[-1]
        record("triton.jit", FAIL, f"exit {p.returncode}: {tail}")


def check_deltanet_path(allow_degraded: bool) -> None:
    """Which memory path will Qwen3.5's 18 gated-deltanet layers take?

    transformers gates on `is_fast_path_available = all(...)` over four
    symbols. fla alone resolves three -> SEQUENTIAL (~25 GB, the proven path).
    Add causal-conv1d and all four resolve -> CHUNKED (~100 GB, SIGKILL).
    Neither -> eager-torch, ~1.3x slower.
    """
    import importlib.util as ilu

    has_fla = ilu.find_spec("fla") is not None
    has_ccc = ilu.find_spec("causal_conv1d") is not None

    if has_fla and has_ccc:
        record("deltanet.path", FAIL,
               "BOTH fla and causal_conv1d present -> CHUNKED path. Measured "
               "~100GB of intermediates and a SIGKILL at seq 4096. Uninstall "
               "causal-conv1d before training.", fla=True, causal_conv1d=True)
    elif has_fla:
        record("deltanet.path", PASS,
               "fla present, causal_conv1d absent -> SEQUENTIAL path (the one "
               "measured at 177.1 s/it on the 0.8B)", fla=True,
               causal_conv1d=False)
    else:
        record("deltanet.path", FAIL if not allow_degraded else SKIP,
               "fla ABSENT -> eager-torch path, ~1.3x slower. Any s/it "
               "measured here understates the GPU."
               + ("  [--allow-degraded: continuing anyway]" if allow_degraded else ""),
               fla=False, causal_conv1d=has_ccc)


def check_versions() -> None:
    """Pin drift is silent and makes a cloud run non-comparable to the Halo."""
    want = {
        "torch": "2.10.0", "transformers": "5.14.1", "trl": "1.9.2",
        "peft": "0.20.0", "datasets": "5.0.1", "accelerate": "1.14.0",
    }
    got, drift = {}, []
    for mod, expect in want.items():
        try:
            v = __import__(mod).__version__
        except Exception as exc:  # noqa: BLE001
            record(f"version.{mod}", FAIL, f"not importable: {exc}")
            continue
        got[mod] = v
        # torch carries a local version (+cu128 / +rocm7.0); compare the base.
        if v.split("+")[0] != expect:
            drift.append(f"{mod} {v} != {expect}")
    if drift:
        record("version.match_halo", FAIL,
               "stack differs from the Halo run: " + "; ".join(drift)
               + " — s/it is then a stack comparison, not a hardware one", **got)
    elif got:
        record("version.match_halo", PASS,
               "stack matches the Halo run exactly; the only difference is the "
               "accelerator", **got)


def check_payload(data: Path, model: Path) -> None:
    """The run's inputs. Cheap to check, expensive to discover missing after
    the model has loaded."""
    for label, path, needed in (
        ("payload.model", model, ["config.json", "tokenizer.json"]),
        ("payload.data", data, ["train.jsonl", "valid.jsonl"]),
    ):
        if not path.is_dir():
            record(label, FAIL, f"{path} is not a directory")
            continue
        missing = [f for f in needed if not (path / f).exists()]
        if missing:
            record(label, FAIL, f"{path} missing {missing}")
            continue
        size = sum(f.stat().st_size for f in path.rglob("*") if f.is_file())
        extra: dict = {"path": str(path), "bytes": size}
        if label == "payload.data":
            train = path / "train.jsonl"
            with open(train, "rb") as fh:
                rows = sum(1 for _ in fh)
            extra["train_rows"] = rows
            # The bucketing cache keys on (size, mtime) of train.jsonl. Copying
            # without preserving mtime silently invalidates it and costs ~2 min
            # of re-tokenization on 76k rows — cheap, but it shows up as
            # unexplained startup time on a paid box, so say it out loud.
            st = train.stat()
            cache = path / f".lengths-{st.st_size}-{int(st.st_mtime)}.json"
            extra["length_cache"] = cache.name if cache.exists() else None
            record(label, PASS,
                   f"{rows} rows, {size/1e6:.0f}MB, length-cache "
                   + ("HIT" if cache.exists() else
                      "MISS (expect ~2 min re-tokenization; use rsync -t)"),
                   **extra)
        else:
            record(label, PASS, f"{size/1e9:.1f}GB at {path}", **extra)


# --------------------------------------------------------------------------
def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data", type=Path, default=Path("data/orpo-76k"))
    ap.add_argument("--model", type=Path, default=Path("models/Qwen3.5-4B"))
    ap.add_argument("--vram-floor-gb", type=float, default=46.0,
                    help="post-fix 4B allocated peak was 36.53 GB on an A100 "
                         "(seq 4096); 44 adds fragmentation headroom. Was 52, "
                         "from a Halo peak measured on over-length sequences.")
    ap.add_argument("--allow-degraded", action="store_true",
                    help="downgrade the fla check to a warning. For "
                         "deliberately measuring the eager-torch path ONLY.")
    ap.add_argument("--skip-payload", action="store_true",
                    help="check the machine only, before data is synced")
    ap.add_argument("--json", type=Path, help="write the manifest here")
    args = ap.parse_args()

    print("=== verifier-v0 pod preflight ===", flush=True)
    try:
        import torch
    except Exception as exc:  # noqa: BLE001
        record("torch.import", FAIL, f"{type(exc).__name__}: {exc}")
        torch = None

    info: dict = {}
    if torch is not None:
        info = check_gpu(torch)
        check_vram_floor(info, args.vram_floor_gb)
        check_compiler()
        check_triton_jit(torch)
        check_deltanet_path(args.allow_degraded)
        check_versions()
    if args.skip_payload:
        record("payload", SKIP, "--skip-payload")
    else:
        check_payload(args.data, args.model)

    n = {v: sum(1 for r in results if r["verdict"] == v) for v in (PASS, FAIL, SKIP)}
    manifest = {
        "gpu": info,
        "host": os.uname().nodename,
        "python": sys.version.split()[0],
        "checks": results,
        "counts": n,
        "verdict": "FIT" if n[FAIL] == 0 else "UNFIT",
    }
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(manifest, indent=2) + "\n")

    print()
    print(f"preflight: {n[PASS]} passed, {n[FAIL]} failed, {n[SKIP]} skipped "
          f"-> {manifest['verdict']}")
    if n[FAIL]:
        print("DO NOT START THE RUN. Each failure above degrades or kills a "
              "run without stopping it; the numbers it produces would not be "
              "about this GPU.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
