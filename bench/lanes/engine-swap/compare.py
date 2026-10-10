#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Line up runs of the engine-swap check, check by check.

Reads runs.tsv (run, lane, t0, t1, stamp) and each stamp's
target/quality-check/<stamp>/lane-<lane>.out. A lane's sub-check is a table
row `<name>  <verdict>  -  <detail>`; its verdict is the last JSON line. Prints
one block per lane: the lane verdict per run, then every sub-check whose
verdict differs between runs, or that is not `passed` in any run, with each
run's detail.

    compare.py L1 L2 R1
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
ROW = re.compile(r"^\s{2}(\S.*?)\s{2,}(passed|failed|could-not-judge|never-ran)\s+-\s+(.*)$")


def lane_rows(stamp: str, lane: str):
    out = ROOT / "target/quality-check" / stamp / f"lane-{lane}.out"
    rows, verdict = {}, None
    if not out.exists():
        return rows, "never-ran (no lane output)"
    for line in out.read_text(errors="replace").splitlines():
        m = ROW.match(line)
        if m:
            rows[m.group(1).strip()] = (m.group(2), m.group(3).strip())
        if line.lstrip().startswith('{"subject"'):
            v = json.loads(line)
            verdict = f'{v["verdict"]}: {v.get("reason", "")}'
    return rows, verdict or "never-ran (no verdict line)"


def main() -> int:
    runs = sys.argv[1:]
    stamps = {}
    for line in (ROOT / "target/engine-swap/runs.tsv").read_text().splitlines():
        run, lane, _, _, stamp = line.split("\t")
        stamps[(run, lane)] = stamp
    lanes = list(dict.fromkeys(l for (r, l) in stamps if r in runs))
    for lane in lanes:
        per = {r: lane_rows(stamps[(r, lane)], lane) if (r, lane) in stamps else ({}, "not run")
               for r in runs}
        print(f"== {lane}")
        for r in runs:
            print(f"   {r:3} {per[r][1][:200]}")
        names = list(dict.fromkeys(n for r in runs for n in per[r][0]))
        for n in names:
            vs = [per[r][0].get(n, ("absent", ""))[0] for r in runs]
            if len(set(vs)) > 1 or any(v != "passed" for v in vs):
                print(f"   - {n}")
                for r in runs:
                    v, d = per[r][0].get(n, ("absent", ""))
                    print(f"       {r:3} {v:15} {d[:170]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
