import json, math, sys, time, urllib.request
pairs, embedded, server, out = sys.argv[1:5]
def post(base, body):
    req = urllib.request.Request(f"{base}/v1/rerank", json.dumps(body).encode(), {"content-type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=900))
def scores(base, q, docs, model):
    r = post(base, {"model": model, "query": q, "documents": docs})
    s = [None] * len(docs)
    for x in r["results"]:
        s[x["index"]] = x["relevance_score"]
    return s
def margin(p):  # the client-side inverse of the server's sigmoid
    p = min(max(p, 1e-7), 1 - 1e-7)
    return math.log(p / (1 - p))
def ranks(xs):
    order = sorted(range(len(xs)), key=lambda i: xs[i]); r = [0] * len(xs)
    for k, i in enumerate(order): r[i] = k
    return r
def spearman(a, b):
    ra, rb = ranks(a), ranks(b); n = len(a); ma = mb = (n - 1) / 2
    cov = sum((x - ma) * (y - mb) for x, y in zip(ra, rb))
    return cov / math.sqrt(sum((x - ma) ** 2 for x in ra) * sum((y - mb) ** 2 for y in rb))
qs = json.load(open(pairs)); rows = []; t = {"embedded": 0.0, "server": 0.0}
allA, allB, overlaps, agree = [], [], [], 0
for q in qs:
    t0 = time.time(); a = scores(embedded, q["query"], q["documents"], "rerank"); t["embedded"] += time.time() - t0
    t0 = time.time(); p = scores(server, q["query"], q["documents"], "reranker"); t["server"] += time.time() - t0
    b = [margin(x) for x in p]
    top = lambda xs: set(sorted(range(len(xs)), key=lambda i: -xs[i])[:10])
    ov = len(top(a) & top(b)); overlaps.append(ov)
    agree += sum((x > 0) == (y > 0) for x, y in zip(a, b))
    allA += a; allB += b
    rows.append({"query": q["query"][:60], "spearman": round(spearman(a, b), 4), "top10_overlap": ov,
                 "embedded": [round(x, 3) for x in a], "server_p": p, "server_margin": [round(x, 3) for x in b]})
n = len(allA)
res = {"pairs": n, "R1_spearman_all": round(spearman(allA, allB), 4), "R2_mean_top10_overlap": sum(overlaps) / len(overlaps),
       "R3_admission_agreement": round(agree / n, 4), "R4_secs": {k: round(v, 1) for k, v in t.items()},
       "per_query": [(r["query"], r["spearman"], r["top10_overlap"]) for r in rows],
       "abs_margin_diff_median": sorted(abs(x - y) for x, y in zip(allA, allB))[n // 2],
       "server_saturated": sum(1 for r in rows for p in r["server_p"] if p <= 1e-7 or p >= 1 - 1e-7)}
json.dump({"summary": res, "rows": rows}, open(f"{out}/rerank-result.json", "w"), indent=1)
print(json.dumps(res, indent=1))
