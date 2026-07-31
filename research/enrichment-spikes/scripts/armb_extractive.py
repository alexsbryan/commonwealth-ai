#!/usr/bin/env python3
"""SP2 Arm B: compute extractive summaries for sep's conv_raptor_nodes.

Per node (plan step 4, let-s-plan-out-our-melodic-tome.md:245-251):
  member texts via direct_member_chunk_ids (level 0) / evidence_chunk_ids
  (upper levels, where direct is NULL) from chunks.lance; sentence-split;
  embed sentences (daemon /v1/embeddings, batched); rank by cosine to the
  node's UNTOUCHED centroid_embedding; take top sentences to the length
  budget (the node's own abstractive summary length); re-embed the result
  -> summary_embedding. Everything else passes through unchanged.

Output JSONL rows mirror ConvRaptorNodeRow (embeddings as f32 arrays);
the writer is the Rust example sovereign-store/examples/armb_write_nodes.rs
(the ONLY sanctioned write path: SqliteStateStore::save_conv_raptor_nodes).

Usage:
    .venv/bin/python scripts/armb_extractive.py \
        --db ~/.svrnmesh/sovereign.db \
        --chunks ~/.svrnmesh/indexes/sep/chunks.lance \
        --corpus sep --out runs/armB/armb_nodes.jsonl
"""

import argparse
import array
import json
import re
import sqlite3
import struct
import urllib.request
from pathlib import Path

import lance

EMBED_URL = "http://localhost:9741/v1/embeddings"
EMBED_MODEL = "qwen-embedding-0.6b"
EMBED_BATCH = 32

SENT_SPLIT = re.compile(r"(?<=[.!?])\s+(?=[A-Z(\"'“])")


def decode_f32(blob: bytes) -> array.array:
    return array.array("f", blob)


def embed_batch(texts: list[str]) -> list[array.array]:
    out: list[array.array] = []
    for i in range(0, len(texts), EMBED_BATCH):
        batch = texts[i : i + EMBED_BATCH]
        req = urllib.request.Request(
            EMBED_URL,
            data=json.dumps({"model": EMBED_MODEL, "input": batch}).encode(),
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=300) as resp:
            data = json.loads(resp.read())
        rows = sorted(data["data"], key=lambda d: d["index"])
        out.extend(array.array("f", d["embedding"]) for d in rows)
    return out


def cosine(a, b) -> float:
    num = sum(x * y for x, y in zip(a, b))
    da = sum(x * x for x in a) ** 0.5
    db = sum(y * y for y in b) ** 0.5
    return num / (da * db) if da and db else 0.0


def split_sentences(text: str) -> list[str]:
    parts = [s.strip() for s in SENT_SPLIT.split(text)]
    return [s for s in parts if len(s) >= 40]  # drop headers/fragments


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", required=True)
    ap.add_argument("--chunks", required=True)
    ap.add_argument("--corpus", default="sep")
    ap.add_argument("--out", required=True)
    ap.add_argument("--max-sentences-per-node", type=int, default=400,
                    help="cap on candidate sentences embedded per node")
    args = ap.parse_args()

    conn = sqlite3.connect(str(Path(args.db).expanduser()))
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        "SELECT * FROM conv_raptor_nodes WHERE corpus_id=? ORDER BY conv_uuid, level, node_id",
        (args.corpus,),
    ).fetchall()
    print(f"nodes: {len(rows)}")

    # Collect every chunk id we need, one lance scan.
    need: set[int] = set()
    node_chunks: dict[str, list[int]] = {}
    for r in rows:
        ids = json.loads(r["direct_member_chunk_ids"] or r["evidence_chunk_ids"])
        ids = [int(i) for i in ids]
        node_chunks[r["node_id"]] = ids
        need.update(ids)
    print(f"distinct member chunks: {len(need)}")

    ds = lance.dataset(str(Path(args.chunks).expanduser()))
    id_list = ",".join(str(i) for i in sorted(need))
    tbl = ds.scanner(filter=f"id IN ({id_list})", columns=["id", "content"]).to_table()
    chunk_text = dict(zip(tbl.column("id").to_pylist(), tbl.column("content").to_pylist()))
    print(f"chunk texts resolved: {len(chunk_text)}/{len(need)}")
    missing = need - set(chunk_text)
    if missing:
        print(f"WARNING: {len(missing)} member chunk ids missing from chunks.lance")

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    n_written = 0
    emb_cache: dict[str, array.array] = {}  # sentences repeat across levels; array("f") keeps RSS sane

    def embed_cached(texts: list[str]) -> list[array.array]:
        fresh = [t for t in dict.fromkeys(texts) if t not in emb_cache]
        if fresh:
            for t, e in zip(fresh, embed_batch(fresh)):
                emb_cache[t] = e
        return [emb_cache[t] for t in texts]

    done: set[str] = set()
    if out_path.exists():
        with out_path.open() as fh:
            for line in fh:
                if line.strip():
                    done.add(json.loads(line)["node_id"])
        print(f"resume: {len(done)} nodes already computed, skipping")

    with out_path.open("a") as fh:
        for r in rows:
            if r["node_id"] in done:
                continue
            centroid = decode_f32(r["centroid_embedding"])
            budget = len(r["summary"])

            # Candidate sentences in member order (chunk order, then position).
            cands: list[str] = []
            seen: set[str] = set()
            for cid in node_chunks[r["node_id"]]:
                text = chunk_text.get(cid)
                if not text:
                    continue
                for s in split_sentences(text):
                    if s not in seen:
                        seen.add(s)
                        cands.append(s)
            cands = cands[: args.max_sentences_per_node]

            if cands:
                embs = embed_cached(cands)
                scored = sorted(
                    range(len(cands)), key=lambda i: cosine(embs[i], centroid), reverse=True
                )
                picked: list[int] = []
                total = 0
                for i in scored:
                    picked.append(i)
                    total += len(cands[i]) + 1
                    if total >= budget:
                        break
                picked.sort()  # restore source order for readability
                summary = " ".join(cands[i] for i in picked)
            else:
                summary = r["summary"]  # no member text resolvable; keep abstractive
                print(f"  fallback (no candidates): {r['node_id']}")

            summary_embedding = embed_batch([summary])[0]  # final summaries are unique; no cache

            fh.write(json.dumps({
                "node_id": r["node_id"],
                "corpus_id": r["corpus_id"],
                "conv_uuid": r["conv_uuid"],
                "level": r["level"],
                "summary": summary,
                "summary_embedding": list(summary_embedding),
                "centroid_embedding": list(centroid),
                "children_node_ids_json": r["children_node_ids"],
                "direct_member_chunk_ids_json": r["direct_member_chunk_ids"],
                "evidence_chunk_ids_json": r["evidence_chunk_ids"],
                "quote_spans_json": r["quote_spans"],
                "primary_entities_json": r["primary_entities"],
                "cluster_coherence": r["cluster_coherence"],
                "created_at": r["created_at"],
            }) + "\n")
            n_written += 1
            if n_written % 20 == 0:
                print(f"  {n_written}/{len(rows)} nodes done")

    print(f"wrote {n_written} rows -> {out_path}")


if __name__ == "__main__":
    main()
