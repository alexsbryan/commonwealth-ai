import json, os, sys, math, urllib.request
import lance, pyarrow.compute as pc

QPREFIX = ("Instruct: Given a search query, retrieve relevant passages "
           "that answer the query\nQuery: ")
LIMIT = 3000
EP = "http://127.0.0.1:9741/v1/embeddings"
MODEL = "qwen-embedding-0.6b"

atoms = json.load(open("wiki_atom_sample.json"))
ids = [int(a["chunk_id"]) for a in atoms]

ds = lance.dataset(os.path.expanduser("~/.svrnmesh/indexes/wikipedia/chunks.lance"))
tbl = ds.to_table(columns=["id", "embedding", "content"],
                  filter=f"id IN ({','.join(map(str, ids))})")
print(f"chunk join: {tbl.num_rows}/{len(ids)} chunk_ids resolved in chunks.lance", file=sys.stderr)
vec = {r["id"]: r["embedding"] for r in tbl.to_pylist()}
content = {r["id"]: r["content"] for r in tbl.to_pylist()}

def embed_text(a):
    t = a["name"] + "\n"
    if a["aliases"]:
        t += ", ".join(a["aliases"]) + "\n"
    t += a["description"]
    return t[:LIMIT]

def embed(batch):
    body = json.dumps({"input": batch, "model": MODEL}).encode()
    req = urllib.request.Request(EP, body, {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=300) as r:
        return [d["embedding"] for d in json.load(r)["data"]]

def cos(a, b):
    n = min(len(a), len(b))
    d = sum(a[i]*b[i] for i in range(n))
    na = math.sqrt(sum(x*x for x in a[:n])); nb = math.sqrt(sum(x*x for x in b[:n]))
    return d/(na*nb) if na and nb else 0.0

work = [a for a in atoms if int(a["chunk_id"]) in vec]
texts = [embed_text(a) for a in work]
qv, dv = [], []
B = 16
for i in range(0, len(texts), B):
    qv += embed([QPREFIX + t for t in texts[i:i+B]])
    dv += embed(texts[i:i+B])
    print(f"  embedded {min(i+B,len(texts))}/{len(texts)}", file=sys.stderr)

rows = []
for a, q, d in zip(work, qv, dv):
    cid = int(a["chunk_id"])
    rows.append({
        "atom": a["id"], "name": a["name"], "chunk_id": cid,
        "has_desc": bool(a["description"]), "n_alias": len(a["aliases"]),
        "cos_query_side": cos(q, vec[cid]),
        "cos_doc_side":   cos(d, vec[cid]),
        "atom_text_len": len(embed_text(a)),
        "chunk_len": len(content[cid]),
    })
# noise floor: same atom text vs an UNRELATED chunk vector (shift by 1)
for i, r in enumerate(rows):
    other = int(work[(i+1) % len(work)]["chunk_id"])
    r["cos_query_side_random"] = cos(qv[i], vec[other])
    r["cos_doc_side_random"]   = cos(dv[i], vec[other])
json.dump(rows, open("seed_probe_results.json","w"), indent=1)

def stats(key):
    v = sorted(r[key] for r in rows); n = len(v)
    return (v[0], v[int(.05*n)], v[n//4], v[n//2], v[3*n//4], v[int(.95*n)], v[-1],
            sum(v)/n, sum(1 for x in v if x >= 0.92)/n)
print(f"\nn = {len(rows)}   bar = 0.92\n", file=sys.stderr)
hdr = f"{'series':<26}{'min':>7}{'p5':>7}{'p25':>7}{'p50':>7}{'p75':>7}{'p95':>7}{'max':>7}{'mean':>8}{'>=.92':>8}"
print(hdr, file=sys.stderr)
for k in ["cos_query_side","cos_doc_side","cos_query_side_random","cos_doc_side_random"]:
    s = stats(k)
    print(f"{k:<26}" + "".join(f"{x:>7.3f}" for x in s[:7]) + f"{s[7]:>8.3f}{100*s[8]:>7.1f}%", file=sys.stderr)
wd = [r for r in rows if r["has_desc"]]
if wd:
    print(f"\nsubset WITH a description (n={len(wd)}): "
          f"query-side mean {sum(r['cos_query_side'] for r in wd)/len(wd):.3f} · "
          f"doc-side mean {sum(r['cos_doc_side'] for r in wd)/len(wd):.3f}", file=sys.stderr)
