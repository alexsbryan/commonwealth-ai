#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Probe A's load generator — see scripts/probe-a-shed-under-load.sh.

Mixed population against one node:
  * N-2 ordinary non-streaming chat clients, arriving together.
  * 1 STALLED-SSE client: opens a streaming completion, reads nothing after
    the headers, and never disconnects. This is the adversary that pinned
    the slot indefinitely before order mesh-scale-t0 item 5.
  * 1 TIGHT-RETRY client: re-fires the instant it is shed, ignoring the
    Retry-After hint. Its job is to make the hint's SPREAD observable
    (item 2) — every hint it receives is recorded.

Every request is classified into one of four outcomes, never three:
  admitted / shed / parked / error. `parked` is the one the probe exists to
  find — a request neither served nor refused inside the park threshold is
  the failure "does the shed hold the line" is asking about, and it must be
  reported as itself rather than folded into `error`.
"""
from __future__ import annotations

import argparse
import json
import re
import threading
import time
import urllib.error
import urllib.request
from collections import Counter

# A request still outstanding this long after the shed window has elapsed is
# PARKED: neither served nor refused. The shed window is 30s
# (DEFAULT_MAX_QUEUE_WAIT_MS), so anything past 4x that is not "slow", it is
# unbounded — which is exactly the condition items 4 and 5 removed.
PARK_THRESHOLD_S = 120.0

lock = threading.Lock()
results: list[dict] = []
retry_hints: list[int] = []
inflight = 0
inflight_peak = 0


def record(**kw) -> None:
    with lock:
        results.append(kw)


def chat_body(prompt: str, stream: bool = False, max_tokens: int = 24) -> bytes:
    # NO `model` field, deliberately. Naming a model routes through the
    # NAMED-model path, which 503s with "no node advertises model 'auto'"
    # before any queue decision is made — the probe would then measure a
    # routing miss and call it a shed. Omitting it takes the OICP router,
    # which is the path 100 real clients take. (Caught by validating the
    # instrument before trusting the result: the first run reported
    # 105,623 "sheds" and zero `inference.queue: SHED` lines in the daemon
    # log, which is what a broken instrument looks like.)
    return json.dumps(
        {
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_tokens,
            "stream": stream,
        }
    ).encode()


def one_turn(url: str, idx: int, kind: str) -> None:
    """One non-streaming completion. Classifies its own outcome."""
    global inflight, inflight_peak
    req = urllib.request.Request(
        f"{url}/v1/chat/completions",
        data=chat_body(f"In one short sentence, what is client {idx} asking?"),
        headers={"Content-Type": "application/json"},
    )
    started = time.monotonic()
    with lock:
        inflight += 1
        inflight_peak = max(inflight_peak, inflight)
    try:
        with urllib.request.urlopen(req, timeout=PARK_THRESHOLD_S) as resp:
            resp.read()
            record(kind=kind, outcome="admitted", status=resp.status,
                   secs=time.monotonic() - started)
    except urllib.error.HTTPError as e:
        body = e.read().decode(errors="replace")
        hint = e.headers.get("Retry-After")
        if e.code == 503:
            if hint is not None:
                with lock:
                    try:
                        retry_hints.append(int(hint))
                    except ValueError:
                        pass
            record(kind=kind, outcome="shed", status=e.code,
                   secs=time.monotonic() - started, retry_after=hint,
                   reason=(json.loads(body).get("reason") if body.startswith("{") else None))
        else:
            record(kind=kind, outcome="error", status=e.code,
                   secs=time.monotonic() - started, detail=body[:200])
    except Exception as e:  # timeout, reset, refused
        elapsed = time.monotonic() - started
        outcome = "parked" if elapsed >= PARK_THRESHOLD_S * 0.95 else "error"
        record(kind=kind, outcome=outcome, secs=elapsed, detail=repr(e)[:200])
    finally:
        with lock:
            inflight -= 1


def stalled_sse(url: str, seconds: float) -> None:
    """Open a stream, read NOTHING, hold the connection open.

    The adversary from §7.2: "the stalled-SSE pin is indefinite, not 300s".
    Deliberately no `resp.read()` — the point is a consumer that never
    consumes and never disconnects.
    """
    req = urllib.request.Request(
        f"{url}/v1/chat/completions",
        # max_tokens high on purpose: the generation must outrun the SSE
        # channel buffer, or a short answer fits entirely in the buffer and the
        # consumer never actually blocks the slot.
        data=chat_body("Write a very long essay about the sea.", stream=True, max_tokens=4096),
        headers={"Content-Type": "application/json"},
    )
    started = time.monotonic()
    try:
        resp = urllib.request.urlopen(req, timeout=PARK_THRESHOLD_S)
        # Hold it. No reads.
        time.sleep(seconds)
        resp.close()
        record(kind="stalled_sse", outcome="held", secs=time.monotonic() - started)
    except Exception as e:
        record(kind="stalled_sse", outcome="error", secs=time.monotonic() - started,
               detail=repr(e)[:200])


def tight_retry(url: str, seconds: float) -> None:
    """Re-fire immediately on every shed, ignoring Retry-After."""
    deadline = time.monotonic() + seconds
    n = 0
    while time.monotonic() < deadline:
        one_turn(url, 9999, "tight_retry")
        n += 1
    record(kind="tight_retry", outcome="loop_done", attempts=n, secs=seconds)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True)
    ap.add_argument("--clients", type=int, default=100)
    ap.add_argument("--seconds", type=float, default=45.0)
    ap.add_argument("--daemon-log", default=None)
    args = ap.parse_args()

    # One warm turn first, alone, so the model load is not charged to the
    # load window and `avg_turn` measures a turn rather than a cold start.
    print("probe-a: warm-up turn (cold model load is NOT part of the measurement)…")
    warm_started = time.monotonic()
    one_turn(args.url, 0, "warmup")
    print(f"probe-a: warm-up took {time.monotonic() - warm_started:.1f}s "
          f"→ {results[-1]['outcome']}")
    results.clear()

    threads: list[threading.Thread] = []
    threads.append(threading.Thread(target=stalled_sse, args=(args.url, args.seconds)))
    threads.append(threading.Thread(target=tight_retry, args=(args.url, args.seconds)))
    for i in range(max(0, args.clients - 2)):
        threads.append(threading.Thread(target=one_turn, args=(args.url, i, "normal")))

    print(f"probe-a: releasing {len(threads)} clients "
          f"(1 stalled-SSE, 1 tight-retry, {len(threads) - 2} normal)…")
    t0 = time.monotonic()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    wall = time.monotonic() - t0

    outcomes = Counter(r["outcome"] for r in results)
    admitted = [r for r in results if r["outcome"] == "admitted"]
    avg_turn = sum(r["secs"] for r in admitted) / len(admitted) if admitted else 0.0
    shed = [r for r in results if r["outcome"] == "shed"]
    shed_secs = sorted(r["secs"] for r in shed)

    print()
    print("PROBE_A_RESULT " + " ".join(f"{k}={v}" for k, v in sorted(outcomes.items())))
    print(f"PROBE_A_RESULT wall_s={wall:.1f} clients={args.clients} "
          f"admitted_n={len(admitted)} avg_turn_s={avg_turn:.2f} "
          f"inflight_peak={inflight_peak}")
    if shed_secs:
        print(f"PROBE_A_RESULT shed_latency_s_min={shed_secs[0]:.3f} "
              f"shed_latency_s_max={shed_secs[-1]:.3f} "
              f"shed_latency_s_p50={shed_secs[len(shed_secs)//2]:.3f}")
    if retry_hints:
        h = Counter(retry_hints)
        print(f"PROBE_A_RESULT retry_after_distinct={len(h)} "
              f"retry_after_min={min(retry_hints)} retry_after_max={max(retry_hints)} "
              f"retry_after_hist={dict(sorted(h.items()))}")
    else:
        print("PROBE_A_RESULT retry_after_distinct=0 (no 503 carried a Retry-After)")

    # ── The architecture's prediction, checked against the DAEMON's own
    # numbers, not the client's.
    #
    # A client's end-to-end latency is queue wait + service, so using it as
    # `avg_turn` would compare two different quantities and produce a
    # confident wrong answer. The slot publishes what the formula actually
    # means on every shed line: `avg_turn_ms` (its EWMA of SERVICE time) and
    # `position` (how deep the queue was when it refused). The deepest
    # position the queue ever accepted before refusing IS the measured
    # admitted concurrency.
    shed_window_s = 30.0  # DEFAULT_MAX_QUEUE_WAIT_MS
    positions: list[int] = []
    avg_turn_ms: list[int] = []
    if args.daemon_log:
        try:
            dlog = open(args.daemon_log, errors="replace").read()
        except OSError:
            dlog = ""
        # The daemon log is ANSI-coloured; strip escapes before matching or
        # every field regex silently finds nothing (which is how a
        # COULD-NOT-JUDGE gets mistaken for a clean result).
        dlog = re.sub(r"\x1b\[[0-9;]*m", "", dlog)
        shed_lines = [ln for ln in dlog.splitlines() if "inference.queue: SHED" in ln]
        for ln in shed_lines:
            if (mp := re.search(r"\bposition=(\d+)", ln)):
                positions.append(int(mp.group(1)))
            if (ma := re.search(r"\bavg_turn_ms=(\d+)", ln)):
                avg_turn_ms.append(int(ma.group(1)))
    if positions and avg_turn_ms:
        slot_turn_s = (sum(avg_turn_ms) / len(avg_turn_ms)) / 1000.0
        predicted = 1 + int(shed_window_s // slot_turn_s) if slot_turn_s > 0 else 0
        print(f"PROBE_A_DERIVED shed_window_s={shed_window_s} "
              f"slot_avg_turn_s={slot_turn_s:.2f} "
              f"predicted_admitted_concurrency={predicted} "
              f"measured_max_queue_position={max(positions)} "
              f"client_end_to_end_avg_s={avg_turn:.2f} "
              f"admitted_total={len(admitted)}")
    else:
        print("PROBE_A_DERIVED COULD-NOT-JUDGE — no `inference.queue: SHED` lines carrying "
              "position/avg_turn_ms in the daemon log, so the formula has no daemon-side "
              "numbers to check against")

    for r in results:
        if r["kind"] == "tight_retry" and r["outcome"] == "loop_done":
            print(f"PROBE_A_RESULT tight_retry_attempts={r['attempts']}")
        if r["kind"] == "stalled_sse":
            print(f"PROBE_A_RESULT stalled_sse_outcome={r['outcome']} "
                  f"secs={r['secs']:.1f}")

    if args.daemon_log:
        try:
            log = open(args.daemon_log, errors="replace").read()
        except OSError:
            log = ""
        for pattern, label in (
            (r"inference\.queue: SHED", "queue_shed_lines"),
            (r"stream consumer stopped reading", "sse_consumer_release_lines"),
            (r"deadline exceeded \(stream\)", "stream_wallclock_deadline_lines"),
            (r"cancelled via receiver-drop", "receiver_drop_lines"),
            (r"RetentionGc started", "retention_gc_started"),
            (r"coalescer armed with a bounded queue", "fast_short_bound_armed"),
        ):
            print(f"PROBE_A_DAEMON {label}={len(re.findall(pattern, log))}")

    parked = outcomes.get("parked", 0)
    print()
    print("PROBE_A_VERDICT " + ("HELD" if parked == 0 else f"PARKED={parked}"))


if __name__ == "__main__":
    main()
