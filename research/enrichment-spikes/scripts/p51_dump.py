#!/usr/bin/env python3
"""P5.1 dump (gate G8): tree + chunk pool + embedded questions for the
budgeted tree-descent probe.

Emits under --out-dir:
  p51_nodes.jsonl     node_id, level, summary, children_node_ids,
                      direct_member_chunk_ids (level 0 only)
  p51_chunks.jsonl    id, content, embedding — union of all level-0 member
                      chunks (= the 14-article production chunk pool)
  p51_questions.json  id, bank, question, expected_facts, query_embedding
                      (daemon /v1/embeddings on the production
                      instruction-prefixed query text, L2-normalized)

Usage:
  .venv/bin/python scripts/p51_dump.py --db ~/.svrnmesh/sovereign.db \
    --chunks ~/.svrnmesh/indexes/sep/chunks.lance --corpus sep \
    --banks ~/dev/commonwealth-ai/sovereign/bench/sep/summarize.toml \
            ~/dev/commonwealth-ai/sovereign/bench/sep/summarize_obscure.toml \
    --out-dir data
"""

import argparse
import json
import sqlite3
import tomllib
import urllib.request
from pathlib import Path

import lance

QUERY_INSTRUCTION = (
    "Instruct: Given a search query, retrieve relevant passages that answer the query\nQuery: "
)


def embed(daemon: str, texts: list[str]) -> list[list[float]]:
    req = urllib.request.Request(
        f"{daemon}/v1/embeddings",
        data=json.dumps({"model": "qwen-embedding-0.6b", "input": texts}).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=120) as r:
        data = json.load(r)["data"]
    out = []
    for d in sorted(data, key=lambda d: d["index"]):
        v = d["embedding"]
        norm = sum(x * x for x in v) ** 0.5 or 1.0
        out.append([x / norm for x in v])
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", required=True)
    ap.add_argument("--chunks", required=True)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--banks", nargs="+", required=True)
    ap.add_argument("--daemon", default="http://localhost:9741")
    ap.add_argument("--out-dir", default="data")
    args = ap.parse_args()
    out = Path(args.out_dir)
    out.mkdir(parents=True, exist_ok=True)

    conn = sqlite3.connect(Path(args.db).expanduser())
    conn.row_factory = sqlite3.Row
    rows = conn.execute(
        "SELECT node_id, level, summary, children_node_ids, direct_member_chunk_ids "
        "FROM conv_raptor_nodes WHERE corpus_id = ? ORDER BY level DESC, node_id",
        (args.corpus,),
    ).fetchall()
    print(f"nodes: {len(rows)}  levels: {sorted({r['level'] for r in rows})}")

    leaf_chunk_ids: set = set()
    with open(out / "p51_nodes.jsonl", "w") as f:
        for r in rows:
            children = json.loads(r["children_node_ids"] or "[]")
            direct = json.loads(r["direct_member_chunk_ids"] or "[]")
            leaf_chunk_ids.update(direct)
            f.write(
                json.dumps(
                    {
                        "node_id": r["node_id"],
                        "level": r["level"],
                        "summary": r["summary"],
                        "children_node_ids": children,
                        "direct_member_chunk_ids": [str(c) for c in direct],
                    }
                )
                + "\n"
            )
    print(f"level-0 member chunks (pool): {len(leaf_chunk_ids)}")

    ds = lance.dataset(str(Path(args.chunks).expanduser()))
    tbl = ds.to_table(columns=["id", "content", "embedding"])
    n = 0
    with open(out / "p51_chunks.jsonl", "w") as f:
        for cid, content, emb in zip(
            tbl.column("id").to_pylist(),
            tbl.column("content").to_pylist(),
            tbl.column("embedding").to_pylist(),
        ):
            if cid in leaf_chunk_ids:
                f.write(json.dumps({"id": str(cid), "content": content, "embedding": emb}) + "\n")
                n += 1
    print(f"chunk rows written: {n}/{len(leaf_chunk_ids)}")

    questions = []
    for bank_path in args.banks:
        bank_name = Path(bank_path).stem
        with open(Path(bank_path).expanduser(), "rb") as f:
            bank = tomllib.load(f)
        for q in bank["questions"]:
            questions.append(
                {
                    "id": q["id"],
                    "bank": bank_name,
                    "question": q["question"],
                    "expected_facts": q.get("expected_facts", []),
                }
            )
    embs = embed(args.daemon, [QUERY_INSTRUCTION + q["question"] for q in questions])
    for q, e in zip(questions, embs):
        q["query_embedding"] = e
    (out / "p51_questions.json").write_text(json.dumps(questions))
    print(f"questions embedded: {len(questions)}")


if __name__ == "__main__":
    main()
