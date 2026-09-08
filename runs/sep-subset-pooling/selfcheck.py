#!/usr/bin/env python3
"""Validate the instrument before the result (ARCH §18.4).

Re-embeds n chunks of a corpus through the daemon's document path and cosines
against the STORED vectors — the same comparison
`CorpusEngine::probe_embedding_space` makes. The pre-registered expectation:

    arm L (re-embedded, last-pooled)   >= 0.99   — the write landed
    arm M (as it stands, mean-pooled)  ~  0.70   — still the old space

If L does not clear 0.99 the re-embed did not land and NO bench number from
this run means anything. If M has moved, the control was disturbed and the
comparison is void. Either way the run stops here rather than reporting a
delta between two things it cannot identify.
"""
import json, math, os, sys, urllib.request
import lance

DAEMON = os.environ.get("EMBED_URL", "http://127.0.0.1:9741/v1/embeddings")
MODEL = os.environ.get("EMBED_MODEL_ID", "embed:default")
ROOT = os.path.expanduser(os.environ.get("INDEXES_ROOT", "~/.svrnmesh/indexes"))
N = int(os.environ.get("SELFCHECK_N", "3"))

def emb(t):
    req = urllib.request.Request(DAEMON, data=json.dumps({"input": t, "model": MODEL}).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=300) as r:
        return [float(x) for x in json.loads(r.read())["data"][0]["embedding"]]

def cos(a, b):
    d = sum(x * y for x, y in zip(a, b))
    na, nb = math.sqrt(sum(x * x for x in a)), math.sqrt(sum(x * x for x in b))
    return d / (na * nb) if na and nb else float("nan")

def probe(corpus):
    ds = lance.dataset(os.path.join(ROOT, corpus, "chunks.lance"))
    rows = ds.take(list(range(0, 200)), columns=["id", "content", "embedding"]).to_pylist()
    picked = [r for r in rows if r["content"] and 300 <= len(r["content"]) <= 1600][:N]
    sims = [cos(emb(r["content"]), [float(x) for x in r["embedding"]]) for r in picked]
    return sum(sims) / len(sims), sims

failed = []
for corpus, floor, ceil, label in [
    (sys.argv[1], 0.99, None, "L re-embedded, expect >= 0.99"),
    (sys.argv[2], None, 0.80, "M as it stands, expect ~0.70"),
]:
    mean, sims = probe(corpus)
    print(f"selfcheck  {corpus:<32} mean={mean:.4f}  per={[round(s,4) for s in sims]}   ({label})")
    if floor is not None and mean < floor:
        failed.append(f"{corpus} scored {mean:.4f} < {floor} — the re-embed did not land")
    if ceil is not None and mean > ceil:
        failed.append(f"{corpus} scored {mean:.4f} > {ceil} — the control is not the space it was")

if failed:
    print("selfcheck: FAILED — no bench number from this run is interpretable:")
    for f in failed:
        print(f"  {f}")
    sys.exit(1)
print("selfcheck: OK — the two arms are the two spaces they claim to be")
