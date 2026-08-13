#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""RED BASELINE — bar `t1-streaming-capacity` (order `mesh-scale-t1-red`).

ONE QUESTION: do two concurrent STREAMING turns decode concurrently, or do
they serialize? And does `SOVEREIGN_PRIMARY_SIBLINGS=2` — the capacity lever —
change the answer on the streaming path?

Method, in one process against one netns daemon (see
`scripts/probe-a-shed-under-load.sh --load scripts/probe_a_streaming_pool.py`):

  * warm-up: one non-streaming turn, so no arm pays the model load.
  * SERIAL arm: two streaming turns back to back. `serial_total` is the wall
    both together cost when nothing overlaps — the denominator.
  * CONCURRENT arm: the same two turns released together. `concurrent_wall`
    is the wall of the pair.
  * concurrency_factor = serial_total / concurrent_wall.
      ~1.0 → fully serialized (the red posture)
      ~2.0 → fully concurrent (the bar)

Both arms run twice so the reported number is a bracket, not a sample.

The pool's existence is asserted from the DAEMON's own log, not assumed: with
`SOVEREIGN_PRIMARY_SIBLINGS=N` set, `engine.rs:1372` logs one "primary sibling
context ready" per extra sibling at info. A run that reports "siblings on" and
finds zero such lines is reported as COULD-NOT-JUDGE, because then the arm
never actually had a pool.
"""
from __future__ import annotations

import argparse
import json
import re
import threading
import time
import urllib.request

PROMPT = "Write a detailed paragraph about the sea, its tides, and its weather."


def stream_turn(url: str, tag: str, max_tokens: int, out: list, stream: bool = True) -> None:
    """One completion, fully consumed. Records TTFT and wall.

    `stream=False` is the CONTROL arm: the sibling pool's branch lives in
    `complete()` (`engine.rs:2928`) and only there, so the same pair of
    requests run non-streamed is what "the lever works, just not here" looks
    like. Without that control, a serialized streaming pair could equally mean
    the pool never built.
    """
    body = json.dumps(
        {
            # No `model` field — same reason as probe_a_load.py: naming a model
            # takes the NAMED-model path and 503s before any slot decision.
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": max_tokens,
            "stream": stream,
        }
    ).encode()
    req = urllib.request.Request(
        f"{url}/v1/chat/completions", data=body, headers={"Content-Type": "application/json"}
    )
    started = time.monotonic()
    ttft = None
    chunks = 0
    try:
        with urllib.request.urlopen(req, timeout=300) as resp:
            for line in resp:
                if not line.strip():
                    continue
                if ttft is None:
                    ttft = time.monotonic() - started
                chunks += 1
        out.append(
            {"tag": tag, "ok": True, "ttft_s": ttft, "wall_s": time.monotonic() - started,
             "chunks": chunks}
        )
    except Exception as e:  # noqa: BLE001 — any failure is a first-class outcome
        out.append(
            {"tag": tag, "ok": False, "wall_s": time.monotonic() - started, "detail": repr(e)[:200]}
        )


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
    ap.add_argument("--clients", type=int, default=2, help="concurrent streams in the concurrent arm")
    ap.add_argument("--seconds", type=float, default=0, help="unused; probe-a passes it")
    ap.add_argument("--daemon-log", default=None)
    ap.add_argument("--max-tokens", type=int, default=128)
    ap.add_argument("--reps", type=int, default=2)
    ap.add_argument("--mode", choices=["stream", "nonstream"], default="stream")
    args = ap.parse_args()
    n = max(2, args.clients)

    print(f"probe-stream: warm-up turn (model load is NOT part of the measurement)…")
    print(f"probe-stream: warm-up took {warmup(args.url):.1f}s")

    for rep in range(1, args.reps + 1):
        # ── SERIAL ──
        serial: list = []
        t0 = time.monotonic()
        for i in range(n):
            stream_turn(args.url, f"serial{i}", args.max_tokens, serial,
                        stream=(args.mode == "stream"))
        serial_total = time.monotonic() - t0
        if not all(r["ok"] for r in serial):
            print(f"PROBE_STREAM rep={rep} COULD-NOT-JUDGE serial arm had a failure: "
                  f"{[r for r in serial if not r['ok']]}")
            continue

        # ── CONCURRENT ──
        conc: list = []
        threads = [
            threading.Thread(target=stream_turn,
                             args=(args.url, f"conc{i}", args.max_tokens, conc,
                                   args.mode == "stream"))
            for i in range(n)
        ]
        t0 = time.monotonic()
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        concurrent_wall = time.monotonic() - t0
        if not all(r["ok"] for r in conc):
            print(f"PROBE_STREAM rep={rep} COULD-NOT-JUDGE concurrent arm had a failure: "
                  f"{[r for r in conc if not r['ok']]}")
            continue

        factor = serial_total / concurrent_wall if concurrent_wall > 0 else 0.0
        print(
            f"PROBE_STREAM rep={rep} mode={args.mode} streams={n} "
            f"serial_total_s={serial_total:.2f} "
            f"serial_each_s={[round(r['wall_s'], 2) for r in serial]} "
            f"concurrent_wall_s={concurrent_wall:.2f} "
            f"concurrent_each_s={[round(r['wall_s'], 2) for r in conc]} "
            f"concurrent_ttft_s={[round(r['ttft_s'], 2) if r.get('ttft_s') is not None else None for r in conc]} "
            f"concurrency_factor={factor:.2f}"
        )

    # ── Daemon-side evidence: was there a pool at all? ──
    if args.daemon_log:
        try:
            log = open(args.daemon_log, errors="replace").read()
        except OSError:
            log = ""
        log = re.sub(r"\x1b\[[0-9;]*m", "", log)
        ready = len(re.findall(r"primary sibling context ready", log))
        building = len(re.findall(r"building primary sibling pool", log))
        dispatch = len(re.findall(r"dispatching to primary sibling", log))
        print(f"PROBE_STREAM_DAEMON sibling_pool_built={building} "
              f"sibling_contexts_ready={ready} sibling_dispatch_lines={dispatch}")
        if building == 0:
            print("PROBE_STREAM_DAEMON note: no sibling pool in this run "
                  "(SOVEREIGN_PRIMARY_SIBLINGS unset) — this is the baseline arm")


if __name__ == "__main__":
    main()
