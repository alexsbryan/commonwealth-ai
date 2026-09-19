#!/usr/bin/env python3
"""Pre-resolve census: what phase 1 emitted per section (runs/_phase1_checkpoint.jsonl)."""
import json, os, collections
HERE = os.path.dirname(os.path.abspath(__file__))
C = os.path.expanduser("~/.svrnmesh/enrichment/spike-fineprint-census/runs/_phase1_checkpoint.jsonl")
sel = set(open(os.path.join(HERE, "chapters_selected.txt")).read().strip().split(","))
chmap = json.load(open(os.path.join(HERE, "chapter_doc_map.json")))
rows = {}
for l in open(C):
    r = json.loads(l)
    if r["chapter_id"] in sel:
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
res = {"sections_ok": len(ok), "sections_failed": len(fail),
       "failed": [(r["chapter_id"], str(r.get("failure") or "")[:160]) for r in fail],
       "facet_totals": dict(facets), "entity_type_sketches": dict(et.most_common()),
       "claim_kind_sketches": {str(k): v for k, v in ck.items()}, "obligation_deontic_sketches": dict(deo),
       "obligation_subject_filled": subj, "sections_at_entity_cap_15": capped,
       "service_attr_filled": dict(svc_attr), "service_attr_missing": dict(svc_attr_missing)}
print(json.dumps(res, indent=1, ensure_ascii=False))
json.dump(res, open(os.path.join(HERE, "sketch_census.json"), "w"), indent=1, ensure_ascii=False)
