#!/usr/bin/env python3
"""Congestion — distinct crates touched per `.rs` commit, by AUTHOR month.

NOUN_CONVERGENCE §10.7 calls this "the number that says whether any of this
works". It is the only OUTCOME metric in the campaign: if the nouns are
converging, a change stops needing to touch five crates at once.

BUCKET BY AUTHOR DATE, NOT COMMITTER DATE. This history was rewritten around
2026-08-11 and committer dates all cluster there, which silently collapses five
months into two quarters (§10.7).

WHAT COUNTS AS A CRATE. `sovereign/crates/foo/...` -> `sovereign/crates/foo`.
Anything else is bucketed by its top path segment. Collapsing all of
`sovereign/crates/*` into one bucket — the obvious wrong reading — understates
the figure by roughly 40% and was caught by cross-checking against §10.7's
published series before this instrument was trusted (ARCH §18.4).

── THE CONFOUND, MEASURED 2026-08-30 ────────────────────────────────────────
The bar row carried this caveat from the start: "composition may explain part
of the August fall, and a month with more small commits reads lower for reasons
that are not convergence." Nobody had measured it. Measured, it explains not
part but effectively all of it, AND THE SIGN FLIPS.

Raw congestion tracks `.rs` files per commit almost exactly (6.0 -> 20.5 -> 9.2
across Apr/Jun/Aug). Stratify by commit size and compare like with like:

    crates per commit, by .rs files touched   2026-04  2026-06  2026-08
      2-3 files                                  1.42     1.53     1.42
      4-8 files                                  1.84     2.18     2.24
      9-20 files                                 3.05     3.83     4.41
      21+ files                                  4.61     9.59    12.88

Within every band above trivial, dispersion ROSE. The headline fall is mix
shift: August ran 92 single-file commits against July's 67, and 16 large ones
against 23. A raw reading of 2.16 passes a 2.30 ceiling while the quantity the
ceiling is about got worse — a well-formed, exit-0, wrong result (ARCH §18).

So this instrument now reports BOTH:
  value     the raw mean. Unchanged in meaning, because the campaign bar
            (quality/campaigns/noun-convergence.toml) reads this key and
            silently re-pointing a bar is the sin §18.6 exists for.
  adjusted  directly standardized to a fixed commit-size mix (the pooled
            band distribution over the whole range), so a month cannot move
            it by committing differently. THIS is the one to read.

A month missing a band is reported, never dropped silently (ARCH §18.3).
"""
import collections
import json
import re
import subprocess
import sys

SHA_MONTH = re.compile(r"^([0-9a-f]{40}) (\d{4}-\d{2})$")

# Commit-size bands. A 1-file commit scores 1.00 by construction and carries no
# information about coupling; it is kept as its own band so the mix shift into
# it is visible rather than absorbed.
BANDS = [(1, 1), (2, 3), (4, 8), (9, 20), (21, 10**9)]
MIN_BAND_N = 8          # below this a band's mean is noise, and is not used


def band_label(lo, hi):
    return f"{lo}-{hi}" if hi < 10**9 else f"{lo}+"


def crate_of(path: str) -> str:
    parts = path.split("/")
    if len(parts) >= 3 and parts[1] == "crates":
        return "/".join(parts[:3])
    return parts[0]


def commits(since: str = "2026-03-01"):
    """[(month, distinct_crates, rs_files)] — one row per `.rs`-touching commit."""
    out = subprocess.run(
        ["git", "log", f"--since={since}", "--pretty=format:%H %ad",
         "--date=format:%Y-%m", "--name-only", "--", "*.rs"],
        capture_output=True, text=True, check=True).stdout
    rows, sha, month, files = [], None, None, set()
    for line in out.splitlines():
        line = line.rstrip()
        if not line:
            continue
        m = SHA_MONTH.match(line)
        if m:
            if sha and files:
                rows.append((month, len({crate_of(f) for f in files}), len(files)))
            sha, month, files = m.group(1), m.group(2), set()
            continue
        files.add(line)
    if sha and files:
        rows.append((month, len({crate_of(f) for f in files}), len(files)))
    return rows


def band_of(nfiles):
    for lo, hi in BANDS:
        if lo <= nfiles <= hi:
            return (lo, hi)
    return BANDS[-1]


def analyse(rows):
    by_month = collections.defaultdict(list)
    for mo, crates, nfiles in rows:
        by_month[mo].append((crates, nfiles))

    # Per (month, band) mean, and the pooled band mix used as the reference.
    cell = collections.defaultdict(list)
    pooled = collections.Counter()
    for mo, crates, nfiles in rows:
        b = band_of(nfiles)
        cell[(mo, b)].append(crates)
        pooled[b] += 1

    out = {}
    for mo, obs in by_month.items():
        raw = sum(c for c, _ in obs) / len(obs)
        used, skipped, num, den = {}, [], 0.0, 0.0
        for b in BANDS:
            vals = cell.get((mo, b), [])
            if len(vals) < MIN_BAND_N:
                if pooled[b]:
                    skipped.append(band_label(*b))
                continue
            mean = sum(vals) / len(vals)
            used[band_label(*b)] = round(mean, 2)
            num += pooled[b] * mean
            den += pooled[b]
        out[mo] = {
            "raw": round(raw, 2),
            "n": len(obs),
            "files_per_commit": round(sum(f for _, f in obs) / len(obs), 2),
            "adjusted": round(num / den, 2) if den else None,
            "bands": used,
            "bands_skipped": skipped,
        }
    return out, {band_label(*b): pooled[b] for b in BANDS}


def main() -> int:
    as_json = "--json" in sys.argv
    rows = commits()
    if not rows:
        print("nc-congestion: no commits in range — value NOT reported", file=sys.stderr)
        return 3
    s, mix = analyse(rows)
    months = sorted(s)
    latest = months[-1]
    cur = s[latest]
    head = subprocess.run(["git", "rev-parse", "HEAD"],
                          capture_output=True, text=True).stdout.strip()

    if as_json:
        print(json.dumps({
            # UNCHANGED KEY, UNCHANGED MEANING — the campaign bar reads this.
            "value": cur["raw"],
            # The de-confounded reading. Read this one.
            "adjusted": cur["adjusted"],
            "commit": head,
            "month": latest,
            "commits_in_month": cur["n"],
            "series": {mo: s[mo]["raw"] for mo in months},
            "series_adjusted": {mo: s[mo]["adjusted"] for mo in months},
            "bands": {mo: s[mo]["bands"] for mo in months},
            "bands_skipped": {mo: s[mo]["bands_skipped"] for mo in months
                              if s[mo]["bands_skipped"]},
            "reference_mix": mix,
        }))
        return 0

    print("\n  congestion — distinct crates per `.rs` commit, AUTHOR month")
    print("  raw is confounded by commit size; adjusted standardizes the mix.\n")
    print(f"  {'month':9} {'raw':>6} {'adj':>6} {'files/commit':>13}  n")
    for mo in months:
        d = s[mo]
        adj = f"{d['adjusted']:.2f}" if d["adjusted"] is not None else "  --"
        print(f"  {mo:9} {d['raw']:>6.2f} {adj:>6} {d['files_per_commit']:>13.2f}  {d['n']}")

    print("\n  by commit size (crates per commit, like-for-like):")
    labels = [band_label(*b) for b in BANDS]
    print("    " + "band".ljust(8) + "".join(f"{mo:>9}" for mo in months))
    for lab in labels:
        cells = "".join(
            f"{s[mo]['bands'][lab]:>9.2f}" if lab in s[mo]["bands"] else f"{'n<8':>9}"
            for mo in months)
        print("    " + lab.ljust(8) + cells)

    skipped = {mo: s[mo]["bands_skipped"] for mo in months if s[mo]["bands_skipped"]}
    if skipped:
        print("\n  bands with too few commits to use (reported, not dropped):")
        for mo, b in skipped.items():
            print(f"    {mo}: {', '.join(b)}")

    print(f"\n  latest: {latest} — raw {cur['raw']:.2f}, adjusted "
          f"{cur['adjusted'] if cur['adjusted'] is not None else '--'} over {cur['n']} commits")
    print("\n  §10.7: the number that says whether any of this works.")
    print("  The RAW series falls; that fall is commit-size mix. Held to a fixed")
    print("  mix, dispersion rose in every band above trivial. A CEILING to hold,")
    print("  not a climb — and the ceiling should be read on `adjusted`.\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
