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


# ---------------------------------------------------------------- M2: citing an act by one forced choice (PREREG part 2)
# The noun follows the arm's unit (U: sentence, L: line); everything else is byte-identical across arms.
WORDINGS = {
    "W1": ("You read a document and point to the {noun} that shows a given fact.\n\nThe user message gives the "
           "document's declared facts, then the document, in which some {noun}s are marked with a letter in brackets, "
           "then the fact. Answer with the letter of the marked {noun} that shows the fact most directly, or 0 if no "
           "marked {noun} shows it.",
           "Which marked {noun} shows this fact? Answer with its letter, or 0 if none does."),
    "W2": ("Find where a document shows something.\n\nYou are given what is declared about the document, the document "
           "itself with certain {noun}s tagged by a bracketed letter, and a fact. Reply with the tag of the tagged "
           "{noun} that most directly supports the fact, or 0 if none of the tagged {noun}s supports it.",
           "Which tagged {noun} supports the fact? Reply with its tag, or 0 if none does."),
}
# Amendment 3: the same wordings with the 0 label and its clause removed, nothing else changed.
WORDINGS["W1N"] = (WORDINGS["W1"][0].replace(", or 0 if no marked {noun} shows it", ""),
                   WORDINGS["W1"][1].replace(", or 0 if none does", ""))
WORDINGS["W2N"] = (WORDINGS["W2"][0].replace(", or 0 if none of the tagged {noun}s supports it", ""),
                   WORDINGS["W2"][1].replace(", or 0 if none does", ""))
ARM_WORDINGS = {"U": ["W1", "W2", "W1N", "W2N"], "L": ["W1", "W2"]}
NOUN = {"U": "sentence", "L": "line"}
M2 = OUT / "m2"


def fact(kind, value, subject):
    """The fact in the recipe's own words: the value's segment of its field's description, or the kind's
    description when the kind has no closed field."""
    if kind["fields"] and value is not None:
        a = kind["fields"][0]
        desc = value_segments(a.get("description", ""))[value].split(": ", 1)[1]
        return f"a {subject} this document is about has {a['name']} '{value}': {desc}"
    return kind["description"]


def act_items(system):
    """[{doc, kind, value, gold lines, fact, facts view, U: (texts, candidates), L: (texts, candidates)}]."""
    leg, corp = L.SYSTEMS[system]
    rec = L.recipe(leg, corp)
    kinds, _ = L.plan(rec)
    by = {k["name"]: k for k in kinds}
    meta_raw = L.documents(leg, corp)
    docs, um = load(system)
    items, lines, _ = items_of(system)
    out = []
    for d, kind, value, gl, _, _ in items:
        asked = {n for n, ln in lines[d].items() if ln["asked"]}
        meta, raw = meta_raw[d]
        us = um[d]
        out.append({"doc": d, "kind": kind, "value": value, "gold": gl,
                    "fact": fact(by[kind], value, by[kind]["subject"]),
                    "facts": L.facts(rec, meta),
                    "U": ([t for t, _ in us], [u for u, (_, ls) in enumerate(us) if ls & asked], [ls for _, ls in us]),
                    "L": ([ln.strip() for ln in raw], [n - 1 for n in sorted(asked)], [{i + 1} for i in range(len(raw))])})
    return out


def act_question(it, arm, wording, marked):
    """(system, user, labels, {label: candidate index}) with `marked` (candidate indices) lettered in order."""
    texts = it[arm][0]
    letter = {c: L.LABELS[i] for i, c in enumerate(marked)}
    shown = "\n".join(f"[{letter[i]}] {t}" if i in letter else t for i, t in enumerate(texts))
    sysp, q = (w.format(noun=NOUN[arm]) for w in WORDINGS[wording])
    user = f"{it['facts']}Document:\n<<<\n{shown}\n>>>\n\nFact: {it['fact']}\n\n{q}"
    labels = [letter[c] for c in marked] + ([] if wording.endswith("N") else [L.NONE])
    return sysp, user, labels, {v: k for k, v in letter.items()}


def cite(it, arm, wording, root=M2):
    """The cited candidate (or None), the calls made, and each question's distribution: pages of 25 in document
    order, then one final question over the pages' picks when two or more pages pick one."""
    cands, calls, picks, dists = it[arm][1], 0, [], []

    def ask(marked):
        nonlocal calls
        sysp, user, labels, back = act_question(it, arm, wording, marked)
        rec = L.post(L.body(sysp, user, labels), root=root)
        calls += 1
        d = L.answer(rec, labels)
        if d is None:
            return "unanswered", None
        dists.append(d)
        top = max(d, key=d.get)
        return (None if top == L.NONE else back[top]), d

    if wording.endswith("N") and len(cands) == 1:
        return cands[0], 0, []
    pages = [cands[i:i + len(L.LABELS)] for i in range(0, len(cands), len(L.LABELS))] or [[]]
    for page in pages:
        if not page:
            continue
        c, _ = ask(page)
        if c == "unanswered":
            return "unanswered", calls, dists
        if c is not None:
            picks.append(c)
    if len(picks) >= 2:
        c, _ = ask(picks)
        return c, calls, dists
    return (picks[0] if picks else None), calls, dists


def letters_ranked(it, arm, wording):
    """Diagnostic, added after W1 was read (most misses answered 0): the candidates ranked by their letter's p with
    '0' set aside, pages pooled, from the cached answers only. Threshold-free, so it shows whether the cited unit
    is there when the choice against none loses it."""
    cands, pooled = it[arm][1], {}
    for i in range(0, len(cands), len(L.LABELS)):
        page = cands[i:i + len(L.LABELS)]
        sysp, user, labels, back = act_question(it, arm, wording, page)
        req = L.body(sysp, user, labels)
        path = M2 / "waves" / (hashlib.sha256(json.dumps(req, sort_keys=True).encode()).hexdigest() + ".json")
        d = L.answer(json.loads(path.read_text()), labels) if path.exists() else None
        if d is None:
            return None
        rest = 1 - d.get(L.NONE, 0)
        for lab, p in d.items():
            if lab != L.NONE:
                pooled[back[lab]] = p / rest if rest > 0 else 0.0
    return sorted(pooled, key=lambda c: -pooled[c])


def cmd_ask(args):
    for system in args.systems:
        items = act_items(system)
        t0 = time.time()
        for arm in args.arms:
            for wording in args.wordings:
                n = sum(cite(it, arm, wording)[1] for it in items)
                print(f"{system} {arm} {wording}: {len(items)} items, {n} questions, {time.time() - t0:.0f}s", flush=True)


def cmd_gate_act(_):
    """Gate: 20 items re-asked under U/W1 outside the cache; the cited unit must equal the first answer's on 19."""
    items = [(s, it) for s in ("ward", "uv") for it in act_items(s)]
    sample = random.Random(L.SEED).sample(items, 20)
    same = sum(cite(it, "U", "W1")[0] == cite(it, "U", "W1", root=OUT / "m2-regate")[0] for _, it in sample)
    res = {"re_asked": 20, "same_cited": same, "passed": same >= 19}
    print(json.dumps(res))
    (DATA / "gate-act.json").write_text(json.dumps(res, indent=1) + "\n")


def cmd_score_act(_):
    report, rows = {}, []
    for system in ("ward", "uv"):
        items = act_items(system)
        res = {}
        cited = {}
        for arm in ("U", "L"):
            for wording in ARM_WORDINGS[arm]:
                agg = collections.defaultdict(lambda: collections.Counter())
                for it in items:
                    c, calls, dists = cite(it, arm, wording)
                    cited[(arm, wording, it["doc"], it["kind"], it["value"])] = c
                    hit = c not in (None, "unanswered") and bool(it[arm][2][c] & it["gold"])
                    informative = len(it["U"][1]) > 3
                    ranked = letters_ranked(it, arm, wording) or []
                    first = next((r + 1 for r, cc in enumerate(ranked) if it[arm][2][cc] & it["gold"]), None)
                    for key in (it["kind"], it["kind"] + (":informative" if informative else ":small")):
                        a = agg[key]
                        a["n"] += 1
                        a["hit"] += hit
                        a["calls"] += calls
                        a["zero"] += c is None
                        a["unanswered"] += c == "unanswered"
                        a["lines"] += len(it[arm][2][c]) if c not in (None, "unanswered") else 0
                        a["diag_top1"] += first == 1
                        a["diag_top3"] += first is not None and first <= 3
                    rows.append({"system": system, "arm": arm, "wording": wording, "doc": it["doc"], "kind": it["kind"],
                                 "value": it["value"], "cited": c, "hit": hit, "calls": calls, "gold_letter_rank": first,
                                 "candidates": len(it[arm][1]),
                                 "cited_text": it[arm][0][c][:160] if c not in (None, "unanswered") else None,
                                 "gold_lines": sorted(it["gold"])})
                res[f"{arm}{wording}"] = {k: {"hit": a["hit"], "n": a["n"], "rate": round(a["hit"] / a["n"], 3),
                                              "calls_per_item": round(a["calls"] / a["n"], 2), "zero": a["zero"],
                                              "unanswered": a["unanswered"],
                                              "diag_letters_top1": a["diag_top1"], "diag_letters_top3": a["diag_top3"],
                                              "lines_per_cited": round(a["lines"] / max(a["n"] - a["zero"] - a["unanswered"], 1), 2)}
                                          for k, a in sorted(agg.items())}
            for w1, w2 in (("W1", "W2"), ("W1N", "W2N")):
                if w1 not in ARM_WORDINGS[arm]:
                    continue
                key = lambda it, w: (arm, w, it["doc"], it["kind"], it["value"])  # noqa: E731
                agree = [(cited[key(it, w1)] == cited[key(it, w2)], it) for it in items]
                hit1 = lambda it: (lambda c: c not in (None, "unanswered") and bool(it[arm][2][c] & it["gold"]))(cited[key(it, w1)])  # noqa: E731
                res[f"{arm}_agreement_{w1}_{w2}"] = {"agree": sum(a for a, _ in agree), "of": len(agree),
                                                    f"{w1}_hit_where_agree": sum(hit1(it) for a, it in agree if a),
                                                    f"{w1}_hit_where_differ": sum(hit1(it) for a, it in agree if not a)}
        report[system] = res
        print(json.dumps({system: res}, indent=1))
    (DATA / "score-act.json").write_text(json.dumps(report, indent=1) + "\n")
    (DATA / "rows-act.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))


def main():
    a = argparse.ArgumentParser()
    sub = a.add_subparsers(dest="cmd", required=True)
    sub.add_parser("count")
    sub.add_parser("gates")
    r = sub.add_parser("run")
    r.add_argument("--systems", nargs="+", default=list(L.SYSTEMS))
    k = sub.add_parser("ask")
    k.add_argument("--systems", nargs="+", default=["ward", "uv"])
    k.add_argument("--arms", nargs="+", default=["U", "L"])
    k.add_argument("--wordings", nargs="+", default=["W1", "W2"])
    sub.add_parser("gate-act")
    sub.add_parser("score-act")
    args = a.parse_args()
    {"count": cmd_count, "gates": cmd_gates, "run": cmd_run, "ask": cmd_ask, "gate-act": cmd_gate_act,
     "score-act": cmd_score_act}[args.cmd](args)


if __name__ == "__main__":
    main()
