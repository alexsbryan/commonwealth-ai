#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""rpc-timing-split — split a distributed decode's per-token cost into
worker compute vs. everything else, from the WORKER's own log.

WHY THIS EXISTS
    `svrn mesh bench` reports `itl_p50_ms`, measured between SSE frame
    arrivals at the HTTP client. That single number bundles five things:
    host compute, the wire, worker compute, sampling, and SSE framing. On
    2026-08-04 a DeepSeek-V4-Flash split measured 505 ms/token and nothing
    recorded could say which of the five it was. This closes the largest
    unknown -- worker compute -- without patching ggml.

    Nothing here modifies vendored llama.cpp. `vendor/llama-cpp-sys-4` is
    verified byte-identical to upstream (scripts/verify-vendored-llama-cpp.sh)
    and must stay that way, so the only admissible instrument is a log
    upstream already emits.

HOW IT WORKS
    ggml's RPC server handles commands SERIALLY on one socket
    (ggml-rpc.cpp: rpc_serve_client runs to completion inside the accept
    loop). Per decode token the worker therefore sees, in order:

        [set_tensor]        <- host pushes the split's input activations
        [graph_recompute]   <- logged BEFORE ggml_backend_graph_compute
        [get_tensor]        <- host blocks here; served AFTER compute ends

    So `t(get_tensor) - t(graph_*)` is worker compute plus the worker's own
    logging overhead, and nothing else. Subtract it from the measured ITL and
    the remainder is wire + host compute + sampling + framing.

    `[graph_compute]` (full serialization) instead of `[graph_recompute]`
    marks a graph REBUILD. Upstream caches the graph and sends 13 bytes per
    token when it can reuse it; a run showing rebuilds on every token has
    lost that optimisation, which is a large regression that is otherwise
    invisible. This script counts both and says so.

CAPTURE (worker side -- the machine LENDING memory)
    GGML_RPC_DEBUG=1 makes ggml emit these lines AND opens the daemon's two
    downstream log gates (sovereign-cli-daemon/src/lib.rs: llama_debug_requested).
    All three gates need it; the var alone is enough.

        sovereign daemon stop
        GGML_RPC_DEBUG=1 sovereign daemon start
        # ... run `svrn mesh bench` on the HOST ...
        sovereign daemon stop

    Then run this against the worker's log:

        ./scripts/rpc-timing-split.py ~/.svrnmesh/logs/daemon.err --itl-p50 505.1

WHAT THIS DOES *NOT* MEASURE
    - It does not separate wire from host compute. Both land in the
      remainder. Splitting those needs a host-side timer around the blocking
      get_tensor, which would mean patching vendored ggml.
    - Worker compute here includes the worker's logging overhead. At
      sub-millisecond log cost against 100ms+ compute that is noise; at 40
      tok/s it is not, so treat fast configurations with suspicion.
    - Timestamps are the daemon's tracing clock, not a monotonic timer.
"""

from __future__ import annotations

import argparse
import re
import sys
from datetime import datetime

# tracing's fmt layer writes RFC3339 with microseconds, wrapped in ANSI SGR
# when stderr is a tty at spawn time. Both forms appear in real logs.
ANSI = re.compile(r"\x1b\[[0-9;]*m")
LINE = re.compile(
    r"^(?P<ts>\d{4}-\d{2}-\d{2}T[\d:.]+Z)\s+"
    r"(?P<level>[A-Z]+)\s+"
    r"\[(?P<func>[a-z_]+)\]"
    r"(?P<rest>.*)$"
)
SIZE = re.compile(r"size:\s*(\d+)")

# The three commands that bracket one decode step, in the order the worker
# handles them. `graph_compute` is the rebuild variant of `graph_recompute`.
GRAPH = ("graph_compute", "graph_recompute")


def parse(path: str):
    """Yield (timestamp_seconds, func, size_or_None) for each ggml RPC line."""
    with open(path, "r", errors="replace") as fh:
        for raw in fh:
            line = ANSI.sub("", raw).strip()
            m = LINE.match(line)
            if not m:
                continue
            func = m.group("func")
            if func not in GRAPH and func not in ("set_tensor", "get_tensor"):
                continue
            ts = datetime.strptime(m.group("ts"), "%Y-%m-%dT%H:%M:%S.%fZ").timestamp()
            sz = SIZE.search(m.group("rest"))
            yield ts, func, int(sz.group(1)) if sz else None


def pct(v: list[float], q: float) -> float:
    """Nearest-rank percentile, matching mesh_bench.rs::percentile."""
    s = sorted(v)
    return s[min(len(s) - 1, round((len(s) - 1) * q))]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("log", help="the WORKER's daemon log (e.g. ~/.svrnmesh/logs/daemon.err)")
    ap.add_argument(
        "--itl-p50",
        type=float,
        help="measured itl_p50_ms from the bench record, to compute the remainder",
    )
    ap.add_argument(
        "--since",
        help="ignore lines before this RFC3339 timestamp (scope to one run)",
    )
    args = ap.parse_args()

    floor = (
        datetime.strptime(args.since, "%Y-%m-%dT%H:%M:%S.%fZ").timestamp()
        if args.since
        else None
    )

    computes: list[float] = []        # worker compute per token, ms
    bytes_out: list[int] = []         # get_tensor payload per token
    bytes_in = 0                      # set_tensor payload, whole run
    rebuilds = reuses = 0
    pending: float | None = None      # timestamp of the graph cmd awaiting its get_tensor

    for ts, func, size in parse(args.log):
        if floor is not None and ts < floor:
            continue
        if func in GRAPH:
            if func == "graph_compute":
                rebuilds += 1
            else:
                reuses += 1
            pending = ts
        elif func == "set_tensor":
            bytes_in += size or 0
        elif func == "get_tensor" and pending is not None:
            computes.append((ts - pending) * 1000.0)
            if size:
                bytes_out.append(size)
            pending = None

    if not computes:
        print(
            "no complete graph->get_tensor cycles found.\n"
            "  This is a FAILED MEASUREMENT, not a fast worker. Check that:\n"
            "  - the daemon was started with GGML_RPC_DEBUG=1 (all three log gates)\n"
            "  - this is the WORKER's log, not the host's (the host logs no per-command lines)\n"
            "  - the run actually distributed (a local load never dials a worker)",
            file=sys.stderr,
        )
        return 2

    n = len(computes)
    print(f"tokens with a complete cycle : {n}")
    print(f"graph reuse / rebuild        : {reuses} reuse, {rebuilds} rebuild")
    if rebuilds > max(4, n // 20):
        print(
            "  ^ WARNING: the graph is being rebuilt on most tokens, so every one of\n"
            "    them re-serialises the subgraph instead of sending 13 bytes. That is a\n"
            "    regression against upstream's graph cache, not a property of the model.",
        )
    print()
    print("worker compute per token (ms)")
    for label, q in (("min", 0.0), ("p50", 0.50), ("p90", 0.90), ("p99", 0.99), ("max", 1.0)):
        print(f"  {label:<4} {pct(computes, q):9.2f}")
    if bytes_out:
        print()
        print(f"get_tensor bytes/token       : p50 {pct([float(b) for b in bytes_out], 0.5):,.0f}")
        print(f"set_tensor bytes (whole run) : {bytes_in:,}")

    if args.itl_p50:
        worker = pct(computes, 0.50)
        rest = args.itl_p50 - worker
        print()
        print(f"measured ITL p50             : {args.itl_p50:9.2f} ms")
        print(f"  worker compute             : {worker:9.2f} ms  ({worker / args.itl_p50 * 100:5.1f}%)")
        print(f"  everything else            : {rest:9.2f} ms  ({rest / args.itl_p50 * 100:5.1f}%)")
        print("    = wire + host compute + sampling + SSE framing, NOT separated here.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
