# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "httpx>=0.27",
#   "numpy>=1.26",
#   "tqdm>=4.66",
# ]
# ///
"""Brief-quality probe: does pre-structuring the retrieval brief help a small
model answer better than the raw chunks alone?

For each (query, variant) pair we:
  1. Retrieve top-K chunks via the variant
  2. Assemble a brief — either raw numbered chunks (flat) or chunks prefixed
     with the matched atom/position label (labeled)
  3. Ask Bonsai-8B "using ONLY these passages, can you answer?" → yes/partial/no
  4. Score: fraction of yes+partial across the query sample

The key comparison the proposal hangs on: same retrieved chunks, different
framing — does a "Position: <atom name>" label improve a 9B model's ability
to express a structured answer? If yes, the atlas earns its keep at brief-
construction time, not just at retrieval time.
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

import httpx
import numpy as np
from tqdm import tqdm


SOVEREIGN_ROOT = Path.home() / ".sovereign"
DAEMON_URL = "http://127.0.0.1:9741"
EMBED_MODEL = "qwen-embedding-0.6b"
DEFAULT_MODEL = "Bonsai-8B-Q1_0"


BRIEF_PROMPT = """Read the question and the numbered passages. Using ONLY information present in the passages, can you answer the question?

Think briefly. End with EXACTLY one of:
VERDICT: yes
VERDICT: partial
VERDICT: no

Question: {query}

Passages:
{passages}"""


VERDICT_RE = re.compile(r"VERDICT:\s*(yes|partial|no)\b", re.IGNORECASE)


# ─── Loaders (mirror run_bench.py shape) ──────────────────────────────


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


def load_atlas(corpus: str):
    root = SOVEREIGN_ROOT / "indexes" / corpus / "atlas"
    atoms = json.loads((root / "atoms.json").read_text()).get("atoms", [])
    edges = []
    ep = root / "edges.json"
    if ep.exists():
        edges = json.loads(ep.read_text()).get("edges", [])
    flat, by_id = [], {}
    for env in atoms:
        d = env.get("data", {})
        rec = {"atom_type": env.get("atom_type"), **d}
        flat.append(rec)
        if rec.get("id"):
            by_id[rec["id"]] = rec
    adj = defaultdict(set)
    for e in edges:
        s, t = e.get("source"), e.get("target")
        if isinstance(s, str) and isinstance(t, str):
            adj[s].add(t); adj[t].add(s)
    return flat, by_id, edges, adj


def atom_text(a):
    parts = [a.get(k) for k in ("canonical_name", "label", "content", "description")
             if isinstance(a.get(k), str) and a.get(k).strip()]
    return " | ".join(parts) or a.get("id", "")


def atom_short_label(a, fmt: str = "descriptive"):
    """One-line tag used as a chunk's structural label.

    fmt:
      - "descriptive" (default): "<atom_type>: <canonical_name|content[:80]>"
        e.g. "Entity: Alyosha", "Claim: God is ultimately just"
      - "minimal": "<atom_type_short>-<id>" — just the typed id, no surface
        text. Tests whether descriptive labels are competing with chunk
        surface for the model's attention (the entity.description /
        relation.labeled regression hypothesis).
      - "type_only": "<atom_type>" — no entity name or id, just the type
        of structural role this chunk plays.
    """
    if fmt == "minimal":
        return a.get("id", a.get("atom_type", "atom").lower())
    if fmt == "type_only":
        return a.get("atom_type", "atom")
    # descriptive (default)
    name = (a.get("canonical_name") or a.get("label")
            or a.get("content") or "").strip()[:80]
    return f"{a.get('atom_type', 'atom')}: {name}" if name else a.get("id", "atom")


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


# ─── BM25 (mirror run_bench.py) ───────────────────────────────────────


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


# ─── Retrieval (top-K per variant) ────────────────────────────────────


def retrieve(variant: str, q_vec: np.ndarray, q_tokens: list[str],
             chunk_mat: np.ndarray, chunks: list[dict], bm25: BM25,
             atoms: list[dict], atom_mat: np.ndarray,
             atoms_by_id: dict[str, dict], atom_id_to_idx: dict[str, int],
             adj: dict[str, set[str]], source_id: str | None,
             k: int) -> tuple[np.ndarray, list[dict]]:
    """Returns (chunk_indices_top_k, list_of_matched_atoms_for_those_chunks).

    The matched-atoms list aligns with chunk_indices. For non-atlas variants
    it is empty (or all None).
    """
    if variant == "flat-fp32":
        scores = chunk_mat @ q_vec
        idx = np.argpartition(-scores, k)[:k]
        idx = idx[np.argsort(-scores[idx])]
        return idx, [None] * len(idx)
    if variant == "bm25-only":
        scores = bm25.score(q_tokens)
        idx = np.argpartition(-scores, min(k, len(scores)-1))[:k]
        idx = idx[np.argsort(-scores[idx])]
        return idx, [None] * len(idx)
    if variant.startswith("atlas-tier"):
        atom_scores = atom_mat @ q_vec
        if "loo" in variant and source_id:
            hide = atom_id_to_idx.get(source_id)
            if hide is not None:
                atom_scores = atom_scores.copy()
                atom_scores[hide] = -np.inf
        top_atom_idx = np.argpartition(-atom_scores, min(5, len(atom_scores)-1))[:5]
        top_atom_idx = top_atom_idx[np.argsort(-atom_scores[top_atom_idx])]
        sections: dict[str, dict] = {}  # section_id -> atom that contributed it
        expand = "hop" in variant or variant == "atlas-tier"
        for ai in top_atom_idx:
            a = atoms[ai]
            for s in atom_sections(a):
                sections.setdefault(s, a)
            if expand:
                for nb in adj.get(a.get("id", ""), set()):
                    nba = atoms_by_id.get(nb)
                    if nba:
                        for s in atom_sections(nba):
                            sections.setdefault(s, nba)
        if not sections:
            scores = chunk_mat @ q_vec
            idx = np.argpartition(-scores, k)[:k]
            return idx[np.argsort(-scores[idx])], [None] * k
        narrowed = [i for i, c in enumerate(chunks) if c["section_id"] in sections]
        if not narrowed:
            scores = chunk_mat @ q_vec
            idx = np.argpartition(-scores, k)[:k]
            return idx[np.argsort(-scores[idx])], [None] * k
        sub_scores = chunk_mat[np.array(narrowed)] @ q_vec
        order = np.argsort(-sub_scores)[:k]
        narrowed_arr = np.array(narrowed)
        idx = narrowed_arr[order]
        matched = [sections.get(chunks[i]["section_id"]) for i in idx]
        return idx, matched
    raise ValueError(f"unknown variant: {variant}")


# ─── Brief assembly ───────────────────────────────────────────────────


def build_brief(chunks_with_atoms: list[tuple[dict, dict | None]],
                labeled: bool, max_chunk_chars: int = 600,
                label_format: str = "descriptive") -> str:
    """Numbered passages. If labeled, prefix each with structural tag."""
    lines: list[str] = []
    for i, (c, a) in enumerate(chunks_with_atoms, start=1):
        text = c["content"][:max_chunk_chars]
        if labeled and a is not None:
            tag = atom_short_label(a, fmt=label_format)
            lines.append(f"[{i}] ({tag})\n{text}")
        else:
            lines.append(f"[{i}] {text}")
    return "\n\n".join(lines)


# ─── Model call ───────────────────────────────────────────────────────


def ask_brief(client: httpx.Client, model: str, query: str, brief: str,
              max_tokens: int = 600) -> dict:
    prompt = BRIEF_PROMPT.format(query=query, passages=brief)
    r = client.post("/v1/chat/completions", json={
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.0,
        "max_tokens": max_tokens,
    }, timeout=240.0)
    r.raise_for_status()
    text = r.json()["choices"][0]["message"]["content"]
    outside = re.sub(r"<think>.*?</think>", "", text, flags=re.DOTALL)
    if "<think>" in outside and "</think>" not in outside:
        outside = ""
    m = VERDICT_RE.search(outside)
    if m:
        return {"label": m.group(1).lower(),
                "answer": outside.split("VERDICT:")[0].strip()[:600]}
    return {"label": "parse_fail",
            "answer": outside.strip()[:600] if outside else "(truncated mid-think)"}


# ─── Sampling ─────────────────────────────────────────────────────────


def stratified_sample(queries, per_class: int, rng: random.Random):
    buckets = defaultdict(list)
    for q in queries:
        base = q["class"].split(".paraphrase")[0]
        buckets[base].append(q)
    picked = []
    for cls, items in buckets.items():
        rng.shuffle(items)
        picked.extend(items[:per_class])
    return picked


# ─── Main ─────────────────────────────────────────────────────────────


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--chunks", type=Path, required=True)
    ap.add_argument("--queries", type=Path, required=True)
    ap.add_argument("--out", type=Path, default=Path("brief-quality.jsonl"))
    ap.add_argument("--out-md", type=Path, default=Path("brief-quality-report.md"))
    ap.add_argument("--variants",
                    default="flat-fp32,bm25-only,atlas-tier-prune,atlas-tier-prune-labeled,atlas-tier-loo-hop,atlas-tier-loo-hop-labeled")
    ap.add_argument("--sample-per-class", type=int, default=2,
                    help="Stratified queries per class")
    ap.add_argument("--top-k", type=int, default=10,
                    help="Top-K chunks shown to the model")
    ap.add_argument("--model", default=DEFAULT_MODEL)
    ap.add_argument("--daemon", default=DAEMON_URL)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--label-format", default="descriptive",
                    choices=["descriptive", "minimal", "type_only"],
                    help="How to render structural labels in the brief")
    ap.add_argument("--restrict-classes", default=None,
                    help="Comma-separated base-class allowlist (e.g. "
                         "'entity.description,relation.labeled') to focus a "
                         "regression diagnosis on specific query classes")
    args = ap.parse_args()

    rng = random.Random(args.seed)
    variants = [v.strip() for v in args.variants.split(",") if v.strip()]

    print("loading chunks/queries/atlas...")
    chunks, chunk_mat = load_chunks(args.chunks)
    all_queries = load_queries(args.queries)
    atoms, atoms_by_id, edges, adj = load_atlas(args.corpus)
    print(f"  chunks={len(chunks):,} queries={len(all_queries):,} "
          f"atoms={len(atoms)} edges={len(edges)}")

    if args.restrict_classes:
        keep = set(args.restrict_classes.split(","))
        all_queries = [q for q in all_queries
                       if q["class"].split(".paraphrase")[0] in keep]
        print(f"restricted to classes {keep}: {len(all_queries)} queries")
    sampled = stratified_sample(all_queries, args.sample_per_class, rng)
    print(f"sampled {len(sampled)} queries")

    client = httpx.Client(base_url=args.daemon)
    print(f"embedding {len(sampled)} queries + {len(atoms)} atoms...")
    q_mat_resp = client.post("/v1/embeddings", json={
        "input": [q["query"] for q in sampled], "model": EMBED_MODEL,
    }, timeout=180.0)
    q_mat_resp.raise_for_status()
    q_mat = np.stack([np.asarray(d["embedding"], dtype=np.float32)
                       for d in q_mat_resp.json()["data"]])
    n = np.linalg.norm(q_mat, axis=1, keepdims=True); n[n == 0] = 1.0
    q_mat /= n

    a_mat_resp = client.post("/v1/embeddings", json={
        "input": [atom_text(a) for a in atoms], "model": EMBED_MODEL,
    }, timeout=180.0)
    a_mat_resp.raise_for_status()
    a_mat = np.stack([np.asarray(d["embedding"], dtype=np.float32)
                       for d in a_mat_resp.json()["data"]])
    n = np.linalg.norm(a_mat, axis=1, keepdims=True); n[n == 0] = 1.0
    a_mat /= n
    atom_id_to_idx = {a.get("id", ""): i for i, a in enumerate(atoms)}

    print("building BM25...")
    bm25 = BM25([tok(c["content"]) for c in chunks])

    # Result accumulator: variant -> class -> {n, yes, partial, no, parse_fail}
    agg: dict[str, dict[str, dict[str, int]]] = {
        v: defaultdict(lambda: {"n": 0, "yes": 0, "partial": 0, "no": 0, "parse_fail": 0})
        for v in variants
    }

    t0 = time.time()
    n_calls = 0
    with open(args.out, "w") as outf:
        for qi, q in enumerate(tqdm(sampled, desc="queries")):
            q_vec = q_mat[qi]
            q_tokens = tok(q["query"])
            for v in variants:
                # The "-labeled" variants run the same retrieval as their base
                # variant, then assemble the brief with atom labels.
                base = v.replace("-labeled", "")
                labeled = v.endswith("-labeled")
                idx, matched = retrieve(
                    base, q_vec, q_tokens, chunk_mat, chunks, bm25,
                    atoms, a_mat, atoms_by_id, atom_id_to_idx, adj,
                    source_id=q.get("source_id"), k=args.top_k,
                )
                pairs = [(chunks[i], matched[ci])
                         for ci, i in enumerate(idx)]
                brief = build_brief(pairs, labeled=labeled,
                                    label_format=args.label_format)
                try:
                    res = ask_brief(client, args.model, q["query"], brief)
                except Exception as e:
                    res = {"label": "error", "answer": f"{type(e).__name__}: {e}"}
                n_calls += 1
                outf.write(json.dumps({
                    "query": q["query"],
                    "class": q["class"],
                    "source_id": q.get("source_id", ""),
                    "variant": v,
                    "labeled": labeled,
                    "top_k_chunks": [chunks[i]["chunk_id"] for i in idx],
                    "label": res["label"],
                    "answer": res["answer"],
                }) + "\n")
                outf.flush()
                lab = res["label"] if res["label"] in (
                    "yes", "partial", "no", "parse_fail") else "parse_fail"
                base_cls = q["class"].split(".paraphrase")[0]
                for cls in (base_cls, "__all__"):
                    bucket = agg[v][cls]
                    bucket["n"] += 1
                    bucket[lab] = bucket.get(lab, 0) + 1
    dt = time.time() - t0

    # ── Markdown report ──
    lines = [
        f"# Brief-quality probe — `{args.corpus}`",
        "",
        f"- queries: **{len(sampled)}** (stratified, --sample-per-class {args.sample_per_class})",
        f"- variants: **{len(variants)}**, top-K = {args.top_k}",
        f"- model: **{args.model}**",
        f"- judge calls: {n_calls}, wall-clock: {dt:.1f}s ({n_calls / max(dt, 0.01):.2f}/s)",
        "",
        "## Headline (all queries)",
        "",
        "| variant | n | yes | partial | no | parse_fail | yes+partial |",
        "|---|---|---|---|---|---|---|",
    ]
    for v in variants:
        a = agg[v]["__all__"]
        nn = a["n"] or 1
        lines.append(
            f"| {v} | {a['n']} | {a['yes']} ({a['yes']/nn:.1%}) | "
            f"{a['partial']} ({a['partial']/nn:.1%}) | "
            f"{a['no']} ({a['no']/nn:.1%}) | "
            f"{a['parse_fail']} ({a['parse_fail']/nn:.1%}) | "
            f"**{(a['yes']+a['partial'])/nn:.1%}** |"
        )

    # Per-class yes+partial table
    classes = sorted({cls for v in variants for cls in agg[v].keys()
                      if cls != "__all__"})
    lines += ["", "## Per-class yes+partial fraction", ""]
    lines.append("| variant | " + " | ".join(f"{c} (n)" for c in classes) + " |")
    lines.append("|" + "---|" * (len(classes) + 1))
    for v in variants:
        row = [v]
        for cls in classes:
            a = agg[v].get(cls)
            if a is None or a["n"] == 0:
                row.append("—")
            else:
                row.append(f"{(a['yes']+a['partial'])/a['n']:.0%} ({a['n']})")
        lines.append("| " + " | ".join(row) + " |")

    args.out_md.write_text("\n".join(lines) + "\n")
    print(f"\nwrote {args.out} and {args.out_md}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
