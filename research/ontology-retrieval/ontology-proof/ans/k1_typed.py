#!/usr/bin/env python3
"""O-T1 inner loop (feature-fidelity): the chat path's typed producer, offline, on the dev K1 rows.

    k1_typed.py [--variant probe|named] [--json k1-typed-<variant>.json]

Per dev K1 row (bank-k1-dev.toml): the question goes to the primary model with the query-layer
probe's documentation and schema, rendered from the atlas's OWN ontology.json with each attribute's
declared description as its gloss (what `typed_prompt` sends; tests/typed_prompt.rs holds the two
byte-equal). The query it writes runs through `svrn enrich atlas-query --typed` (the executor the
chat path runs, over ft-ans-dev-b's atlas) and the table's names are scored with
records_recall.found against the row's gold `facts` and `members_local`. No synthesis and no chat
turn: this is TABLE recall, the most a chat answer built on the table can list. Seconds per row.

--variant named adds ONE out-of-domain example: a named far end written with an identifying
description ("the Radcliffe library, founded 1749") stays `other_name` and the description adds
no condition. Read 1 through chat (2026-10-03) found the model turning K1's identifying clauses
("the Olympia hoard, found 1939") into filters the atlas's hoards cannot meet.
"""
import argparse, json, pathlib, subprocess, sys, time, tomllib

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import k2_query as K  # noqa: E402  (the probe's grammar, schema, documentation and call)
from records_recall import found  # noqa: E402  (one matcher: the bank's)

NAMED_EXAMPLE = [
    'Q: Which books are held by the Radcliffe library, founded 1749?',
    'A: {"target_type": "book", "filters": [], "relations": [{"relation": "held_by", "other_type": '
    '"library", "other_name": "Radcliffe", "negate": false, "where": null}], "aggregate": "none", '
    '"aggregate_over": null}']


def prompt(variant):
    onto = json.loads((K.ATLAS / "ontology.json").read_text())["policies"]
    types, guidance = onto["shape"]["types"], onto["prose"]["guidance"]
    K.GLOSS.clear()                       # the declaration's own descriptions, as the Rust port reads them
    for t in types:
        for a in t.get("attributes") or []:
            K.GLOSS[(t["name"], a["name"])] = a.get("description") or ""
    ents, attrs, edges = K.grammar(types)
    doc = K.documentation(types, guidance, ents, attrs, edges)
    if variant == "named":
        doc = doc + "\n" + "\n".join(NAMED_EXAMPLE)
    return doc, K.schema(ents, attrs, edges)


def table(query):
    out = subprocess.run(["sovereign", "enrich", "atlas-query", K.CORPUS, "--typed", json.dumps(query), "--json"],
                         capture_output=True, text=True, timeout=120)
    d = json.loads(out.stdout)
    t = d.get("table") or d
    return [r["name"] for r in t.get("rows", [])], t.get("notes", [])


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--variant", choices=["probe", "named"], default="probe")
    ap.add_argument("--json", type=pathlib.Path)
    a = ap.parse_args()
    doc, sch = prompt(a.variant)
    bank = tomllib.loads((HERE / "bank-k1-dev.toml").read_text())["questions"]
    rows, tot = [], {"facts": [0, 0], "local": [0, 0], "strays": 0, "parsed": 0}
    for q in bank:
        gold = json.loads((HERE / "gold" / f"{q['id']}.json").read_text())
        t0 = time.time()
        query, raw, secs, attempts = K.parse(doc, sch, q["question"])
        names, notes = table(query) if query else ([], ["did not parse"])
        f, loc = found(gold["facts"], names), found(gold.get("members_local") or [], names)
        strays = [n for n in names if not found([n], gold["facts"] + (gold.get("members_in_corpus") or []))]
        tot["facts"][0] += len(f); tot["facts"][1] += len(gold["facts"])
        tot["local"][0] += len(loc); tot["local"][1] += len(gold.get("members_local") or [])
        tot["strays"] += len(strays); tot["parsed"] += query is not None
        rows.append({"id": q["id"], "query": query, "raw": None if query else raw, "attempts": attempts,
                     "secs": round(time.time() - t0, 1), "table": names, "notes": notes,
                     "found": f, "found_local": loc, "strays": strays})
        print(f"{q['id']:<22} parsed={query is not None} rows={len(names):>2} facts {len(f)}/{len(gold['facts'])} "
              f"local {len(loc)}/{len(gold.get('members_local') or [])} strays={len(strays)}", flush=True)
    r = lambda k: round(tot[k][0] / tot[k][1], 3) if tot[k][1] else None  # noqa: E731
    summary = {"variant": a.variant, "rows": len(bank), "parsed": tot["parsed"], "table_recall_facts": r("facts"),
               "table_recall_local": r("local"), "strays": tot["strays"],
               "nonempty_tables": sum(1 for x in rows if x["table"])}
    print(json.dumps(summary))
    out = a.json or HERE / f"k1-typed-{a.variant}.json"
    out.write_text(json.dumps({"summary": summary, "rows": rows}, indent=1, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
