#!/usr/bin/env python3
"""THE HOST MEMORY WALL — one decider, one name (ARCH §10.6).

Every caller that asks "can this host hold the next prefill?" asks HERE.
Before 2026-08-26 there were two answers and they disagreed in the worst
possible way: `score_race.py` polled MemAvailable (which SPIKES right after an
OOM kill, admitting the next attempt at the single worst moment), while
`arms/bed/run-ceiling.sh` compared daemon RSS against 25 GiB and RESTARTED THE
DAEMON above it — and a daemon restart spawns a ~11 GiB `rust-analyzer scip`
child, which is the competitor that makes a survivable prefill unsurvivable.
The harness's remedy was the disease.

THE MODEL, measured (journalctl -b -1 -k on 2026-08-26, seven global OOM kills):
  wall ~55 GiB, not the box's 125 GB — the Halo's GPU holds the rest
    unreclaimably, so MemAvailable is not the deciding quantity.
  daemon + rust-analyzer RSS at each kill:
    54.5 / 49.1 / 55.4 / 53.7 / 56.0 / 55.7 GiB
  prefill cost ~0.082 GiB per 1k prompt chars over a ~28.2 GiB in-inference
    base (t56 137,233 chars -> ~39.5 GiB; t62 208,513 -> 45.3).
  THE ARENA IS REUSED, NOT ACCUMULATED: the daemon settles around its largest
    prefill and does not fall back to its ~6 GiB idle, so the projection is
    max(resident, base + prefill) — NOT a sum. Summing refused t65, a smaller
    prompt than the t62 that had just succeeded, on a host with room.
  THE MODEL IS NOT IN RSS: loaded and idle the daemon reads ~6 GiB because the
    weights live in GPU/unified memory. A small idle RSS is not headroom.

CONFIRMED IN PRODUCTION 2026-08-26 09:13: t62 predicted 45.3 GiB, peaked 44.7.

    host_wall.py <prompt_chars>     # prints a verdict line, exit 0 admit / 3 wait
    host_wall.py --wait <chars> [--timeout S]
"""
import subprocess, sys, time

PREFILL_GIB_PER_1K_CHARS = 0.082
MODEL_BASE_GIB = 28.2
SETTLE_MARGIN_GIB = 6.0
OBSERVED_WALL_GIB = 55.0
SETTLE_MAX_WAIT_S = 900


def _rss_gib(pattern: str) -> float:
    total = 0.0
    try:
        pids = subprocess.run(["pgrep", "-f", pattern],
                              capture_output=True, text=True).stdout.split()
        for pid in pids:
            try:
                with open(f"/proc/{pid}/statm") as f:
                    total += int(f.read().split()[1]) * 4096 / (1024 ** 3)
            except OSError:
                continue
    except Exception:                                       # noqa: BLE001
        return 0.0
    return total


def daemon_rss_gib() -> float:
    return _rss_gib("sovereign-cli-daemon daemon run")


def scip_child_gib() -> float:
    """The daemon's `rust-analyzer scip` child (~11 GiB).

    NOT only spawned by a restart: measured 2026-08-26, the daemon (pid 2042,
    up since 08:58) spawned one at 09:56 unprompted, mid-flight. Over a
    multi-hour arm the collision is near-certain, so every gate must price it.
    NEVER kill it — a half-killed export wipes the code-intel graph. Wait.
    """
    return _rss_gib("rust-analyzer scip")


def daemon_pid() -> str | None:
    out = subprocess.run(["pgrep", "-f", "sovereign-cli-daemon daemon run"],
                         capture_output=True, text=True).stdout.split()
    return out[0] if out else None


def project(prompt_chars: int) -> tuple[float, float, float, float]:
    """(projected_peak, prefill, competitors, resident) for THIS prompt."""
    prefill = prompt_chars / 1000.0 * PREFILL_GIB_PER_1K_CHARS
    competitors = scip_child_gib()
    resident = daemon_rss_gib()
    peak = max(resident, MODEL_BASE_GIB + prefill) + competitors + SETTLE_MARGIN_GIB
    return peak, prefill, competitors, resident


def explain(prompt_chars: int) -> str:
    peak, prefill, comp, res = project(prompt_chars)
    return (f"{prompt_chars:,}-char prompt projects to {peak:.1f}G = "
            f"max(resident {res:.1f}, base {MODEL_BASE_GIB:.1f} + prefill "
            f"{prefill:.1f}) + competitors {comp:.1f} + margin "
            f"{SETTLE_MARGIN_GIB:.1f} against a {OBSERVED_WALL_GIB:.0f}G wall")


def admits(prompt_chars: int) -> bool:
    return project(prompt_chars)[0] <= OBSERVED_WALL_GIB


def main(argv) -> int:
    wait = False
    timeout = SETTLE_MAX_WAIT_S
    args = []
    i = 0
    while i < len(argv):
        if argv[i] == "--wait":
            wait = True
        elif argv[i] == "--timeout":
            i += 1; timeout = int(argv[i])
        else:
            args.append(argv[i])
        i += 1
    if not args:
        print(__doc__.strip()); return 2
    chars = int(args[0])
    if admits(chars):
        print(f"ADMIT: {explain(chars)}"); return 0
    print(f"WAIT: {explain(chars)}", flush=True)
    if not wait:
        return 3
    deadline = time.time() + timeout
    while time.time() < deadline:
        time.sleep(15)
        if admits(chars):
            print(f"ADMIT: {explain(chars)}", flush=True); return 0
    print(f"REFUSED after {timeout}s: {explain(chars)} — NOT attempted. "
          f"Competitors must clear first; never kill a running scip export.",
          flush=True)
    return 3


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
