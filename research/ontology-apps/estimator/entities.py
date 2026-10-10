#!/usr/bin/env python3
"""Generic entities as identity evidence, probed offline (campaign ontology-layer E7, order ontology-layer-7-entities
steps 1-2): the daemon's served extractor (POST /v1/ner, gliner_small-v2.1, its own fixed labels) over every document
on either side of E2b's labelled pairs, then one agreement feature per pair, scored like E2b's candidates.

    entities.py TABLES --ner --cache DIR          # step 1: mentions per document, cached under DIR/ner/<system>.jsonl
    entities.py TABLES --features --cache DIR     # per pair, the mentions shared by the two sides: DIR/features/<run>.jsonl

A document's text is the corpus's own (GVC raw body; uv title and body; a Ward message file whole, headers included,
since a correspondent's name is an entity the text carries). Texts go to the extractor in windows of whole lines
(<= WINDOW chars) with offsets mapped back, so no document is truncated; a line longer than WINDOW is cut at WINDOW.

Scopes. `document`: every mention of the document. `lines`: the mentions on the statement's cited lines (`doc@..#"lA-B"`
on atlas-resolve; the mention's sentence `doc/sX..` on resolve-statements, one sentence per line in GVC's raw body),
read through the same line extents ladder_gvc reads gold mentions through; only GVC's reader lines are verified
against the raw body (ladder_gvc, 8e1eff5e8), so `lines` is judged on GVC and absent on Ward and uv, named.

A mention is normalised to its casefolded, whitespace-collapsed text; two sides share an entity when a mention of the
same label normalises the same on both. The feature row keeps, per scope and label, the shared texts and each side's
count, so any rule over them is a read of the row, not another extraction.
"""
import argparse, collections, hashlib, json, pathlib, re, sys, urllib.request

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.dont_write_bytecode = True
import ladder as L  # noqa: E402

NER_URL = "http://localhost:9741/v1/ner"
WINDOW, BATCH = 1200, 16
RUNS = ["c2-lines-gvc-resolve-alone", "blind-r2--gvc-resolve-alone", "c2-lines--ward-tune", "c2-lines--uv-third", "c2-lines--gvc",
        "blind-r2--ward-tune", "blind-r2--uv-third", "blind-r2--gvc"]
CORPORA = pathlib.Path.home() / ".svrnmesh/bench-corpora"
STATEMENT = re.compile(r'^(.*)@(\d+)\.\.(\d+)#"l(\d+)-(\d+)"$')
MENTION = re.compile(r"^(.*)/s(\d+)t(\d+)-(\d+)$")


def system_of(name):
    return "ward" if "ward" in name else "uv" if "uv" in name else "gvc"


def doc_of(sid):
    return sid.split("@", 1)[0] if "@" in sid else sid.rsplit("/", 1)[0]


def lines_of(sid):
    """(first, last) 0-based line indices a statement id cites, or None."""
    m = STATEMENT.match(sid)
    if m:
        return int(m.group(4)) - 1, int(m.group(5)) - 1
    m = MENTION.match(sid)
    if m:
        return int(m.group(2)), int(m.group(2))
    return None


# ---------------------------------------------------------------- documents
def texts(system, docs):
    """{document id as the tables name it: text}; a document the corpus cannot supply is absent, counted by the caller."""
    out = {}
    if system == "gvc":
        for d in map(json.loads, filter(str.strip, (CORPORA / "gvc/raw/documents.jsonl").read_text().splitlines())):
            if d["id"] in docs:
                out[d["id"]] = d["body"]
    elif system == "uv":
        for d in map(json.loads, filter(str.strip, (CORPORA / "uv-support/raw/documents.jsonl").read_text().splitlines())):
            if d.get("url") in docs:
                out[d["url"]] = (d.get("title") or "") + "\n" + (d.get("body") or "")
    else:
        W = L.load_module("ladder_ward", HERE.parent / "ladder_ward.py")
        path_of = {}
        for e in json.loads((CORPORA / "enron-ward/manifest.json").read_text()):
            for mid in e.get("message_ids") or []:
                path_of[W.doc_id(mid)] = e["path"]
        for d in docs:
            p = path_of.get(d)
            if p and (CORPORA / "enron-ward/sample" / p).exists():
                out[d] = (CORPORA / "enron-ward/sample" / p).read_text(errors="replace")
    return out


def windows(text):
    """[(offset, chunk)] of whole lines up to WINDOW chars; a longer line is cut at WINDOW."""
    out, at, buf, start = [], 0, [], 0
    for raw in text.split("\n"):
        line = raw + "\n"
        while len(line) > WINDOW:
            if buf:
                out.append((start, "".join(buf))); buf = []
            out.append((at, line[:WINDOW])); at += WINDOW; line = line[WINDOW:]
        if buf and sum(map(len, buf)) + len(line) > WINDOW:
            out.append((start, "".join(buf))); buf = []
        if not buf:
            start = at
        buf.append(line); at += len(line)
    if buf:
        out.append((start, "".join(buf)))
    return out


def ner_call(chunks):
    req = urllib.request.Request(NER_URL, data=json.dumps({"texts": chunks}).encode(), headers={"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as r:
        return json.loads(r.read())


def ner(system, docs, cache):
    """Mentions per document, from the cache or the extractor; the cache row carries the text's sha256 and the
    extractor block the daemon reported, so a changed text or model is a new row, never a stale hit."""
    path = cache / "ner" / f"{system}.jsonl"
    path.parent.mkdir(parents=True, exist_ok=True)
    have = {}
    if path.exists():
        for row in map(json.loads, filter(str.strip, path.read_text().splitlines())):
            have[row["document"]] = row
    want = texts(system, docs)
    missing = [d for d in sorted(want) if d not in have or have[d]["sha256"] != hashlib.sha256(want[d].encode()).hexdigest()]
    counts = {"documents": len(docs), "texts": len(want), "absent": len(docs - set(want)), "cached": len(want) - len(missing), "extracted": 0}
    pending = []  # (document, offset, chunk)
    for d in missing:
        pending.extend((d, off, chunk) for off, chunk in windows(want[d]))
    extractor, got = None, collections.defaultdict(list)
    for i in range(0, len(pending), BATCH):
        batch = pending[i:i + BATCH]
        res = ner_call([c for _, _, c in batch])
        extractor = res["extractor"]
        for (d, off, _), ms in zip(batch, res["mentions"]):
            got[d].extend({**m, "char_start": m["char_start"] + off, "char_end": m["char_end"] + off} for m in ms)
    with open(path, "a") as f:
        for d in missing:
            row = {"document": d, "sha256": hashlib.sha256(want[d].encode()).hexdigest(), "extractor": extractor,
                   "lines": [len(x) + 1 for x in want[d].split("\n")], "mentions": sorted(got[d], key=lambda m: m["char_start"])}
            f.write(json.dumps(row) + "\n")
            have[d] = row
            counts["extracted"] += 1
    return {d: have[d] for d in want}, counts


# ---------------------------------------------------------------- features
def norm(text):
    return " ".join(text.casefold().split())


def mentions_in(row, line_range):
    """{label: {normalised text}} of the document's mentions, restricted to the 0-based line range when given."""
    out = collections.defaultdict(set)
    if line_range is not None:
        starts, at = [], 0
        for n in row["lines"]:
            starts.append(at); at += n
        a, b = line_range
        if not (0 <= a <= b < len(starts)):
            return None
        lo, hi = starts[a], starts[b] + row["lines"][b]
    for m in row["mentions"]:
        if line_range is None or (lo <= m["char_start"] and m["char_end"] <= hi):
            out[m["label"]].add(norm(m["text"]))
    return out


def features(name, rows, ner_rows, verified_lines):
    out = []
    for r in rows:
        feat = {"statement": r["statement"], "alternative": r["alternative"]}
        ds, da = doc_of(r["statement"]), doc_of(r["alternative"])
        if ds not in ner_rows or da not in ner_rows:
            feat["absent"] = True
            out.append(feat)
            continue
        scopes = {"document": (None, None)}
        if verified_lines:
            scopes["lines"] = (lines_of(r["statement"]), lines_of(r["alternative"]))
        for scope, (ls, la) in scopes.items():
            ms, ma = mentions_in(ner_rows[ds], ls), mentions_in(ner_rows[da], la)
            if ms is None or ma is None or (scope == "lines" and (ls is None or la is None)):
                feat[scope] = None
                continue
            labels = sorted(set(ms) | set(ma))
            feat[scope] = {lab: {"shared": sorted(ms.get(lab, set()) & ma.get(lab, set())), "statement": len(ms.get(lab, ())), "alternative": len(ma.get(lab, ()))}
                           for lab in labels}
        out.append(feat)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("tables", type=pathlib.Path)
    ap.add_argument("--cache", type=pathlib.Path, required=True)
    ap.add_argument("--ner", action="store_true")
    ap.add_argument("--features", action="store_true")
    a = ap.parse_args()
    by_system = collections.defaultdict(set)
    rows_of = {}
    judged = json.loads((a.tables / "labelled.json").read_text())
    for name in RUNS:
        rows = [r for r in map(json.loads, filter(str.strip, (a.tables / f"{name}.jsonl").read_text().splitlines())) if r["type"] == judged[name]["judged_type"]]
        rows_of[name] = rows
        for r in rows:
            by_system[system_of(name)].update((doc_of(r["statement"]), doc_of(r["alternative"])))
    ner_rows = {}
    for system, docs in sorted(by_system.items()):
        got, counts = ner(system, docs, a.cache)
        ner_rows[system] = got
        print(f"ner {system}: {counts} extractor={next(iter(got.values()))['extractor'] if got else None}")
    if a.features:
        (a.cache / "features").mkdir(parents=True, exist_ok=True)
        for name in RUNS:
            system = system_of(name)
            feats = features(name, rows_of[name], ner_rows[system], verified_lines=(system == "gvc"))
            with open(a.cache / "features" / f"{name}.jsonl", "w") as f:
                for x in feats:
                    f.write(json.dumps(x) + "\n")
            absent = sum(1 for x in feats if x.get("absent"))
            print(f"features {name}: {len(feats)} pairs, {absent} with a side the corpus could not supply")


if __name__ == "__main__":
    main()
