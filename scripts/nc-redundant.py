#!/usr/bin/env python3
"""First-party redundant lines — the BEHAVIOUR tier of noun convergence.

Wraps `svrn code dry-report`, which finds repeated code from the per-symbol
embeddings already in the corpus index. Two tiers: exact clones (byte-identical
by content hash) and near clones (cosine >= threshold).

WHY THIS BAR EXISTS. The type census (`converge census`) keys on NAMES and the
reach bar keys on VISIBILITY; neither can see one decider copied three ways
under three different names, which is what this campaign kept actually finding
(NOUN_CONVERGENCE §10.6: "the two instruments see different halves and the
halves need different fixes").

WHAT THE NUMBER DESCRIBES. dry-report reads the SCIP/corpus index, NOT the
working tree, so like nc-boundary.py and nc-reach.py its figure describes
`last_indexed_head`. That commit is emitted so a row can never be mistaken for
one about HEAD. dry-report prints no commit stamp of its own, so we take it
from the same index provenance the sibling instruments use.

A MIRROR, NOT A GATE (§10.7): advisory. A human decides what to factor out.
"""
import json
import os
import re
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

BIN = Path(__file__).resolve().parent.parent / "target" / "debug" / "sovereign-cli"
# "**199** exact-clone groups · **467** near-clone clusters · ~**14162** redundant lines"
HEADLINE = re.compile(
    r"\*\*(\d+)\*\*\s+exact-clone groups.*?\*\*(\d+)\*\*\s+near-clone clusters.*?~\*\*(\d+)\*\*\s+redundant lines"
)


def main() -> int:
    as_json = "--json" in sys.argv
    if not BIN.exists():
        print(f"nc-redundant: {BIN} not built — cargo build -p sovereign-cli --features dev-tools",
              file=sys.stderr)
        return 4
    proc = subprocess.run([str(BIN), "code", "dry-report"], capture_output=True, text=True)
    if proc.returncode != 0:
        print(f"nc-redundant: dry-report exited {proc.returncode}", file=sys.stderr)
        print(proc.stderr[-2000:], file=sys.stderr)
        return 3
    m = HEADLINE.search(proc.stdout)
    if not m:
        # Absence is reported, never defaulted (ARCH §18.3). A missing headline
        # means the instrument changed its output shape; it does NOT mean zero.
        print("nc-redundant: could not find the dry-report headline — instrument shape changed, "
              "value NOT reported", file=sys.stderr)
        return 3
    exact, near, lines = int(m.group(1)), int(m.group(2)), int(m.group(3))

    if not os.path.exists(nb.GRAPH):
        print(f"nc-redundant: no graph at {nb.GRAPH} — svrn refresh", file=sys.stderr)
        return 4
    db = sqlite3.connect(f"file:{nb.GRAPH}?mode=ro", uri=True)
    indexed_head, indexed_at = nb.index_provenance(db)

    if as_json:
        print(json.dumps({
            "value": float(lines),
            "commit": indexed_head,
            "redundant_lines": lines,
            "exact_groups": exact,
            "near_clusters": near,
            "indexed_head": indexed_head,
            "indexed_at": indexed_at,
        }))
        return 0

    print("\n  first-party redundant lines — the behaviour tier\n")
    print(f"  exact-clone groups   {exact:>7}")
    print(f"  near-clone clusters  {near:>7}")
    print(f"  redundant lines      {lines:>7}   (lower bound)")
    print(f"\n  index describes {(indexed_head or '(unknown)')[:12]}")
    print("\n  A MIRROR, not a gate. This is where one decider copied three ways")
    print("  shows up — the name census and the reach bar are both blind to it")
    print("  (NOUN_CONVERGENCE §10.6).\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
