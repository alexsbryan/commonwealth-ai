#!/usr/bin/env python3
"""DRB run driver (order deep-research-t2b) — flies both arms on the frozen
subset, per the pre-registered arm protocols:

  local:  --backend auto --search-source corpus --corpora wikipedia
          --search 12 --fetch 12 --max-rounds 3
  hybrid: --backend auto --search-source web --consent personal
          --search 4 --fetch 4 --max-rounds 3
  deep:   --backend auto --search-source web --consent personal
          --search 10 --fetch 12 --max-rounds 6
          (order deep-research-t6a phase 1, pre-registered:
          research-grade depth on the same frozen subset; tavily keyed —
          the 10-task arm consumes the 100-call daily allowance exactly)

Arms run strictly sequentially (local first, then hybrid). One flight's
failure does not stop the driver; the exit code is non-zero if any flight
failed. Subprocess argv (no shell), so the prompts' apostrophes are safe.

Run: python3 run-drb-arms.py [--bin sovereign] [--arm local|hybrid]
        [--run-root <dir>]

--run-root (order deep-research-t4a, pre-registered): the t4a re-flight
writes to demo/demo12/runs/{local,hybrid}/ — the frozen drb/runs/ is
never touched. Default: HERE/runs (the historical root, verbatim
pre-t4a behavior).
"""
import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).parent

ARM_FLAGS = {
    "local": ["--search-source", "corpus", "--corpora", "wikipedia",
              "--search", "12", "--fetch", "12", "--max-rounds", "3"],
    "hybrid": ["--search-source", "web", "--consent", "personal",
               "--search", "4", "--fetch", "4", "--max-rounds", "3"],
    "deep": ["--search-source", "web", "--consent", "personal",
             "--search", "10", "--fetch", "12", "--max-rounds", "6"],
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", default="sovereign")
    ap.add_argument("--arm", choices=["local", "hybrid", "deep"], default=None,
                    help="fly only one arm (default: both, local first)")
    ap.add_argument("--run-root", default=str(HERE / "runs"),
                    help="run root (t4a: demo/demo12/runs — the frozen drb/runs is never touched)")
    args = ap.parse_args()

    run_root = Path(args.run_root)
    rows = [json.loads(l) for l in open(HERE / "query.subset.jsonl", encoding="utf-8")]
    arms = [args.arm] if args.arm else ["local", "hybrid"]
    failures = []

    def manifest_of(run_dir: Path):
        """The manifest lives in the nested run-id dir (drb-<id>/dr-<ts>/)."""
        cands = sorted(run_dir.glob("dr-*/manifest.json"),
                       key=lambda p: p.stat().st_mtime)
        return cands[-1] if cands else None

    for arm in arms:
        flags = ARM_FLAGS[arm]
        for r in rows:
            tid = r["id"]
            run_dir = run_root / arm / f"drb-{tid}"
            log_path = run_root / arm / f"drb-{tid}.console.log"
            run_dir.mkdir(parents=True, exist_ok=True)
            # resume: a completed flight (nested manifest present) is skipped
            mp = manifest_of(run_dir)
            if mp is not None:
                try:
                    m = json.load(open(mp, encoding="utf-8"))
                    if m.get("terminal_state") in ("done", "done-partial", "done-full"):
                        print(f"[{arm}] task {tid} already complete "
                              f"({m.get('terminal_state')}, {mp.parent.name}) — skipped",
                              flush=True)
                        continue
                except Exception:
                    pass
            cmd = [args.bin, "deep-research", r["prompt"],
                   "--backend", "auto", *flags,
                   "--run-dir", str(run_dir)]
            t0 = time.time()
            print(f"[{arm}] task {tid} start  (cmd: {cmd[:4]} ...)", flush=True)
            with open(log_path, "w", encoding="utf-8") as logf:
                proc = subprocess.run(cmd, stdout=logf, stderr=subprocess.STDOUT)
            wall = time.time() - t0
            state = "?"
            mp = manifest_of(run_dir)
            if mp is not None:
                try:
                    m = json.load(open(mp, encoding="utf-8"))
                    state = m.get("terminal_state")
                except Exception:
                    pass
            ok = proc.returncode == 0 and state in ("done", "done-partial", "done-full")
            print(f"[{arm}] task {tid} exit={proc.returncode} "
                  f"terminal={state} wall={wall:.0f}s {'OK' if ok else 'FAIL'}", flush=True)
            if not ok:
                failures.append((arm, tid, proc.returncode, state))

    if failures:
        print("FAILURES:", failures, flush=True)
        return 1
    print("ALL FLIGHTS OK", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
