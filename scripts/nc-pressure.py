#!/usr/bin/env python3
"""nc-pressure — the noun-convergence headline number, minted not typed.

RE-DERIVATION PRESSURE = defs + kin, summed over the register's in_program
rows. It counts how many times somebody built a new type instead of reaching
for the canonical one — which is the disease, stated as a number.

  defs  distinct first-party production definitions of the exact name
  kin   distinct names that end or start with it (SomethingVerdict,
        VerdictRow). Over-collects on purpose: an exact-name census misses
        `Record`, which has ZERO definitions and 45 kin — nobody ever made a
        Record, forty-five people made a SomethingRecord.

Reads quality/CONCEPTS.toml for the register and `svrn code converge noun`
for the measurement. No new instrument: both already exist.

  scripts/nc-pressure.py            # table + total
  scripts/nc-pressure.py --json     # {total, rows:[...]} for a bar
"""
import json, re, subprocess, sys, pathlib

REPO = pathlib.Path(__file__).resolve().parent.parent
CLI = REPO / "target/debug/sovereign-cli-dev"


def register():
    """in_program noun names, in register order."""
    text = (REPO / "quality/CONCEPTS.toml").read_text()
    out = []
    for block in text.split("[[concept]]")[1:]:
        name = re.search(r'^name\s*=\s*"(.*?)"', block, re.M)
        inp = re.search(r"^in_program\s*=\s*(\S+)", block, re.M)
        if name and inp and inp.group(1).strip() == "true":
            out.append(name.group(1))
    return out


def dossier(noun):
    r = subprocess.run([str(CLI), "code", "converge", "noun", noun, "--json"],
                       capture_output=True, text=True)
    if r.returncode != 0:
        return None  # absent, named — never defaulted to zero (ARCH §18.3)
    return json.loads(r.stdout)


def main():
    if not CLI.exists():
        sys.exit(f"nc-pressure: no {CLI} — cargo build -p sovereign-cli-dev")
    rows, missing = [], []
    for noun in register():
        d = dossier(noun)
        if d is None:
            missing.append(noun)
            continue
        defs, kin = len(d["defs"]), len(d["kin"])
        rows.append({"noun": noun, "defs": defs, "kin": kin,
                     "pressure": defs + kin, "sites": d["reference_sites"],
                     "has_canonical": defs >= 1,
                     # WHERE the definitions are, not just how many. Additive
                     # 2026-08-20 (nc-15-generate) so the register's declared
                     # `canonical` can be checked against the graph rather than
                     # trusted. Render only — no number above changes.
                     "def_sites": [{"krate": t["krate"], "file": t["file"],
                                    "line": t["line"]} for t in d["defs"]],
                     "move": "collapse" if defs >= 2 else ("mint" if defs == 0 else "confirm")})
    total = sum(r["pressure"] for r in rows)
    reach = sum(1 for r in rows if r["has_canonical"])
    # EXCESS DEFINITIONS is the judgeable half: how many redundant definitions
    # of a register noun exist. Target 0 = exactly one canonical each. `kin` is
    # deliberately NOT in this number — kin over-collects (PartialKvVerdict is
    # not a Verdict), so driving it to zero would mean renaming coincidences
    # apart, which is churn. Kin is tracked with a floor (must not rise), not a
    # target.
    excess = sum(max(0, r["defs"] - 1) for r in rows)

    if "--json" in sys.argv:
        print(json.dumps({"total_pressure": total, "excess_definitions": excess,
                          "reach": reach, "of": len(rows),
                          "unmeasurable": missing, "rows": rows}, indent=2))
        return 3 if missing else 0

    rows.sort(key=lambda r: -r["pressure"])
    print(f"{'noun':<15}{'defs':>5}{'kin':>5}{'PRESSURE':>10}{'sites':>7}  move")
    print("-" * 56)
    for r in rows:
        print(f"{r['noun']:<15}{r['defs']:>5}{r['kin']:>5}{r['pressure']:>10}"
              f"{r['sites']:>7}  {r['move']}")
    print("-" * 56)
    print(f"excess definitions (JUDGED, target 0) : {excess}")
    print(f"nouns with a canonical (JUDGED, -> all): {reach}/{len(rows)}")
    print(f"total re-derivation pressure (tracked) : {total}")
    if missing:
        print(f"UNMEASURABLE (reported, not defaulted): {', '.join(missing)}")
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
