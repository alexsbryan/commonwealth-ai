#!/usr/bin/env python3
"""Prototype of ONTOLOGY_PRIMITIVES.md §8 over a resolved atlas, driven by a facet file, not by deals.

    compose.py --corpus crm-ward-acts --facets compose.toml --out <dir>    # writes <dir>/atoms.json

Reads what the product has after Resolve: atoms.json, chapters.json (section -> chunk ids) and the
chunk store, whose per-chunk metadata is the document's own fields. Applies the declared facets:

  [[source]]   typed atoms from document metadata, no model, over EVERY document in the corpus;
               model-extracted atoms of that type merge into them on the declared identity key
               (strict merge), else on a unique folded name
  [clock]      a document date per atom (RFC 2822 -> ISO), so order is time, not section id
  [[compose]]  member claims (`of`) grouped inside a `block` value (the member's own attribute, else
               its document's outside party), linked by an equal `anchor`, a shared document
               `thread`, or `link` attributes that agree where both carry them; one entity of the
               composed type per group, members' subject set to it
  [[derive]]   one state per member of the composed type, mapped from a member attribute and dated by
               the clock. Written as `stage_update`-kind claims so crm-proof's scorer reads them
               unchanged; in Rust they are State atoms of the declared state type.

Every decision is counted in <dir>/compose_report.json (groups, why each link was made, members left
uncomposed and why) — the prototype's glass box.
"""
import argparse, collections, email.utils, hashlib, json, pathlib, re, sys, tomllib, unicodedata

HOME = pathlib.Path.home()


def fold(s):
    s = unicodedata.normalize("NFKD", str(s or "")).encode("ascii", "ignore").decode().lower()
    return " ".join(re.sub(r"[^a-z0-9]+", " ", s).split())


def squash(s):
    return " ".join(str(s or "").split()).lower()


def meta_value(v):
    return None if v in (None, "None", "", "[]") else v


def addresses(v):
    return [a.lower() for a in re.findall(r"[A-Za-z0-9._%+'-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}", str(v or ""))]


def iso(v):
    try:
        return email.utils.parsedate_to_datetime(v).isoformat()
    except (TypeError, ValueError):
        return None


def thread_key(subject):
    return fold(re.sub(r"^\s*((re|fw|fwd)\s*:\s*)+", "", str(subject or ""), flags=re.I)) or None


def documents(corpus):
    """message id -> {meta, text}; section id -> [message ids] — from chapters.json and the chunk store."""
    import lance  # noqa: PLC0415
    idx = HOME / ".svrnmesh/indexes" / corpus
    rows = lance.dataset(str(idx / "chunks.lance")).to_table(columns=["id", "metadata", "content"]).to_pylist()
    docs, chunk_doc = {}, {}
    for r in rows:
        m = json.loads(r["metadata"] or "{}")
        mid = m.get("message_id") or f"chunk:{r['id']}"
        d = docs.setdefault(mid, {"meta": m, "text": ""})
        d["text"] += " " + squash(r["content"])
        chunk_doc[str(r["id"])] = mid
    sections = {ch["id"]: sorted({chunk_doc[str(i)] for i in ch["chunk_ids"] if str(i) in chunk_doc})
                for ch in json.loads((idx / "chapters.json").read_text())["chapters"]}
    return docs, sections


def project(facets, docs):
    """[[source]] over every document: projected atoms per type, keyed by their identity value."""
    out = {}
    for src in facets.get("source", []):
        t, excl, own = src["type"], set(src.get("exclude_domains", [])), src.get("own")
        keyattr = next(iter(src["attributes"]))  # the first declared attribute is the identity key
        atoms = out.setdefault(t, {})
        for mid, d in docs.items():
            for field in src["metadata"]:
                for addr in addresses(meta_value(d["meta"].get(field))):
                    dom = addr.split("@", 1)[1]
                    key = addr if src["attributes"][keyattr] == "address" else dom
                    if src["attributes"][keyattr] == "domain" and dom in excl:
                        continue
                    a = atoms.setdefault(key, {"key": key, "docs": set(), "own": bool(own) and dom.endswith(own)})
                    a["docs"].add(mid)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--corpus", default="crm-ward-acts")
    ap.add_argument("--facets", type=pathlib.Path, default=pathlib.Path(__file__).resolve().parent / "compose.toml")
    ap.add_argument("--atoms", type=pathlib.Path, help="input atoms.json (default: the corpus atlas)")
    ap.add_argument("--out", type=pathlib.Path, required=True)
    a = ap.parse_args()
    facets = tomllib.loads(a.facets.read_text())
    docs, sections = documents(a.corpus)
    atoms = json.loads((a.atoms or HOME / ".svrnmesh/indexes" / a.corpus / "atlas/atoms.json").read_text())["atoms"]
    report = collections.Counter()

    # clock
    clock_field = facets.get("clock", {}).get("metadata")
    for d in docs.values():
        d["date"] = iso(meta_value(d["meta"].get(clock_field))) if clock_field else None
        d["thread"] = thread_key(meta_value(d["meta"].get("subject")))

    def doc_of(sec, anchor):
        mids = sections.get(sec, [])
        if anchor and len(mids) > 1:
            hit = [m for m in mids if squash(anchor)[:80] in docs[m]["text"]]
            if hit:
                return hit[0]
        return mids[0] if mids else None

    # [[source]]: projected atoms, model atoms merged on the identity key, else a unique folded name
    proj = project(facets, docs)
    ent = {x["data"]["id"]: x for x in atoms if x["atom_type"] == "Entity"}
    remap = {}
    new_atoms = [x for x in atoms if not (x["atom_type"] == "Entity" and x["data"].get("entity_type") in proj)]
    for t, keyed in proj.items():
        src = next(s for s in facets["source"] if s["type"] == t)
        keyattr = next(iter(src["attributes"]))
        pid = {k: f"{t}:{k}" for k in keyed}  # identity from essence: the declared key's value
        names = collections.defaultdict(set)
        for x in [e for e in ent.values() if e["data"].get("entity_type") == t]:
            v = (x["data"].get("attributes") or {}).get(keyattr)
            k = str(v).lower().strip() if v else None
            if k in pid:
                remap[x["data"]["id"]] = pid[k]; report[f"{t}: model atom merged on {keyattr}"] += 1
                names[k].add(x["data"].get("canonical_name"))
            else:
                names[None].add(x["data"]["id"])
        for k, i in pid.items():
            nm = sorted(n for n in names.get(k, set()) if n)
            new_atoms.append({"atom_type": "Entity", "data": {
                "id": i, "entity_type": t, "canonical_name": nm[0] if nm else k, "aliases": nm[1:],
                "attributes": {keyattr: k}, "provenance": {"signal_kind": "metadata_projection"},
                "own": keyed[k]["own"], "first_appearance": {}}})
        by_name = collections.defaultdict(list)
        for k, i in pid.items():
            for n in names.get(k, set()):
                by_name[fold(n)].append(i)
        for x in [e for e in ent.values() if e["data"].get("entity_type") == t and e["data"]["id"] not in remap]:
            hit = by_name.get(fold(x["data"].get("canonical_name")), [])
            if len(hit) == 1:
                remap[x["data"]["id"]] = hit[0]; report[f"{t}: model atom merged on unique name"] += 1
            else:
                new_atoms.append(x); report[f"{t}: model atom kept apart (no key, no unique name)"] += 1
        report[f"{t}: projected"] += len(pid)
    for x in new_atoms:
        d = x["data"]
        if d.get("subject") in remap:
            d["subject"] = remap[d["subject"]]
        for k, v in list((d.get("attributes") or {}).items()):
            if isinstance(v, str) and v in remap:
                d["attributes"][k] = remap[v]
    ent = {x["data"]["id"]: x["data"] for x in new_atoms if x["atom_type"] == "Entity"}

    # [[compose]] and [[derive]]
    claims = [x["data"] for x in new_atoms if x["atom_type"] == "Claim"]
    company_by_name = collections.defaultdict(set)
    for e in ent.values():
        company_by_name[fold(e.get("canonical_name"))].add(e["id"])
    for spec in facets.get("compose", []):
        members = [c for c in claims if c.get("claim_kind") in spec["of"]]
        own_ids = {i for i, e in ent.items() if e.get("own")}
        keyed = {i for i, e in ent.items() if (e.get("provenance") or {}).get("signal_kind") == "metadata_projection"}
        for c in members:
            ev = (c.get("evidence") or [{}])[0].get("chunk_id")
            c["_doc"] = doc_of(ev, c.get("anchor"))
            d = docs.get(c["_doc"], {})
            c["_date"], c["_thread"] = d.get("date"), d.get("thread")
            parties = sorted({pid for pid, e in ent.items() if e.get("entity_type") == "company" and not e.get("own")
                              and c["_doc"] in proj.get("company", {}).get((e.get("attributes") or {}).get("domain"), {}).get("docs", set())})
            attr = (c.get("attributes") or {}).get(spec["block"][0])
            own_attr = attr in own_ids
            if attr in keyed and not own_attr:
                c["_block"], why = attr, "member attribute, keyed"
            elif len(parties) == 1:
                c["_block"], why = parties[0], "document's one outside party"
            elif attr in ent and not own_attr:
                c["_block"], why = attr, "member attribute, unkeyed"
            elif attr and not own_attr and company_by_name.get(fold(attr)):
                c["_block"], why = sorted(company_by_name[fold(attr)])[0], "member attribute by name"
            else:
                c["_block"], why = None, ("several outside parties" if len(parties) > 1 else "no counterparty")
            report[f"{spec['type']}: block from {why}"] += 1
        parent = {id(c): id(c) for c in members}

        def find(x):
            while parent[x] != x:
                parent[x] = parent[parent[x]]; x = parent[x]
            return x

        def union(x, y, why):
            rx, ry = find(id(x)), find(id(y))
            if rx != ry:
                parent[rx] = ry; report[f"{spec['type']}: linked by {why}"] += 1

        blocks = collections.defaultdict(list)
        for c in members:
            if c["_block"]:
                blocks[c["_block"]].append(c)
        for blk, cs in blocks.items():
            for x, y in [(cs[i], cs[j]) for i in range(len(cs)) for j in range(i + 1, len(cs))]:
                ax = fold((x.get("attributes") or {}).get(spec.get("anchor"))) if spec.get("anchor") else ""
                ay = fold((y.get("attributes") or {}).get(spec.get("anchor"))) if spec.get("anchor") else ""
                if ax and ay:
                    if ax == ay:
                        union(x, y, "anchor")
                    continue  # two different named deals never link
                if spec.get("thread") and x["_thread"] and x["_thread"] == y["_thread"]:
                    union(x, y, "thread"); continue
                shared = [k for k in spec.get("link", []) if (x.get("attributes") or {}).get(k) and (y.get("attributes") or {}).get(k)]
                if shared and all(fold(x["attributes"][k]) == fold(y["attributes"][k]) for k in shared):
                    union(x, y, "link attributes")
        groups = collections.defaultdict(list)
        for c in members:
            if c["_block"]:
                groups[find(id(c))].append(c)
            else:
                report[f"{spec['type']}: member left uncomposed"] += 1
        der = next((d for d in facets.get("derive", []) if d["of"] == spec["type"]), None)
        for cs in groups.values():
            # identity from essence: the block plus the member set, never the group's position in a run
            did = f"{spec['type']}:" + hashlib.sha1("|".join([cs[0]["_block"]] + sorted(c["id"] for c in cs)).encode()).hexdigest()[:12]
            cs.sort(key=lambda c: c["_date"] or "")
            common = {k: collections.Counter(fold(c["attributes"][k]) for c in cs if (c.get("attributes") or {}).get(k)).most_common(1)
                      for k in spec.get("link", [])}
            cp = ent.get(cs[0]["_block"], {})
            new_atoms.append({"atom_type": "Entity", "data": {
                "id": did, "entity_type": spec["type"],
                "canonical_name": " / ".join([cp.get("canonical_name") or cs[0]["_block"]] + [v[0][0] for v in common.values() if v]),
                "attributes": {spec["block"][0]: cs[0]["_block"], **{k: v[0][0] for k, v in common.items() if v}},
                "first_appearance": (cs[0].get("evidence") or [{}])[0], "provenance": {"signal_kind": "composed"},
                "members": [c["id"] for c in cs]}})
            for c in cs:
                c["subject"] = did
                if der:
                    field = der["from"].split(".", 1)[1]
                    val = (c.get("attributes") or {}).get(field)
                    if val in der["map"]:
                        new_atoms.append({"atom_type": "Claim", "data": {
                            "id": f"{der['type']}-{c['id']}", "claim_kind": "stage_update", "subject": did,
                            "attributes": {"stage": der["map"][val], "document_date": c["_date"]},
                            "evidence": c.get("evidence"), "anchor": c.get("anchor"), "derived_from": c["id"]}})
                        report[f"{der['type']}: derived"] += 1
                    else:
                        report[f"{der['type']}: member has no mappable {field}"] += 1
        report[f"{spec['type']}: composed"] += len(groups)
    for c in claims:
        for k in [k for k in c if k.startswith("_")]:
            del c[k]
    a.out.mkdir(parents=True, exist_ok=True)
    (a.out / "atoms.json").write_text(json.dumps({"schema_version": "prototype", "atoms": new_atoms}, default=list))
    (a.out / "compose_report.json").write_text(json.dumps(dict(sorted(report.items())), indent=1) + "\n")
    print(json.dumps(dict(sorted(report.items())), indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
