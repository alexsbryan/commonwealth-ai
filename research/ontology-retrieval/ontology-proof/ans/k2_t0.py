#!/usr/bin/env python3
"""K2 T0 (feature-fidelity campaign, Ontology): two model-free ceilings per K2 question.

    python3 k2_t0.py [--bank bank-k2-dev.toml] [--json k2-t0.json]

RECORDS: a fixed program per class computes the answer from the typed records of the
corpus's atlas alone (atoms.json: hoard / mint / coin entities and their attributes, coin
`hoard` refs, Relations, Claims' structured `subject` + `proposed_date`). No chunk text, no
section titles, no claim prose. Hoards are grouped by the IGCH number their canonical name
cites (the only identity criterion a record carries); a mint name maps to a truth label by
the bank's own [[mints]] match table. Scored against the bank's gold: set P/R/F1 + made-up
count for lists, exact for counts and superlatives; neutral items are ignored either way.
Each missed gold item is split by the first stage where the records stop, mirroring
records_recall.py: `hoard` (no entity group resolves to the hoard; `merged` when another
entity's aliases name it), `link` (the hoard resolves but no Relation / coin ref links the
mint; `spurious link` for a negation the records contradict), `attribute` (findspot or burial
missing or on the wrong side; when a burial date is missing, whether a Claim in the hoard's
sections states one with a `proposed_date` but no hoard subject, or only in its prose). The
diagnosis may read Claim prose to say WHY; it never feeds an answer. An exact count or
superlative whose needed facts the records lack is listed as `missing_support`.

RAG: dense top-k chunks for the question (cosine against chunks.lance `embedding`; the
question embedded with the qwen3 query instruction, embed_quirks.rs, as sim.py does). A gold
item is answerable at k when, for one of its witnesses, every fact has an attesting chunk in
the top k: a perfect reader's ceiling (precision 1, so F1 = 2R/(1+R)). A count or superlative
is answerable only when every needed fact is. k = 20 (plain RAG) and 80 (wide); 5 and 10 are
printed too because the fixture has only 51 chunks, so k=80 is the whole corpus.
"""
import argparse, collections, json, pathlib, re, sys, time, tomllib, urllib.error, urllib.request

import lance
import numpy as np

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from make_bank import fold, rx  # noqa: E402  (one matcher: the bank's)
from records_recall import load_atlas  # noqa: E402  (one atlas reader)

IDX = pathlib.Path.home() / ".svrnmesh/indexes"
DAEMON = "http://127.0.0.1:9741/v1/embeddings"
# sovereign-contracts/src/embed_quirks.rs qwen3_embedding.query_instruction (as sim.py)
QI = "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: "
KS = (5, 10, 20, 80)
LISTS = ("hoards", "mints")


# ── RAG ────────────────────────────────────────────────────────────────────
def embed(texts, batch=64):
    out = []
    for i in range(0, len(texts), batch):
        chunk = texts[i:i + batch]
        for attempt in range(8):
            try:
                req = urllib.request.Request(DAEMON, data=json.dumps({"model": "embed", "input": chunk}).encode(),
                                             headers={"Content-Type": "application/json"})
                d = json.load(urllib.request.urlopen(req, timeout=180))
                out += [np.asarray(x["embedding"], np.float32) for x in sorted(d["data"], key=lambda x: x["index"])]
                break
            except (urllib.error.URLError, TimeoutError, ConnectionError) as e:   # HTTPError 503 is a URLError
                if attempt == 7:
                    raise
                print(f"embed retry {attempt + 1}: {e}", file=sys.stderr)
                time.sleep(2 * (attempt + 1))
    return out


def topk(corpus, questions):
    t = lance.dataset(str(IDX / corpus / "chunks.lance")).to_table(columns=["id", "embedding"]).to_pydict()
    ids = np.asarray(t["id"])
    M = np.asarray(t["embedding"], np.float32)
    M /= np.linalg.norm(M, axis=1, keepdims=True)
    Q = np.stack(embed([QI + q for q in questions]))
    Q /= np.linalg.norm(Q, axis=1, keepdims=True)
    order = np.argsort(-(Q @ M.T), axis=1, kind="stable")
    return [[int(ids[j]) for j in row] for row in order], len(ids)


def rag(q, ranked):
    res = {}
    for k in KS:
        top = set(ranked[:k])
        hit = lambda facts: all(set(f["chunks"]) <= top if f.get("all_chunks") else set(f["chunks"]) & top  # noqa: E731
                                for f in facts)
        if q["answer_type"] in LISTS:
            ok = [any(hit(w) for w in g["witnesses"]) for g in q["gold_items"]]
            r = sum(ok) / len(ok)
            res[k] = {"recall": r, "f1": 2 * r / (1 + r), "answerable": [g["id"] for g, o in zip(q["gold_items"], ok) if o]}
        else:
            res[k] = {"exact": float(hit(q["needed_facts"]))}
    return res


# ── records ────────────────────────────────────────────────────────────────
class Records:
    def __init__(self, atlas, bank, corpus):
        self.ents, self.rels = load_atlas(atlas)
        atoms = json.loads((atlas / "atoms.json").read_text())["atoms"]
        self.claims = [a["data"] for a in atoms if a["atom_type"] == "Claim"]
        self.mints = bank["mints"]
        self.hoards = {h["id"]: h for h in bank["hoards"]}
        chapters = json.loads((IDX / corpus / "chapters.json").read_text())["chapters"]
        self.c2s = {cid: c["id"] for c in chapters for cid in c["chunk_ids"]}
        groups = collections.defaultdict(list)
        for e in self.ents.values():
            if e.get("entity_type") == "hoard":
                n = re.search(r"IGCH\s*(\d+)", e.get("canonical_name") or "")
                groups[f"igch{int(n.group(1)):04d}" if n else e["id"]].append(e)
        self.groups = dict(groups)
        self.members = {g: self._members(es) for g, es in self.groups.items()}
        self.findspot = {g: " ; ".join((e.get("attributes") or {}).get("findspot") or "" for e in es) for g, es in self.groups.items()}
        self.burial = {g: self._burial(es) for g, es in self.groups.items()}
        self.resolved = {g: self.resolve(self.display(g)) for g in self.groups}

    def display(self, g):
        es = self.groups[g]
        return next((e["canonical_name"] for e in es if re.search(r"IGCH", e["canonical_name"])), es[0]["canonical_name"])

    def labels(self, name):
        n = fold(name or "")
        return {m["id"] for m in self.mints if any(rx(p).search(n) for p in m["match"])}

    def _mint_of(self, coin):
        v = (coin.get("attributes") or {}).get("mint")
        return self.ents[v]["canonical_name"] if v in self.ents else v

    def _members(self, es):
        ids, names = {e["id"] for e in es}, []
        for r in self.rels:
            p = set(r.get("participants") or [])
            if p & ids:
                for pid in p - ids:
                    t = self.ents.get(pid, {}).get("entity_type")
                    names.append(self.ents[pid]["canonical_name"] if t == "mint" else self._mint_of(self.ents[pid]) if t == "coin" else None)
        names += [self._mint_of(c) for c in self.ents.values()
                  if c.get("entity_type") == "coin" and (c.get("attributes") or {}).get("hoard") in ids]
        return set().union(*[self.labels(n) for n in names if n]) if names else set()

    def _burial(self, es):
        ids, texts = {e["id"] for e in es}, []
        texts += [(e.get("attributes") or {}).get("buried") for e in es]
        texts += [(c.get("attributes") or {}).get("proposed_date") for c in self.claims if c.get("subject") in ids]
        ys = [int(y) for t in texts if t for y in re.findall(r"(?<!\d)(\d{3})(?!\d)", t) if 100 <= int(y) <= 400]
        return (max(ys), min(ys)) if ys else None

    def resolve(self, name):
        n = re.search(r"IGCH\s*(\d+)", name or "")
        if n:
            return f"igch{int(n.group(1)):04d}"
        hits = [h["id"] for h in self.hoards.values() if any(rx(p).search(fold(name)) for p in h["match"] if not p.startswith("igch"))]
        return hits[0] if len(hits) == 1 else f"unresolved:{name}"

    # predicates over a group
    def has(self, g, m):
        return m in self.members[g]

    def in_region(self, g, r):
        return bool(re.search(rf"(?<!\w){re.escape(fold(r))}\w*", fold(self.findspot[g])))

    def buried(self, g, way, y):
        b = self.burial[g]
        if not b:
            return False
        return b[1] > y if way == "before" else b[0] < y

    # the fixed program, one per class
    def answer(self, q):
        p, G = q["params"], list(self.groups)
        mints, region = p.get("mints", []), p.get("region")
        cls = q["class"]
        if cls in ("intersection",) or cls == "count" and len(mints) == 2:
            hit = [g for g in G if all(self.has(g, m) for m in mints)]
        elif cls == "count":
            hit = [g for g in G if self.has(g, mints[0])]
        elif cls == "constraint-date":
            way, y = p["burial"]
            hit = [g for g in G if self.has(g, mints[0]) and self.buried(g, way, y)]
        elif cls == "constraint-region":
            hit = [g for g in G if self.has(g, mints[0]) and self.in_region(g, region)]
        elif cls == "negation":
            if region:
                hit = [g for g in G if self.in_region(g, region) and not self.has(g, mints[0])]
            else:
                hit = [g for g in G if self.has(g, mints[0]) and not self.has(g, mints[1])]
        elif cls == "co-occurrence":
            return sorted(set().union(*[self.members[g] for g in G if self.has(g, mints[0])] or [set()]) - {mints[0]})
        elif cls == "uncertainty":
            return []
        elif cls == "superlative":
            return self.superlative(q)
        else:
            raise ValueError(cls)
        return len(hit) if cls == "count" else sorted({self.resolved[g] for g in hit})

    def superlative(self, q):
        p, form = q["params"], q.get("form") or q["id"]
        scope = [g for g in self.groups if not p.get("region") or self.in_region(g, p["region"])]
        if "mint-most-hoards" in q["id"]:
            score = collections.Counter(m for g in scope for m in self.members[g])
            return self._argmax(score)
        if "hoard-most-mints" in q["id"]:
            return self._argmax({self.resolved[g]: len(self.members[g]) for g in scope})
        way = p["way"]
        cand = [g for g in (scope if "region" in p else self.groups) if ("mint" not in p or self.has(g, p["mint"])) and self.burial[g]]
        if not cand:
            return None
        key = (lambda g: self.burial[g][0]) if way == "earliest" else (lambda g: -self.burial[g][1])
        best = max(key(g) for g in cand)
        win = sorted({self.resolved[g] for g in cand if key(g) == best})
        return win[0] if len(win) == 1 else win

    @staticmethod
    def _argmax(score):
        if not score:
            return None
        top = max(score.values())
        win = sorted(k for k, v in score.items() if v == top)
        return win[0] if len(win) == 1 else win

    # stage of the first fact the records cannot supply
    def stage(self, fact):
        kind, hid, *rest = fact["fact"].split()
        gs = [g for g in self.groups if self.resolved[g] == hid]
        if not gs:
            n = int(hid[4:]) if hid.startswith("igch") else None
            pats = self.hoards.get(hid, {}).get("match", [])
            merged = [e["canonical_name"] for e in self.ents.values() if e.get("entity_type") == "hoard"
                      and any(n and re.search(rf"IGCH\s*{n}(?!\d)", a) or any(rx(pp).search(fold(a)) for pp in pats if not pp.startswith("igch"))
                              for a in (e.get("aliases") or []))]
            return ("hoard", f"merged into {merged[0]!r}" if merged else "no hoard entity")
        if kind == "contains":
            label = next((m["id"] for m in self.mints if m["name"] == " ".join(rest)), " ".join(rest))
            return None if any(self.has(g, label) for g in gs) else ("link", "mint not linked to the hoard")
        if kind == "lacks":
            label = next((m["id"] for m in self.mints if m["name"] == " ".join(rest)), " ".join(rest))
            return ("link", "spurious link: records say the hoard has it") if all(self.has(g, label) for g in gs) else None
        if kind == "region":
            r = " ".join(rest)
            if any(self.in_region(g, r) for g in gs):
                return None
            return ("attribute", "findspot missing" if not any(self.findspot[g].strip(" ;") for g in gs) else "findspot does not name the region")
        if kind == "burial":
            if rest[-1] == "date":
                return None if any(self.burial[g] for g in gs) else self._no_date(hid)
            way, y = rest[0], int(rest[1])
            if any(self.buried(g, way, y) for g in gs):
                return None
            return self._no_date(hid) if not any(self.burial[g] for g in gs) else ("attribute", "burial date on the other side")
        return ("uncertainty", "not represented")

    def _no_date(self, hid):
        """Diagnosis only (never feeds an answer): is the burial date in the records at all?"""
        secs = {self.c2s[c] for c in self.hoards[hid]["chunks"]}
        here = [c for c in self.claims if {e.get("chunk_id") for e in c.get("evidence") or []} & secs
                and re.search(r"buri|interr|deposit", c.get("content") or "", re.I) and re.search(r"\d{3}", c.get("content") or "")]
        if any((c.get("attributes") or {}).get("proposed_date") for c in here):
            return ("attribute", "burial date on a Claim with no hoard subject")
        return ("attribute", "burial date only in Claim prose" if here else "no burial date in the records")


# ── scoring ────────────────────────────────────────────────────────────────
def score_records(q, ans, rec):
    if q["answer_type"] in LISTS:
        gold, neutral = set(q["answer"]), set(q["neutral"])
        got = set(ans or [])
        found, made = got & gold, got - gold - neutral
        P = len(found) / (len(found) + len(made)) if found or made else 1.0
        R = len(found) / len(gold)
        miss = []
        for g in q["gold_items"]:
            if g["id"] in found:
                continue
            stages = [[s for s in (rec.stage(f) for f in w) if s] for w in g["witnesses"]]
            best = min(stages, key=len)
            miss.append({"id": g["id"], "stage": best[0][0] if best else "program", "why": best[0][1] if best else "facts present, item not produced"})
        return {"f1": 2 * P * R / (P + R) if P + R else 0.0, "precision": P, "recall": R, "found": sorted(found),
                "made_up": sorted(made), "neutral_hits": sorted(got & neutral), "missed": miss, "answer": sorted(got)}
    exact = ans == q["answer"]
    fails = [{"fact": f["fact"], "stage": s[0], "why": s[1]} for f in q["needed_facts"] if (s := rec.stage(f))]
    # an exact hit whose needed facts the records lack is right for a reason the records do not hold
    return {"exact": float(exact), "answer": ans, "gold": q["answer"], "missed": [] if exact else fails,
            "missing_support": fails if exact else []}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--bank", type=pathlib.Path, default=HERE / "bank-k2-dev.toml")
    ap.add_argument("--json", type=pathlib.Path, default=HERE / "k2-t0.json")
    a = ap.parse_args()
    bank = tomllib.loads(a.bank.read_text())
    corpus = bank["bank"]["corpus"]
    qs = bank["questions"]
    for q in qs:
        q.setdefault("form", None)
    rec = Records(IDX / corpus / "atlas", bank, corpus)
    ranked, n_chunks = topk(corpus, [q["question"] for q in qs])
    rows = []
    for q, r in zip(qs, ranked):
        ans = rec.answer(q)
        rows.append({"id": q["id"], "class": q["class"], "question": q["question"], "answer_type": q["answer_type"],
                     "open_world_sensitive": q["open_world_sensitive"], "records": score_records(q, ans, rec),
                     "rag": rag(q, r), "top20": r[:20]})
    order = list(dict.fromkeys(q["class"] for q in qs))
    metric = lambda row, side, k=None: (row["records"]["f1"] if "f1" in row["records"] else row["records"]["exact"]) if side == "records" \
        else row["rag"][k]["f1" if row["answer_type"] in LISTS else "exact"]  # noqa: E731
    mean = lambda xs: round(sum(xs) / len(xs), 3) if xs else None  # noqa: E731
    per_class, split, versus = {}, {}, {}
    for cls in order:
        rs = [r for r in rows if r["class"] == cls]
        per_class[cls] = {"n": len(rs), "metric": "set F1" if rs[0]["answer_type"] in LISTS else "exact",
                          "records": mean([metric(r, "records") for r in rs]),
                          "records_made_up": sum(len(r["records"].get("made_up", [])) for r in rs),
                          "records_exact_without_support": sum(1 for r in rs if r["records"].get("missing_support")),
                          **{f"rag@{k}": mean([metric(r, "rag", k) for r in rs]) for k in KS}}
        split[cls] = dict(collections.Counter(f"{m['stage']}: {m['why']}" for r in rs for m in r["records"]["missed"]))
        versus[cls] = {"records_beat_rag20": [r["id"] for r in rs if metric(r, "records") > metric(r, "rag", 20) + 1e-9],
                       "rag20_beat_records": [r["id"] for r in rs if metric(r, "rag", 20) > metric(r, "records") + 1e-9]}
    census = {"hoard_groups": {g: {"name": rec.display(g), "resolves_to": rec.resolved[g], "mints": sorted(rec.members[g]),
                                   "findspot": rec.findspot[g], "burial": rec.burial[g]} for g in rec.groups}}
    out = {"bank": str(a.bank.name), "corpus": corpus, "chunks": n_chunks, "ks": KS, "per_class": per_class,
           "record_stage_split": split, "records_vs_rag20": versus, "census": census, "questions": rows}
    a.json.write_text(json.dumps(out, indent=1, ensure_ascii=False))

    print(f"corpus {corpus}: {n_chunks} chunks (k=80 is the whole corpus); {len(rows)} questions; "
          f"{len(rec.groups)} record hoard groups")
    print(f"\n{'class':18} {'n':>2} {'metric':7} {'records':>7} {'madeup':>6} " + " ".join(f"{'rag@' + str(k):>7}" for k in KS))
    for cls in order:
        c = per_class[cls]
        print(f"{cls:18} {c['n']:2} {c['metric']:7} {c['records']:7} {c['records_made_up']:6} "
              + " ".join(f"{c['rag@' + str(k)]:7}" for k in KS))
    print("\nrecord-side misses by stage:")
    for cls in order:
        if split[cls]:
            print(f"  {cls:18} {split[cls]}")
    print("\nper question (records | rag@20):")
    for r in rows:
        rr = r["records"]
        recs = f"F1 {rr['f1']:.2f} made {len(rr['made_up'])}" if "f1" in rr else f"exact {rr['exact']:.0f} ({rr['answer']!s:.30})"
        rg = r["rag"][20]
        rgs = f"F1 {rg['f1']:.2f}" if "f1" in rg else f"exact {rg['exact']:.0f}"
        print(f"  {r['id']:40} {recs:34} | {rgs}")
    print(f"-> {a.json.name}")


if __name__ == "__main__":
    main()
