# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "httpx>=0.27",
#   "numpy>=1.26",
#   "tqdm>=4.66",
# ]
# ///
"""Build an independent judge-derived ground-truth set.

The atom-derived ground truth in `queries-*.jsonl` is circular for atlas-tier
variants — both the labels and the tier's candidate set come from the same
atoms. This script produces a second ground-truth file by asking an LLM to
judge each (query, chunk) pair on its own merits, blind to the atlas.

Sampling strategy:
    1. Stratified-sample --sample-per-class queries from the template set.
    2. For each query, build a candidate pool = union of top-K results from
       a few fast retrievers (flat-fp32, bm25, atlas-tier-prune). This bounds
       judge calls to ~K × n_retrievers × n_queries, and ensures every
       variant's top picks appear in the pool.
    3. Ask the chat model per (query, chunk): "does this passage materially
       answer the question? yes/partial/no".
    4. Write golden_gt.jsonl with {query, relevant_chunks, pool, judgements}.

Using a fast model (default Bonsai-8B-Q1_0) keeps the per-call latency
tractable at a few-thousand-call scale. Temperature low for consistency.
"""
from __future__ import annotations

import argparse
import json
import math
import random
import re
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

import httpx
import numpy as np
from tqdm import tqdm


DAEMON_URL = "http://127.0.0.1:9741"
DEFAULT_JUDGE = "Bonsai-8B-Q1_0"
EMBED_MODEL = "qwen-embedding-0.6b"


JUDGE_PROMPT = """You are labeling passages from Dostoevsky's "Brothers Karamazov" for a retrieval benchmark. Read the question and the passage. Decide whether the passage materially answers or helps answer the question — i.e., would a researcher citing this passage consider it relevant to the question?

Think briefly. Your final response MUST end with a line of exactly this form:

VERDICT: yes
or
VERDICT: partial
or
VERDICT: no

Question: {query}

Passage:
{passage}"""


JUDGE_BATCH_PROMPT = """You are labeling passages from Dostoevsky's "Brothers Karamazov" for a retrieval benchmark. Read the question. For each numbered passage decide whether it materially answers or helps answer the question — i.e., would a researcher citing this passage consider it relevant?

Think briefly. Your final response MUST end with EXACTLY one line per passage in this format (no other trailing text):
[1] VERDICT: yes|partial|no
[2] VERDICT: yes|partial|no
…

Question: {query}

{numbered_passages}"""


VERDICT_RE = re.compile(r"VERDICT:\s*(yes|partial|no)\b", re.IGNORECASE)
BATCHED_VERDICT_RE = re.compile(
    r"\[\s*(\d+)\s*\]\s*VERDICT:\s*(yes|partial|no)\b", re.IGNORECASE
)


# ─── Loaders ──────────────────────────────────────────────────────────


def load_chunks(path: Path):
    rows = []
    with open(path) as f:
        for line in f:
            d = json.loads(line)
            rows.append({
                "chunk_id": d["chunk_id"],
                "section_id": d["section_id"],
                "content": d["content"],
                "embedding": np.asarray(d["embedding"], dtype=np.float32),
            })
    mat = np.stack([r["embedding"] for r in rows])
    n = np.linalg.norm(mat, axis=1, keepdims=True); n[n == 0] = 1.0
    return rows, mat / n


def load_queries(path: Path):
    out = []
    with open(path) as f:
        for line in f:
            out.append(json.loads(line))
    return out


def stratified_sample(queries, per_class: int, rng: random.Random):
    buckets = defaultdict(list)
    for q in queries:
        # Collapse paraphrase subclasses back to base class for sampling.
        base = q["class"].split(".paraphrase")[0]
        buckets[base].append(q)
    picked = []
    for cls, items in buckets.items():
        rng.shuffle(items)
        picked.extend(items[:per_class])
    return picked


# ─── BM25 (mirrors run_bench.py) ──────────────────────────────────────


TOKEN_RE = re.compile(r"[a-z0-9]+")
def tok(s: str): return TOKEN_RE.findall(s.lower())


class BM25:
    def __init__(self, docs, k1=1.5, b=0.75):
        self.k1, self.b = k1, b
        self.N = len(docs)
        self.dl = np.array([len(d) for d in docs], dtype=np.float32)
        self.avgdl = float(self.dl.mean()) if self.N else 0.0
        self.postings = defaultdict(dict)
        for i, d in enumerate(docs):
            for t, tf in Counter(d).items():
                self.postings[t][i] = tf
        self.idf = {t: math.log((self.N - len(p) + 0.5) / (len(p) + 0.5) + 1.0)
                    for t, p in self.postings.items()}

    def score(self, q):
        s = np.zeros(self.N, dtype=np.float32)
        for t in q:
            p = self.postings.get(t)
            if not p:
                continue
            idf = self.idf[t]
            for d, tf in p.items():
                denom = tf + self.k1 * (1 - self.b + self.b * self.dl[d] / max(self.avgdl, 1))
                s[d] += idf * (tf * (self.k1 + 1)) / denom
        return s


# ─── Minimal atlas loader + atlas-tier-prune retriever ────────────────


def load_atlas(corpus: str):
    root = Path.home() / ".sovereign" / "indexes" / corpus / "atlas"
    atoms = json.loads((root / "atoms.json").read_text()).get("atoms", [])
    flat, by_id = [], {}
    for env in atoms:
        d = env.get("data", {})
        rec = {"atom_type": env.get("atom_type"), **d}
        flat.append(rec)
        if rec.get("id"):
            by_id[rec["id"]] = rec
    return flat, by_id


def atom_text(a):
    parts = [a.get(k) for k in ("canonical_name", "label", "content", "description")
             if isinstance(a.get(k), str) and a.get(k).strip()]
    return " | ".join(parts) or a.get("id", "")


def atom_sections(a):
    out = set()
    fa = a.get("first_appearance")
    if isinstance(fa, dict) and isinstance(fa.get("chunk_id"), str) and fa["chunk_id"].startswith("sec_"):
        out.add(fa["chunk_id"])
    for ev in a.get("evidence") or []:
        if isinstance(ev, dict) and isinstance(ev.get("chunk_id"), str) and ev["chunk_id"].startswith("sec_"):
            out.add(ev["chunk_id"])
    sr = a.get("section_range")
    if isinstance(sr, dict):
        try:
            s, e = int(sr["start"].split("_")[1]), int(sr["end"].split("_")[1])
            out.update(f"sec_{i:04d}" for i in range(s, e + 1))
        except Exception:
            pass
    ra = a.get("raised_at")
    if isinstance(ra, dict) and isinstance(ra.get("chunk_id"), str) and ra["chunk_id"].startswith("sec_"):
        out.add(ra["chunk_id"])
    return out


# ─── LLM client ───────────────────────────────────────────────────────


def embed_many(client: httpx.Client, texts: list[str], batch: int = 32) -> np.ndarray:
    out = []
    for i in range(0, len(texts), batch):
        r = client.post("/v1/embeddings",
                        json={"input": texts[i : i + batch], "model": EMBED_MODEL},
                        timeout=180.0)
        r.raise_for_status()
        for d in r.json()["data"]:
            out.append(np.asarray(d["embedding"], dtype=np.float32))
    m = np.stack(out)
    n = np.linalg.norm(m, axis=1, keepdims=True); n[n == 0] = 1.0
    return m / n


def _parse_verdict(text: str) -> tuple[str | None, str]:
    """Return (label_or_None, free_text). Strips closed <think> blocks; if
    truncated mid-think, returns (None, '')."""
    outside = re.sub(r"<think>.*?</think>", "", text, flags=re.DOTALL)
    if "<think>" in outside and "</think>" not in outside:
        return None, ""
    m = VERDICT_RE.search(outside)
    if m:
        return m.group(1).lower(), outside.split("VERDICT:")[0].strip()[:240]
    return None, outside.strip()[:240]


def judge(client: httpx.Client, model: str, query: str, passage: str,
          max_retries: int = 2, max_tokens: int = 600) -> dict:
    """Return {label, reason}. label ∈ yes|partial|no|parse_fail|error.

    On parse_fail, retry once with a follow-up reminder that pins the format.
    parse_fail surfaces explicitly (not silently → "no") so we can audit
    judge reliability separately from judge accuracy.
    """
    prompt = JUDGE_PROMPT.format(query=query, passage=passage[:1800])
    last_text = ""
    for attempt in range(max_retries + 1):
        try:
            messages: list[dict[str, str]]
            if attempt == 0:
                messages = [{"role": "user", "content": prompt}]
            else:
                # Nudge with the prior response + reminder. Bonsai responds
                # better to seeing its own truncated output than to a fresh
                # prompt — this also bypasses any over-thinking lock-in.
                messages = [
                    {"role": "user", "content": prompt},
                    {"role": "assistant", "content": last_text or "(no output)"},
                    {"role": "user", "content":
                        "Your previous response did not end with a 'VERDICT: yes|partial|no' "
                        "line. Reply now with EXACTLY one line in that form (no other text)."},
                ]
            r = client.post("/v1/chat/completions", json={
                "model": model,
                "messages": messages,
                "temperature": 0.0,
                "max_tokens": max_tokens if attempt == 0 else 80,
            }, timeout=180.0)
            r.raise_for_status()
            text = r.json()["choices"][0]["message"]["content"]
            last_text = text
            label, reason = _parse_verdict(text)
            if label is not None:
                return {"label": label, "reason": reason,
                        "retried": attempt > 0}
        except Exception as e:
            if attempt == max_retries:
                return {"label": "error", "reason": f"{type(e).__name__}: {e}"}
            time.sleep(1.0 * (attempt + 1))
    return {
        "label": "parse_fail",
        "reason": f"no VERDICT after {max_retries + 1} attempts; "
                  f"last response {len(last_text)} chars",
    }


# ─── Main ─────────────────────────────────────────────────────────────


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--chunks", type=Path, required=True)
    ap.add_argument("--queries", type=Path, required=True)
    ap.add_argument("--out", type=Path, default=Path("golden-gt.jsonl"))
    ap.add_argument("--sample-per-class", type=int, default=3,
                    help="Queries per class (stratified)")
    ap.add_argument("--pool-k", type=int, default=15,
                    help="Top-K from each retriever contributing to pool")
    ap.add_argument("--judge", default=DEFAULT_JUDGE)
    ap.add_argument("--daemon", default=DAEMON_URL)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--resume", action="store_true",
                    help="Append to existing --out, skipping queries already judged")
    args = ap.parse_args()

    rng = random.Random(args.seed)

    print("loading chunks...")
    chunks, chunk_mat = load_chunks(args.chunks)
    print(f"  {len(chunks):,} chunks")

    print("loading queries...")
    all_queries = load_queries(args.queries)
    print(f"  {len(all_queries):,} queries")

    sampled = stratified_sample(all_queries, args.sample_per_class, rng)
    print(f"sampled {len(sampled)} queries across classes")

    # Resume logic.
    already_judged: set[str] = set()
    if args.resume and args.out.exists():
        with open(args.out) as f:
            for line in f:
                d = json.loads(line)
                already_judged.add(d["query"])
        print(f"resume: skipping {len(already_judged)} already judged")
    sampled = [q for q in sampled if q["query"] not in already_judged]
    if not sampled:
        print("nothing to do"); return 0

    print("loading atlas + building BM25...")
    atoms, atoms_by_id = load_atlas(args.corpus)
    bm25 = BM25([tok(c["content"]) for c in chunks])

    client = httpx.Client(base_url=args.daemon)
    print("embedding queries + atoms...")
    q_mat = embed_many(client, [q["query"] for q in sampled])
    a_mat = embed_many(client, [atom_text(a) for a in atoms])

    # ── Judge loop ──
    out_mode = "a" if (args.resume and args.out.exists()) else "w"
    t0 = time.time()
    total_judged = 0
    with open(args.out, out_mode) as outf:
        for qi, q in enumerate(tqdm(sampled, desc="queries")):
            q_vec = q_mat[qi]
            # Build candidate pool.
            pool: set[int] = set()
            # Flat-fp32
            scores = chunk_mat @ q_vec
            pool.update(np.argpartition(-scores, args.pool_k)[:args.pool_k].tolist())
            # BM25
            bm_scores = bm25.score(tok(q["query"]))
            pool.update(np.argpartition(-bm_scores, args.pool_k)[:args.pool_k].tolist())
            # Atlas-tier-prune
            atom_scores = a_mat @ q_vec
            top_atoms = np.argpartition(-atom_scores, min(5, len(atom_scores) - 1))[:5]
            cand_sections: set[str] = set()
            for ai in top_atoms:
                cand_sections |= atom_sections(atoms[ai])
            if cand_sections:
                # Rank within narrowed set, take top-pool_k.
                narrowed = [i for i, c in enumerate(chunks)
                            if c["section_id"] in cand_sections]
                if narrowed:
                    sub_scores = chunk_mat[narrowed] @ q_vec
                    order = np.argsort(-sub_scores)[: args.pool_k]
                    pool.update(narrowed[i] for i in order)

            pool_list = sorted(pool)

            # Judge each pool chunk.
            judgements: list[dict] = []
            relevant_chunk_ids: list[str] = []
            for ci in tqdm(pool_list, desc=f"  q{qi}", leave=False):
                c = chunks[ci]
                res = judge(client, args.judge, q["query"], c["content"])
                j = {"chunk_id": c["chunk_id"],
                     "section_id": c["section_id"],
                     "label": res["label"],
                     "reason": res["reason"]}
                judgements.append(j)
                if res["label"] in ("yes", "partial"):
                    relevant_chunk_ids.append(c["chunk_id"])
                total_judged += 1

            outf.write(json.dumps({
                "query": q["query"],
                "class": q["class"],
                "source_id": q.get("source_id", ""),
                "atom_relevant_sections": q.get("relevant_sections", []),
                "pool_size": len(pool_list),
                "relevant_chunks": relevant_chunk_ids,
                "judgements": judgements,
            }) + "\n")
            outf.flush()

    dt = time.time() - t0
    print(f"\ndone: wrote {args.out} — {len(sampled)} queries, "
          f"{total_judged} judge calls, {dt:.1f}s "
          f"({total_judged / max(dt, 0.01):.2f}/s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
