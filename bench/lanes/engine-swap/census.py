#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Sum engine_proxy.py's rows by lane and extension field, for one R run.

Each proxy row is attributed to the lane whose wall window (runs.tsv, written
by arm.sh check) contains its timestamp; rows outside every window are
counted under "(between lanes)". Prints, per lane: requests by path and
status, then every extension field's outcome count.

    census.py --run R1 [--dir target/engine-swap]
"""
import argparse
import collections
import json
import sys
from pathlib import Path


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--run", required=True)
    ap.add_argument("--dir", default=str(Path(__file__).resolve().parents[3] / "target/engine-swap"))
    a = ap.parse_args()
    d = Path(a.dir)
    windows = []
    for line in (d / "runs.tsv").read_text().splitlines():
        run, lane, t0, t1, _ = line.split("\t")
        if run == a.run:
            windows.append((lane, float(t0), float(t1)))
    if not windows:
        print(f"census: no lanes recorded for run {a.run} in runs.tsv", file=sys.stderr)
        return 4
    lo, hi = min(w[1] for w in windows), max(w[2] for w in windows)
    paths = collections.defaultdict(collections.Counter)
    fields = collections.defaultdict(collections.Counter)
    for line in (d / "proxy.jsonl").open():
        r = json.loads(line)
        if not lo <= r["ts"] <= hi:
            continue
        lane = next((w[0] for w in windows if w[1] <= r["ts"] <= w[2]), "(between lanes)")
        paths[lane][f'{r["path"]} {r.get("status")}'] += 1
        for k, v in (r.get("ext") or {}).items():
            fields[lane][f"{k}: {v}"] += 1
    for lane, _, _ in windows + [("(between lanes)", 0, 0)]:
        if lane not in paths:
            continue
        print(f"== {lane}")
        for k, n in sorted(paths[lane].items()):
            print(f"   {n:5}  {k}")
        for k, n in sorted(fields[lane].items()):
            print(f"   {n:5}  ext {k}")
    total = collections.Counter()
    for c in fields.values():
        total.update(c)
    print("== all lanes")
    for k, n in sorted(total.items()):
        print(f"   {n:5}  ext {k}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
