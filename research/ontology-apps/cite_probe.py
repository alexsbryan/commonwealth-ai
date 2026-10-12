"""Cite probe (M1 of the reader's second pass, cite-probe/PREREG.md): sentence units ranked by embeddings against a
claim's value, and furniture marked by near-duplicates across threads, scored against Locate's logged ranking.

No chat call. Embeddings go to the daemon's /v1/embeddings (the production embedder) and are cached by text under
runs/cite-probe/. `count` sizes the work, `gates` runs G1 and G2, `run` embeds and scores.
"""
import argparse
import collections
import hashlib
import json
import math
import pathlib
import random
import re
import sys
import time
import urllib.request

import numpy as np

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import locate_probe as L  # noqa: E402  the trace, gold, recipe and plan the Locate probe and its baseline read

OUT = L.ROOT / "runs/cite-probe"
DATA = HERE / "cite-probe"
EMBED_URL = "http://localhost:9741/v1/embeddings"
EMBED_MODEL = "qwen-embedding-0.6b"
QUERY = "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: "  # embed_quirks.rs
NEAR, THREADS = 0.95, 5  # PREREG: furniture = a near-duplicate (cosine >= NEAR) in >= THREADS other threads
BOUNDARY = re.compile(r"[.?!](?=\s)")  # chunkers/sentence.rs split_sentences, on the paragraph's joined lines
FOLD = json.loads((HERE / "cdcr/fold_values.json").read_text())["values"]  # declared value -> gold type


# ---------------------------------------------------------------- units (S0)
def corpus(leg, corp):
    """{document: (thread, text)}: the reader's body (chunks in order) and the document's declared thread, or the
    document itself when none is declared."""
    import lance  # noqa: PLC0415
    ds = lance.dataset(str(L.RUNS / leg / f"data/indexes/{corp}/chunks.lance"))
    by = collections.defaultdict(list)
    for r in ds.to_table(columns=["id", "source_doc_id", "metadata", "content"]).to_pylist():
        by[r["source_doc_id"]].append(r)
    out = {}
    for d, chunks in by.items():
        chunks.sort(key=lambda r: int(r["id"]))
        meta = json.loads(chunks[0]["metadata"] or "{}")
        out[d] = (str(meta.get("thread_id") or meta.get("thread") or d), "\n".join(c["content"] for c in chunks))
    return out


def units(text):
    """[(unit text, {numbered lines it touches})]. Lines are numbered as the reader numbers them (non-blank lines
    from 1); a paragraph is a run of non-blank lines, joined with a space and split at a sentence boundary."""
    out, para, n = [], [], 0

    def flush():
        if not para:
            return
        joined, owner = "", []
        for ln, s in para:
            if joined:
                joined += " "
                owner.append(ln)
            joined += s
            owner.extend([ln] * len(s))
        start = 0
        for m in BOUNDARY.finditer(joined):
            out.append((joined[start:m.end()], owner[start:m.end()]))
            start = m.end()
        out.append((joined[start:], owner[start:]))
        para.clear()

    for raw in text.split("\n"):
        if raw.strip():
            n += 1
            para.append((n, raw.rstrip()))
        else:
            flush()
    flush()
    res = []
    for t, own in out:
        s = t.strip()
        if s:
            lead = len(t) - len(t.lstrip())
            res.append((s, set(own[lead:lead + len(s)])))
    return res


# ---------------------------------------------------------------- embeddings (cached by text)
def key(text):
    return hashlib.sha256(("embed\0" + text).encode()).hexdigest()


class Embedder:
    def __init__(self):
        OUT.mkdir(parents=True, exist_ok=True)
        self.path = OUT / "vectors.jsonl"
        self.vec = {}
        if self.path.exists():
            for line in open(self.path):
                r = json.loads(line)
                self.vec[r["k"]] = np.array(r["v"], dtype=np.float32)
        self.calls = 0

    def post(self, texts):
        body = json.dumps({"model": EMBED_MODEL, "input": texts}).encode()
        req = urllib.request.Request(EMBED_URL, data=body, headers={"content-type": "application/json"})
        d = json.load(urllib.request.urlopen(req, timeout=600))
        self.calls += 1
        return [np.array(x["embedding"], dtype=np.float32) for x in sorted(d["data"], key=lambda x: x["index"])]

    def need(self, texts, batch=64, log=None):
        todo = sorted({t for t in texts if key(t) not in self.vec})
        t0 = time.time()
        with open(self.path, "a") as f:
            for i in range(0, len(todo), batch):
                part = todo[i:i + batch]
                for t, v in zip(part, self.post(part)):
                    self.vec[key(t)] = v
                    f.write(json.dumps({"k": key(t), "v": [round(float(x), 6) for x in v]}) + "\n")
                if log and (i // batch) % 20 == 0:
                    print(f"  {log}: {i + len(part)}/{len(todo)} embedded, {time.time() - t0:.0f}s", flush=True)
        return len(todo)

    def unit(self, text):
        v = self.vec[key(text)]
        return v / max(np.linalg.norm(v), 1e-12)


# ---------------------------------------------------------------- items and baselines
def value_segments(description):
    """'lead: inquiry ...; proposal: ...' -> {value: 'lead: inquiry ...'}: the field's own per-value segments."""
    out = {}
    for seg in description.split("; "):
        if ": " in seg:
            out[seg.split(": ", 1)[0].strip()] = seg.strip()
    return out


def items_of(system):
    """[(doc, kind, value, gold lines, Qv text, Qk text)] and the Locate trace: Ward and uv per (document, kind,
    value), GVC per (document, folded gold type)."""
    leg, corp = L.SYSTEMS[system]
    kinds, _ = L.plan(L.recipe(leg, corp))
    by = {k["name"]: k for k in kinds}
    lines, _ = L.trace(leg)
    L.TYPES.clear()
    gold, _ = L.gold_lines(system, lines)
    out = []
    if system == "gvc":
        inv = {t: v for v, t in FOLD.items()}
        g = collections.defaultdict(set)
        for (d, n), types in L.TYPES.items():
            if lines[d][n]["asked"]:
                for t in types:
                    if t in inv:
                        g[(d, t)].add(n)
        k = kinds[0]
        for (d, t), ns in sorted(g.items()):
            out.append((d, k["name"], t, ns, inv[t], k["description"]))
    else:
        g = collections.defaultdict(set)
        for (d, n), (k, v) in gold.items():
            if lines[d][n]["asked"] and v != "reference-only":
                g[(d, k, v)].add(n)
        for (d, k, v), ns in sorted(g.items(), key=lambda x: (x[0][0], x[0][1], str(x[0][2]))):
            kd = by[k]
            segs = value_segments(kd["fields"][0].get("description", "")) if kd["fields"] else {}
            out.append((d, k, v, ns, segs.get(v, kd["description"]), kd["description"]))
    label = {k["name"]: chr(ord("A") + i) for i, k in enumerate(kinds)}
    return out, lines, label


def topk_hit(scores, gold, k=3):
    """Expected P(some gold candidate in the top k), ties broken uniformly."""
    above = 0
    for s in sorted(set(scores.values()), reverse=True):
        tier = [c for c in scores if scores[c] == s]
        if above + len(tier) <= k:
            if any(c in gold for c in tier):
                return 1.0
            above += len(tier)
            continue
        slots, g = k - above, sum(c in gold for c in tier)
        return 1 - math.comb(len(tier) - g, slots) / math.comb(len(tier), slots)
    return 0.0


def chance(n, g, k=3):
    return 1.0 if n <= k else 1 - math.comb(n - g, k) / math.comb(n, k)


def gold_rank(scores, gold):
    order = sorted(scores, key=lambda c: -scores[c])
    return next((i + 1 for i, c in enumerate(order) if c in gold), None)


# ---------------------------------------------------------------- furniture
def furniture(emb, docs, unit_map):
    """{(doc, unit index)}: units with a near-duplicate in >= THREADS threads other than the unit's own."""
    texts = sorted({t for us in unit_map.values() for t, _ in us})
    idx = {t: i for i, t in enumerate(texts)}
    threads_of = collections.defaultdict(set)
    for d, us in unit_map.items():
        for t, _ in us:
            threads_of[idx[t]].add(docs[d][0])
    E = np.stack([emb.unit(t) for t in texts])
    near_threads = [None] * len(texts)
    for i in range(0, len(texts), 1024):
        S = E[i:i + 1024] @ E.T
        for r in range(S.shape[0]):
            js = np.nonzero(S[r] >= NEAR)[0]
            ts = set()
            for j in js:
                ts |= threads_of[j]
            near_threads[i + r] = ts
    out = set()
    for d, us in unit_map.items():
        own = docs[d][0]
        for u, (t, _) in enumerate(us):
            if len(near_threads[idx[t]] - {own}) >= THREADS:
                out.add((d, u))
    return out


# ---------------------------------------------------------------- the verbs
def load(system):
    leg, corp = L.SYSTEMS[system]
    docs = corpus(leg, corp)
    unit_map = {d: units(text) for d, (_, text) in docs.items()}
    return docs, unit_map


def cmd_count(_):
    for system in L.SYSTEMS:
        docs, um = load(system)
        texts = {t for us in um.values() for t, _ in us}
        chars = sum(len(t) for t in texts)
        print(f"{system}: documents {len(docs)} units {sum(map(len, um.values()))} unique {len(texts)} "
              f"chars {chars} longest {max(map(len, texts))}")


def cmd_gates(_):
    """G1: the endpoint reproduces the stored Ward chunk vectors. G2: one-per-request equals batched."""
    import lance  # noqa: PLC0415
    emb, res = Embedder(), {}
    leg, corp = L.SYSTEMS["ward"]
    rows = lance.dataset(str(L.RUNS / leg / f"data/indexes/{corp}/chunks.lance")).to_table(
        columns=["content", "embedding"]).to_pylist()
    random.Random(L.SEED).shuffle(rows)
    rows = rows[:20]
    got = emb.post([r["content"] for r in rows])
    cos = [float(np.dot(v, r["embedding"]) / (np.linalg.norm(v) * np.linalg.norm(r["embedding"]))) for r, v in zip(rows, got)]
    res["G1"] = {"min": round(min(cos), 5), "mean": round(sum(cos) / len(cos), 5), "n": len(cos), "passed": min(cos) >= 0.99}
    docs, um = load("ward")
    pool = sorted({t for us in um.values() for t, _ in us})
    sample = random.Random(L.SEED).sample(pool, 50)
    batched = emb.post(sample)
    single = [emb.post([t])[0] for t in sample]
    c2 = [float(np.dot(a, b) / (np.linalg.norm(a) * np.linalg.norm(b))) for a, b in zip(single, batched)]
    res["G2"] = {"min": round(min(c2), 6), "n": len(c2), "passed": min(c2) >= 0.9999}
    print(json.dumps(res, indent=1))
    DATA.mkdir(exist_ok=True)
    (DATA / "gates.json").write_text(json.dumps(res, indent=1) + "\n")


def cmd_run(args):
    emb = Embedder()
    report, misses, furn_rows = {}, [], []
    for system in args.systems:
        docs, um = load(system)
        items, lines, label = items_of(system)
        raw = {d: [ln.strip() for ln in docs[d][1].split("\n") if ln.strip()] for d in {it[0] for it in items}}
        n = emb.need([t for us in um.values() for t, _ in us] + [ln for r in raw.values() for ln in r], log=f"{system} units")
        qtexts = {QUERY + q for it in items for q in (it[4], it[5])}
        emb.need(qtexts)
        print(f"{system}: {n} new unit vectors, {emb.calls} requests so far", flush=True)
        furn = furniture(emb, docs, um)
        allu = sum(map(len, um.values()))
        arms = collections.defaultdict(lambda: {"hit": 0.0, "n": 0, "chance": 0.0, "lines_covered": 0.0,
                                                "informative_hit": 0.0, "informative_n": 0})
        gold_units = gold_furn = 0
        by_kind = {}
        for d, kind, value, gl, qv, qk in items:
            asked = {n_: ln for n_, ln in lines[d].items() if ln["asked"]}
            us = um[d]
            cand = [u for u, (_, ls) in enumerate(us) if ls & set(asked)]
            gu = {u for u in cand if us[u][1] & gl}
            gold_units += len(gu)
            gold_furn += len(gu & {u for (dd, u) in furn if dd == d})
            informative = len(asked) > 3
            loc = {n_: ln["dist"].get(label[kind], 0) for n_, ln in asked.items()}
            scored = {"locate": (loc, gl, {c: {c} for c in loc})}
            for q, qtext in (("v", qv), ("k", qk)):
                qv_ = emb.unit(QUERY + qtext)
                scored[f"L{q}"] = ({n_: float(emb.unit(raw[d][n_ - 1]) @ qv_) for n_ in asked}, gl, {c: {c} for c in asked})
                kept = [u for u in cand if (d, u) not in furn]
                scored[f"U{q}"] = ({u: float(emb.unit(us[u][0]) @ qv_) for u in kept}, gu, {u: us[u][1] for u in kept})
                scored[f"UF{q}"] = ({u: float(emb.unit(us[u][0]) @ qv_) for u in cand}, gu, {u: us[u][1] for u in cand})
            for arm, (sc, g, cover) in scored.items():
                a = arms[arm]
                h = topk_hit(sc, g) if sc else 0.0
                by_kind[(arm, d, kind)] = max(by_kind.get((arm, d, kind), 0.0), h)
                a["hit"] += h
                a["n"] += 1
                a["chance"] += chance(len(sc), len(g & set(sc))) if sc else 0.0
                top = sorted(sc, key=lambda c: -sc[c])[:3]
                a["lines_covered"] += len(set().union(*[cover[c] for c in top])) if top else 0
                if informative:
                    a["informative_hit"] += h
                    a["informative_n"] += 1
                if arm == "Uv" and h < 1:
                    misses.append({"system": system, "doc": d, "kind": kind, "value": value,
                                   "gold_rank": gold_rank(sc, g), "candidates": len(sc),
                                   "gold_furniture": len(gu & {u for (dd, u) in furn if dd == d}),
                                   "top": [[us[u][0][:140], round(sc[u], 3)] for u in sorted(sc, key=lambda c: -sc[c])[:3]],
                                   "gold": [us[u][0][:140] for u in g]})
        res = {}
        for arm, a in arms.items():
            r = {"hit": round(a["hit"], 2), "n": a["n"], "rate": round(a["hit"] / a["n"], 3),
                 "chance": round(a["chance"] / a["n"], 3), "mean_lines_in_top3": round(a["lines_covered"] / a["n"], 2)}
            if system == "uv":
                r["informative"] = [round(a["informative_hit"], 2), a["informative_n"],
                                    round(a["informative_hit"] / max(a["informative_n"], 1), 3)]
            res[arm] = r
        if system == "ward":  # the 26 (document, kind) items, Locate's rule: a hit when any value hits
            for arm in arms:
                hs = [h for (a_, _, k), h in by_kind.items() if a_ == arm and k == "stage_update"]
                res[arm]["stage_update_by_document"] = [round(sum(hs), 2), len(hs)]
        res["furniture"] = {"units": allu, "marked": len(furn), "share": round(len(furn) / allu, 4),
                            "gold_units": gold_units, "gold_marked": gold_furn,
                            "bar2_passed": gold_furn <= 0.01 * gold_units}
        report[system] = res
        rng = random.Random(L.SEED)
        sample = rng.sample(sorted(furn), min(40, len(furn)))
        for d, u in sample:
            furn_rows.append({"system": system, "doc": d, "unit": um[d][u][0][:200]})
        print(json.dumps({system: res}, indent=1), flush=True)
    DATA.mkdir(exist_ok=True)
    (DATA / "score.json").write_text(json.dumps(report, indent=1) + "\n")
    (DATA / "misses.jsonl").write_text("".join(json.dumps(m) + "\n" for m in misses))
    (DATA / "furniture-sample.jsonl").write_text("".join(json.dumps(r) + "\n" for r in furn_rows))


def main():
    a = argparse.ArgumentParser()
    sub = a.add_subparsers(dest="cmd", required=True)
    sub.add_parser("count")
    sub.add_parser("gates")
    r = sub.add_parser("run")
    r.add_argument("--systems", nargs="+", default=list(L.SYSTEMS))
    args = a.parse_args()
    {"count": cmd_count, "gates": cmd_gates, "run": cmd_run}[args.cmd](args)


if __name__ == "__main__":
    main()
