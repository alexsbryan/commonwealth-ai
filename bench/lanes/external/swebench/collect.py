#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# ///
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Assemble one arm's per-instance patches into a predictions.jsonl.

Also reports the pre-grading facts that decide whether a run is even
gradeable: how many instances the arm attempted, how many produced an
empty patch, and how many are missing entirely. An arm that emitted 40
empty patches out of 100 has not scored 0.0 — it has scored on 60 and
could-not-judge on 40, and the summary says so rather than letting the
harness silently count absence as failure.

    ./collect.py --arm native
    ./collect.py --arm comaintainer --out predictions/comaintainer.jsonl
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from lib import ROOT, load_instances  # noqa: E402


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--arm", required=True)
    p.add_argument("--out", default=None)
    args = p.parse_args()

    src = ROOT / "preds" / args.arm
    if not src.is_dir():
        print(f"no predictions for arm {args.arm!r} at {src}", file=sys.stderr)
        return 1

    expected = {i.instance_id for i in load_instances()}
    rows, empty = [], []
    for f in sorted(src.glob("*.json")):
        d = json.loads(f.read_text())
        rows.append(d)
        if not d.get("model_patch", "").strip():
            empty.append(d["instance_id"])

    got = {r["instance_id"] for r in rows}
    missing = sorted(expected - got)

    out = Path(args.out) if args.out else ROOT / "predictions" / f"{args.arm}.jsonl"
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as fh:
        for r in sorted(rows, key=lambda x: x["instance_id"]):
            fh.write(json.dumps(r) + "\n")

    print(f"arm={args.arm}")
    print(f"  expected      {len(expected)}")
    print(f"  attempted     {len(rows)}")
    print(f"  empty patch   {len(empty)}   (submitted, will grade as unresolved)")
    print(f"  MISSING       {len(missing)}   (never ran — not a failure, a gap)")
    if missing:
        print("    " + ", ".join(missing[:8]) + (" …" if len(missing) > 8 else ""))
    print(f"  wrote {out}")
    if missing:
        print(
            "\nA denominator that includes never-ran instances is not a score. "
            "Re-run with --resume, or grade against the attempted set only.",
            file=sys.stderr,
        )
        return 4
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
