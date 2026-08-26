#!/usr/bin/env python3
"""Congestion — distinct crates touched per `.rs` commit, by AUTHOR month.

NOUN_CONVERGENCE §10.7 calls this "the number that says whether any of this
works". It is the only outcome metric measurable with no new tooling: if the
nouns are converging, a change stops needing to touch five crates at once.

BUCKET BY AUTHOR DATE, NOT COMMITTER DATE. This history was rewritten around
2026-08-11 and committer dates all cluster there, which silently collapses five
months into two quarters (§10.7).

WHAT COUNTS AS A CRATE. `sovereign/crates/foo/...` -> `sovereign/crates/foo`.
Anything else is bucketed by its top path segment. Collapsing all of
`sovereign/crates/*` into one bucket — the obvious wrong reading — understates
the figure by roughly 40% and was caught by cross-checking against §10.7's
published series before this instrument was trusted (ARCH §18.4: validate the
instrument before the result).

A MIRROR, NOT A GATE (§10.7). The bar holds a ceiling; it is not a climb.
"""
import collections
import json
import re
import subprocess
import sys

SHA_MONTH = re.compile(r"^([0-9a-f]{40}) (\d{4}-\d{2})$")


def crate_of(path: str) -> str:
    parts = path.split("/")
    if len(parts) >= 3 and parts[1] == "crates":
        return "/".join(parts[:3])
    return parts[0]


def series(since: str = "2026-03-01"):
    out = subprocess.run(
        ["git", "log", f"--since={since}", "--pretty=format:%H %ad",
         "--date=format:%Y-%m", "--name-only", "--", "*.rs"],
        capture_output=True, text=True, check=True).stdout
    counts = collections.defaultdict(list)
    sha = month = None
    cur: set[str] = set()
    for line in out.splitlines():
        line = line.rstrip()
        if not line:
            continue
        m = SHA_MONTH.match(line)
        if m:
            if sha and cur:
                counts[month].append(len(cur))
            sha, month = m.group(1), m.group(2)
            cur = set()
            continue
        cur.add(crate_of(line))
    if sha and cur:
        counts[month].append(len(cur))
    return {mo: (sum(v) / len(v), len(v)) for mo, v in counts.items()}


def main() -> int:
    as_json = "--json" in sys.argv
    s = series()
    if not s:
        print("nc-congestion: no commits in range — value NOT reported", file=sys.stderr)
        return 3
    months = sorted(s)
    latest = months[-1]
    value, n = s[latest]
    head = subprocess.run(["git", "rev-parse", "HEAD"],
                          capture_output=True, text=True).stdout.strip()

    if as_json:
        print(json.dumps({
            "value": round(value, 2),
            "commit": head,
            "month": latest,
            "commits_in_month": n,
            "series": {mo: round(v, 2) for mo, (v, _) in s.items()},
        }))
        return 0

    print("\n  congestion — distinct crates per `.rs` commit, AUTHOR month\n")
    for mo in months:
        v, k = s[mo]
        bar = "#" * int(v * 8)
        print(f"  {mo}  {v:>5.2f}  n={k:<5} {bar}")
    print(f"\n  latest: {latest} at {value:.2f} over {n} commits")
    print("\n  §10.7: the number that says whether any of this works.")
    print("  It rose across the accretion period and bends down while the")
    print("  campaign runs. A CEILING to hold, not a climb.\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
