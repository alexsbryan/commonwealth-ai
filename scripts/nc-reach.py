#!/usr/bin/env python3
"""nc-reach — how far does a type travel from the file that declares it?

WHY THIS EXISTS. The campaign's three original bars count TYPES CROSSING CRATE
BOUNDARIES. After three waves and ~30 commits they read 477 / 143 / 23 against
targets of 50 / 0 / 10 — essentially flat, one moving backwards. In the same
period the work shipped a live HTML-truncation bug fix, three desktop panics,
1,405 deleted duplicate lines, a trust envelope that exposed ten bench banks
being scored as fine, and tool descriptors going 0 -> 68% data-sourced.

None of that is visible in those three bars, because they measure a dimension
that was already healthy: `cargo xtask layer-gate` exits 0 — every crate edge
points down or sideways — and NOUN_CONVERGENCE §10.2 measures 0 of 441
load-bearing types carrying an upward edge.

The defect §10.2 actually locates is INSIDE crates: **46% of first-party
production types are referenced by no other file at all.** `sovereign-cli-llm`
is the extreme — 847 types, 75% private, 0% exported, across 122,268 production
lines, with `enrich_cmd` at 32,813 lines and `bench_cmd` at 29,460 inside a leaf
binary that exports nothing. Neither the desktop nor the daemon nor MCP can
reach them, so anyone who needs enrichment orchestration re-derives it.

This scores that. It exists so the destination the campaign is ACTUALLY tracking
toward can be measured instead of argued — the same reason `nc-thesis` was
instrumented on 2026-08-20 after rendering UNMEASURED while three type-counting
bars were fully instrumented. A bar nobody can score is a bar nobody can fail,
and an outcome nobody can score has to be re-negotiated in prose every turn.

THE THREE BANDS, per §10.2:
  private   referenced by NO other file — used only where it is declared
  local     referenced elsewhere in its OWN crate, but never outside it
  exported  referenced by at least one other crate

`private` is the headline. Some of it is correct Rust — a one-endpoint DTO
should be private — which is why the target is not zero and why the per-crate
spread is the real signal: same authorship, same five months, 15% to 79%. A
fivefold spread is not idiom, it is the absence of anything to reach for.

PROVENANCE. Like `nc-boundary.py`, this reads the daemon's SCIP index and NOT
the working tree, so its numbers describe `last_indexed_head`. It emits that
head so a measurement row can be stamped with the tree it describes rather than
whatever happened to be checked out (see `nc-boundary.py:index_provenance`).
"""
import json
import os
import sqlite3
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
# Reuse, do not re-derive: nc-boundary already owns first-party classification,
# the production filter and the domain map (ARCH §19, and §10.6's own rule that
# a second implementation of one decider is the defect).
import importlib.util

_spec = importlib.util.spec_from_file_location(
    "nc_boundary", os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                "nc-boundary.py"))
nb = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(nb)


def crate_of(path):
    """Owning crate directory for a repo-relative path, or None."""
    parts = path.split("/")
    for i, p in enumerate(parts):
        if p == "crates" and i + 1 < len(parts):
            return parts[i + 1]
    return parts[0] if parts and parts[0] else None


def main():
    if not os.path.exists(nb.GRAPH):
        sys.exit(f"nc-reach: no graph at {nb.GRAPH} — svrn refresh")
    db = sqlite3.connect(f"file:{nb.GRAPH}?mode=ro", uri=True)
    indexed_head, indexed_at = nb.index_provenance(db)
    owner = nb.load(db)                       # {qualified: (bare, domain)}

    # Declaring file + crate for every production type the boundary tool owns.
    home = {}
    for qn, fp in db.execute(
            "SELECT qualified_name, file_path FROM symbols "
            "WHERE qualified_name LIKE '%#'"):
        if qn in owner:
            home[qn] = (fp, crate_of(fp))

    # Distinct referencing files per type. A type's own declaring file does not
    # count as reach — that is the whole question being asked.
    seen_files = {}
    for cq, fp in db.execute(
            "SELECT callee_qualified, file_path FROM refs "
            "WHERE callee_qualified LIKE '%#'"):
        if cq in home:
            seen_files.setdefault(cq, set()).add(fp)

    per_crate = {}
    totals = {"private": 0, "local": 0, "exported": 0}
    for qn, (decl_fp, crate) in home.items():
        if not crate:
            continue
        others = {f for f in seen_files.get(qn, ()) if f != decl_fp}
        if not others:
            band = "private"
        elif any(crate_of(f) != crate for f in others):
            band = "exported"
        else:
            band = "local"
        row = per_crate.setdefault(
            crate, {"types": 0, "private": 0, "local": 0, "exported": 0})
        row["types"] += 1
        row[band] += 1
        totals[band] += 1

    n = sum(totals.values())
    private_share = round(100.0 * totals["private"] / n, 1) if n else 0.0

    if "--json" in sys.argv:
        print(json.dumps({
            "value": private_share,
            "types": n,
            "bands": totals,
            "per_crate": per_crate,
            "indexed_head": indexed_head,
            "indexed_at": indexed_at}, indent=2))
        return 0

    print("\n  REACH — how far does a type travel from the file that declares it?\n")
    print(f"  index describes {(indexed_head or '(unknown)')[:12]} "
          f"exported {indexed_at or '(unknown)'} — NOT your working tree\n")
    print(f"  {'crate':<32}{'types':>7}{'private':>9}{'local':>7}{'exported':>10}")
    print(f"  {'-' * 65}")
    ranked = sorted(per_crate.items(),
                    key=lambda kv: (-kv[1]["private"] / max(kv[1]["types"], 1),
                                    -kv[1]["types"]))
    for crate, r in ranked:
        if r["types"] < 40:          # the tail is noise, not signal
            continue
        pr = 100.0 * r["private"] / r["types"]
        ex = 100.0 * r["exported"] / r["types"]
        lo = 100.0 * r["local"] / r["types"]
        print(f"  {crate:<32}{r['types']:>7}{pr:>8.0f}%{lo:>6.0f}%{ex:>9.0f}%")
    print(f"  {'-' * 65}")
    print(f"  {'ALL first-party production':<32}{n:>7}"
          f"{private_share:>8.1f}%"
          f"{100.0 * totals['local'] / max(n, 1):>6.0f}%"
          f"{100.0 * totals['exported'] / max(n, 1):>9.0f}%")
    print(f"\n  {private_share}% of {n} types are referenced by no other file.\n")
    print("  Not all of that is wrong — a one-endpoint DTO should be private.")
    print("  The SPREAD is the signal: same authorship, same months, and the")
    print("  range above is fivefold. That is the absence of anything to reach")
    print("  for, not idiom (NOUN_CONVERGENCE §10.2, §10.3).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
