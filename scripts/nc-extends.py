#!/usr/bin/env python3
"""nc-extends — is adding a thing a DATA change or a CODE change?

The campaign's other three bars count TYPES crossing domain boundaries. That
number can be driven to target by publishing a narrow surface of CONCRETE
types, which is not the same as the architecture being extensible — so this
instrument exists to keep the easy axis from standing in for the goal.

ONE QUESTION PER AXIS: to add one more of this thing, how many Rust sites must
change? An axis is DATA (passes) at zero, and CODE (fails) at anything above.
No partial credit: one `impl` block is the whole difference between shipping a
feature as a file and shipping it as a release.

The score is the count of passing axes, 0-3, matching the hand-scored `today`
already recorded on the bar. That agreement is the instrument's own validation
(ARCH §18.4 — validate the instrument before the result): if this reports
something other than 1 on an unchanged tree, it is measuring something other
than what the bar declared, and the number is void.

Deliberately grep-shaped, with no dependency on the SCIP graph. The three
graph-backed bars all read zero off an empty database on 2026-08-20 and would
have been recorded as total success; an instrument that cannot be silently
emptied is worth more here than a more precise one.
"""
import json
import re
import subprocess
import sys

SKIP = (".claude/worktrees/", "target/", "/tests/")


def rg(pattern, glob="*.rs"):
    """Hit count and files for a pattern over TRACKED sources.

    `git grep` rather than a filesystem walk: the question is how many code
    sites a maintainer must edit, so the universe is what is committed. It also
    cannot wander into `target/` (46GB here) or an untracked agent worktree —
    a plain `grep -r` took 23s and blew the 10s measurement budget.
    """
    out = subprocess.run(
        ["git", "grep", "-nI", "-e", pattern, "--", glob],
        capture_output=True, text=True).stdout.splitlines()
    hits = [l for l in out if not any(s in l for s in SKIP)]
    files = {l.split(":", 1)[0] for l in hits}
    return len(hits), sorted(files)


def axis_tool():
    """Adding a tool: does it need a new `impl Tool for` block?"""
    n, files = rg("impl Tool for")
    return n, files, "each tool is a hand-written trait impl, not a row"


def axis_intent():
    """Adding an intent: how many files fan out per-variant?"""
    n, files = rg(r"Intent::")
    return len(files), files, f"{n} `Intent::` sites fan out across {len(files)} files"


def axis_corpus():
    """Adding a corpus: recipes are TOML, so the Rust side should be zero.

    Counts Rust that enumerates INDIVIDUAL corpora by name — a match arm or
    const list naming specific corpora would mean a new corpus needs a code
    edit. Recipe TOMLs are data and never counted.
    """
    n, files = rg(r"CorpusId::[A-Z]")
    return n, files, "corpora are TOML recipes; no Rust enumerates them by name"


AXES = [("corpus", axis_corpus), ("tool", axis_tool), ("intent", axis_intent)]


def main():
    rows, score = [], 0
    for name, fn in AXES:
        sites, files, note = fn()
        passing = sites == 0
        score += passing
        rows.append({"axis": name, "code_sites": sites, "passes": passing,
                     "note": note, "top_files": files[:5]})

    if "--json" in sys.argv:
        print(json.dumps({"value": score, "axes": rows}, indent=2))
        return 0

    print("EXTENSIBILITY — adding a thing: DATA change, or CODE change?\n")
    print(f"  {'axis':<10} {'rust sites':>11}  {'verdict':<7}  why")
    print("  " + "-" * 76)
    for r in rows:
        v = "DATA" if r["passes"] else "CODE"
        print(f"  {r['axis']:<10} {r['code_sites']:>11}  {v:<7}  {r['note']}")
    print("  " + "-" * 76)
    print(f"\n  score: {score}/3 axes are data-driven")
    if score < 3:
        print("\n  A bar reading 3 means one commit adds a feature as FILES ONLY —")
        print("  no new `impl` block, full suite green. Until then this is the")
        print("  axis the boundary bars cannot speak to.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
