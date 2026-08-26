#!/usr/bin/env python3
"""Role adoption — did authors reach for the shared thing, unprompted?

NOUN_CONVERGENCE §10.7's pre-registered adoption test. §10.3's finding is that
adoption is monotone in WORK CARRIED and in nothing else: `Tool` carries
dispatch + execution and reaches 52%; `Response` carries nothing and reaches 2%.
`Report` at 3% is the control experiment already run — the most obvious shared
vocabulary candidate in the codebase, which never spread.

A MIRROR, NOT A GATE, and deliberately so. If adoption rises only where it was
mandated, the abstraction did not win on cost and the honest outcome is DELETION
(§10.8 row 3). Never add a gate to rescue this number: a gate converts the
experiment into its own confirmation.

WHY THIS WRAPPER EXISTS rather than a one-liner over `converge roles --json`:
that JSON reports `adoption` as a FRACTION (0.029), not a percent, and carries
NO graph commit at all — the human renderer prints `graph: <sha>` but the JSON
consumer cannot tell which graph the number describes. Both would have produced
a silently wrong bar row. The commit is taken from the same index provenance the
sibling instruments use (ARCH §18.4: validate the instrument before the result).
"""
import argparse
import json
import os
import sqlite3
import subprocess
import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent
import importlib.util
_spec = importlib.util.spec_from_file_location("nc_boundary", _HERE / "nc-boundary.py")
nb = importlib.util.module_from_spec(_spec)
sys.modules["nc_boundary"] = nb
_spec.loader.exec_module(nb)

BIN = _HERE.parent / "target" / "debug" / "sovereign-cli"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--role", default="Report", help="role head noun to report (default: Report)")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    if not BIN.exists():
        print(f"nc-adoption: {BIN} not built", file=sys.stderr)
        return 4
    proc = subprocess.run([str(BIN), "code", "converge", "roles", "--json"],
                          capture_output=True, text=True)
    if proc.returncode != 0:
        print(f"nc-adoption: converge roles exited {proc.returncode}", file=sys.stderr)
        return 3
    doc = json.loads(proc.stdout)
    rows = [r for r in doc.get("roles", []) if r.get("role") == args.role]
    if not rows:
        # Absence is reported, never defaulted (ARCH §18.3). A role that is not
        # in the table is NOT a role at 0% — it means the census did not see it.
        print(f"nc-adoption: role {args.role!r} not present in the roles table — "
              f"value NOT reported", file=sys.stderr)
        return 3
    row = rows[0]
    pct = 100.0 * float(row["adoption"])

    if not os.path.exists(nb.GRAPH):
        print(f"nc-adoption: no graph at {nb.GRAPH} — svrn refresh", file=sys.stderr)
        return 4
    db = sqlite3.connect(f"file:{nb.GRAPH}?mode=ro", uri=True)
    indexed_head, indexed_at = nb.index_provenance(db)

    if args.json:
        print(json.dumps({
            "value": round(pct, 2),
            "commit": indexed_head,
            "role": args.role,
            "population": row["population"],
            "adopted": row["adopted"],
            "crates": row["crates"],
            "best": row.get("best"),
            "indexed_at": indexed_at,
        }))
        return 0

    print(f"\n  role adoption — {args.role}\n")
    print(f"  population   {row['population']:>6} types across {row['crates']} crates")
    print(f"  adopted      {row['adopted']:>6}  (reaching 3+ distinct crates)")
    print(f"  adoption     {pct:>6.1f}%")
    b = row.get("best") or {}
    if b:
        print(f"  best         {b.get('name')} in {b.get('krate')}, reach {b.get('reach')}")
    print(f"\n  index describes {(indexed_head or '(unknown)')[:12]}")
    print("\n  Extract WORK, not shape (§10.3). If this only rises where it was")
    print("  mandated, the honest outcome is deletion, not enforcement.\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
