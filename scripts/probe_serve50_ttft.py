#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""RED BASELINE — bar `serve50-ttft` (order `mesh-serve-50-red`).

ONE QUESTION: with C principals asking at once, how long does a principal wait
before the first token appears — p50 and p95?

Run as a load generator under the existing Probe A netns harness, so the sealed
netns and the recorded bind assertion stay the only implementation of both:

    scripts/probe-a-shed-under-load.sh --load scripts/probe_serve50_ttft.py \
        --clients 50 --load-args "--reps 2"

── The two TTFTs, and why reporting only one is a lie ────────────────────────

A node under load answers some callers and refuses (503) the rest. That gives
two different numbers, and they diverge hard:

  * `ttft_admitted`  — TTFT over the requests that were admitted ON FIRST TRY.
    This is the bar's literal reading, and it is SURVIVORSHIP-BIASED: the more
    aggressively a node sheds, the better this number looks, because every
    caller who had to wait has been removed from the sample. A node that
    admitted 2 of 50 can post a beautiful p95 here.

  * `ttft_from_first_attempt` — for every principal that was EVENTUALLY served,
    wall clock from that principal's FIRST attempt to its first token, with
    every 503 and honoured Retry-After in between counted. This is what the
    person in front of the screen experiences.

Both are printed for every run, together with `admitted_first_try_frac`, which
is the number that says how far apart they are allowed to be. Reading
`ttft_admitted` without it is the failure mode this docstring exists to block.

`parked` stays a fourth outcome, never folded into `error` — same rule as
Probe A (§8.1). A request still open at PARK_THRESHOLD_S is parked, not slow.

TTFT is measured client-side, from the moment the request goes out to the first
SSE byte back. Queue wait is INSIDE that number on purpose: a caller sitting in
the admission queue is a caller staring at a blank screen.
"""
from __future__ import annotations

import argparse
import json
import statistics
import threading
import time
import urllib.error
import urllib.request

PROMPT = "Write a detailed paragraph about the sea, its tides, and its weather."
PARK_THRESHOLD_S = 120.0

lock = threading.Lock()
results: list[dict] = []


def record(**kw) -> None:
    with lock:
        results.append(kw)


def one_stream(url: str, max_tokens: int) -> tuple[str, float | None, float, int | None]:
    """One streaming attempt. Returns (outcome, ttft_s, elapsed_s, retry_after)."""
    body = json.dumps(
        {
            # No `model` field — naming a model takes the NAMED-model path and
            # 503s before any slot decision (same reason as probe_a_load.py).
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": max_tokens,
            "stream": True,
        }
    ).encode()
    req = urllib.request.Request(
        f"{url}/v1/chat/completions", data=body, headers={"Content-Type": "application/json"}
    )
    started = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=PARK_THRESHOLD_S) as resp:
            ttft = None
            for line in resp:
                if not line.strip():
                    continue
                if ttft is None:
                    ttft = time.monotonic() - started
            return "admitted", ttft, time.monotonic() - started, None
    except urllib.error.HTTPError as e:
        e.read()
        if e.code == 503:
            hint = e.headers.get("Retry-After")
            try:
                hint_i = int(hint) if hint is not None else None
            except (TypeError, ValueError):
                hint_i = None
            return "shed", None, time.monotonic() - started, hint_i
        return "error", None, time.monotonic() - started, None
    except Exception:  # noqa: BLE001 — any failure is a first-class outcome
        elapsed = time.monotonic() - started
        return ("parked" if elapsed >= PARK_THRESHOLD_S * 0.95 else "error"), None, elapsed, None


def principal(url: str, idx: int, max_tokens: int, deadline: float, retry_cap: float) -> None:
    """One principal: asks once, and on a 503 honours Retry-After and re-asks.

    `first_try` records what happened on attempt 0 — that is what feeds
    `ttft_admitted` and `admitted_first_try_frac`. The loop keeps going so the
    principal's REAL time-to-first-token can be measured too.
    """
    began = time.monotonic()
    attempts = 0
    first_outcome = None
    while True:
        outcome, ttft, elapsed, hint = one_stream(url, max_tokens)
        attempts += 1
        if first_outcome is None:
            first_outcome = outcome
            if outcome == "admitted":
                record(client=idx, outcome="admitted", first_try=True, attempts=1,
                       ttft_s=ttft, wall_s=elapsed,
                       ttft_from_first_attempt_s=ttft)
                return
        if outcome == "admitted":
            record(client=idx, outcome="admitted", first_try=False, attempts=attempts,
                   ttft_s=ttft, wall_s=elapsed,
                   ttft_from_first_attempt_s=(time.monotonic() - began) - elapsed + (ttft or 0.0))
            return
        if outcome != "shed" or time.monotonic() >= deadline:
            record(client=idx, outcome=outcome, first_try=(attempts == 1), attempts=attempts,
                   ttft_s=None, wall_s=elapsed, ttft_from_first_attempt_s=None,
                   gave_up_after_s=time.monotonic() - began)
            return
        time.sleep(min(hint if hint else 1.0, retry_cap))


def pct(xs: list[float], q: float) -> float | None:
    if not xs:
        return None
    xs = sorted(xs)
    if len(xs) == 1:
        return xs[0]
    i = min(len(xs) - 1, int(round(q * (len(xs) - 1))))
    return xs[i]


def warmup(url: str) -> float:
    body = json.dumps(
        {"messages": [{"role": "user", "content": "Say hello."}], "max_tokens": 8}
    ).encode()
    req = urllib.request.Request(
        f"{url}/v1/chat/completions", data=body, headers={"Content-Type": "application/json"}
    )
    t0 = time.monotonic()
    with urllib.request.urlopen(req, timeout=600) as resp:
        resp.read()
    return time.monotonic() - t0


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--clients", type=int, default=50, help="concurrent principals")
    ap.add_argument("--seconds", type=float, default=90.0, help="retry window per principal")
    ap.add_argument("--daemon-log", default=None)
    ap.add_argument("--max-tokens", type=int, default=64)
    ap.add_argument("--retry-cap", type=float, default=5.0)
    ap.add_argument("--reps", type=int, default=2, help="runs; >1 makes the report a bracket")
    args = ap.parse_args()
    n = max(1, args.clients)

    print("probe-ttft: warm-up turn (model load is NOT part of the measurement)…")
    print(f"probe-ttft: warm-up took {warmup(args.url):.1f}s")

    for rep in range(1, args.reps + 1):
        with lock:
            results.clear()
        deadline = time.monotonic() + args.seconds
        threads = [
            threading.Thread(target=principal,
                             args=(args.url, i, args.max_tokens, deadline, args.retry_cap))
            for i in range(n)
        ]
        t0 = time.monotonic()
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        wall = time.monotonic() - t0

        with lock:
            rows = list(results)
        first_try_admitted = [r for r in rows if r["outcome"] == "admitted" and r["first_try"]]
        served = [r for r in rows if r["outcome"] == "admitted"]
        shed_only = [r for r in rows if r["outcome"] == "shed"]
        parked = [r for r in rows if r["outcome"] == "parked"]
        errors = [r for r in rows if r["outcome"] == "error"]

        ft = [r["ttft_s"] for r in first_try_admitted if r["ttft_s"] is not None]
        fa = [r["ttft_from_first_attempt_s"] for r in served
              if r.get("ttft_from_first_attempt_s") is not None]

        if not served:
            print(f"PROBE_TTFT rep={rep} COULD-NOT-JUDGE — nothing was served in the window; "
                  f"this run measured the harness, not the system "
                  f"(shed={len(shed_only)} parked={len(parked)} error={len(errors)})")
            continue

        print(
            f"PROBE_TTFT rep={rep} principals={n} window_wall_s={wall:.1f} "
            f"admitted_first_try={len(first_try_admitted)} "
            f"admitted_first_try_frac={len(first_try_admitted) / n:.3f} "
            f"eventually_served={len(served)} gave_up={len(shed_only)} "
            f"parked={len(parked)} error={len(errors)}"
        )
        print(
            f"PROBE_TTFT rep={rep} ttft_admitted_p50_s={pct(ft, 0.50)} "
            f"ttft_admitted_p95_s={pct(ft, 0.95)} ttft_admitted_max_s={max(ft) if ft else None}"
        )
        print(
            f"PROBE_TTFT rep={rep} ttft_from_first_attempt_p50_s={pct(fa, 0.50)} "
            f"ttft_from_first_attempt_p95_s={pct(fa, 0.95)} "
            f"ttft_from_first_attempt_max_s={max(fa) if fa else None} "
            f"mean_attempts_to_serve="
            f"{statistics.mean([r['attempts'] for r in served]):.2f}"
        )
        print(f"PROBE_TTFT_VERDICT rep={rep} "
              + ("no-parking" if not parked else f"PARKED={len(parked)}"))

    if args.daemon_log:
        try:
            log = open(args.daemon_log, errors="replace").read()
        except OSError:
            log = ""
        shed_lines = log.count("inference.queue: SHED")
        print(f"PROBE_TTFT_DAEMON shed_log_lines={shed_lines}")


if __name__ == "__main__":
    main()
