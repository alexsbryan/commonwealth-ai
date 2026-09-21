#!/usr/bin/env python3
"""Pre-resolve census: what phase 1 emitted per section (runs/_phase1_checkpoint.jsonl).
Writes sketch_census.json into --out (default: next to this script)."""
import argparse, json, os, collections
import chapter_doc_map
HERE = os.path.dirname(os.path.abspath(__file__))
ap = argparse.ArgumentParser(description=__doc__)
ap.add_argument("--corpus", default="spike-fineprint-census")
ap.add_argument("--out", default=HERE)
a = ap.parse_args()
OUT = os.path.abspath(a.out)
C = os.path.expanduser(f"~/.svrnmesh/enrichment/{a.corpus}/runs/_phase1_checkpoint.jsonl")
chmap, chmap_source, chmap_services = chapter_doc_map.load(a.corpus, OUT)
# The spike censused a 449-section selection and pinned it in chapters_selected.txt.
# Absent that file the whole checkpoint is the census, which is what a --full run is.
selfile = os.path.join(OUT, "chapters_selected.txt")
sel = set(open(selfile).read().strip().split(",")) if os.path.exists(selfile) else None
rows = {}
for l in open(C):
    r = json.loads(l)
    if sel is None or r["chapter_id"] in sel:
        rows[r["chapter_id"]] = r   # last write wins
ok = [r for r in rows.values() if r["kind"] == "success"]
fail = [r for r in rows.values() if r["kind"] != "success"]
et = collections.Counter(); ck = collections.Counter(); deo = collections.Counter()
subj = 0; nclaims = 0; capped = []; svc_attr = collections.Counter(); svc_attr_missing = collections.Counter()
facets = collections.Counter()
for r in ok:
    se = r["extracted"].get("section_extraction") or {}
    for k, v in se.items():
        if isinstance(v, list): facets[k] += len(v)
    ents = se.get("entities_introduced") or []
    if len(ents) >= 15: capped.append((r["chapter_id"], chmap[r["chapter_id"]]["title"], chmap[r["chapter_id"]]["words"]))
    for e in ents:
        et[e.get("entity_type")] += 1
        if e.get("entity_type") in ("data_type", "recipient", "purpose"):
            if (e.get("attributes") or {}).get("service"): svc_attr[e["entity_type"]] += 1
            else: svc_attr_missing[e["entity_type"]] += 1
    for c in se.get("claims") or []:
        nclaims += 1; ck[c.get("claim_kind")] += 1
        if c.get("claim_kind") == "obligation":
            deo[str(c.get("deontic") or (c.get("attributes") or {}).get("deontic"))] += 1; subj += bool(c.get("subject"))
res = {"corpus": a.corpus, "checkpoint": C,
       "chapter_doc_map": {"source": chmap_source, "chapters": len(chmap), "services": chmap_services},
       "selection": selfile if sel is not None else "whole checkpoint",
       "sections_ok": len(ok), "sections_failed": len(fail),
       "failed": [(r["chapter_id"], str(r.get("failure") or "")[:160]) for r in fail],
       "facet_totals": dict(facets), "entity_type_sketches": dict(et.most_common()),
       "claim_kind_sketches": {str(k): v for k, v in ck.items()}, "obligation_deontic_sketches": dict(deo),
       "obligation_subject_filled": subj, "sections_at_entity_cap_15": capped,
       "service_attr_filled": dict(svc_attr), "service_attr_missing": dict(svc_attr_missing)}
print(json.dumps(res, indent=1, ensure_ascii=False))
os.makedirs(OUT, exist_ok=True)
json.dump(res, open(os.path.join(OUT, "sketch_census.json"), "w"), indent=1, ensure_ascii=False)
