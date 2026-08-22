#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""DRB-II flight driver (order deep-research-t7a, pre-registration §4).

Flies the loop AS-IS on the 8 sampled DRB-II tasks:
  deep-research "<task>" --backend auto --search-source web
    --consent personal --search 12 --fetch 12 --max-rounds 3
    --run-dir <root>/idx-<N>

Hard guards (structural, not remembered):
  - selection.json must have exactly 8 draws (the 96-search budget)
  - the binary must be the pinned battery instrument (sha256 check;
    refuses to fly otherwise)
  - each flight's exit code propagates; the driver exits non-zero if
    any flight failed (never masks)
  - the judged artifact (dr-*/report.md) is copied to
    reports/ours/idx-<N>.md only on a successful flight
  - --only N resumes a specific task; --force re-flies (overwrites)

Flights under systemd-run --user --wait --collect --unit=drb2-<N> when
--via-systemd is given (systemd-run --wait returns the service's exit
status, so inner exit codes propagate).
"""

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

PINNED_SHA256 = "3892178302ecefa706a216566897d615b68d5fd2c12e7529f2772c2101828267"
ARM_FLAGS = ["--backend", "auto", "--search-source", "web", "--consent", "personal",
             "--search", "12", "--fetch", "12", "--max-rounds", "3"]


def sha256_of(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check_binary(bin_path: Path):
    if not bin_path.exists():
        sys.exit(f"[err] binary missing: {bin_path}")
    actual = sha256_of(bin_path)
    if actual != PINNED_SHA256:
        sys.exit(f"[err] binary sha256 mismatch: {actual} != pinned {PINNED_SHA256}\n"
                 f"  the t7a flight must run the battery-#5 instrument (pre-registration §4)")
    print(f"[ok] binary pin verified: {bin_path} ({actual[:12]}...)")


def load_tasks(path: str) -> dict:
    tasks = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            obj = json.loads(line)
            tasks[int(obj["idx"])] = obj.get("content", {})
    return tasks


def main():
    ap = argparse.ArgumentParser(description="DRB-II flight driver (t7a)")
    ap.add_argument("--selection", default=str(Path(__file__).parent / "selection.json"))
    ap.add_argument("--tasks", default="/home/alexbryan/dev/DeepResearch-Bench-II/tasks_and_rubrics.jsonl")
    ap.add_argument("--run-root", default=str(Path(__file__).parent.parent / "arms" / "runs-drb2-baseline"))
    ap.add_argument("--reports-dir", default=str(Path(__file__).parent / "reports"))
    ap.add_argument("--bin", default=str(Path(__file__).parent.parent.parent.parent / "target" / "debug" / "sovereign-cli"))
    ap.add_argument("--only", type=int, default=None, help="fly only this idx")
    ap.add_argument("--force", action="store_true", help="re-fly already-flown tasks")
    ap.add_argument("--via-systemd", action="store_true",
                    help="wrap each flight in systemd-run --user --wait (inner exit code propagates)")
    args = ap.parse_args()

    selection = json.load(open(args.selection, encoding="utf-8"))
    draws = selection["draws"]
    if len(draws) != 8:
        sys.exit(f"[err] selection has {len(draws)} draws, expected 8 "
                 f"(the 96-search web budget is 8 x 12)")
    idxs = sorted(int(d["idx"]) for d in draws)
    if args.only is not None:
        if args.only not in idxs:
            sys.exit(f"[err] --only {args.only} not in selection {idxs}")
        idxs = [args.only]

    bin_path = Path(args.bin)
    check_binary(bin_path)
    tasks = load_tasks(args.tasks)
    root = Path(args.run_root)
    root.mkdir(parents=True, exist_ok=True)
    reports_dir = Path(args.reports_dir)
    (reports_dir / "ours").mkdir(parents=True, exist_ok=True)

    failed = []
    for idx in idxs:
        run_dir = root / f"idx-{idx}"
        dest = reports_dir / "ours" / f"idx-{idx}.md"
        if dest.exists() and not args.force:
            print(f"[skip] idx={idx} already flown ({dest}) — --force to re-fly")
            continue
        content = tasks.get(idx, {})
        if not content or not content.get("task"):
            print(f"[err] idx={idx} has no task text")
            failed.append((idx, "no task text"))
            continue
        prompt = content["task"]
        cmd = [str(bin_path), "deep-research", prompt, "--run-dir", str(run_dir)] + ARM_FLAGS
        if args.via_systemd:
            unit = f"drb2-{idx}"
            inner = " ".join(f"'{c}'" for c in cmd)
            full = ["systemd-run", "--user", "--wait", "--collect", f"--unit={unit}",
                    "--", "sh", "-c", inner]
            print(f"[fly] idx={idx} via systemd unit {unit}: {cmd[0]} deep-research <prompt> ...")
        else:
            full = cmd
            print(f"[fly] idx={idx}: {cmd[0]} deep-research <prompt> ...")
        r = subprocess.run(full, capture_output=False)
        if r.returncode != 0:
            print(f"[err] idx={idx} flight exit code {r.returncode} (propagated, never masked)")
            failed.append((idx, r.returncode))
            continue
        # locate the flight's report.md
        drs = sorted(run_dir.glob("dr-*/report.md"))
        if not drs:
            print(f"[err] idx={idx} flight exited 0 but no dr-*/report.md found under {run_dir}")
            failed.append((idx, "no report.md"))
            continue
        src = drs[-1]
        shutil.copy2(src, dest)
        print(f"[ok] idx={idx} report copied {src} -> {dest} ({dest.stat().st_size} bytes)")

    if failed:
        print(f"[done] {len(failed)} flight(s) failed: {failed}")
        sys.exit(1)
    print(f"[done] all {len(idxs)} flights ok")
    sys.exit(0)


if __name__ == "__main__":
    main()
