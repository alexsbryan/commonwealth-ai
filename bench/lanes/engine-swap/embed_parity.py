#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The embedding-parity probe of PREREG_ENGINE_SWAP_20261009.md.

For 20 bank questions and 20 corpus chunks (taken from the daemon's own
search hits for those questions), three vectors each:

  L            the daemon's /v1/embeddings (its embed slot adds the EOS);
               questions carry the Qwen3-Embedding query instruction, as the
               daemon's query path sends them
  R-as-built   llama-server on the bare text: what the remote engine sends
  R-prepared   llama-server on the text prepared as L prepares it

Prints min / median cosine of L vs each R per kind and the bar verdict, and
writes every pair to --out.

    embed_parity.py --daemon http://127.0.0.1:9741 --server http://127.0.0.1:18310 \\
        --out target/engine-swap/embed-parity.jsonl
"""
import argparse
import json
import math
import statistics
import sys
import tomllib
import urllib.request
from pathlib import Path

# EmbedQuirks::qwen3_embedding (shared/crates/sovereign-contracts/src/embed_quirks.rs:85-89).
QUERY_INSTRUCTION = ("Instruct: Given a search query, retrieve relevant passages "
                     "that answer the query\nQuery: ")
EOS = "<|endoftext|>"
BAR = 0.995
REPO = Path(__file__).resolve().parents[3]


def post(url: str, body: dict) -> dict:
    req = urllib.request.Request(url, json.dumps(body).encode(), {"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=300) as r:
        return json.load(r)


def embed(base: str, texts: list[str], model: str) -> list[list[float]]:
    out = post(f"{base}/v1/embeddings", {"model": model, "input": texts})
    return [d["embedding"] for d in sorted(out["data"], key=lambda d: d["index"])]


def cos(a, b) -> float:
    dot = sum(x * y for x, y in zip(a, b))
    return dot / (math.sqrt(sum(x * x for x in a)) * math.sqrt(sum(y * y for y in b)))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--daemon", required=True)
    ap.add_argument("--server", required=True, help="llama-server serving the embed GGUF")
    ap.add_argument("--server-model", default="Qwen3-Embedding-0.6B-Q8_0")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()

    questions = []
    for bank in ("bench/lanes/sep/questions.toml", "bench/lanes/wikipedia/questions.toml"):
        questions += [q["question"] for q in tomllib.load(open(REPO / bank, "rb"))["questions"][:10]]
    chunks, seen = [], set()
    for q in questions:
        hits = post(f"{a.daemon}/v1/knowledge/search",
                    {"query_embedding": [], "query_text": q, "limit": 3})["results"]
        for h in hits:
            text = h.get("content") or ""
            if text and text not in seen:
                seen.add(text)
                chunks.append(text)
                break
    chunks = chunks[:20]

    rows, verdict = [], {}
    for kind, texts, prep in (
        ("question", questions, lambda t: QUERY_INSTRUCTION + t),
        ("chunk", chunks, lambda t: t),
    ):
        l_vecs = embed(a.daemon, [prep(t) for t in texts], "embed")
        bare = embed(a.server, texts, a.server_model)
        prepared = embed(a.server, [prep(t) + EOS for t in texts], a.server_model)
        for t, l, b, p in zip(texts, l_vecs, bare, prepared):
            rows.append({"kind": kind, "text": t[:120], "dims": [len(l), len(b)],
                         "cos_as_built": cos(l, b), "cos_prepared": cos(l, p)})
        for arm in ("as_built", "prepared"):
            cs = [r[f"cos_{arm}"] for r in rows if r["kind"] == kind]
            verdict[(kind, arm)] = (min(cs), statistics.median(cs), len(cs))
    with open(a.out, "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")
    for (kind, arm), (mn, md, n) in verdict.items():
        print(f"{kind:8} R-{arm:9} n={n:2} min cos {mn:.5f} median {md:.5f}  {'PASS' if mn >= BAR else 'MISS'} (bar {BAR})")
    built = min(v[0] for (k, arm), v in verdict.items() if arm == "as_built")
    prepared = min(v[0] for (k, arm), v in verdict.items() if arm == "prepared")
    print("verdict:", "R-as-built passes" if built >= BAR else
          ("R-as-built misses, R-prepared passes: the client's input preparation" if prepared >= BAR
           else "both miss: stop before R1"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
