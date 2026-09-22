#!/usr/bin/env python3
"""ei7 ref census: the declared ref attributes' fill + resolution rates, and
the structural K1 test the ontology factor depends on.

The K1 theory (PRE-REG §Theory) says a typed graph beats the ~12-passage
window on enumeration questions. That requires the graph to HOLD the
enumeration: coins must carry `hoard`/`mint` refs. Measured 2026-09-22
(pre-fix): mint 247/362 coins (the row prints it), hoard 49/362 (section
context, named in the heading — never emitted; zero unresolved-hoard
failures prove absence of emission, not resolution loss).

The structural test: for each bank K1 question, resolve the named hoard's
atoms, collect coins referencing them, project onto mints, and compare with
the gold list. No retrieval, no model — pure graph arithmetic.

Usage: ref_census.py [--index-dir DIR] [--bank FILE]
"""
import argparse
import json
import os
import sys
from collections import Counter

import tomllib

DEFAULT_INDEX = os.path.expanduser("~/.svrnmesh/indexes/ei7-ans")
DEFAULT_BANK = os.path.join(os.path.dirname(__file__), "bank.attested.toml")


def load_atoms(index_dir):
    return json.load(open(os.path.join(index_dir, "atlas", "atoms.json")))["atoms"]


def load_failures(index_dir):
    p = os.path.join(index_dir, "atlas", "resolution_failures.json")
    return json.load(open(p))["failures"]


def census(atoms, failures):
    coins = [a for a in atoms if (a.get("data") or {}).get("entity_type") == "coin"]
    rates = {}
    for attr in ("hoard", "mint", "ruler"):
        filled = sum(1 for a in coins if attr in ((a.get("data") or {}).get("attributes") or {}))
        rates[attr] = (filled, len(coins))
    unresolved = Counter(
        r.get("reason", "").split("`")[3] if r.get("kind") == "unresolved_attribute_ref" and r.get("reason", "").count("`") > 3 else "?"
        for r in failures
        if r.get("kind") == "unresolved_attribute_ref"
    )
    return coins, rates, unresolved


def structural_k1(atoms, bank):
    id2 = {a["data"]["id"]: a for a in atoms}
    rows = []
    for q in bank["questions"]:
        if q["category"] != "k1_list_them_all":
            continue
        if "mints" not in q["id"]:
            continue
        # The hoard the question names: from the gold note's IGCH id and the
        # question's own words, matched against hoard atom names.
        qwords = {w.strip(",.?'\"").lower() for w in q["question"].split()}
        hoard_atoms = [
            a for a in atoms
            if (a.get("data") or {}).get("entity_type") == "hoard"
            and any(w in qwords for w in str((a.get("data") or {}).get("canonical_name", "")).lower().split())
        ]
        hids = {a["data"]["id"] for a in hoard_atoms}
        linked = [
            a for a in atoms
            if (a.get("data") or {}).get("entity_type") == "coin"
            and ((a.get("data") or {}).get("attributes") or {}).get("hoard") in hids
        ]
        mints = Counter()
        for a in linked:
            m = id2.get(((a.get("data") or {}).get("attributes") or {}).get("mint"))
            if m:
                mints[(m.get("data") or {}).get("canonical_name")] += 1
        gold = set(q["expected_facts"])
        got = set(mints)
        rows.append({
            "question_id": q["id"],
            "hoard_atoms": len(hids),
            "coins_linked": len(linked),
            "mints_via_graph": sorted(got),
            "gold": sorted(gold),
            "gold_covered": sorted(gold & got),
            "coverage": round(len(gold & got) / max(len(gold), 1), 3),
        })
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--index-dir", default=DEFAULT_INDEX)
    ap.add_argument("--bank", default=DEFAULT_BANK)
    args = ap.parse_args()

    atoms = load_atoms(args.index_dir)
    failures = load_failures(args.index_dir)
    coins, rates, unresolved = census(atoms, failures)

    out = {
        "index_dir": args.index_dir,
        "coins": len(coins),
        "ref_fill": {k: {"filled": v[0], "of": v[1], "rate": round(v[0] / max(v[1], 1), 3)} for k, v in rates.items()},
        "unresolved_attribute_refs_by_attr": dict(unresolved),
        "structural_k1": [],
    }
    if os.path.exists(args.bank):
        bank = tomllib.load(open(args.bank, "rb"))
        out["structural_k1"] = structural_k1(atoms, bank)

    print(json.dumps(out, indent=1))
    cov = [r["coverage"] for r in out["structural_k1"]]
    if cov:
        print(f"\nstructural K1 mint coverage (graph-only, no retrieval): "
              f"mean {sum(cov)/len(cov):.3f} over {len(cov)} mint questions", file=sys.stderr)


if __name__ == "__main__":
    main()
