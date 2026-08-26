#!/usr/bin/env python3
"""factory-scale.py — the refactor factory's THROUGHPUT bars.

Operator standard, 2026-08-23, verbatim: "if we're not actually able to scale the
refactor factory to churn 10k+ lines of code in one session safely I'll question
how much we've actually built an effective factory for the work we're doing."

TWO BARS, BECAUSE ONE NUMBER WOULD SELECT AGAINST THE DESIGN.

  factory-scale     mentions brought under TYPE CONTROL by factory rungs.
  factory-deletion  net lines REMOVED by a single rung.

WHY NOT ONE "LINES CHURNED" BAR. The factory's economic argument is that a ~50
line `prepare` edit makes thousands of call-site edits UNNECESSARY: once
`CorpusId` carries AsRef<str> / Borrow<str> / PartialEq<str>, most of corpus_id's
6,935 mentions keep compiling untouched. So on the newtype kind, H3 SUCCEEDING
MEANS FEWER LINES CHANGE. A lines-churned bar is maximised by skipping prepare
and hand-editing every call site — the metric would select for the worse machine.
Hence `factory-scale`, which counts what came under type control rather than what
was typed over.

The 10k line standard is kept, and kept where it is HONEST: the merge-shape and
delete-loser kinds, where converging really does delete a type, its impls and its
conversions. 282 duplicate types across 112 groups at even ~40 lines each is the
10k session, and deletion is not gameable by verbosity — you cannot pad a
negative.

A RUNG IS A SESSION. `factory-deletion` reports the MAX over rungs, not the sum,
because the standard is about single-session capability. Rungs are grouped by the
`rf-N` token in the commit subject, which is how a rung's commits are already
tagged; the sum is reported too, and is never the headline.

FLOORS ARE ZERO AND THAT IS A MEASUREMENT, NOT AN ASSUMPTION: at 63c72af8
`svrn code refactor` returns "Unknown code subcommand: refactor", no `rf-` commit
exists, and `corpus_id: CorpusId` has 0 declaration sites against 369 on String.
"""
import argparse, collections, json, pathlib, re, subprocess, sys

# The atoms the factory has scheduled. (field name -> converged type).
# An atom enters this registry when its entry gate PASSES, never before.
ATOMS = {"corpus_id": "CorpusId"}

EXCLUDE = {"vendor", "node_modules", ".cargo-container", "research",
           "external", "target", ".claude", "tests", "benches", "examples"}
RUNG_RE = re.compile(r'\brf-(\d+)\b', re.I)


def sh(a):
    return subprocess.run(a, capture_output=True, text=True, cwd=REPO).stdout


def in_scope(p):
    return p.endswith(".rs") and not (set(pathlib.PurePath(p).parts) & EXCLUDE)


def rungs():
    """rf-N -> [shas]. Grouped by rung because the standard is per-session."""
    out, sha = collections.defaultdict(list), None
    for line in sh(["git", "log", "--format=@@C %H%n%s%n%b"]).splitlines():
        if line.startswith("@@C "):
            sha = line[4:].strip()
        elif sha:
            m = RUNG_RE.search(line)
            if m and sha not in out[f"rf-{m.group(1)}"]:
                out[f"rf-{m.group(1)}"].append(sha)
    return dict(out)


def net_removed(shas):
    """deletions - insertions over in-scope .rs, rename-aware so a move is 0."""
    ins = dele = 0
    for sha in shas:
        for row in sh(["git", "show", "--numstat", "--format=", "-M", "-C", sha]).splitlines():
            parts = row.split("\t")
            if len(parts) != 3 or parts[0] == "-":
                continue
            if in_scope(parts[2]):
                ins += int(parts[0]); dele += int(parts[1])
    return dele - ins


def under_type_control():
    """Mentions of an atom, prorated by how far its declarations are converted.

    Prorated rather than all-or-nothing: a half-migrated atom has genuinely
    brought half its surface under the compiler, and rounding that to zero would
    make the bar jump discontinuously at the last call site.
    """
    total, detail = 0, {}
    for atom, typ in ATOMS.items():
        raw = len(sh(["git", "grep", "-h", "-E",
                      # POSIX ERE: git grep -E does NOT understand \s
                      rf'^[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?'
                      rf'{atom}:[[:space:]]*String[[:space:]]*,?[[:space:]]*$',
                      "HEAD", "--", "*.rs"]).splitlines())
        typed = len(sh(["git", "grep", "-h", "-E",
                        rf'^[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?'
                        rf'{atom}:[[:space:]]*{typ}[[:space:]]*,?[[:space:]]*$',
                        "HEAD", "--", "*.rs"]).splitlines())
        mentions = 0
        for row in sh(["git", "grep", "-c", atom, "HEAD", "--", "*.rs"]).splitlines():
            bits = row.split(":")
            if len(bits) >= 3 and in_scope(bits[1]):
                mentions += int(bits[-1])
        share = (typed / (typed + raw)) if (typed + raw) else 0.0
        controlled = int(mentions * share)
        total += controlled
        detail[atom] = {"type": typ, "raw_decls": raw, "typed_decls": typed,
                        "mentions": mentions, "converted_share": round(share, 4),
                        "under_control": controlled}
    return total, detail


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--emit", choices=["scale", "deletion"], default="scale")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()

    rg = rungs()
    per_rung = {k: net_removed(v) for k, v in rg.items()}
    controlled, detail = under_type_control()

    res = {
        "commit": sh(["git", "rev-parse", "HEAD"]).strip(),
        "dirty": bool(sh(["git", "status", "--porcelain"]).strip()),
        "rungs_found": sorted(rg),
        "atoms": detail,
        "under_type_control": controlled,
        "net_removed_per_rung": per_rung,
        "net_removed_max_rung": max(per_rung.values()) if per_rung else 0,
        "net_removed_total": sum(per_rung.values()) if per_rung else 0,
    }
    res["bar"] = f"factory-{a.emit}"
    res["value"] = controlled if a.emit == "scale" else res["net_removed_max_rung"]

    if a.json:
        print(json.dumps(res))       # single line: co-lineage reads the LAST line
        return 0

    print(f"factory-{a.emit}: {res['value']}")
    print(f"  rungs: {', '.join(res['rungs_found']) or 'none yet'}")
    for k, d in detail.items():
        print(f"  {k}: {d['typed_decls']} typed / {d['raw_decls']} raw decls, "
              f"{d['mentions']} mentions, {d['under_control']} under control")
    if per_rung:
        print(f"  net lines removed — max rung {res['net_removed_max_rung']}, "
              f"total {res['net_removed_total']}")
    if res["dirty"]:
        print("  !! DIRTY TREE — the ref does not fully describe what ran")
    return 0


if __name__ == "__main__":
    REPO = pathlib.Path(__file__).resolve().parent.parent
    sys.exit(main())
