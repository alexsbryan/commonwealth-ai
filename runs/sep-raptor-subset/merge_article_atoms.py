#!/usr/bin/env python3
"""Round 2: give BOTH subset arms the real per-article atoms.

WHY. Round 1 compared "injector, no atlas" (OFF) against "walk over a
Summary-only atlas" (ON). That is not the done-when's question, which is the
SAME atlas with and without `Summary`. The subset atlas held only the 2,004
projected summaries, so the 6 tension-row questions seeded nothing at all and
the walk's 8 appended summaries were the only atlas contribution any question
got — a worst case for the port, and its -3/66 an upper bound on harm rather
than an effect.

WHAT. Every kept article's real `sep-<slug>/atlas` atoms and edges are merged
into BOTH subset atlases. After this the two differ in exactly one thing: the
ON arm additionally carries `Summary`. The census assertion at the end is what
makes that a checked fact rather than an intention — if the two atlases ever
differ in any kind OTHER than Summary, this refuses.

THE CHUNK IDS STILL RESOLVE. The merged atoms' `first_appearance.chunk_id`
point into `sep`'s row ids, and the subset copied the `id` COLUMN verbatim, so
they address the same passages inside the subset. That is the reason the
builder preserves ids and it is what makes this merge sound.

SEEDING, NAMED NOT FIXED. The merged atoms carry no vectors here: their seed
rows would have to come from each `sep-<slug>/atlas/atoms_ann.lance`, and
almost none exist (24 of 1,770 corpus-wide). This script REPORTS the coverage
it found rather than embedding them — an embed pass over ~28k atoms is a model
call and a different decision. So in both arms the merged atoms are reachable
by the walk but do not seed it by vector; the seed table remains Summary-only,
which is itself part of what Summary brings and is why the ON arm has a table
at all. Reported in the lane row, not papered over.
"""
import json
import os
import sys
from collections import Counter
from pathlib import Path

import lance

INDEXES = Path(os.environ.get("SOVEREIGN_INDEXES", Path.home() / ".svrnmesh/indexes"))
ARMS = ["raptor-subset-off", "raptor-subset-on"]


def kind_census(atoms):
    return Counter(a.get("atom_type", "?") for a in atoms)


def main() -> int:
    # The kept articles are whatever the subset's own summary rows name, so
    # this cannot drift from what the builder produced.
    ds = lance.dataset(str(INDEXES / ARMS[1] / "raptor_summaries.lance"))
    articles = sorted({
        u.rstrip("/").rsplit("/", 1)[-1]
        for u in ds.to_table(columns=["conv_uuid"]).column("conv_uuid").to_pylist()
    })
    print(f"articles to merge: {len(articles)}")

    merged_atoms, merged_edges = [], []
    missing, with_ann = [], 0
    for slug in articles:
        adir = INDEXES / f"sep-{slug}" / "atlas"
        af = adir / "atoms.json"
        if not af.exists():
            missing.append(slug)
            continue
        merged_atoms += json.loads(af.read_text()).get("atoms", [])
        ef = adir / "edges.json"
        if ef.exists():
            merged_edges += json.loads(ef.read_text()).get("edges", [])
        if (adir / "atoms_ann.lance").is_dir():
            with_ann += 1

    print(f"merged {len(merged_atoms)} atoms, {len(merged_edges)} edges "
          f"from {len(articles) - len(missing)} article atlases")
    if missing:
        print(f"  DEGRADED: {len(missing)} article(s) had no atlas on disk: {missing[:6]}")
    print(f"  seed vectors: {with_ann}/{len(articles)} article atlases carry atoms_ann.lance "
          f"— merged atoms seed by NAME-MATCH only, not by vector")

    for arm in ARMS:
        adir = INDEXES / arm / "atlas"
        af, ef = adir / "atoms.json", adir / "edges.json"
        cur = json.loads(af.read_text())
        cur_edges = json.loads(ef.read_text())
        have = {a.get("data", {}).get("id") for a in cur["atoms"]}
        add = [a for a in merged_atoms if a.get("data", {}).get("id") not in have]
        cur["atoms"] += add
        # Edge ids are positional; renumber the merged block after whatever
        # this atlas already holds so a merged edge cannot collide with a
        # `Composes` edge the projection wrote.
        base = len(cur_edges["edges"])
        for i, e in enumerate(merged_edges):
            e = dict(e)
            e["id"] = f"edge-{base + i:05d}"
            cur_edges["edges"].append(e)
        af.write_text(json.dumps(cur, indent=2))
        ef.write_text(json.dumps(cur_edges, indent=2))
        print(f"{arm}: {len(cur['atoms'])} atoms, {len(cur_edges['edges'])} edges")

    # THE ASSERTION. Both arms, per kind, and the ONLY permitted difference is
    # Summary. A silent divergence here would make every lane row meaningless
    # while every count still looked plausible.
    census = {}
    for arm in ARMS:
        census[arm] = kind_census(json.loads((INDEXES / arm / "atlas" / "atoms.json").read_text())["atoms"])
    off, on = census[ARMS[0]], census[ARMS[1]]
    print(f"\ncensus off: {dict(off)}\ncensus on : {dict(on)}")
    diff = {k: (off.get(k, 0), on.get(k, 0)) for k in set(off) | set(on) if off.get(k, 0) != on.get(k, 0)}
    if set(diff) - {"Summary"}:
        print(f"REFUSING: the arms differ in kinds other than Summary: {diff}", file=sys.stderr)
        return 1
    print(f"OK — the arms differ in Summary only: {diff}")
    print("\nnext: svrn atlas migrate-all <arm>   # rebuild atoms.lance + edges.csr for BOTH")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
