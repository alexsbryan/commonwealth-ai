#!/usr/bin/env python3
"""Tests for the memory tripwire's threshold and predicate.

Run: .venv/bin/python scripts/test_mem_tripwire.py   (exit 0 = pass)

WHY THIS EXISTS. Until 2026-08-04 the tripwire read amdgpu's sysfs GTT counter
directly and compared `box == box and box > limit`. On any non-amdgpu box that
read raises OSError, returns NaN, and the comparison short-circuits to False —
so the guard never fired, never said so, and a run's log was indistinguishable
from a guarded one. We were about to take that on a rented CUDA GPU for a
~20-hour paid run.

That is ARCH_PRINCIPLES §18.1's "a check with no failing input you can name"
and §18.3's "an Err collapsed into a success-shaped value" in one line of code.
The fix splits the decision in two — over_limit() answers "is it over?" and
returns False for could-not-judge, resolve_mem_limit() answers "what is the
limit and can it even be armed?" — so that the caller can report NEVER-RAN as
its own verdict rather than as a pass.

The cases below name the failing inputs the old code did not have.

2026-08-05 — THE SECOND BUG, AND WHY THE CARD TABLE IS IN HERE. Arming the
guard on CUDA exposed what it was arming: it judged DEVICE-WIDE used VRAM,
which is our demand plus our own allocator's cache plus anyone else's. On a
card small enough for the cache to fill it, the guard aborted on its own
reserve. Two paid cheap-tier probes died that way and no OOM was ever observed.
The tell was printed every time — "this process holds nanGB of it".

So the composition below is the real subject: resolve_mem_limit gives the
POOL's limit, demand_ceiling turns it into OURS, over_limit judges. Each of
the three measured cards is a case, run through BOTH the old and the new
decider, because a fix you cannot see reproduce the original failure is not a
fix you have watched work (§18.1).
"""
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import train_orpo_trl  # noqa: E402
from train_orpo_trl import (  # noqa: E402
    demand_ceiling, mem_reading, over_limit, resolve_mem_limit,
)

NAN = float("nan")
failures: list[str] = []


def check(name: str, got, want) -> None:
    ok = (got is want) or (got == want) or (
        isinstance(got, float) and isinstance(want, float)
        and math.isnan(got) and math.isnan(want))
    print(f"  {'ok  ' if ok else 'FAIL'}  {name}: got {got!r}, want {want!r}")
    if not ok:
        failures.append(name)


print("over_limit() — the predicate that silently passed on CUDA")
# The ordinary cases.
check("clearly over", over_limit(120.0, 112.0), True)
check("clearly under", over_limit(40.0, 112.0), False)
check("exactly at the limit is NOT over (strict >)", over_limit(112.0, 112.0), False)

# THE REGRESSION. Each of these was False under the old inline comparison too —
# but the old code could not tell them apart from "clearly under", and neither
# could the operator reading the log.
check("NaN reading (non-amdgpu sysfs miss)", over_limit(NAN, 112.0), False)
check("NaN limit (tripwire could not be armed)", over_limit(120.0, NAN), False)
check("both NaN", over_limit(NAN, NAN), False)

print()
print("resolve_mem_limit() — platform-derived, never a silent constant")

limit, why = resolve_mem_limit(95.0, 125.0, "amdgpu-sysfs-gtt")
check("explicit value wins on amdgpu", limit, 95.0)
check("  and says so", "explicit" in why, True)

limit, why = resolve_mem_limit(64.0, 79.2, "cuda-mem-get-info")
check("explicit value wins on CUDA", limit, 64.0)

limit, why = resolve_mem_limit(None, 125.0, "amdgpu-sysfs-gtt")
check("amdgpu default is the historic 112", limit, 112.0)

# THE CASE THAT MOTIVATED THE CHANGE: 112 is meaningless on an 80 GB card. It
# sits ABOVE the device total, so a guard carrying it forward could never fire
# no matter how the reading was obtained.
limit, why = resolve_mem_limit(None, 79.2, "cuda-mem-get-info")
check("CUDA 80GB derives from the device, not 112", limit, 72.9)
check("  and is below the device total", limit < 79.2, True)

limit, why = resolve_mem_limit(None, 95.6, "cuda-mem-get-info")
check("CUDA 96GB (RTX PRO 6000) derives 88.0", limit, 88.0)

# Could-not-judge must stay NaN so over_limit() refuses, and the caller prints
# NEVER-RAN. Returning any number here would fabricate a guard.
limit, why = resolve_mem_limit(None, NAN, "unavailable")
check("no source -> NaN limit, not a fabricated one", limit, NAN)
check("  and the rationale says it cannot be armed", "cannot be armed" in why, True)

limit, _ = resolve_mem_limit(None, NAN, "cuda-mem-get-info")
check("CUDA with unreadable total -> NaN, not 0.92*NaN nonsense", limit, NAN)

print()
print("mem_reading() — attribution, on a faked CUDA device")

GB = 1024**3


class _FakeCuda:
    """Just the four counters mem_reading reads, in bytes as torch returns them."""

    def __init__(self, total, free, alloc_peak, reserved):
        self._t, self._f, self._a, self._r = total, free, alloc_peak, reserved

    def is_available(self):
        return True

    def mem_get_info(self):
        return (int(self._f * GB), int(self._t * GB))

    def max_memory_allocated(self):
        return int(self._a * GB)

    def memory_reserved(self):
        return int(self._r * GB)


class _FakeTorch:
    def __init__(self, **kw):
        self.cuda = _FakeCuda(**kw)


def cuda_reading(total, ours, reserved, pool_used):
    """mem_reading() against a fake CUDA card, with the amdgpu path forced off.

    This host IS an amdgpu box, so without the stub every CUDA case would take
    the sysfs branch and silently test nothing.
    """
    real = train_orpo_trl.host_gtt_gb
    train_orpo_trl.host_gtt_gb = lambda: float("nan")
    try:
        return mem_reading(_FakeTorch(total=total, free=total - pool_used,
                                      alloc_peak=ours, reserved=reserved))
    finally:
        train_orpo_trl.host_gtt_gb = real


# The PRO 5000 as measured: 44.36 of the 44.97 GB resident on the card is our
# own allocator's cache. Attributing that to a co-tenant is exactly the error.
r = cuda_reading(total=47.27, ours=35.89, reserved=44.36, pool_used=44.97)
check("ours is our demand, not the device total", round(r.ours, 2), 35.89)
check("reserved is carried separately", round(r.reserved, 2), 44.36)
check("unattributed is device minus OUR reserve", round(r.unattributed, 2), 0.61)
check("  which on a quiet card is just our GPU context", r.unattributed < 2.0, True)
check("source names the platform", r.source, "cuda-mem-get-info")

# A co-tenant is the case the guard was FOR, and it is now the only thing that
# moves `unattributed`: 30 GB of someone else's model on the same card.
r = cuda_reading(total=47.27, ours=11.0, reserved=12.5, pool_used=42.5)
check("a real co-tenant shows up as GB, not as our cache",
      round(r.unattributed, 2), 30.0)

# Never negative. mem_get_info and the allocator are sampled microseconds
# apart, so reserved can legitimately read higher than pool_used.
r = cuda_reading(total=47.27, ours=35.0, reserved=44.0, pool_used=43.9)
check("skewed samples clamp at 0, never a negative co-tenant", r.unattributed, 0.0)

print()
print("THE DECIDER, COMPOSED — the three cards measured on 2026-08-05")


def new_verdict(total, ours, reserved, pool_used, explicit=None):
    """ABORT / ok under the fixed guard: our demand vs our ceiling."""
    r = cuda_reading(total, ours, reserved, pool_used)
    limit, _ = resolve_mem_limit(explicit, r.total, r.source)
    return over_limit(r.ours, demand_ceiling(limit, r.unattributed))


def old_verdict(total, pool_used):
    """ABORT / ok under the guard as it stood: device-wide vs 92% of total."""
    limit, _ = resolve_mem_limit(None, total, "cuda-mem-get-info")
    return over_limit(pool_used, limit)


CARDS = [  # name, total, ours, reserved, pool_used, what actually happened
    ("A100 SXM4 80GB", 79.25, 36.53, 60.22, 61.87, "completed 25 steps"),
    ("RTX PRO 5000",   47.27, 35.89, 44.36, 44.97, "ABORTED at step 4"),
    ("RTX A6000",      44.43, 35.89, 40.49, 40.96, "ABORTED at step 4"),
]

# FIRST: reproduce the observed outcomes with the old decider. If these three
# do not match what the cards did, the diagnosis is wrong and the fix below is
# unearned.
for name, total, ours, reserved, pool_used, outcome in CARDS:
    check(f"OLD guard reproduces {name} ({outcome})",
          old_verdict(total, pool_used), outcome.startswith("ABORT"))

# THEN: the fix. Demand is 35.9-36.5 GB on all three and fits every card, so
# none of them may abort.
for name, total, ours, reserved, pool_used, _outcome in CARDS:
    check(f"NEW guard passes {name} (demand {ours} of {total}GB)",
          new_verdict(total, ours, reserved, pool_used), False)

# And it still fires when it should. Same PRO 5000, same workload, but 30 GB of
# someone else's model on the card: our 35.9 GB no longer fits and the guard
# must stop us BEFORE the allocator fails.
check("NEW guard aborts on a genuine 30GB co-tenant",
      new_verdict(47.27, 35.89, 12.0, 42.0), True)

# Raising --mem-limit-gb is NOT the fix and must not silently become one: an
# explicit limit still applies to the pool, less the co-tenant.
check("explicit --mem-limit-gb still fires when a co-tenant crowds us",
      new_verdict(47.27, 35.89, 12.0, 42.0, explicit=47.0), True)

print()
print("demand_ceiling() — amdgpu behaviour is arithmetically UNCHANGED")
# ours > 112 - cotenant is the same predicate as box > 112. The Halo's limit
# was set at 112 after 95 killed arm A at step 118; this fix must not perturb
# that box at all.
for box, cotenant in ((115.0, 30.0), (110.0, 30.0), (113.0, 0.0), (100.0, 12.0)):
    ours = box - cotenant
    check(f"box={box} cotenant={cotenant}: new verdict == old verdict",
          over_limit(ours, demand_ceiling(112.0, cotenant)),
          over_limit(box, 112.0))

# Could-not-judge propagates through the ceiling instead of being swallowed.
check("NaN limit -> NaN ceiling -> refuses to judge",
      over_limit(35.9, demand_ceiling(NAN, 0.61)), False)
check("NaN attribution -> refuses to judge",
      over_limit(35.9, demand_ceiling(43.5, NAN)), False)

print()
if failures:
    print(f"FAILED {len(failures)}: {', '.join(failures)}")
    sys.exit(1)
print("all tripwire tests passed")
