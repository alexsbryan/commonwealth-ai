#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Which desktop commands did a run actually reach?

Diffs the commands registered in the desktop's `generate_handler!` against the
ones recorded at the invoke chokepoint by `invoke_coverage.rs`.

    # 1. record a run
    SOVEREIGN_INVOKE_COVERAGE=/tmp/invoked.txt npm run test:e2e:real

    # 2. read the answer
    scripts/desktop-invoke-coverage.py --recorded /tmp/invoked.txt

Why this metric and not an assertion count: assertion counts inflate for free.
You can add ten asserts to a spec that already passes and move the number
without covering anything new. The only way to move THIS number is to reach a
command you were not reaching before, which makes it a poor thing to game and a
good thing to track.

`--min-percent` turns it into a ratchet for CI. Prefer raising it as coverage
lands over setting it aspirationally high and muting the failure.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
MAIN_RS = REPO / "sovereign/crates/sovereign-desktop/src-tauri/src/main.rs"

# `commands::foo,` / `foo,` — strip any module path, keep the command name.
_ENTRY = re.compile(r"^(?:[A-Za-z0-9_]+::)*([a-z_][A-Za-z0-9_]*)\s*,?\s*$")


def registered(main_rs: Path) -> set[str]:
    """Command names inside `tauri::generate_handler![ ... ]`.

    Parsed rather than hand-maintained: a list that has to be kept in sync by
    hand is a list that silently rots, and this whole tool exists because of a
    number nobody was maintaining.
    """
    text = main_rs.read_text(encoding="utf-8")
    start = text.find("generate_handler![")
    if start == -1:
        sys.exit(f"no generate_handler! found in {main_rs}")
    start += len("generate_handler![")

    depth, end = 1, None
    for i in range(start, len(text)):
        if text[i] == "[":
            depth += 1
        elif text[i] == "]":
            depth -= 1
            if depth == 0:
                end = i
                break
    if end is None:
        sys.exit("unterminated generate_handler! block")

    names: set[str] = set()
    for raw in text[start:end].splitlines():
        line = raw.split("//", 1)[0].strip()
        if not line:
            continue
        for part in line.split(","):
            part = part.strip()
            if not part:
                continue
            m = _ENTRY.match(part + ",")
            if m:
                names.add(m.group(1))
    return names


def recorded(path: Path) -> set[str]:
    if not path.exists():
        sys.exit(
            f"no coverage file at {path}\n"
            "Run with SOVEREIGN_INVOKE_COVERAGE=<path> set, or the app records nothing."
        )
    return {
        line.strip()
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--recorded", type=Path, required=True, help="file written by invoke_coverage")
    ap.add_argument("--main-rs", type=Path, default=MAIN_RS)
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--list-uncovered", action="store_true", help="print every unreached command")
    ap.add_argument(
        "--min-percent",
        type=float,
        default=None,
        help="exit 1 below this coverage percentage (CI ratchet)",
    )
    args = ap.parse_args()

    reg = registered(args.main_rs)
    hit = recorded(args.recorded)

    covered = reg & hit
    uncovered = sorted(reg - hit)
    # Recorded-but-unregistered means the parser and the app disagree. Surface
    # it: silently dropping it would let the denominator drift unnoticed.
    unknown = sorted(hit - reg)
    pct = (len(covered) / len(reg) * 100) if reg else 0.0

    if args.json:
        print(json.dumps({
            "registered": len(reg),
            "covered": len(covered),
            "percent": round(pct, 1),
            "uncovered": uncovered,
            "recorded_but_not_registered": unknown,
        }, indent=2))
    else:
        print(f"desktop invoke coverage: {len(covered)}/{len(reg)} commands reached ({pct:.1f}%)")
        if unknown:
            print(f"  ! {len(unknown)} recorded but not registered (parser drift?): {', '.join(unknown[:5])}")
        if args.list_uncovered and uncovered:
            print(f"\n  {len(uncovered)} unreached:")
            for name in uncovered:
                print(f"    {name}")
        elif uncovered:
            print(f"  {len(uncovered)} unreached — rerun with --list-uncovered to enumerate")

    if args.min_percent is not None and pct < args.min_percent:
        print(
            f"\nFAIL: coverage {pct:.1f}% is below the {args.min_percent:.1f}% floor.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
