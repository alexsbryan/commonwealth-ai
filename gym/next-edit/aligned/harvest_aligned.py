#!/usr/bin/env python3
"""Mine the index-aligned next-edit bank (see README.md — PRE-REGISTERED).

The whole idea is one constraint: for every `.rs` file alive at HEAD, take the
MOST RECENT commit that touched it. No later commit modified that file, so its
content at that commit is byte-identical to HEAD — and the SCIP index, which
describes HEAD, therefore describes exactly the state the episode is mined
from. The constraint is ASSERTED per case, not assumed.

Episode construction is `harvest.py`'s, reused unchanged: this file chooses
WHICH (commit, file) pairs to mine, and nothing about how a case is built.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("harvest", HERE.parent / "harvest.py")
H = importlib.util.module_from_spec(spec)
spec.loader.exec_module(H)


def last_touching_commits(window: int) -> dict[str, str]:
    """path -> the newest commit that touched it, in one newest-first pass."""
    out = subprocess.run(
        ["git", "log", "--no-merges", "--format=%x00%H", "--name-only", f"-n{window}"],
        capture_output=True, text=True, check=True).stdout
    last: dict[str, str] = {}
    cur = None
    for line in out.split("\n"):
        if line.startswith("\x00"):
            cur = line[1:].strip()
            continue
        p = line.strip()
        if p and p not in last and cur:
            last[p] = cur
    return last


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--window", type=int, default=3000, help="commits to walk")
    ap.add_argument("--pos", type=int, default=200)
    ap.add_argument("--neg", type=int, default=100)
    # harvest.py caps a rule at 2 cases to keep a 120-case bank diverse. The
    # aligned pool holds only 134 groups of >=3 in TOTAL, so that cap discards
    # data this bank cannot spare. Raised to 3, recorded in README.md, chosen
    # before any case was scored.
    ap.add_argument("--rule-cap", type=int, default=3)
    ap.add_argument("--suffix", default=".rs",
                    help="the index carries scip for rust only (scip_meta.languages_with_scip)")
    ap.add_argument("--out", type=Path, default=HERE / "cases.jsonl")
    args = ap.parse_args()

    last = last_touching_commits(args.window)
    counters = {"replay_did_not_fire": 0, "held_site_mismatch": 0,
                "held_site_shadowed": 0, "commits_scanned": 0,
                "not_aligned": 0, "candidates": 0}
    pos, neg = [], []
    rule_use: dict[tuple, int] = {}
    aligned_paths: list[str] = []

    for path, commit in sorted(last.items()):
        if len(pos) >= args.pos and len(neg) >= args.neg:
            break
        if not path.endswith(args.suffix) or Path(path).suffix not in H.LANG_OF:
            continue
        if any(s in path for s in H.SKIP_SUBSTR) or not Path(path).exists():
            continue
        counters["candidates"] += 1
        try:
            new_b = H.git("show", f"{commit}:{path}")
            old_b = H.git("show", f"{commit}^:{path}")
        except subprocess.CalledProcessError:
            continue
        # THE CONSTRUCTION, ASSERTED: the mined state must BE the indexed state.
        # A file that fails this is dropped and counted, never quietly kept.
        if new_b != Path(path).read_bytes():
            counters["not_aligned"] += 1
            continue
        if len(old_b) > H.MAX_FILE_BYTES or len(new_b) > H.MAX_FILE_BYTES:
            continue
        try:
            old, new = old_b.decode(), new_b.decode()
        except UnicodeDecodeError:
            continue
        hunks = H.hunks_of(old, new)
        if not hunks:
            continue
        aligned_paths.append(path)
        counters["commits_scanned"] += 1

        groups: dict[tuple, list[dict]] = {}
        for h in hunks:
            groups.setdefault(H.rule_key(h["rule"]), []).append(h)

        made_here = 0
        for key, group in sorted(groups.items()):
            if len(pos) >= args.pos or made_here >= 2:
                break
            if len(group) < 3 or rule_use.get(key, 0) >= args.rule_cap:
                continue
            case = H.build_positive(commit, path, old, group, counters)
            if case:
                case["aligned"] = {"commit": commit, "repo_path": path}
                pos.append(case)
                rule_use[key] = rule_use.get(key, 0) + 1
                made_here += 1

        if len(neg) < args.neg:
            singles = [g[0] for g in groups.values() if len(g) == 1]
            case = H.build_neg_dissimilar(commit, path, old, singles)
            if case:
                case["aligned"] = {"commit": commit, "repo_path": path}
                neg.append(case)
        if len(neg) < args.neg:
            for key, group in sorted(groups.items()):
                case = H.build_neg_exhausted(commit, path, old, group)
                if case:
                    case["aligned"] = {"commit": commit, "repo_path": path}
                    neg.append(case)
                    break

    cases = pos + neg
    with args.out.open("w", encoding="utf-8") as fh:
        for c in cases:
            fh.write(json.dumps(c, ensure_ascii=False) + "\n")
    print(f"wrote {len(cases)} cases ({len(pos)} pos, {len(neg)} neg) to {args.out}")
    print(f"  files considered: {counters['candidates']}, "
          f"aligned + with hunks: {len(aligned_paths)}")
    print(f"  counters: {counters}")
    if counters["not_aligned"]:
        print(f"  note: {counters['not_aligned']} file(s) failed the byte-identity "
              f"assertion and were DROPPED (expected 0 by construction — a non-zero "
              f"count means the working tree is dirty)", file=sys.stderr)


if __name__ == "__main__":
    main()
