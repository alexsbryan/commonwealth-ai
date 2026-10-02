#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["datasets>=3.0"]
# ///
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Sample SWE-bench Verified and materialize the shared repo cache.

Stratified by `difficulty` and capped per repo, so a 100-instance cut
stays representative instead of collapsing onto whichever project has
the most instances (django, by a lot).

The gold `patch` is split into `gold/` and never enters `instances.jsonl`
— the arms read only the working record, so no arm can be handed the
answer by accident.

    ./prepare.py --n 100            # sample + write instances.jsonl
    ./prepare.py --n 100 --clone    # …and pre-clone every repo (slow, one-time)
"""

from __future__ import annotations

import argparse
import json
import random
import sys
from collections import defaultdict
from pathlib import Path

from datasets import load_dataset

sys.path.insert(0, str(Path(__file__).parent))
from lib import ROOT, Instance, ensure_bare  # noqa: E402

DATASET = "princeton-nlp/SWE-bench_Verified"
FIELDS = list(Instance.__dataclass_fields__)


def stratified(rows: list[dict], n: int, seed: int, per_repo_cap: int) -> list[dict]:
    by_diff: dict[str, list[dict]] = defaultdict(list)
    for r in rows:
        by_diff[r["difficulty"]].append(r)
    rng = random.Random(seed)
    for v in by_diff.values():
        rng.shuffle(v)

    # Round-robin across difficulty bands, honouring a per-repo cap so no
    # single project dominates the cut.
    picked: list[dict] = []
    repo_count: dict[str, int] = defaultdict(int)
    cursors = {k: 0 for k in by_diff}
    while len(picked) < n:
        progressed = False
        for band in sorted(by_diff):
            pool, i = by_diff[band], cursors[band]
            while i < len(pool):
                cand = pool[i]
                i += 1
                if repo_count[cand["repo"]] < per_repo_cap:
                    picked.append(cand)
                    repo_count[cand["repo"]] += 1
                    progressed = True
                    break
            cursors[band] = i
            if len(picked) >= n:
                break
        if not progressed:
            break
    return picked


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--n", type=int, default=100, help="instances to sample; 0 = all 500")
    p.add_argument("--seed", type=int, default=1729)
    p.add_argument("--per-repo-cap", type=int, default=12)
    p.add_argument("--clone", action="store_true", help="pre-clone every repo now")
    args = p.parse_args()

    rows = [dict(r) for r in load_dataset(DATASET, split="test")]
    picked = rows if args.n == 0 else stratified(rows, args.n, args.seed, args.per_repo_cap)

    (ROOT / "gold").mkdir(exist_ok=True)
    with (ROOT / "instances.jsonl").open("w") as fh:
        for r in picked:
            fh.write(json.dumps({k: r[k] for k in FIELDS}) + "\n")
    with (ROOT / "gold" / "gold.jsonl").open("w") as fh:
        for r in picked:
            fh.write(
                json.dumps(
                    {
                        "instance_id": r["instance_id"],
                        "patch": r["patch"],
                        "test_patch": r["test_patch"],
                        "FAIL_TO_PASS": r["FAIL_TO_PASS"],
                        "PASS_TO_PASS": r["PASS_TO_PASS"],
                    }
                )
                + "\n"
            )

    by_diff: dict[str, int] = defaultdict(int)
    by_repo: dict[str, int] = defaultdict(int)
    for r in picked:
        by_diff[r["difficulty"]] += 1
        by_repo[r["repo"]] += 1
    print(f"sampled {len(picked)} of {len(rows)}  seed={args.seed}")
    print("  difficulty:", dict(sorted(by_diff.items())))
    print("  repos:", dict(sorted(by_repo.items(), key=lambda kv: -kv[1])))
    print(f"  wrote {ROOT / 'instances.jsonl'} (gold held out in gold/gold.jsonl)")

    if args.clone:
        seen = set()
        for r in picked:
            inst = Instance(**{k: r[k] for k in FIELDS})
            if inst.slug in seen:
                continue
            seen.add(inst.slug)
            print(f"  cloning {inst.repo} …", flush=True)
            ensure_bare(inst)
        print(f"  {len(seen)} bare repos cached in {ROOT / 'repos'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
