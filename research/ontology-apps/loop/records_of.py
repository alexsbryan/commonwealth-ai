"""records_of.py <corpus id> <recipe.toml> <out.json>: a corpus's records as the public typed query gives them.

Every type the recipe's ontology declares is listed with `sovereign enrich atlas-query <id> --typed`; an
entity record carries its attributes (references by the referenced record's name), its first passage (the
passage it was first read from, when a reader's name names it) and the statements whose subject it is; a statement with no subject record stands alone. The shape is score_bank.py's (and a one-shot's).
The typed listing shows at most 64 rows per type (ENUMERATE_MAX); a type it capped is named in `capped`.
"""
import ast, json, subprocess, sys, tomllib

cid, recipe, out = sys.argv[1:4]
types = tomllib.load(open(recipe, "rb"))["enrichment"]["ontology"]["types"]


def rows(t):
    r = subprocess.run(["sovereign", "enrich", "atlas-query", cid, "--typed", json.dumps({"target_type": t}), "--json"],
                       capture_output=True, text=True)
    if r.returncode != 0:
        return [], [f"{t}: query failed: {r.stderr.strip()[-200:]}"]
    d = json.loads(r.stdout)
    # The row's own name is the atom's whole name (a claim's content, an event's description); the nested
    # record's name is cut at 121 characters and its text near 400, which dropped the killer's name.
    recs = []
    for x in d.get("rows", []):
        rec = ast.literal_eval(x["record"]) if isinstance(x["record"], str) else x["record"]
        recs.append(dict(rec, name=x.get("name") or rec.get("name"), text=x.get("name") or rec.get("text"),
                         passage=rec.get("text") or ""))
    return recs, [n for n in d.get("notes", []) if "showing the first" in n]


def plain(rec):
    names = {r["id"]: r.get("name") for r in rec.get("refs") or []}
    attrs = rec.get("attributes") or {}
    return {k: (names.get(v, v) if isinstance(v, str) else [names.get(x, x) for x in v] if isinstance(v, list) else v)
            for k, v in attrs.items() if not k.startswith("__")}


records, by_id, capped = [], {}, []
claims = []
for t in types:
    recs, notes = rows(t["name"])
    capped += [f"{t['name']}: {n}" for n in notes]
    for rec in recs:
        if rec.get("kind") == "claim" or t.get("subject"):
            claims.append((t["name"], rec))
        else:
            r = dict(type=t["name"], name=rec.get("name"), attributes=plain(rec), claims=[])
            # An entity's text is its first passage (the record's opening passage, once a reader's name names it).
            if rec["passage"] and rec["passage"] != rec.get("name"):
                r["first_passage"] = rec["passage"]
            records.append(r); by_id[rec["id"]] = r
for kind, rec in claims:
    subj = next((r["id"] for r in rec.get("refs") or [] if r.get("attribute") == "subject"), None)
    c = dict(kind=kind, quote=rec.get("text") or rec.get("name"), attributes={k: v for k, v in plain(rec).items() if k != "subject"})
    if subj in by_id: by_id[subj]["claims"].append(c)
    else: records.append(dict(type=kind, name=c["quote"], attributes=c["attributes"], claims=[]))
json.dump(dict(records=records, capped=capped), open(out, "w"), indent=1)
print(f"{len(records)} records, {len(claims)} statements; capped: {capped or 'none'}")
