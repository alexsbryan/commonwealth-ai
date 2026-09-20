#!/usr/bin/env python3
"""Extraction census over a built atlas. Read-only on the index; writes
census.json + census.txt into --out (default: next to this script)."""
import argparse
import collections
import json
import os
import random
import re
import sys

import chapter_doc_map

HERE = os.path.dirname(os.path.abspath(__file__))
ap = argparse.ArgumentParser(description=__doc__)
ap.add_argument("--corpus", default="spike-fineprint-census")
ap.add_argument("--out", default=HERE)
args = ap.parse_args()
CORPUS = args.corpus
OUT = os.path.abspath(args.out)
ATLAS = os.path.expanduser(f"~/.svrnmesh/indexes/{CORPUS}/atlas")
DECLARED_ENTITY = ["organization", "party", "service", "agreement", "defined_term",
                   "data_type", "recipient", "purpose"]
FOCUS = ["obligation", "defined_term", "data_type", "recipient", "purpose", "service"]
random.seed(20260919)
out = {}
lines = []


def P(*a):
    s = " ".join(str(x) for x in a)
    print(s)
    lines.append(s)


def load(name):
    p = os.path.join(ATLAS, name)
    if not os.path.exists(p):
        return None
    return json.load(open(p))


atoms_doc = load("atoms.json")
atoms = atoms_doc["atoms"] if isinstance(atoms_doc, dict) else atoms_doc
chmap, chmap_source, chmap_services = chapter_doc_map.load(CORPUS, OUT)
out["corpus"] = CORPUS
out["chapter_doc_map"] = {"source": chmap_source, "chapters": len(chmap), "services": chmap_services}
P("corpus", CORPUS)
P("chapter doc map", len(chmap), "chapters, services", chmap_services, "—", chmap_source)
out["atoms_total"] = len(atoms)
by_type = collections.Counter(a["atom_type"] for a in atoms)
out["by_atom_type"] = dict(by_type)
P("atoms total", len(atoms))
P("by atom_type", dict(by_type))

# which data keys exist per atom_type
keys = collections.defaultdict(collections.Counter)
for a in atoms:
    for k in a["data"]:
        keys[a["atom_type"]][k] += 1
out["data_keys_by_atom_type"] = {k: dict(v) for k, v in keys.items()}
P("\ndata keys per atom_type:")
for k, v in keys.items():
    P(" ", k, dict(v))

ents = [a["data"] for a in atoms if a["atom_type"] == "Entity"]
claims = [a["data"] for a in atoms if a["atom_type"] == "Claim"]
ent_by_id = {e["id"]: e for e in ents}

et = collections.Counter(e.get("entity_type") for e in ents)
out["entity_type_counts"] = dict(et.most_common())
P("\nentity_type counts:", dict(et.most_common()))
undeclared = {k: v for k, v in et.items() if k not in DECLARED_ENTITY}
out["entity_undeclared_types"] = undeclared
P("undeclared entity types:", undeclared)

ck = collections.Counter(c.get("claim_kind") for c in claims)
out["claim_kind_counts"] = {str(k): v for k, v in ck.items()}
P("\nclaim_kind counts:", dict(ck))


def chunk_of(d):
    fa = d.get("first_appearance") or {}
    if fa.get("chunk_id"):
        return fa["chunk_id"]
    ev = d.get("evidence") or []
    return ev[0].get("chunk_id") if ev else None


def doc_of(d):
    c = chunk_of(d)
    return (chmap.get(c) or {}).get("doc")


# ---- per declared entity type
per_type = {}
for t in DECLARED_ENTITY:
    es = [e for e in ents if e.get("entity_type") == t]
    attr_fill = collections.Counter()
    for e in es:
        for k, v in (e.get("attributes") or {}).items():
            if v not in (None, "", [], {}):
                attr_fill[k] += 1
    names = collections.Counter(re.sub(r"[^a-z0-9 ]", "", e["canonical_name"].lower()).strip() for e in es)
    dup_norm = {n: c for n, c in names.items() if c > 1}
    # heading-as-entity: canonical name equals (case-folded) a chapter title
    titles = {re.sub(r"^[0-9. ]+", "", v["title"]).strip().lower() for v in chmap.values()}
    heading_like = [e["canonical_name"] for e in es if e["canonical_name"].strip().lower() in titles]
    no_desc = sum(1 for e in es if not (e.get("description") or "").strip())
    by_doc = collections.Counter(doc_of(e) for e in es)
    per_type[t] = {
        "count": len(es),
        "attr_filled": dict(attr_fill),
        "with_any_attr": sum(1 for e in es if e.get("attributes")),
        "unmerged_duplicate_names": dup_norm,
        "heading_like": heading_like,
        "empty_description": no_desc,
        "by_first_doc": {str(k): v for k, v in by_doc.items()},
    }
    P(f"\n=== entity {t}: {len(es)}  attrs filled {dict(attr_fill)}  dup-names {len(dup_norm)}  heading-like {len(heading_like)}")
    ex = random.sample(es, min(8, len(es)))
    per_type[t]["examples"] = []
    for e in ex:
        row = {"name": e["canonical_name"], "attributes": e.get("attributes"),
               "aliases": e.get("aliases"), "description": e.get("description"),
               "first_doc": doc_of(e), "first_chunk": chunk_of(e)}
        per_type[t]["examples"].append(row)
        P("  -", json.dumps(row, ensure_ascii=False)[:420])
out["entity_types"] = per_type

# ---- obligation
obl = [c for c in claims if c.get("claim_kind") == "obligation"]
P(f"\n=== claim obligation: {len(obl)} of {len(claims)} claims")
P("claim keys sample:", sorted(obl[0].keys()) if obl else None)
deontic = collections.Counter(str(c.get("deontic") or (c.get("attributes") or {}).get("deontic")) for c in obl)
subj_filled = sum(1 for c in obl if c.get("subject") or c.get("subject_id"))
subj_vals = collections.Counter()
for c in obl:
    s = c.get("subject") or c.get("subject_id")
    if s:
        name = ent_by_id.get(s, {}).get("canonical_name", s)
        typ = ent_by_id.get(s, {}).get("entity_type", "?")
        subj_vals[f"{name} [{typ}]"] += 1
attr_fill = collections.Counter()
for c in obl:
    for k, v in (c.get("attributes") or {}).items():
        if v not in (None, "", [], {}):
            attr_fill[k] += 1
out["obligation"] = {
    "count": len(obl), "deontic": dict(deontic), "subject_filled": subj_filled,
    "subject_top": dict(subj_vals.most_common(15)), "attr_filled": dict(attr_fill),
    "attributed_to_filled": sum(1 for c in obl if c.get("attributed_to")),
    "discourse_act": dict(collections.Counter(c.get("discourse_act") for c in obl)),
}
P("deontic split:", dict(deontic))
P("subject filled:", subj_filled, "/", len(obl))
P("subject top:", dict(subj_vals.most_common(15)))
P("attrs filled:", dict(attr_fill))
out["obligation"]["examples"] = []
for c in random.sample(obl, min(8, len(obl))):
    s = c.get("subject") or c.get("subject_id")
    row = {"content": c.get("content"), "deontic": c.get("deontic") or (c.get("attributes") or {}).get("deontic"),
           "subject": ent_by_id.get(s, {}).get("canonical_name", s),
           "attributes": c.get("attributes"), "doc": doc_of(c)}
    out["obligation"]["examples"].append(row)
    P("  -", json.dumps(row, ensure_ascii=False)[:420])
other = [c for c in claims if c.get("claim_kind") != "obligation"]
out["non_obligation_claims"] = {"count": len(other), "examples": [c.get("content") for c in random.sample(other, min(5, len(other)))]}
P("\nnon-obligation claims:", len(other))
for x in out["non_obligation_claims"]["examples"]:
    P("  -", (x or "")[:200])

# ---- edges
edges_doc = load("edges.json")
edges = edges_doc["edges"] if isinstance(edges_doc, dict) else (edges_doc or [])
eb = collections.Counter(e["edge_type"] for e in edges)
out["edges_total"] = len(edges)
out["edges_by_type"] = dict(eb)
P("\nedges total", len(edges), dict(eb))

# ---- side files
for f in ["ontology.json", "schema_validation.json", "resolution_failures.json", "_summary.json"]:
    d = load(f)
    if d is None:
        P(f"\n{f}: ABSENT")
        out[f] = None
        continue
    if f == "resolution_failures.json":
        fl = d.get("failures", [])
        kinds = collections.Counter(str(x.get("kind") or x.get("reason") or x.get("phase") or sorted(x.keys())) for x in fl)
        out[f] = {"count": len(fl), "kinds": dict(kinds.most_common(12)), "sample": fl[:6]}
        P(f"\n{f}: {len(fl)} failures; kinds {dict(kinds.most_common(12))}")
        for x in fl[:6]:
            P("  -", json.dumps(x, ensure_ascii=False)[:300])
    elif f == "ontology.json":
        types = d.get("policies", {}).get("shape", {}).get("types", [])
        out[f] = {"ontology_version": d.get("ontology_version"), "pipeline_id": d.get("pipeline_id"),
                  "types": [(t["name"], t["kind"]) for t in types], "policy_keys": list(d.get("policies", {}).keys())}
        P(f"\n{f}:", json.dumps(out[f]))
    else:
        out[f] = d
        P(f"\n{f}:", json.dumps(d, ensure_ascii=False)[:2500])

os.makedirs(OUT, exist_ok=True)
json.dump(out, open(os.path.join(OUT, "census.json"), "w"), indent=1, ensure_ascii=False)
open(os.path.join(OUT, "census.txt"), "w").write("\n".join(lines) + "\n")
