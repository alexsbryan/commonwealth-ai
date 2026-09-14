#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""massif-growth.py — which allocation sites GREW between two moments of a run.

heaptrack's peak/leak lists rank what was held, which mixes boot-time fixed costs
(model contexts, a parsed meta-atlas, an ONNX session) with what accumulated while
serving. A staircase is the second kind, and only a time series separates them.
This reads the massif file `read-heaptrack.sh` writes and diffs per-site bytes
between the detailed snapshots nearest two wall-clock times.

    massif-growth.py <massif.out> <start HH:MM:SS> <from HH:MM:SS> <to HH:MM:SS> [--depth N]

<start> is the wall-clock time the profiled process started (launch.txt
`started_at`); massif `time=` is seconds since then. --depth picks the tree level
used as the site label: 1 = allocating function, 2 = its caller, and so on.
"""
import argparse
import re
import sys
from collections import defaultdict

NODE = re.compile(r"^( *)n\d+: (\d+) (.*)$")


def secs(hms: str) -> int:
    h, m, s = (int(x) for x in hms.split(":"))
    return h * 3600 + m * 60 + s


def snapshots(path, depth):
    """Yield (time_s, total_heap, {label: bytes}) for DETAILED snapshots only.

    A label at `depth` carries the bytes of its subtree. Labels are the node text
    with the address stripped, so the same site matches across snapshots.
    """
    t = heap = None
    sites = None
    for line in open(path, errors="replace"):
        if line.startswith("snapshot="):
            if sites is not None:
                yield t, heap, sites
            t = heap = None
            sites = None
        elif line.startswith("time="):
            t = float(line.split("=", 1)[1])
        elif line.startswith("mem_heap_B="):
            heap = int(line.split("=", 1)[1])
        elif line.startswith("heap_tree="):
            sites = defaultdict(int) if line.strip().endswith("detailed") else None
        elif sites is not None:
            m = NODE.match(line)
            if m and len(m.group(1)) == depth:
                label = re.sub(r"^0x[0-9a-f]+: ", "", m.group(3)).strip()
                label = re.sub(r" \(in /[^)]*\)$", "", label)
                sites[label] += int(m.group(2))
    if sites is not None:
        yield t, heap, sites


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("massif")
    ap.add_argument("start")
    ap.add_argument("frm")
    ap.add_argument("to")
    ap.add_argument("--depth", type=int, default=1)
    ap.add_argument("--top", type=int, default=15)
    a = ap.parse_args()

    t0 = secs(a.start)
    want_from, want_to = secs(a.frm) - t0, secs(a.to) - t0
    best_from = best_to = None
    for t, heap, sites in snapshots(a.massif, a.depth):
        if t is None:
            continue
        if best_from is None or abs(t - want_from) < abs(best_from[0] - want_from):
            best_from = (t, heap, dict(sites))
        if best_to is None or abs(t - want_to) < abs(best_to[0] - want_to):
            best_to = (t, heap, dict(sites))
    if best_from is None or best_to is None:
        sys.exit("no detailed snapshots found — was the massif pass written with detailed snapshots?")

    def clock(ts):
        s = int(t0 + ts)
        return f"{s // 3600:02d}:{s % 3600 // 60:02d}:{s % 60:02d}"

    (tf, hf, sf), (tt, ht, st) = best_from, best_to
    for name, want, got in (("from", want_from, tf), ("to", want_to, tt)):
        if abs(got - want) > 180:
            print(f"WARNING: nearest detailed snapshot to '{name}' is {abs(got - want):.0f}s away", file=sys.stderr)
    print(f"from {clock(tf)} heap {hf / 2**30:.2f} GiB  ->  to {clock(tt)} heap {ht / 2**30:.2f} GiB"
          f"  (delta {(ht - hf) / 2**30:+.2f} GiB)")
    deltas = sorted(((st.get(k, 0) - sf.get(k, 0), k) for k in set(sf) | set(st)), reverse=True)
    print(f"top growth at depth {a.depth}:")
    for d, k in deltas[: a.top]:
        print(f"  {d / 2**20:+10.1f} MiB  now {st.get(k, 0) / 2**20:9.1f} MiB  {k[:150]}")
    shrink = [x for x in deltas if x[0] < 0][-5:]
    if shrink:
        print("largest shrink:")
        for d, k in shrink:
            print(f"  {d / 2**20:+10.1f} MiB  {k[:150]}")


if __name__ == "__main__":
    main()
