"""Rerank through the daemon: the engine-swap A4 proof (ROW es-r4-rerank).

The 2026-10-12 spike (rerank-compare.py) put the embedded slot against a bare
llama-server. A4 changes the far end — the daemon on `kind = "llama-server"`
serves /v1/rerank from its router's `[models.kinds] rerank` section, and the
client returns ln(p/(1-p)) — so this script is the spike's harness with the
server arm pointed at the DAEMON, and the reference arm the same daemon's
embedded engine (a second scratch daemon, `kind = "llama"`, same GGUF).

Bars, unchanged from the spike (svrn-docs ENGINE_SWAP_PLAN "De-risk spikes R"):
  R1 Spearman over all 320 pairs >= 0.95
  R2 mean top-10 overlap per question >= 9
  R3 admission (margin > 0) agreement >= 0.95
  R4 tracked: wall seconds for the 320 pairs on each side.

Usage: af4-rerank-through-daemon.py <pairs.json> <embedded-base> <daemon-base> <out-dir>
"""
import json
import math
import sys
import time
import urllib.request

pairs, embedded, daemon, out = sys.argv[1:5]


def post(base, body):
    req = urllib.request.Request(
        f"{base}/v1/rerank",
        json.dumps(body).encode(),
        {"content-type": "application/json"},
    )
    return json.load(urllib.request.urlopen(req, timeout=900))


def scores(base, q, docs, model):
    r = post(base, {"model": model, "query": q, "documents": docs})
    s = [None] * len(docs)
    for x in r["results"]:
        s[x["index"]] = x["relevance_score"]
    return s


def margin(p):
    # The client's inverse of a sigmoid. The embedded side already answers
    # margins, so this is the TEST's instrument only (the daemon's own client
    # does the same arithmetic in oicp-client/src/rerank.rs).
    p = min(max(p, 1e-7), 1 - 1e-7)
    return math.log(p / (1 - p))


def ranks(xs):
    order = sorted(range(len(xs)), key=lambda i: xs[i])
    r = [0] * len(xs)
    for k, i in enumerate(order):
        r[i] = k
    return r


def spearman(a, b):
    ra, rb = ranks(a), ranks(b)
    n = len(a)
    ma = mb = (n - 1) / 2
    cov = sum((x - ma) * (y - mb) for x, y in zip(ra, rb))
    return cov / math.sqrt(
        sum((x - ma) ** 2 for x in ra) * sum((y - mb) ** 2 for y in rb)
    )


qs = json.load(open(pairs))
rows = []
t = {"embedded": 0.0, "daemon": 0.0}
allA, allB, overlaps, agree = [], [], [], 0
for q in qs:
    t0 = time.time()
    a = scores(embedded, q["query"], q["documents"], "rerank")
    t["embedded"] += time.time() - t0
    t0 = time.time()
    b = scores(daemon, q["query"], q["documents"], q["rerank_model"])
    t["daemon"] += time.time() - t0
    top = lambda xs: set(sorted(range(len(xs)), key=lambda i: -xs[i])[:10])
    ov = len(top(a) & top(b))
    overlaps.append(ov)
    agree += sum((x > 0) == (y > 0) for x, y in zip(a, b))
    allA += a
    allB += b
    rows.append(
        {
            "query": q["query"][:60],
            "spearman": round(spearman(a, b), 4),
            "top10_overlap": ov,
            "embedded": [round(x, 3) for x in a],
            "daemon_margin_raw": b,
            "daemon_margin": [round(x, 3) for x in b],
        }
    )

n = len(allA)
res = {
    "pairs": n,
    "R1_spearman_all": round(spearman(allA, allB), 4),
    "R2_mean_top10_overlap": sum(overlaps) / len(overlaps),
    "R3_admission_agreement": round(agree / n, 4),
    "R4_secs": {k: round(v, 1) for k, v in t.items()},
    "per_query": [(r["query"], r["spearman"], r["top10_overlap"]) for r in rows],
    "abs_margin_diff_median": sorted(abs(x - y) for x, y in zip(allA, allB))[n // 2],
    # Saturation is now a property of the RAW daemon answer: a margin that is
    # ±30 pins p to 1-1e-14, so the rank survives but the value is a ceiling.
    "daemon_saturated_margins": sum(
        1 for r in rows for x in r["daemon_margin_raw"] if abs(x) > 30
    ),
}
json.dump({"summary": res, "rows": rows}, open(f"{out}/rerank-result.json", "w"), indent=1)
print(json.dumps(res, indent=1))
