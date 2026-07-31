#!/usr/bin/env python3
"""Dump a corpus's RAPTOR nodes + resolved member chunk texts to JSONL.

SP3 fixture: the judge-throughput probe (examples/sp3_judge_probe.rs) reads
this file so the Rust harness does no lance/sqlite I/O of its own — it only
measures inference. Member texts resolve via direct_member_chunk_ids (level 0)
/ evidence_chunk_ids (upper levels, where direct is NULL), same as
armb_extractive.py.

Usage:
    .venv/bin/python scripts/sp3_dump_nodes.py \
        --db ~/.svrnmesh/sovereign.db \
        --corpus obsidian-vault-959ee8a8f330 \
        --chunks ~/.svrnmesh/indexes/obsidian-vault-959ee8a8f330/chunks.lance \
        --out data/sp3_nodes_obsidian.jsonl
"""

import argparse
import json
import sqlite3
from pathlib import Path

import lance


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", required=True)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--chunks", required=True, help="path to the corpus's chunks.lance")
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    conn = sqlite3.connect(str(Path(args.db).expanduser()))
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        "SELECT node_id, level, summary, direct_member_chunk_ids, evidence_chunk_ids "
        "FROM conv_raptor_nodes WHERE corpus_id = ? ORDER BY level, node_id",
        (args.corpus,),
    ).fetchall()
    if not rows:
        raise SystemExit(f"no conv_raptor_nodes rows for corpus {args.corpus}")

    need: set = set()
    node_chunks: dict[str, list] = {}
    for r in rows:
        ids = json.loads(r["direct_member_chunk_ids"] or r["evidence_chunk_ids"])
        need.update(ids)
        node_chunks[r["node_id"]] = ids
    print(f"nodes: {len(rows)}  distinct member chunks: {len(need)}")

    ds = lance.dataset(str(Path(args.chunks).expanduser()))
    tbl = ds.to_table(columns=["id", "content"], filter=f"id IN ({','.join(map(str, need))})")
    chunk_text = dict(zip(tbl.column("id").to_pylist(), tbl.column("content").to_pylist()))
    missing = need - set(chunk_text)
    print(f"chunk texts resolved: {len(chunk_text)}/{len(need)}")
    if missing:
        print(f"WARNING: {len(missing)} member chunk ids missing from chunks.lance")

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    n_empty = 0
    with out.open("w") as f:
        for r in rows:
            texts = [chunk_text[c] for c in node_chunks[r["node_id"]] if c in chunk_text]
            if not texts:
                n_empty += 1
            f.write(
                json.dumps(
                    {
                        "node_id": r["node_id"],
                        "level": r["level"],
                        "summary": r["summary"],
                        "member_chunk_ids": node_chunks[r["node_id"]],
                        "member_texts": texts,
                    },
                    ensure_ascii=False,
                )
                + "\n"
            )
    print(f"wrote {len(rows)} nodes -> {out}  (nodes with zero resolved texts: {n_empty})")


if __name__ == "__main__":
    main()
