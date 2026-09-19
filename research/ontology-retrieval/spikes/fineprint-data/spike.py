#!/usr/bin/env python3
"""fineprint-data spike: can ToS;DR points serve as held-out truth for OTA policy text?

Idempotent: every network response is cached under raw/ (fetch.py), so a re-run
costs zero requests. Run:  python3 spike.py
Prereq (done once by hand):  git clone --depth 1 https://github.com/OpenTermsArchive/pga-versions.git ota-pga-versions
"""
import collections, html, json, os, random, re, urllib.parse
from fetch import get, RAW, HERE

TOSDR = "https://api.tosdr.org"
GH = "https://api.github.com/repos/OpenTermsArchive"
SERVICES = {"Reddit": 194, "Spotify": 225, "LinkedIn": 193, "Zoom": 2198,
            "Discord": 536, "Google": 217, "Facebook": 182, "Slack": 206}
OTA_REPOS = ["contrib-versions", "pga-versions", "vlopses-us-versions", "genai-contrib-versions"]
CASES = {220: "targeted third-party advertising", 504: "automated decisions / profiling / AI training",
         166: "shares data with non-essential third parties"}
PER_SERVICE_CAP = 26     # quote_text costs ONE request per point; 484 approved points > budget
SEED = 20260919
WINDOW = 12


def norm(s):
    s = html.unescape(re.sub(r"<[^>]+>", " ", s or ""))
    s = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", s)          # md links -> anchor text
    s = re.sub(r"[^0-9a-z]+", " ", s.lower())               # drops md/html punctuation, quotes, dashes
    return " ".join(s.split())


def anchors(quote, docs):
    """(exact, loose): loose = any contiguous WINDOW-word run of the quote occurs."""
    q = norm(quote)
    if not q or not docs:
        return False, False
    exact = any(q in d for d in docs)
    w = q.split()
    if exact or len(w) <= WINDOW:
        return exact, exact
    wins = {" ".join(w[i:i + WINDOW]) for i in range(len(w) - WINDOW + 1)}
    return False, any(x in d for x in wins for d in docs)


def nname(s):
    return re.sub(r"[^0-9a-z]", "", s.lower())


# ---------- ToS;DR: cases (free quote_text, 100 points/page) ----------
def case_points(cid):
    first = get(f"{TOSDR}/point/v1?case_id={cid}", f"points_case{cid}_p1.json")
    pts = list(first["points"])
    for p in range(2, first["page"]["end"] + 1):
        pts += get(f"{TOSDR}/point/v1?case_id={cid}&page={p}", f"points_case{cid}_p{p}.json")["points"]
    return pts, first["page"]["total"]


case_pts, case_total = {}, {}
for cid in CASES:
    case_pts[cid], case_total[cid] = case_points(cid)
free_quotes = {p["id"]: p for pts in case_pts.values() for p in pts}

# ---------- ToS;DR: full service list (21 pages x 500) ----------
first = get(f"{TOSDR}/service/v3?page=1", "services_p1.json")
all_services = list(first["services"])
for p in range(2, first["page"]["end"] + 1):
    all_services += get(f"{TOSDR}/service/v3?page={p}", f"services_p{p}.json")["services"]
url2svc = {}
for s in all_services:
    for u in s.get("urls") or []:
        url2svc.setdefault(u.lower().strip().removeprefix("www."), s["id"])
svc_name = {s["id"]: s["name"] for s in all_services}

# ---------- OTA: root listings + documents ----------
ota_dirs = {}
for r in OTA_REPOS:
    t = get(f"{GH}/{r}/git/trees/main", f"gh_tree_{r}.json")
    ota_dirs[r] = [x["path"] for x in t["tree"] if x["type"] == "tree"]
ota_all = {nname(d): d for r in OTA_REPOS for d in ota_dirs[r]}

ota_docs = collections.defaultdict(dict)       # service -> {"repo:doctype": normalised text}
contrib_root = {x["path"]: x["sha"] for x in json.load(open(f"{RAW}/gh_tree_contrib-versions.json"))["tree"]}
for svc in SERVICES:
    if svc in contrib_root:                    # contrib-versions is 75 MB -> not cloned; files fetched singly
        t = get(f"{GH}/contrib-versions/git/trees/{contrib_root[svc]}", f"gh_tree_contrib_{svc}.json")
        for b in t["tree"]:
            if b["type"] == "blob" and b["path"].endswith(".md"):
                u = ("https://raw.githubusercontent.com/OpenTermsArchive/contrib-versions/main/"
                     + urllib.parse.quote(f"{svc}/{b['path']}"))
                txt = get(u, f"ota_contrib_{svc}_{b['path'].replace(' ', '_')}", as_json=False)
                ota_docs[svc]["contrib:" + b["path"][:-3]] = norm(txt)
    d = os.path.join(HERE, "ota-pga-versions", svc)
    if os.path.isdir(d):
        for f in sorted(os.listdir(d)):
            if f.endswith(".md"):
                ota_docs[svc]["pga:" + f[:-3]] = norm(open(os.path.join(d, f)).read())

# ---------- RE-ANCHOR ----------
rng = random.Random(SEED)
rows, detail = [], []
for svc, sid in SERVICES.items():
    rec = get(f"{TOSDR}/service/v3?id={sid}", f"service_{sid}.json")
    approved = [p for p in rec["points"] if p["status"] == "approved"]
    frame = [p for p in approved if p["document_id"]]
    free = [p for p in frame if p["id"] in free_quotes]
    rest = [p for p in frame if p["id"] not in free_quotes]
    pick = sorted(rest, key=lambda p: p["id"])
    rng.shuffle(pick)
    pick = pick[:PER_SERVICE_CAP]   # cap applies to PAID points; free ones ride along
    c = collections.Counter()
    for p in free + pick:
        full = free_quotes.get(p["id"]) or get(f"{TOSDR}/point/v1?id={p['id']}", f"point_{p['id']}.json")
        try:
            doc = get(f"{TOSDR}/document/v1?id={p['document_id']}", f"doc_{p['document_id']}.json")["parameters"]
            tdoc = [norm(doc.get("text"))] if doc.get("text") else []
        except RuntimeError:
            tdoc = []
        q = full.get("quote_text")
        if not q:
            c["no_quote"] += 1
            continue
        c["n"] += 1
        ea, la = anchors(q, tdoc)
        eb, lb = anchors(q, list(ota_docs[svc].values()))
        c["a_exact"] += ea; c["a_loose"] += la; c["b_exact"] += eb; c["b_loose"] += lb
        c["no_tosdr_text"] += (not tdoc)
        detail.append({"service": svc, "point": p["id"], "case": p["case"]["id"], "doc": p["document_id"],
                       "words": len(norm(q).split()), "a_exact": ea, "a_loose": la, "b_exact": eb, "b_loose": lb})
    rows.append({"service": svc, "tosdr_id": sid, "approved": len(approved), "no_document_id": len(approved) - len(frame),
                 "tested": c["n"], "no_quote_text": c["no_quote"], "tosdr_doc_text_missing": c["no_tosdr_text"],
                 "ota_docs": sorted(ota_docs[svc]), "a_exact": c["a_exact"], "a_loose": c["a_loose"],
                 "b_exact": c["b_exact"], "b_loose": c["b_loose"]})
tot = {k: sum(r[k] for r in rows) for k in ("approved", "tested", "a_exact", "a_loose", "b_exact", "b_loose")}
ota_cov = [r for r in rows if r["ota_docs"]]
tot_cov = {k: sum(r[k] for r in ota_cov) for k in ("tested", "b_exact", "b_loose")}

# ---------- SET SIZES ----------
def host_service(src):
    h = (urllib.parse.urlparse(src or "").hostname or "").lower().removeprefix("www.")
    parts = h.split(".")
    for i in range(len(parts) - 1):
        k = ".".join(parts[i:])
        if k in url2svc:
            return url2svc[k], h
    return None, h


# `source` is null on most recent points (48/54 approved in case 504), and a point carries no
# service id, so service identity needs document/v1 -> service_id: one request per document.
# Budget allows that only for case 504 (DOC_RESOLVE_BUDGET docs); 220/166 are extrapolated.
DOC_RESOLVE_BUDGET = 32
import glob
doc2svc = {}
def load_doc_cache():
    for f in glob.glob(os.path.join(RAW, "doc_*.json")):
        d = json.load(open(f)).get("parameters") or {}
        if d.get("id") and d.get("service_id"):
            doc2svc[d["id"]] = d["service_id"]
load_doc_cache()
# The sample is PINNED in case504_doc_sample.json: recomputing it against the cache made every
# re-run pull the next unresolved docs (4 unplanned requests, 396 -> 400, before this was pinned).
SAMPLE = os.path.join(HERE, "case504_doc_sample.json")
if os.path.exists(SAMPLE):
    need = json.load(open(SAMPLE))
else:
    need = sorted({p["document_id"] for p in case_pts[504] if p["status"] == "approved" and p["document_id"]
                   and p["document_id"] not in doc2svc and not host_service(p.get("source"))[0]})
    random.Random(SEED).shuffle(need)
    need = need[:DOC_RESOLVE_BUDGET]
    json.dump(need, open(SAMPLE, "w"))
for did in need:
    try:
        get(f"{TOSDR}/document/v1?id={did}", f"doc_{did}.json")
    except RuntimeError:
        pass
load_doc_cache()

sets = {}
for cid, pts in case_pts.items():
    ap = [p for p in pts if p["status"] == "approved"]
    sids, unresolved = set(), 0
    for p in ap:
        sid = doc2svc.get(p["document_id"]) or host_service(p.get("source"))[0]
        if sid:
            sids.add(sid)
        else:
            unresolved += 1
    resolved = len(ap) - unresolved
    in_ota = sorted(svc_name.get(s, str(s)) for s in sids if nname(svc_name.get(s, "")) in ota_all)
    docs = len({p["document_id"] for p in ap})
    rate = len(in_ota) / len(sids) if sids else 0
    sets[cid] = {"title": CASES[cid], "points_total": case_total[cid], "points_fetched": len(pts),
                 "status": dict(collections.Counter(p["status"] for p in pts)), "approved_points": len(ap),
                 "approved_distinct_documents_UPPER_BOUND_on_services": docs,
                 "approved_points_resolved_to_service": resolved, "approved_points_unresolved": unresolved,
                 "distinct_services_resolved": len(sids), "resolved_services_in_ota": len(in_ota),
                 "resolved_services_in_ota_names": in_ota,
                 "EXTRAPOLATED_distinct_services": min(docs, round(len(sids) * len(ap) / resolved)) if resolved else None,
                 "EXTRAPOLATED_services_in_ota": round(rate * min(docs, len(sids) * len(ap) / resolved)) if resolved else None}

# ---------- OVERLAP ----------
tosdr_names = collections.defaultdict(list)
for s in all_services:
    tosdr_names[nname(s["name"])].append(s["id"])
overlap = {}
for r in OTA_REPOS:
    hit = [d for d in ota_dirs[r] if nname(d) in tosdr_names]
    overlap[r] = {"folders": len(ota_dirs[r]), "match_tosdr_name": len(hit),
                  "unmatched_sample": [d for d in ota_dirs[r] if nname(d) not in tosdr_names][:25]}
uniq = {nname(d) for r in OTA_REPOS for d in ota_dirs[r]}
overlap["_union"] = {"folders": len(uniq), "match_tosdr_name": sum(1 for n in uniq if n in tosdr_names),
                     "tosdr_services": len(all_services)}

# ---------- CASE 504 sample ----------
ap504 = sorted((p for p in case_pts[504] if p["status"] == "approved" and p.get("quote_text")), key=lambda p: p["id"])
random.Random(SEED).shuffle(ap504)
sample504 = [{"id": p["id"], "source": p["source"], "quote_text": p["quote_text"]} for p in ap504[:15]]
lab = os.path.join(HERE, "case504_labels.json")
labels = json.load(open(lab)) if os.path.exists(lab) else {}
for s in sample504:
    s["label"] = labels.get(str(s["id"]), "UNLABELLED")

req = json.load(open(os.path.join(RAW, "_requests.json")))
out = {"reanchor": rows, "reanchor_total": tot, "reanchor_total_ota_covered_services": tot_cov,
       "reanchor_detail": detail, "set_sizes": sets, "overlap": overlap, "ota_dirs": ota_dirs,
       "case504_sample": sample504, "case504_tally": dict(collections.Counter(s["label"] for s in sample504)),
       "requests_total": req["n"], "requests_non200": [x for x in req["log"] if x[1] != 200]}
json.dump(out, open(os.path.join(HERE, "results.json"), "w"), indent=1)

pc = lambda a, b: f"{a}/{b} ({100 * a / b:.0f}%)" if b else "n/a"
print("| service | approved | tested | (a) ToS;DR exact | (a) 12w | (b) OTA exact | (b) 12w | OTA docs |")
print("|---|---|---|---|---|---|---|---|")
for r in rows:
    print(f"| {r['service']} | {r['approved']} | {r['tested']} | {pc(r['a_exact'], r['tested'])} | {pc(r['a_loose'], r['tested'])} | "
          f"{pc(r['b_exact'], r['tested']) if r['ota_docs'] else 'no OTA folder'} | {pc(r['b_loose'], r['tested']) if r['ota_docs'] else '-'} | {len(r['ota_docs'])} |")
print(f"| ALL | {tot['approved']} | {tot['tested']} | {pc(tot['a_exact'], tot['tested'])} | {pc(tot['a_loose'], tot['tested'])} | "
      f"{pc(tot['b_exact'], tot['tested'])} | {pc(tot['b_loose'], tot['tested'])} | |")
print(f"| ALL, OTA-covered services only | | {tot_cov['tested']} | | | {pc(tot_cov['b_exact'], tot_cov['tested'])} | {pc(tot_cov['b_loose'], tot_cov['tested'])} | |")
print(json.dumps(sets, indent=1))
print(json.dumps({k: {x: y for x, y in v.items() if x != 'unmatched_sample'} for k, v in overlap.items()}, indent=1))
for s in sample504:
    print(s["id"], s["label"], "|", s["quote_text"][:400].replace("\n", " "))
print("requests:", req["n"])
