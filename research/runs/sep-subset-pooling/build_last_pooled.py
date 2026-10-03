#!/usr/bin/env python3
"""Arm L: a copy of the sep-subset fixture with ONLY the chunk vectors
re-embedded through the daemon's document path (the current, last-pooled
stack). Rows, ids, titles, content and the atlas are carried over byte for
byte — the vector column is the single variable.

Traps this script exists to not step in, both banked:
  * `lance.write_dataset` copies ROWS, NOT INDICES, and `CorpusIndex::search`
    gates BOTH legs on an index being present above FLAT_SCAN_THRESHOLD
    (10,000 rows; search.rs:296-298). At 33,884 rows a copied fixture retrieves
    NOTHING and still exits 0 (note 29f1f14a). So the indices are rebuilt here
    and ASSERTED in the exact terms `gate_info` reads.
  * `svrn corpus optimize` reports "already maintained" and SKIPS the index
    pass on a table with zero indices, so it cannot repair this.

Index parameters are MIRRORED from `corpus-engine/src/index/create.rs`, not
guessed: IVF_PQ, partitions = sqrt(rows) clamped [8, 4096]
(`optimal_partitions`), distance Cosine (`build_vector_index_with_progress`),
sub-vectors = dims/16; Inverted on `content` and `title`.
"""
import json, math, os, shutil, sys, time, urllib.request

SRC = sys.argv[1]           # e.g. raptor-subset-off
DST = sys.argv[2]           # e.g. raptor-subset-pooled-last
BATCH = int(os.environ.get("EMBED_BATCH", "128"))
DAEMON = os.environ.get("EMBED_URL", "http://127.0.0.1:9741/v1/embeddings")
MODEL = os.environ.get("EMBED_MODEL_ID", "embed:default")
ROOT = os.path.expanduser(os.environ.get("INDEXES_ROOT", "~/.svrnmesh/indexes"))

import lance, pyarrow as pa

src_dir, dst_dir = os.path.join(ROOT, SRC), os.path.join(ROOT, DST)
if not os.path.isdir(src_dir):
    sys.exit(f"build: source corpus {src_dir} not found")
if os.path.exists(dst_dir):
    sys.exit(f"build: {dst_dir} already exists — arm L must be a NEW id, refusing")

def post(payload, timeout=900):
    req = urllib.request.Request(DAEMON, data=json.dumps(payload).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read())

ds = lance.dataset(os.path.join(src_dir, "chunks.lance"))
n = ds.count_rows()
tbl = ds.to_table()
contents = tbl.column("content").to_pylist()
print(f"build: {SRC} -> {DST}, {n} rows, re-embedding through {DAEMON}", flush=True)

vectors, t0 = [], time.time()
for i in range(0, n, BATCH):
    batch = [c if c else " " for c in contents[i:i + BATCH]]
    got = post({"input": batch, "model": MODEL})["data"]
    if len(got) != len(batch):
        sys.exit(f"build: asked for {len(batch)} embeddings, got {len(got)} — refusing a short batch")
    vectors.extend([float(x) for x in d["embedding"]] for d in got)
    done = len(vectors)
    rate = done / max(time.time() - t0, 1e-9)
    if (i // BATCH) % 10 == 0 or done >= n:
        eta = (n - done) / max(rate, 1e-9) / 60
        print(f"  {done}/{n}  {rate:.1f} chunk/s  eta {eta:.1f} min", flush=True)
if len(vectors) != n:
    sys.exit(f"build: embedded {len(vectors)} of {n} rows")

dims = len(vectors[0])
if any(len(v) != dims for v in vectors):
    sys.exit("build: ragged embedding widths from the endpoint")
print(f"build: {n} vectors at {dims}-d in {(time.time()-t0)/60:.1f} min", flush=True)

# Swap ONLY the vector column; every other column is carried over unchanged.
field = tbl.schema.field("embedding")
arr = pa.FixedSizeListArray.from_arrays(pa.array([x for v in vectors for x in v], type=pa.float32()), dims)
tbl = tbl.set_column(tbl.schema.get_field_index("embedding"), field, arr)

os.makedirs(dst_dir)
for entry in os.listdir(src_dir):
    if entry == "chunks.lance":
        continue
    s = os.path.join(src_dir, entry)
    (shutil.copytree if os.path.isdir(s) else shutil.copy2)(s, os.path.join(dst_dir, entry))

lance.write_dataset(tbl, os.path.join(dst_dir, "chunks.lance"), mode="create")
out = lance.dataset(os.path.join(dst_dir, "chunks.lance"))
print(f"build: wrote {out.count_rows()} rows (indices are NOT copied — building them now)", flush=True)

parts = max(8, min(4096, int(math.sqrt(n))))
print(f"build: IVF_PQ partitions={parts} sub_vectors={max(1, dims//16)} distance=cosine", flush=True)
t = time.time()
out.create_index("embedding", index_type="IVF_PQ", num_partitions=parts,
                 num_sub_vectors=max(1, dims // 16), metric="cosine", replace=True)
out.create_scalar_index("content", index_type="INVERTED", replace=True)
out.create_scalar_index("title", index_type="INVERTED", replace=True)
print(f"build: indices built in {time.time()-t:.1f}s", flush=True)

# ASSERT both retrieval legs are live, in the terms `gate_info` reads them.
out = lance.dataset(os.path.join(dst_dir, "chunks.lance"))
cols = {c for i in out.list_indices() for c in i["fields"]}
print(f"build: indexed columns = {sorted(cols)}", flush=True)
if "embedding" not in cols:
    sys.exit("build: NO vector index — the vector leg would be dark and the run would score 0")
if not ({"content", "title"} & cols):
    sys.exit("build: NO full-text index — the FTS leg would be dark")

meta_path = os.path.join(dst_dir, "_corpus_meta.json")
meta = json.load(open(meta_path))
meta["corpus_id"] = DST
meta["embedding_model"] = os.environ.get("EMBED_MODEL_LABEL", "Qwen3-Embedding-0.6B-Q8_0")
meta["corpus_name"] = f"{meta.get('corpus_name', DST)} (re-embedded last-pooled)"
json.dump(meta, open(meta_path, "w"), indent=2)
print(f"build: OK — {DST} is {out.count_rows()} rows, both legs indexed", flush=True)
