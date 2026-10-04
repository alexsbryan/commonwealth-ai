#!/usr/bin/env python3
"""O-T1 bound (feature-fidelity): the most the typed table can add to a chat answer on the dev K1 rows.

    k1_bound.py [--read runs-ft-k1dev/read1] [--json k1-bound.json]

Per dev K1 row, against gold members_local, no model call:
- bare / full: what read 1's chat answers listed (eval.json synth text). Reproduces read 1's
  23/112 and 27/112 — the instrument check, printed first.
- oracle: records_recall.row's members for the row — the hoard as the bank resolves it, its coins
  and relations followed — a producer that named the hoard perfectly (the O-T0 records ceiling).
- full|oracle: an answer that copies that whole table beside what full already listed.
- name_only / ident_contains: read 1's own typed queries (k1-typed-probe.json), relaxed — the far
  end's `where` keeps only `name` (resp. `name` + `findspot`, `eq` read as `contains`) — through
  `atlas-query --typed`, unioned with full, with the table's strays (names in no gold list).
Over the CURRENT ft-ans-dev-b atlas: re-run after any re-extract.
"""
import argparse, ast, copy, json, pathlib, sys, tomllib

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import k1_typed as T  # noqa: E402  (table(): the product executor)
import records_recall as R  # noqa: E402  (one decider for the records ceiling and the matcher)

ATLAS = pathlib.Path.home() / ".svrnmesh/indexes/ft-ans-dev-b/atlas"


def answers(read, arm):
    out = {}
    for r in json.loads((read / arm / "run-1/eval.json").read_text())["results"]:
        s = r["synth"] if isinstance(r["synth"], dict) else ast.literal_eval(r["synth"])
        out[r["question_id"]] = s.get("answer") or ""
    return out


def relax(query, keep):
    q = copy.deepcopy(query)
    for r in q["relations"]:
        if r.get("where"):
            r["where"]["filters"] = [dict(f, op="contains" if f["op"] == "eq" else f["op"])
                                     for f in r["where"]["filters"] if f["attribute"] in keep]
    return q


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--read", type=pathlib.Path, default=HERE / "runs-ft-k1dev/read1")
    ap.add_argument("--json", type=pathlib.Path, default=HERE / "k1-bound.json")
    a = ap.parse_args()
    bare, full = answers(a.read, "bare"), answers(a.read, "full")
    ents, rels = R.load_atlas(ATLAS)
    queries = {r["id"]: r["query"] for r in json.loads((HERE / "k1-typed-probe.json").read_text())["rows"]}
    keeps = {"name_only": {"name"}, "ident_contains": {"name", "findspot"}}
    tot, rows = {}, []
    for q in tomllib.loads((HERE / "bank-k1-dev.toml").read_text())["questions"]:
        gold = json.loads((HERE / "gold" / f"{q['id']}.json").read_text())
        loc, known = gold.get("members_local") or [], gold["facts"] + (gold.get("members_in_corpus") or [])
        hit = lambda names: set(R.found(loc, names))  # noqa: E731
        f, o = hit([full[q["id"]]]), set(loc) - set(R.row(ents, rels, q, gold)["missed_local"])
        row = {"id": q["id"], "local": len(loc), "bare": len(hit([bare[q["id"]]])), "full": len(f),
               "oracle": len(o), "full|oracle": len(f | o)}
        for v, keep in keeps.items():
            names, _ = T.table(relax(queries[q["id"]], keep)) if queries.get(q["id"]) else ([], [])
            row[f"full|{v}"] = len(f | hit(names))
            row[f"{v}_strays"] = sum(1 for n in names if not R.found([n], known))
        rows.append(row)
        for k, v in row.items():
            if k != "id":
                tot[k] = tot.get(k, 0) + v
    L = tot.pop("local")
    summary = {"atlas_hoards": sum(e.get("entity_type") == "hoard" for e in ents.values()),
               "holds_coins_of": sum(r.get("relation_type") == "holds_coins_of" for r in rels), "local": L,
               **{k: (round(v / L, 3) if "strays" not in k else v) for k, v in tot.items()},
               "check_read1": tot["bare"] == 23 and tot["full"] == 27}
    print(json.dumps(summary))
    a.json.write_text(json.dumps({"summary": summary, "rows": rows}, indent=1) + "\n")


if __name__ == "__main__":
    main()
