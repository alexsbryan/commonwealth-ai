#!/usr/bin/env python3
"""ei-7a: does the STORED summary vector still stand in for a fresh embed?

WHY THIS EXISTS. `write_summary_atoms` reuses the vector already sitting beside
each summary in `raptor_summaries.lance` as its ANN seed row, rather than
re-embedding the text. That is only sound if the stored vector is in the space
the live query slot embeds into — the seed table and the queries that search it
must share one space, and a stale space degrades grounding without failing
anything.

BAR, REGISTERED BEFORE THE RUN (borrowed, not invented: it is the bar
`project_prebuilt_snapshot_embedding_probe` set and ei-7c's wiki seed probe
used): a ~200-row sample must hold cosine >= 0.92.

WHAT THE BAR DOES NOT ANSWER (the seat's caveat, kept in front of the number):
it answers "same vector?", not "can the seed do its job". A pass here does not
predict lane yield, and a miss is reported with the seed yield beside it rather
than tuned away.

THE INSTRUMENT, BEFORE THE RESULT (ARCH §18.4). Two things could make a high
cosine meaningless, so both are measured:

  - `cos_random` pairs each stored vector with a DIFFERENT summary's fresh
    embed. If summaries are simply all alike in this space, that control rises
    with the real series and the comparison is worthless. It is the noise
    floor, and the real series has to clear it by a wide margin.
  - `cos_doc_side` and `cos_query_side` are both computed, because a stored
    vector embedded doc-side and compared query-side reads as a space mismatch
    when it is only a prefix mismatch. Reporting one without the other is how
    that gets misdiagnosed.
"""
import json
import math
import os
import random
import sys
import urllib.request

import lance

EP = "http://127.0.0.1:9741/v1/embeddings"
MODEL = "qwen-embedding-0.6b"
QPREFIX = ("Instruct: Given a search query, retrieve relevant passages "
           "that answer the query\nQuery: ")
SAMPLE = 200
BAR = 0.92
SEED = 7  # fixed: the sample is reproducible, so a re-run is a re-run

corpus = os.environ.get("EI7A_CORPUS", "raptor-subset-on")
path = os.path.expanduser(f"~/.sovereign/indexes/{corpus}/raptor_summaries.lance")
ds = lance.dataset(path)
rows = ds.to_table(columns=["node_id", "summary", "embedding"]).to_pylist()
print(f"rows in {corpus}: {len(rows)}", file=sys.stderr)
random.seed(SEED)
work = random.sample(rows, min(SAMPLE, len(rows)))


def embed(batch):
    body = json.dumps({"input": batch, "model": MODEL}).encode()
    req = urllib.request.Request(EP, body, {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as r:
        return [d["embedding"] for d in json.load(r)["data"]]


def cos(a, b):
    n = min(len(a), len(b))
    d = sum(a[i] * b[i] for i in range(n))
    na = math.sqrt(sum(x * x for x in a[:n]))
    nb = math.sqrt(sum(x * x for x in b[:n]))
    return d / (na * nb) if na and nb else 0.0


texts = [r["summary"] for r in work]
doc, qry = [], []
B = 16
for i in range(0, len(texts), B):
    doc += embed(texts[i:i + B])
    qry += embed([QPREFIX + t for t in texts[i:i + B]])
    print(f"  embedded {min(i + B, len(texts))}/{len(texts)}", file=sys.stderr)

stored = [r["embedding"] for r in work]
shift = list(range(1, len(stored))) + [0]  # pair i with i+1's fresh embed
series = {
    "cos_doc_side": [cos(s, d) for s, d in zip(stored, doc)],
    "cos_query_side": [cos(s, q) for s, q in zip(stored, qry)],
    "cos_random": [cos(stored[i], doc[shift[i]]) for i in range(len(stored))],
}


def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, int(p * len(v)))]


print(f"\nei-7a stored-vector probe — {corpus}, n={len(work)}, bar >= {BAR}\n")
print("| series | min | p5 | p50 | p95 | max | mean | >=bar |")
print("|---|---|---|---|---|---|---|---|")
out = {"corpus": corpus, "n": len(work), "bar": BAR, "seed": SEED, "series": {}}
for name, v in series.items():
    row = {
        "min": min(v), "p5": pct(v, .05), "p50": pct(v, .50),
        "p95": pct(v, .95), "max": max(v), "mean": sum(v) / len(v),
        "at_or_above_bar": sum(1 for x in v if x >= BAR) / len(v),
    }
    out["series"][name] = row
    print(f"| `{name}` | {row['min']:.3f} | {row['p5']:.3f} | {row['p50']:.3f} | "
          f"{row['p95']:.3f} | {row['max']:.3f} | {row['mean']:.3f} | "
          f"{row['at_or_above_bar']*100:.1f}% |")

best = max(series["cos_doc_side"] and [out["series"]["cos_doc_side"]["p50"]],
           default=0)
qs = out["series"]["cos_query_side"]["p50"]
noise = out["series"]["cos_random"]["p50"]
print(f"\nnoise floor (random pairing) median: {noise:.3f}")
print(f"doc-side median {best:.3f}, query-side median {qs:.3f}")
verdict = "PASS" if out["series"]["cos_doc_side"]["at_or_above_bar"] >= 0.95 else "MISS"
print(f"VERDICT (doc-side, >=95% of sample at or above {BAR}): {verdict}")
out["verdict"] = verdict
here = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(here, "cosine_probe_results.json"), "w") as f:
    json.dump(out, f, indent=2)
print(f"wrote {here}/cosine_probe_results.json")
