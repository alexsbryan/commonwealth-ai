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


def ask(cache, system, user, schema, model="commonwealth/primary"):
    """One structured call to the local daemon, cached by its whole input so a rerun replays it."""
    import time, urllib.error, urllib.request  # noqa: PLC0415, E401
    key = hashlib.sha1(json.dumps([model, system, user, schema], sort_keys=True).encode()).hexdigest()[:16]
    path = cache / f"{key}.json"
    if path.exists():
        return json.loads(path.read_text())["answer"], True
    body = {"model": model, "temperature": 0, "max_tokens": 16384,
            "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
            "response_format": {"type": "json_schema", "json_schema": {"name": "answer", "strict": True, "schema": schema}}}
    req = urllib.request.Request("http://localhost:9741/v1/chat/completions", json.dumps(body).encode(),
                                 {"Content-Type": "application/json"})
    t0 = time.time()
    for attempt in range(20):  # the daemon sheds past its queue budget with 503 + Retry-After: honour it
        try:
            with urllib.request.urlopen(req, timeout=1800) as r:
                resp = json.loads(r.read())
            break
        except urllib.error.HTTPError as e:
            if e.code != 503 or attempt == 19:
                raise
            time.sleep(float(e.headers.get("Retry-After") or 15))
    answer = json.loads(resp["choices"][0]["message"]["content"])
    cache.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"model": model, "wall_s": round(time.time() - t0, 1), "shed_retries": attempt, "usage": resp.get("usage"),
                                "system": system, "user": user, "answer": answer}, indent=1))
    return answer, False


def group_schema(t):
    return {"type": "object", "required": ["groups"], "properties": {"groups": {"type": "array", "items": {
        "type": "object", "required": ["canonical", "names", "keys", "parent"], "properties": {
            "canonical": {"type": "string"}, "names": {"type": "array", "items": {"type": "string"}},
            "keys": {"type": "array", "items": {"type": "string"}}, "parent": {"type": "string"}}}}}}


BLOCK_SCHEMA = {"type": "object", "required": ["groups"], "properties": {"groups": {"type": "array", "items": {
    "type": "object", "required": ["canonical", "members", "parent"], "properties": {
        "canonical": {"type": "string"}, "members": {"type": "array", "items": {"type": "string"}},
        "parent": {"type": "string"}}}}}}
LEGAL = {"inc", "incorporated", "corp", "corporation", "co", "company", "llc", "llp", "lp", "ltd", "limited",
         "plc", "the", "of", "and", "s"}
# words that name what kind of organisation something is, not which one: a closed set (ARCH 9)
GENERIC = {"energy", "power", "electric", "electrical", "gas", "natural", "services", "service", "utilities", "utility",
           "resources", "group", "associates", "partners", "international", "holdings", "systems", "technologies",
           "industries", "department", "district", "water", "supply", "products", "marketing", "trading", "capital",
           "management", "operations", "pipeline", "transmission", "generation", "cooperative", "coop", "authority",
           "municipal", "public", "project", "plant", "plants", "american", "national", "western", "southwest",
           "pacific", "arizona", "california", "texas", "socal", "north", "south", "east", "west", "city", "county"}
KEY_NOISE = {"com", "org", "net", "us", "gov", "edu", "co", "uk", "au", "ca", "www", "mail", "ci", "city", "state"}


def candidate_blocks(names, keys):
    """High-recall candidate blocks for identity, in code: names sharing a rare word, a name and its
    acronym, a near-identical spelling, a key whose label carries a name's word or acronym, keys sharing a
    label. Recall is the job; the model splits a block, it never joins two."""
    def words(n):
        return [w for w in fold(re.sub(r"\(.*?\)", " ", n)).split() if w not in LEGAL]

    def acros(n):
        f = fold(re.sub(r"\(.*?\)", " ", n)).split()
        out = {"".join(w[0] for w in f if w not in {"of", "and", "the", "s"}), "".join(w[0] for w in words(n))}
        out |= {fold(x) for x in re.findall(r"\(([^)]*)\)", n)}
        if len(f) == 1 and 2 <= len(f[0]) <= 6:
            out.add(f[0])
        return {a for a in out if len(a) >= 2}

    def labels(k):
        return {p for p in fold(k.replace(".", " ").replace("-", "")).split() if p not in KEY_NOISE and len(p) >= 2}

    def registrable(k):  # the organisation's own label: bhp.com.au -> bhp, ci.mesa.az.us -> mesa
        p = k.lower().split(".")
        if len(p) >= 3 and len(p[-1]) == 2 and (p[-2] in {"com", "co", "org", "net", "gov", "ac"} or p[-1] == "us"):
            return p[-3]
        return p[-2] if len(p) >= 2 else p[0]
    items = list(names) + list(keys)
    parent = {i: i for i in items}

    def find(x):
        while parent[x] != x:
            parent[x] = parent[parent[x]]; x = parent[x]
        return x

    def union(a, b):
        parent[find(a)] = find(b)
    W = {n: set(words(n)) for n in names}
    A = {n: acros(n) for n in names}
    L = {k: labels(k) for k in keys}
    df = collections.Counter(w for n in names for w in W[n])
    rare = {w for w, c in df.items() if c <= 25 and len(w) >= 4 and w not in GENERIC}
    by_word, by_acro, by_label = collections.defaultdict(list), collections.defaultdict(list), collections.defaultdict(list)
    for n in names:
        for w in W[n] & rare:
            by_word[w].append(n)
        for a in A[n]:
            by_acro[a].append(n)
    by_reg = collections.defaultdict(list)
    for k in keys:
        for lab in L[k]:
            by_label[lab].append(k)
        by_reg[registrable(k)].append(k)
    for group in list(by_word.values()) + list(by_reg.values()):
        for x in group[1:]:
            union(group[0], x)
    for n in names:
        whole = "".join(words(n))
        for w in list(W[n]) + [whole]:
            for k in by_label.get(w, []):
                union(n, k)
        f = fold(n).replace(" ", "")
        for m in by_acro.get(f, []):  # n is the acronym of m
            union(n, m)
        for k in by_label.get(f, []):
            union(n, k)
        for a in A[n]:
            for k in by_label.get(a, []):
                union(n, k)
        for lab, ks in by_label.items():  # a label that carries a long rare word: tucsonelectric > tucson
            if len(lab) > 6 and any(len(w) >= 5 and w in lab for w in W[n] & rare):
                for k in ks:
                    union(n, k)
    spelled = sorted(names, key=lambda n: " ".join(words(n)))
    for a, b in zip(spelled, spelled[1:]):  # neighbours in sorted order: Tuscon / Tucson
        x, y = " ".join(words(a)), " ".join(words(b))
        if len(x) >= 8 and len(y) >= 8 and edits(x, y) <= 2:
            union(a, b)
    out = collections.defaultdict(list)
    for i in items:
        out[find(i)].append(i)
    return [sorted(v) for v in out.values() if any(i in names for i in v) or len(v) > 1]


def edits(a, b):
    """Levenshtein distance, capped early: only small distances matter."""
    if abs(len(a) - len(b)) > 2:
        return 3
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, 1):
        cur = [i]
        for j, cb in enumerate(b, 1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def identity_pass(spec, facets, proj, new_atoms, report, cache):
    """[[identity]] adjudicate: every name and key a type was seen under, grouped by the model in one pass;
    the answer is held to the input (no invented name or key), a part folds into its whole, and an
    atom's id stays its key when it has one (identity from essence), else its group's folded name."""
    t = spec["type"]
    src = next(s for s in facets["source"] if s["type"] == t)
    keyattr, own = next(iter(src["attributes"])), src.get("own")
    E = [x["data"] for x in new_atoms if x["atom_type"] == "Entity" and x["data"].get("entity_type") == t]
    keyed = {e["attributes"][keyattr]: e for e in E if (e.get("provenance") or {}).get("signal_kind") == "metadata_projection"}
    loose = [e for e in E if e is not keyed.get((e.get("attributes") or {}).get(keyattr))]
    docs = {k: len(proj[t][k]["docs"]) for k in keyed}
    names = collections.Counter()
    for e in loose:
        for n in [e.get("canonical_name")] + list(e.get("aliases") or []):
            if n:
                names[n] += 1
    for k, e in keyed.items():
        for n in [e.get("canonical_name")] + list(e.get("aliases") or []):
            if n and n != k:
                names[n] += 1
    claims = [x["data"] for x in new_atoms if x["atom_type"] == "Claim"]
    slots = [tuple(s.split(".", 1)) for s in spec.get("names_from", [])]
    ids = {e["id"] for e in E}
    for c in claims:
        for kind, attr in slots:
            v = (c.get("attributes") or {}).get(attr)
            if c.get("claim_kind") == kind and isinstance(v, str) and v and v not in ids:
                names[v] += 1
    means = f" A {t} here means {spec['description']}." if spec.get("description") else ""
    system = f"You resolve the identity of {t} records found in one archive. Answer only from the items given."
    taken, groups = {}, []

    def add(canonical, members, parent=""):
        ns = [m for m in members if m in names and m not in taken]
        ks = [m for m in members if m in keyed and m not in taken]
        if not ns and not ks:
            return None
        canonical = canonical if canonical and fold(canonical) else max(ns or ks, key=len)
        g = {"canonical": canonical, "names": ns, "keys": ks, "parent": parent, "kind": t,
             "id": f"{t}:{max(ks, key=lambda k: (docs[k], k))}" if ks else f"{t}:name:{fold(canonical)}"}
        for m in ns + ks:
            taken[m] = g
        groups.append(g)
        return g

    # the owner's family first: one focused question, its answer joins the owner's key
    if own and own in keyed:
        ans, cached = ask(cache, system, (
            f"The archive belongs to the {t} whose {keyattr} is {own}. Which of these names refer to that {t} "
            f"itself or to one of its divisions, subsidiaries or affiliates (acronyms included)? Answer with the "
            f"names exactly as written.\n\nNAMES:\n" + "\n".join(sorted(names))),
            {"type": "object", "required": ["names"], "properties": {"names": {"type": "array", "items": {"type": "string"}}}})
        report[f"{t} identity: owner question {'replayed' if cached else 'asked'}"] += 1
        fam = [n for n in ans["names"] if n in names]
        report[f"{t} identity: names in the owner's family"] += len(fam)
        add(keyed[own].get("canonical_name"), fam + [own])
    # candidate blocks, high recall, in code; one small question per block of two or more
    blocks = candidate_blocks([n for n in names if n not in taken], [k for k in keyed if k not in taken])
    report[f"{t} identity: candidate blocks of 2+"] += sum(1 for b in blocks if len(b) > 1)
    for b in blocks:
        if len(b) == 1 or not any(m in names for m in b):  # keys sharing their registrable label: one thing
            add(b[0] if b[0] in names else None, b)
            continue
        ans, cached = ask(cache, system, (
            f"These {t} names and {keyattr}s were seen in one archive.{means} Group the items that refer to the "
            f"same {t}: short forms, acronyms and misspellings of one name belong together, and a {keyattr} "
            f"belongs with the {t} it is the {keyattr} of. Every item is in exactly one group. canonical is the "
            f"fullest name in the group. When a group is a division, subsidiary or affiliate of another group "
            f"here, parent is that group's canonical, else \"\".\n\nITEMS:\n" +
            "\n".join(sorted(b))), BLOCK_SCHEMA)
        report[f"{t} identity: block question {'replayed' if cached else 'asked'}"] += 1
        for gr in ans["groups"]:
            report[f"{t} identity: items outside the block, dropped"] += sum(1 for m in gr["members"] if m not in b)
            add(gr["canonical"], [m for m in gr["members"] if m in b], gr["parent"])
        for m in b:  # an item the answer left out keeps its own group
            if m not in taken and m in names:
                add(m, [m]); report[f"{t} identity: item the answer left out, kept alone"] += 1
    by_canon = {fold(g["canonical"]): g for g in groups}

    def root(g, seen=()):
        p = by_canon.get(fold(g["parent"])) if g["parent"] and spec.get("hierarchy", "fold") == "fold" else None
        return g if p is None or p is g or p["id"] in seen else root(p, seen + (g["id"],))
    final = {}
    for g in groups:
        r = root(g)
        final[g["id"]] = None if g["kind"] != t else r["id"]
        if r is not g:
            report[f"{t} identity: part folded into its whole"] += 1
    owners = {g["id"] for g in groups if own and any(k.endswith(own) for k in g["keys"])}
    report[f"{t} identity: groups"] += len(groups)
    report[f"{t} identity: names not grouped (kept as they were)"] += sum(1 for n in names if n not in taken)
    report[f"{t} identity: names that are not a {t}"] += sum(len(g["names"]) for g in groups if g["kind"] != t)
    remap = {}
    for e in loose:
        g = taken.get(e.get("canonical_name"))
        if g is not None:
            remap[e["id"]] = final[g["id"]]
    for k, e in keyed.items():
        g = taken.get(k)
        if g is not None and final[g["id"]] is None:
            report[f"{t} identity: keyed atom kept although its group is not a {t}"] += 1
        elif g is not None and final[g["id"]] != e["id"]:
            remap[e["id"]] = final[g["id"]]
    # rebuild: one atom per surviving group id, keyed atoms keep their record
    keep = [x for x in new_atoms if not (x["atom_type"] == "Entity" and x["data"]["id"] in remap)]
    have = {x["data"]["id"]: x["data"] for x in keep if x["atom_type"] == "Entity"}
    for g in groups:
        fid = final[g["id"]]
        if fid is None:
            continue
        members = [x for x in groups if final[x["id"]] == fid]
        top = next(x for x in members if x["id"] == fid) if any(x["id"] == fid for x in members) else g
        alias = sorted({n for x in members for n in x["names"]} - {top["canonical"]})
        d = have.get(fid)
        if d is None:
            d = {"id": fid, "entity_type": t, "canonical_name": top["canonical"], "attributes": {},
                 "provenance": {"signal_kind": "identity_adjudicated"}, "first_appearance": {}}
            keep.append({"atom_type": "Entity", "data": d}); have[fid] = d
        d["canonical_name"], d["aliases"] = top["canonical"], alias
        d["own"] = bool(owners & {x["id"] for x in members}) or d.get("own", False)
    for x in keep:
        d = x["data"]
        if d.get("subject") in remap:
            d["subject"] = remap[d["subject"]]
        for k, v in list((d.get("attributes") or {}).items()):
            if isinstance(v, str) and v in remap:
                d["attributes"][k] = remap[v]
            elif isinstance(v, str) and v in taken and (d.get("claim_kind"), k) in slots:
                d["attributes"][k] = final[taken[v]["id"]]
                report[f"{t} identity: claim's raw {k} resolved" if final[taken[v]["id"]] else f"{t} identity: claim's raw {k} is not a {t}"] += 1
    return keep


def partition(spec, blk, cs, ent, state, clash, cache, report):
    """[[compose]] adjudicate: the block's members as a dated timeline, grouped by the model into composed
    atoms with a declared kind. Members are offered as an enum of their own labels, so an invented member
    cannot be written; a member the answer repeats keeps its first group, one it leaves out stands alone,
    and a group that breaks a distinct attribute is split by code."""
    kinds = list(spec.get("kinds", {"default": ""})) if not spec.get("kind_from") else ["any"]
    if len(cs) == 1:
        return [(kinds[0], cs)]
    cs = sorted(cs, key=lambda c: (c["_date"] or "", c["id"]))
    lab = {f"m{i}": c for i, c in enumerate(cs)}
    rows = []
    for l, c in lab.items():
        at = c.get("attributes") or {}
        bits = [l, (c["_date"] or "")[:10], f"subject: {c.get('_subject') or ''}"]
        bits += [f"{k}: {v}" for k, v in at.items() if v and k not in spec["block"]]
        bits.append(f'"{(c.get("anchor") or "")[:220]}"')
        rows.append(" | ".join(bits))
    name = (ent.get(blk) or {}).get("canonical_name") or blk
    kd = "; ".join(f"{k} = {v}" for k, v in spec.get("kinds", {}).items())
    system = "You decide which records describe the same real-world thing. Answer only from the records given."
    user = (f"These are mentions of {spec['type']}s with {name}, in date order. Group them so that each group is one "
            f"{spec['type']}: mentions of the same {spec['type']} talk about the same arrangement; a different "
            f"product, a different period or a fresh round of the same business is a different {spec['type']}. "
            f"Every mention is in exactly one group." + (f" kind: {kd}." if kinds != ["any"] else "") + "\n\n" + "\n".join(rows))
    schema = {"type": "object", "required": ["groups"], "properties": {"groups": {"type": "array", "items": {
        "type": "object", "required": ["members", "kind"], "properties": {
            "members": {"type": "array", "minItems": 1, "items": {"type": "string", "enum": list(lab)}},
            "kind": {"type": "string", "enum": kinds}}}}}}
    ans, cached = ask(cache, system, user, schema)
    report[f"{spec['type']}: partition {'replayed' if cached else 'asked'}"] += 1
    seen, out = set(), []
    for gr in ans["groups"]:
        part = [lab[m] for m in dict.fromkeys(gr["members"]) if m in lab and m not in seen]
        seen |= {m for m in gr["members"] if m in lab}
        subs = []  # split by code where the group breaks a distinct attribute
        for c in part:
            for sub in subs:
                merged = dict(sub["st"])
                if not clash(merged, state[id(c)]):
                    sub["cs"].append(c)
                    for k, v in state[id(c)].items():
                        merged[k] = (merged[k] | v) if isinstance(v, set) and k in merged else (
                            (max(merged[k][0], v[0]), min(merged[k][1], v[1])) if k in merged else v)
                    sub["st"] = merged
                    break
            else:
                subs.append({"cs": [c], "st": dict(state[id(c)])})
        report[f"{spec['type']}: partition group split by code, distinct broken"] += len(subs) - 1 if subs else 0
        out += [(gr["kind"], sub["cs"]) for sub in subs]
    for l, c in lab.items():
        if l not in seen:
            out.append((kinds[0], [c])); report[f"{spec['type']}: member the partition left out, alone"] += 1
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--corpus", default="crm-ward-acts")
    ap.add_argument("--facets", type=pathlib.Path, default=pathlib.Path(__file__).resolve().parent / "compose.toml")
    ap.add_argument("--atoms", type=pathlib.Path, help="input atoms.json (default: the corpus atlas)")
    ap.add_argument("--out", type=pathlib.Path, required=True)
    ap.add_argument("--cache", type=pathlib.Path, default=HOME / ".svrnmesh/bench-corpora/enron-ward/cache/adjudicate",
                    help="model answers, keyed by their whole input (mailbox-derived: never in git)")
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

    # [[identity]]: one model pass over a type's whole candidate set (names, keys) groups them into
    # one atom per real-world thing; code holds the model to the input and applies the answer
    for spec in facets.get("identity", []):
        new_atoms = identity_pass(spec, facets, proj, new_atoms, report, a.cache)
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

        def company(v):  # a member's value as a company atom: its id, else a name with an atom
            return v if v in ent else sorted(company_by_name[fold(v)])[0] if v and company_by_name.get(fold(v)) else None
        # an agent is a role a company plays across the mailbox: named as some member's via, it is never the
        # block of the documents it writes, whose members then keep their own party
        agents = {company(c["attributes"]["via"]) for c in members if (c.get("attributes") or {}).get("via")} - {None}
        report[f"{spec['type']}: companies named as an agent (via)"] += len(agents)
        for c in members:
            ev = (c.get("evidence") or [{}])[0].get("chunk_id")
            c["_doc"] = doc_of(ev, c.get("anchor"))
            d = docs.get(c["_doc"], {})
            c["_date"], c["_thread"], c["_subject"] = d.get("date"), d.get("thread"), meta_value(d.get("meta", {}).get("subject"))
            parties = sorted({pid for pid, e in ent.items() if e.get("entity_type") == "company" and not e.get("own")
                              and c["_doc"] in proj.get("company", {}).get((e.get("attributes") or {}).get("domain"), {}).get("docs", set())})
            attr = (c.get("attributes") or {}).get(spec["block"][0])
            # the owner is never a block, whether the member names it by id or by name
            own_attr = attr in own_ids or bool(company_by_name.get(fold(attr), set()) & own_ids)
            c["_block"], why = None, ("several outside parties" if len(parties) > 1 else "no counterparty")
            via_id = company((c.get("attributes") or {}).get("via"))
            # the order the block is read in is declared: member = the member's own party; document = its
            # document's one outside party unless that party is an agent; agent = the member's own party
            # when the member names that correspondent as its via (an agent writes for its principal)
            for src in spec.get("block_from", ["member", "document"]):
                if src == "agent" and not (len(parties) == 1 and via_id == parties[0]):
                    continue
                if src == "document" and len(parties) == 1 and parties[0] in agents:
                    report[f"{spec['type']}: document's one outside party is an agent, not its block"] += 1
                    continue
                if src in ("member", "agent") and attr in ent and not own_attr:
                    c["_block"], why = attr, "member's own party" + (", its via the correspondent" if src == "agent" else "")
                elif src in ("member", "agent") and attr and not own_attr and company_by_name.get(fold(attr)):
                    c["_block"], why = sorted(company_by_name[fold(attr)])[0], "member's own party by name" + (", its via the correspondent" if src == "agent" else "")
                elif src == "document" and len(parties) == 1:
                    c["_block"], why = parties[0], "document's one outside party"
                else:
                    continue
                break
            if why == "member's own party" and parties and c["_block"] not in parties:
                c["_via"] = parties
                report[f"{spec['type']}: member's party is not its document's correspondent (via)"] += 1
            report[f"{spec['type']}: block from {why}"] += 1
        if spec.get("thread"):
            # a thread links its members, so an unblocked member inherits its thread's block when there
            # is exactly one: first from blocked members in the thread, else from the thread's documents
            outside = {e["id"]: e for e in ent.values() if e.get("entity_type") == "company" and not e.get("own")}
            doc_parties = collections.defaultdict(set)
            for key, p in proj.get("company", {}).items():
                if f"company:{key}" in outside:
                    for mid in p["docs"]:
                        doc_parties[mid].add(f"company:{key}")
            thread_blocks, thread_parties = collections.defaultdict(set), collections.defaultdict(set)
            for c in members:
                if c["_block"] and c["_thread"]:
                    thread_blocks[c["_thread"]].add(c["_block"])
            for mid, d in docs.items():
                if d["thread"]:
                    thread_parties[d["thread"]] |= doc_parties.get(mid, set())
            for c in members:
                if c["_block"] or not c["_thread"]:
                    continue
                for src, cand in (("thread's one member block", thread_blocks), ("thread's one outside party", thread_parties)):
                    if len(cand.get(c["_thread"], ())) == 1:
                        c["_block"] = next(iter(cand[c["_thread"]]))
                        report[f"{spec['type']}: block inherited from {src}"] += 1
                        break
                else:
                    report[f"{spec['type']}: block not inherited ({len(thread_blocks.get(c['_thread'], ()))} member blocks, "
                           f"{len(thread_parties.get(c['_thread'], ()))} outside parties in thread)"] += 1
        parent = {id(c): id(c) for c in members}
        # distinct: typed identity attributes two members of one composed atom can never disagree on,
        # held per CLUSTER so no chain of links can join them either (an equal set for "equal", a common
        # window for "overlap"); a merge that would break one is refused, whatever the evidence
        distinct = spec.get("distinct", {})
        state, refusals = {}, []
        for c in members:
            at = c.get("attributes") or {}
            st = {}
            for k, kind in distinct.items():
                if kind == "equal" and at.get(k):
                    st[k] = {fold(at[k])}
                elif kind == "overlap":
                    lo, hi = at.get(f"{k}_start"), at.get(f"{k}_end")
                    if lo or hi:
                        st[k] = (lo or hi, hi or lo)
            state[id(c)] = st

        def find(x):
            while parent[x] != x:
                parent[x] = parent[parent[x]]; x = parent[x]
            return x

        def clash(a, b):
            for k, kind in distinct.items():
                if k in a and k in b:
                    if kind == "equal" and len(a[k] | b[k]) > 1:
                        return k
                    if kind == "overlap" and max(a[k][0], b[k][0]) > min(a[k][1], b[k][1]):
                        return k
            return None

        def union(x, y, why):
            rx, ry = find(id(x)), find(id(y))
            if rx == ry:
                return
            k = clash(state[rx], state[ry])
            if k:
                report[f"{spec['type']}: link by {why} refused, {k} disagrees"] += 1
                refusals.append({"x": x["id"], "y": y["id"], "evidence": why, "distinct": k})
                return
            a, b = state[rx], state[ry]
            for key, kind in distinct.items():
                if key in a and key in b:
                    b[key] = a[key] | b[key] if kind == "equal" else (max(a[key][0], b[key][0]), min(a[key][1], b[key][1]))
                elif key in a:
                    b[key] = a[key]
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
        kind_of = {}
        if spec.get("adjudicate"):
            # pattern-level identity: one counterparty's whole timeline, partitioned by the model; code holds
            # the answer to the type (each member once, distinct never broken, kinds from the declared set)
            groups, kind_of = {}, {}
            for blk, cs in blocks.items():
                for kind, part in partition(spec, blk, cs, ent, state, clash, a.cache, report):
                    key = ("adj", blk, min(c["id"] for c in part))
                    groups[key] = part
                    if not spec.get("kind_from"):  # a declared member kind outranks the partition's guess
                        kind_of[key] = kind
        if spec.get("kind_from"):  # a declared member attribute names the composed kind, by its members' majority
            for key, cs in groups.items():
                if key not in kind_of:
                    ks = collections.Counter((c.get("attributes") or {}).get(spec["kind_from"]) for c in cs)
                    ks.pop(None, None)
                    kind_of[key] = ks.most_common(1)[0][0] if ks else None
                    report[f"{spec['type']}: kind by majority over mixed members"] += len(ks) > 1
        a.out.mkdir(parents=True, exist_ok=True)
        (a.out / f"refusals-{spec['type']}.json").write_text(json.dumps(refusals))
        der = next((d for d in facets.get("derive", []) if d["of"] == spec["type"]), None)
        for key, cs in groups.items():
            ctype = spec.get("route", {}).get(kind_of.get(key), spec["type"])
            # identity from essence: the block plus the member set, never the group's position in a run
            did = f"{ctype}:" + hashlib.sha1("|".join([cs[0]["_block"]] + sorted(c["id"] for c in cs)).encode()).hexdigest()[:12]
            cs.sort(key=lambda c: c["_date"] or "")
            common = {k: collections.Counter(fold(c["attributes"][k]) for c in cs if (c.get("attributes") or {}).get(k)).most_common(1)
                      for k in spec.get("link", [])}
            cp = ent.get(cs[0]["_block"], {})
            new_atoms.append({"atom_type": "Entity", "data": {
                "id": did, "entity_type": ctype, "kind": kind_of.get(key),
                "canonical_name": " / ".join([cp.get("canonical_name") or cs[0]["_block"]] + [v[0][0] for v in common.values() if v]),
                "attributes": {spec["block"][0]: cs[0]["_block"], **{k: v[0][0] for k, v in common.items() if v}},
                "first_appearance": (cs[0].get("evidence") or [{}])[0], "provenance": {"signal_kind": "composed"},
                "members": [c["id"] for c in cs]}})
            for c in cs:
                c["subject"] = did
                if der and ctype == spec["type"]:
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
