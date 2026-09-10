#!/usr/bin/env python3
"""sep-repool: a copy of an installed corpus with ONLY its vector columns
re-embedded through the daemon's document path (the current, last-pooled
stack). Rows, ids, titles, content, atlas, checkpoints — everything else is
carried over byte for byte. Generalises runs/sep-subset-pooling/
build_last_pooled.py (33,884-chunk subset, 2026-09-08) to the whole corpus
and to the RAPTOR summary table, which that run carried over unchanged.

Two vector tables are re-embedded when present:
  chunks.lance            content  -> embedding   (indices rebuilt: IVF_PQ + 2 Inverted)
  raptor_summaries.lance  summary  -> embedding   (the live table carries no index; none built)

Index parameters are MIRRORED from corpus-engine/src/index/create.rs (see the
subset script's header for the two banked traps this avoids: write_dataset
copies rows not indices, and a >10,000-row table with no index retrieves
NOTHING and exits 0).

usage: build_last_pooled.py <src_corpus> <dst_corpus>      (dst must not exist)
"""
import json, math, os, shutil, sys, time, urllib.request

SRC, DST = sys.argv[1], sys.argv[2]
BATCH = int(os.environ.get("EMBED_BATCH", "128"))
DAEMON = os.environ.get("EMBED_URL", "http://127.0.0.1:9741/v1/embeddings")
MODEL = os.environ.get("EMBED_MODEL_ID", "embed:default")
ROOT = os.path.expanduser(os.environ.get("INDEXES_ROOT", "~/.svrnmesh/indexes"))

import lance, pyarrow as pa

src_dir, dst_dir = os.path.join(ROOT, SRC), os.path.join(ROOT, DST)
if not os.path.isdir(src_dir):
    sys.exit(f"build: source corpus {src_dir} not found")
if os.path.exists(dst_dir):
    sys.exit(f"build: {dst_dir} already exists — the copy must be a NEW id, refusing")

def post(payload, timeout=900):
    req = urllib.request.Request(DAEMON, data=json.dumps(payload).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())

def reembed(texts, label):
    n = len(texts); vectors, t0 = [], time.time()
    for i in range(0, n, BATCH):
        batch = [c if c else " " for c in texts[i:i + BATCH]]
        got = post({"input": batch, "model": MODEL})["data"]
        if len(got) != len(batch):
            sys.exit(f"build[{label}]: asked for {len(batch)} embeddings, got {len(got)} — refusing a short batch")
        vectors.extend([float(x) for x in d["embedding"]] for d in got)
        done = len(vectors); rate = done / max(time.time() - t0, 1e-9)
        if (i // BATCH) % 20 == 0 or done >= n:
            print(f"  [{label}] {done}/{n}  {rate:.1f}/s  eta {(n - done) / max(rate, 1e-9) / 60:.1f} min", flush=True)
    if len(vectors) != n:
        sys.exit(f"build[{label}]: embedded {len(vectors)} of {n} rows")
    dims = len(vectors[0])
    if any(len(v) != dims for v in vectors):
        sys.exit(f"build[{label}]: ragged embedding widths from the endpoint")
    print(f"build[{label}]: {n} vectors at {dims}-d in {(time.time() - t0) / 60:.1f} min", flush=True)
    return vectors, dims

def swap_vectors(tbl, vectors, dims):
    field = tbl.schema.field("embedding")
    arr = pa.FixedSizeListArray.from_arrays(pa.array([x for v in vectors for x in v], type=pa.float32()), dims)
    return tbl.set_column(tbl.schema.get_field_index("embedding"), field, arr)

# 1) chunks
ds = lance.dataset(os.path.join(src_dir, "chunks.lance"))
n = ds.count_rows()
tbl = ds.to_table()
print(f"build: {SRC} -> {DST}, {n} chunk rows, re-embedding through {DAEMON} batch={BATCH}", flush=True)
vectors, dims = reembed(tbl.column("content").to_pylist(), "chunks")
tbl = swap_vectors(tbl, vectors, dims)
del vectors

# 2) raptor summaries (when present)
raptor_src = os.path.join(src_dir, "raptor_summaries.lance")
rtbl = None
if os.path.isdir(raptor_src):
    rds = lance.dataset(raptor_src)
    rtbl = rds.to_table()
    print(f"build: raptor_summaries {rtbl.num_rows} rows", flush=True)
    rvec, rdims = reembed(rtbl.column("summary").to_pylist(), "raptor")
    rtbl = swap_vectors(rtbl, rvec, rdims)
    del rvec

# 3) write the copy: everything but the two vector tables byte for byte
os.makedirs(dst_dir)
for entry in os.listdir(src_dir):
    if entry in ("chunks.lance", "raptor_summaries.lance"):
        continue
    s = os.path.join(src_dir, entry)
    (shutil.copytree if os.path.isdir(s) else shutil.copy2)(s, os.path.join(dst_dir, entry))

lance.write_dataset(tbl, os.path.join(dst_dir, "chunks.lance"), mode="create")
out = lance.dataset(os.path.join(dst_dir, "chunks.lance"))
print(f"build: wrote {out.count_rows()} chunk rows (indices are NOT copied — building them now)", flush=True)
parts = max(8, min(4096, int(math.sqrt(n))))
print(f"build: IVF_PQ partitions={parts} sub_vectors={max(1, dims // 16)} distance=cosine", flush=True)
t = time.time()
out.create_index("embedding", index_type="IVF_PQ", num_partitions=parts,
                 num_sub_vectors=max(1, dims // 16), metric="cosine", replace=True)
out.create_scalar_index("content", index_type="INVERTED", replace=True)
out.create_scalar_index("title", index_type="INVERTED", replace=True)
print(f"build: indices built in {time.time() - t:.1f}s", flush=True)
out = lance.dataset(os.path.join(dst_dir, "chunks.lance"))
cols = {c for i in out.list_indices() for c in i["fields"]}
print(f"build: indexed columns = {sorted(cols)}", flush=True)
if "embedding" not in cols:
    sys.exit("build: NO vector index — the vector leg would be dark")
if not ({"content", "title"} & cols):
    sys.exit("build: NO full-text index — the FTS leg would be dark")

if rtbl is not None:
    lance.write_dataset(rtbl, os.path.join(dst_dir, "raptor_summaries.lance"), mode="create")
    print(f"build: wrote {lance.dataset(os.path.join(dst_dir, 'raptor_summaries.lance')).count_rows()} raptor rows", flush=True)

meta_path = os.path.join(dst_dir, "_corpus_meta.json")
meta = json.load(open(meta_path))
meta["corpus_id"] = DST
meta["embedding_model"] = os.environ.get("EMBED_MODEL_LABEL", meta.get("embedding_model", "Qwen3-Embedding-0.6B-Q8_0"))
json.dump(meta, open(meta_path, "w"), indent=2)
print(f"build: OK — {DST} is {out.count_rows()} chunk rows, both legs indexed", flush=True)
