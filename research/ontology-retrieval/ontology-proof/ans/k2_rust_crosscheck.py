#!/usr/bin/env python3
"""K2 Rust cross-check (feature-fidelity campaign, Ontology, D2 read side): does the product's typed
query executor (`svrn enrich atlas-query <corpus> --typed '<json>' --json`) answer the K2 reference
queries the way k2_query.py's Executor does, over the same atlas?

    python3 k2_rust_crosscheck.py [--cli sovereign] [--json k2-rust-crosscheck.json]

For every question in k2-query.json with a non-null `reference`, the reference query is run twice:
by the CLI on CORPUS, and by `Executor(live, live.ents).run(reference)`. The CLI's rows are mapped onto
bank ids with k2_t0's own Records: a hoard row by `Records.resolve(name)` (the IGCH number its name
cites, else the bank's match patterns), a mint row by `Records.labels(name)` (the bank's [[mints]]
match table). The mapped answer is shaped as Executor.run shapes its own: a sorted list, a count
(`matched`), or for argmax/argmin a single id, a sorted list of tied ids, or None.

Each item the two answers disagree on is attributed by checking, on the same records, the
normalizations Records applies and the product does not (`cause`): membership read through a coin's
`hoard` + `mint` refs where no holds_coins_of Relation joins the hoard to a mint atom
(`coin-composition`); a mint the bank's patterns name whose atom is not named that (`mint-name`);
hoard atoms unioned because their names cite one IGCH number (`igch-group`). An item none of these
explains is `unexplained` — a candidate Rust bug, read by hand.
"""
import argparse, collections, json, os, pathlib, subprocess, sys, tomllib

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import k2_query  # noqa: E402  (the probe's Executor and grammar)
from k2_query import ATLAS, CORPUS, Executor, declared, grammar  # noqa: E402
from k2_t0 import Records  # noqa: E402
from make_bank import fold  # noqa: E402


def run_cli(cli, corpus, query):
    p = subprocess.run([cli, "enrich", "atlas-query", corpus, "--typed", json.dumps(query), "--json"],
                       capture_output=True, text=True, env={**os.environ, "SOVEREIGN_NO_STALE_WARN": "1"})
    try:
        return json.loads(p.stdout), p.returncode
    except json.JSONDecodeError:
        return {"hit": False, "headline": (p.stderr or p.stdout).strip()[-400:]}, p.returncode


def mapped(rec, target, rows):
    """Row names -> the bank ids Executor.run answers in."""
    if target == "hoard":
        return {rec.resolve(r["name"]) for r in rows}
    if target == "mint":
        return set().union(*[rec.labels(r["name"]) for r in rows]) if rows else set()
    return {f"{target}:{r['name']}" for r in rows}


def shaped(rec, q, out):
    agg, t = q["aggregate"], q["target_type"]
    if not out.get("hit"):
        return {"refused": out.get("headline")}
    if agg == "count":
        return out["matched"]
    ids = mapped(rec, t, out["rows"])
    if agg == "none":
        return sorted(ids)
    if not ids:
        return None
    return next(iter(ids)) if len(ids) == 1 else sorted(ids)


# ── attribution: which Records normalization the product does not have ─────
def relation_labels(rec, ids):
    """Bank mint labels joined to entity ids by a Relation to a mint ATOM — the link the product reads."""
    out = set()
    for r in rec.rels:
        p = set(r.get("participants") or [])
        if p & ids:
            for pid in p - ids:
                e = rec.ents.get(pid, {})
                if e.get("entity_type") == "mint":
                    out |= rec.labels(e["canonical_name"])
    return out


def product_named(rec, ids, name):
    """Does a Relation join one of `ids` to a mint atom NAMED `name` (fold-equal canonical or alias)?"""
    for r in rec.rels:
        p = set(r.get("participants") or [])
        if p & ids:
            for pid in p - ids:
                e = rec.ents.get(pid, {})
                forms = [e.get("canonical_name") or ""] + list(e.get("aliases") or [])
                if e.get("entity_type") == "mint" and any(fold(f).strip() == fold(name).strip() for f in forms):
                    return True
    return False


def why_hoard(rec, q, hid):
    """For one hoard id the two answers disagree on: which normalization of `rec.has` decides it."""
    groups = [g for g in rec.groups if rec.resolved[g] == hid]
    ents = [e["id"] for g in groups for e in rec.groups[g]]
    out = set()
    for c in q["relations"]:
        if not c.get("other_name") or c["other_type"] != "mint":
            continue
        labels = rec.labels(c["other_name"])
        py = any(rec.has(g, m) for g in groups for m in labels)
        by_relation = [bool(relation_labels(rec, {e}) & labels) for e in ents]
        by_name = [product_named(rec, {e}, c["other_name"]) for e in ents]
        if py and not any(by_relation):
            out.add("coin-composition")        # Records.members reads coin `hoard` + `mint` refs too
        elif any(by_relation) and not any(by_name):
            out.add("mint-name")               # the bank's patterns name the mint; no atom is named that
        if len(ents) > 1 and len(set(by_name)) > 1:
            out.add("igch-group")              # Records unions the hoard atoms that cite one IGCH number
    return out


def why_mint(rec, q, label):
    """For one mint label the two answers disagree on, in a query over mints."""
    atoms = [e for e in rec.ents.values() if e.get("entity_type") == "mint" and label in rec.labels(e["canonical_name"])]
    if not atoms:
        return {"coin-composition"}            # no mint atom: Records reaches it only through a coin's `mint` text
    linked = [g for g in rec.groups if rec.has(g, label)]
    if any(label not in relation_labels(rec, {e["id"] for e in rec.groups[g]}) for g in linked):
        return {"coin-composition"}
    return set()


def cause(rec, q, py, rs):
    as_set = lambda v: set(v) if isinstance(v, list) else set() if v is None else {v}  # noqa: E731
    out = set()
    if isinstance(py, int) and isinstance(rs, int):
        out.add("igch-group" if any(len(es) > 1 for es in rec.groups.values()) else "count")
    else:
        for item in as_set(py) ^ as_set(rs):
            out |= why_hoard(rec, q, item) if q["target_type"] == "hoard" else why_mint(rec, q, item)
    return sorted(out) or ["unexplained"]


def product_shaped(live):
    """The instrument check on the attribution: a Records with the two normalizations the product
    lacks switched off — one group per hoard atom (no IGCH union) and members only through a Relation
    to a mint atom (no coin composition). Everything else (names, findspots, burials, the bank's
    patterns) is live's. If the attribution is complete, the Executor over THIS agrees with Rust on
    every question; a residual disagreement is something the attribution did not name."""
    r = object.__new__(Records)
    r.__dict__.update(live.__dict__)
    r.groups = {e["id"]: [e] for es in live.groups.values() for e in es}
    r.members = {g: relation_labels(live, {g}) for g in r.groups}
    r.findspot = {g: (es[0].get("attributes") or {}).get("findspot") or "" for g, es in r.groups.items()}
    r.burial = {g: live._burial(es) for g, es in r.groups.items()}
    r.resolved = {g: live.resolve(es[0]["canonical_name"]) for g, es in r.groups.items()}
    return r


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--cli", default="sovereign", help="the CLI that dispatches `enrich` (svrn / sovereign)")
    ap.add_argument("--queries", type=pathlib.Path, default=HERE / "k2-query.json")
    ap.add_argument("--bank", type=pathlib.Path, default=HERE / "bank-k2-dev.toml")
    ap.add_argument("--json", type=pathlib.Path, default=HERE / "k2-rust-crosscheck.json")
    a = ap.parse_args()

    types, _ = declared()
    k2_query.EDGES = grammar(types)[2]          # the generic argmax branch reads it, as main() sets it
    bank = tomllib.loads(a.bank.read_text())
    live = Records(ATLAS, bank, CORPUS)
    ex = Executor(live, live.ents)
    prod = product_shaped(live)
    ex_prod = Executor(prod, live.ents)
    qs = [q for q in json.loads(a.queries.read_text())["questions"] if q.get("reference") is not None]

    rows, per_class = [], collections.defaultdict(lambda: [0, 0])
    for q in qs:
        ref = q["reference"]
        out, code = run_cli(a.cli, CORPUS, ref)
        py, rs = ex.run(ref), shaped(live, ref, out)
        agree = py == rs
        py_prod = ex_prod.run(ref)
        per_class[q["class"]][0] += agree
        per_class[q["class"]][1] += 1
        row = {"id": q["id"], "class": q["class"], "agree": agree, "python": py, "rust": rs, "exit": code,
               "rust_rows": [r["name"] for r in out.get("rows", [])], "rust_notes": out.get("notes", []),
               "python_product_shaped": py_prod, "agree_product_shaped": py_prod == rs}
        if not agree:
            row["cause"] = cause(live, ref, py, rs)
        rows.append(row)
        print(f"{'ok ' if agree else 'DIS'} {q['id']:44} py={json.dumps(py)[:70]:70} rs={json.dumps(rs)[:70]}"
              + ("" if agree else f"  cause={','.join(row['cause'])}")
              + ("" if row["agree_product_shaped"] else "  RESIDUAL vs product-shaped records"), flush=True)

    n = sum(r["agree"] for r in rows)
    print(f"\nagreement {n}/{len(rows)}")
    for c, (k, t) in sorted(per_class.items()):
        print(f"  {c:20} {k}/{t}")
    causes = collections.Counter(c for r in rows if not r["agree"] for c in r["cause"])
    print("disagreement causes:", dict(causes))
    n_prod = sum(r["agree_product_shaped"] for r in rows)
    print(f"instrument check — agreement with coin composition and IGCH union switched off: {n_prod}/{len(rows)}")
    a.json.write_text(json.dumps({"corpus": CORPUS, "cli": a.cli, "agree": n, "total": len(rows),
                                  "agree_product_shaped": n_prod,
                                  "per_class": {c: {"agree": k, "total": t} for c, (k, t) in per_class.items()},
                                  "causes": dict(causes), "questions": rows}, indent=1, ensure_ascii=False))


if __name__ == "__main__":
    main()
