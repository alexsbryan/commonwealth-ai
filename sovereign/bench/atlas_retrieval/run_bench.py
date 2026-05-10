# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "httpx>=0.27",
#   "numpy>=1.26",
#   "tqdm>=4.66",
# ]
# ///
"""Atlas retrieval variant runner.

Loads chunks.jsonl, queries.jsonl, and atlas atoms/edges for a corpus.
Runs each variant against every query, computes recall@K + MRR stratified
by query class, and emits JSON + a markdown summary.

Variants implemented here (in-memory, no ANN index required — corpus scale
is ~thousands of chunks, brute-force cosine is <10ms/query):

    flat-fp32             cosine top-K over all chunks, 1024-dim fp32
    flat-fp16             same but embeddings quantized to fp16 (2× smaller)
    flat-pq               IVF-PQ simulation (16 subquantizers × 256 codes
                          = 16 bytes/vector) — tests whether aggressive
                          compression costs recall meaningfully
    bm25-only             no embeddings, BM25 over chunk text
    bm25-rerank           BM25 top-100 → fp32 cosine rerank
    atlas-tier            query→atom cosine match → chunks narrowed to
                          atom's provenance sections + 1-hop edges,
                          then cosine rank within narrowed set
    atlas-tier-prune      atlas-tier but no 1-hop expansion — only chunks
                          directly referenced by the matched atom

Ground truth is a set of section ids (sec_NNNN) per query. A returned
paragraph chunk is relevant iff its section_id is in the set.
"""
from __future__ import annotations

import argparse
import dataclasses
import json
import math
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
DEFAULT_EMBED_MODEL = "qwen-embedding-0.6b"


# ─── Data loading ─────────────────────────────────────────────────────


@dataclasses.dataclass
class Chunk:
    chunk_id: str
    section_id: str
    content: str
    embedding: np.ndarray  # fp32, shape (dim,)


@dataclasses.dataclass
class Query:
    query: str
    cls: str
    relevant_sections: set[str]
    source_id: str
    # Optional chunk-level GT from an LLM judge. When non-empty, recall/MRR
    # switch from section-id containment to chunk-id containment.
    golden_chunks: set[str] = dataclasses.field(default_factory=set)


def load_chunks(path: Path) -> tuple[list[Chunk], np.ndarray]:
    """Load chunks and stack embeddings into a single (N, dim) fp32 matrix."""
    rows: list[Chunk] = []
    with open(path) as f:
        for line in f:
            d = json.loads(line)
            rows.append(Chunk(
                chunk_id=d["chunk_id"],
                section_id=d["section_id"],
                content=d["content"],
                embedding=np.asarray(d["embedding"], dtype=np.float32),
            ))
    mat = np.stack([r.embedding for r in rows])
    # Normalize for cosine via dot product.
    norms = np.linalg.norm(mat, axis=1, keepdims=True)
    norms[norms == 0] = 1.0
    mat_norm = mat / norms
    return rows, mat_norm


def load_queries(path: Path) -> list[Query]:
    rows: list[Query] = []
    with open(path) as f:
        for line in f:
            d = json.loads(line)
            rows.append(Query(
                query=d["query"],
                cls=d["class"],
                relevant_sections=set(d["relevant_sections"]),
                source_id=d["source_id"],
            ))
    return rows


def load_golden_gt(path: Path) -> dict[str, set[str]]:
    """Return query_text -> set[chunk_id] from a label_gt.py output file.

    When --golden-gt is passed, the bench restricts to queries present in
    this file AND scores recall/MRR on chunk_id containment (not section).
    """
    out: dict[str, set[str]] = {}
    with open(path) as f:
        for line in f:
            d = json.loads(line)
            out[d["query"]] = set(d.get("relevant_chunks", []))
    return out


def load_atlas(corpus: str) -> tuple[list[dict], dict[str, dict], list[dict]]:
    """Return (atoms_flat, atoms_by_id, edges)."""
    def _load(name: str, default):
        p = SOVEREIGN_ROOT / "indexes" / corpus / "atlas" / name
        return json.loads(p.read_text()) if p.exists() else default

    atoms_env = _load("atoms.json", {"atoms": []}).get("atoms", [])
    edges = _load("edges.json", {"edges": []}).get("edges", [])
    atoms_flat: list[dict] = []
    by_id: dict[str, dict] = {}
    for env in atoms_env:
        data = env.get("data", {})
        rec = {"atom_type": env.get("atom_type"), **data}
        atoms_flat.append(rec)
        aid = rec.get("id")
        if aid:
            by_id[aid] = rec
    return atoms_flat, by_id, edges


def atom_display_text(a: dict) -> str:
    """Concatenate the fields most descriptive of what this atom covers.

    Used as the text the skeleton tier embeds — so the query→atom match
    can find atoms by description + claim + label, not just by id.
    """
    parts: list[str] = []
    for k in ("canonical_name", "label", "content", "description"):
        v = a.get(k)
        if isinstance(v, str) and v.strip():
            parts.append(v.strip())
    # Add a few alias variants for Entity atoms so misspellings still match.
    aliases = a.get("aliases") or []
    if isinstance(aliases, list):
        parts.extend([str(x) for x in aliases[:3] if isinstance(x, str)])
    return " | ".join(parts) if parts else (a.get("id", ""))


def atom_sections(a: dict) -> set[str]:
    """All sec_NNNN ids this atom is grounded in."""
    out: set[str] = set()
    fa = a.get("first_appearance")
    if isinstance(fa, dict):
        cid = fa.get("chunk_id")
        if isinstance(cid, str) and cid.startswith("sec_"):
            out.add(cid)
    for ev in a.get("evidence") or []:
        if isinstance(ev, dict):
            cid = ev.get("chunk_id")
            if isinstance(cid, str) and cid.startswith("sec_"):
                out.add(cid)
    sr = a.get("section_range")
    if isinstance(sr, dict):
        try:
            s = int(sr["start"].split("_")[1])
            e = int(sr["end"].split("_")[1])
            out.update(f"sec_{i:04d}" for i in range(s, e + 1))
        except (KeyError, IndexError, ValueError, AttributeError):
            pass
    ra = a.get("raised_at")
    if isinstance(ra, dict):
        cid = ra.get("chunk_id")
        if isinstance(cid, str) and cid.startswith("sec_"):
            out.add(cid)
    return out


def edge_neighbors(atoms_by_id: dict[str, dict], edges: list[dict]) -> dict[str, set[str]]:
    """atom_id -> set of atom_ids connected via any edge."""
    adj: dict[str, set[str]] = defaultdict(set)
    for e in edges:
        s = e.get("source")
        t = e.get("target")
        if isinstance(s, str) and isinstance(t, str):
            adj[s].add(t)
            adj[t].add(s)
    return adj


# ─── Embedding client ─────────────────────────────────────────────────


def embed_many(client: httpx.Client, texts: list[str], model: str,
               batch: int = 32) -> np.ndarray:
    """Call daemon /v1/embeddings in batches, return (N, dim) fp32 normalized."""
    out: list[np.ndarray] = []
    for i in range(0, len(texts), batch):
        payload = {"input": texts[i : i + batch], "model": model}
        r = client.post("/v1/embeddings", json=payload, timeout=180.0)
        r.raise_for_status()
        for d in r.json()["data"]:
            out.append(np.asarray(d["embedding"], dtype=np.float32))
    mat = np.stack(out)
    norms = np.linalg.norm(mat, axis=1, keepdims=True)
    norms[norms == 0] = 1.0
    return mat / norms


# ─── BM25 ─────────────────────────────────────────────────────────────


TOKEN_RE = re.compile(r"[a-z0-9]+")

def tokenize(text: str) -> list[str]:
    return TOKEN_RE.findall(text.lower())


class BM25:
    """Okapi BM25. Built once over the chunk corpus, queried many times."""
    def __init__(self, docs: list[list[str]], k1: float = 1.5, b: float = 0.75):
        self.k1, self.b = k1, b
        self.N = len(docs)
        self.dl = np.array([len(d) for d in docs], dtype=np.float32)
        self.avgdl = float(self.dl.mean()) if self.N else 0.0
        # term -> doc_id -> tf
        self.postings: dict[str, dict[int, int]] = defaultdict(dict)
        for i, d in enumerate(docs):
            for tok, tf in Counter(d).items():
                self.postings[tok][i] = tf
        # idf cache
        self.idf: dict[str, float] = {}
        for tok, post in self.postings.items():
            n = len(post)
            # BM25 variant with +0.5 smoothing
            self.idf[tok] = math.log((self.N - n + 0.5) / (n + 0.5) + 1.0)

    def score(self, query: list[str]) -> np.ndarray:
        scores = np.zeros(self.N, dtype=np.float32)
        for tok in query:
            post = self.postings.get(tok)
            if not post:
                continue
            idf = self.idf.get(tok, 0.0)
            for doc_id, tf in post.items():
                dl = self.dl[doc_id]
                denom = tf + self.k1 * (1.0 - self.b + self.b * dl / max(self.avgdl, 1.0))
                scores[doc_id] += idf * (tf * (self.k1 + 1.0)) / denom
        return scores


# ─── Product Quantization (simple simulation) ─────────────────────────


def pq_encode_decode(embeddings: np.ndarray, m: int = 16, ks: int = 256,
                     iters: int = 20, seed: int = 0) -> np.ndarray:
    """Encode then decode via PQ to simulate the reconstruction loss.

    m subquantizers × ks codes each. For 1024-dim with m=16 that's 64-dim
    subvectors, ks=256 codes → 1 byte per subquantizer → 16 bytes per
    vector. Returns the decoded (lossy-reconstructed) matrix, same shape
    as input.

    Uses a light k-means on a random sample for each subspace. Not the
    fastest implementation, but accurate for bench purposes.
    """
    rng = np.random.default_rng(seed)
    N, D = embeddings.shape
    assert D % m == 0, f"dim {D} not divisible by m={m}"
    dsub = D // m
    # Sample up to 10k vectors for codebook learning.
    sample_idx = rng.choice(N, size=min(N, 10_000), replace=False)
    sample = embeddings[sample_idx]
    codebooks = np.empty((m, ks, dsub), dtype=np.float32)
    codes = np.empty((N, m), dtype=np.int32)
    for sub in range(m):
        seg = sample[:, sub * dsub : (sub + 1) * dsub]
        # k-means++ init with numpy-only.
        init_idx = rng.choice(seg.shape[0], size=ks, replace=False)
        centroids = seg[init_idx].copy()
        for _ in range(iters):
            d = np.linalg.norm(seg[:, None, :] - centroids[None, :, :], axis=2)
            assign = np.argmin(d, axis=1)
            for k in range(ks):
                mask = assign == k
                if mask.any():
                    centroids[k] = seg[mask].mean(axis=0)
        codebooks[sub] = centroids
        # Encode full corpus against this subspace.
        full_seg = embeddings[:, sub * dsub : (sub + 1) * dsub]
        d_full = np.linalg.norm(full_seg[:, None, :] - centroids[None, :, :], axis=2)
        codes[:, sub] = np.argmin(d_full, axis=1)
    # Decode.
    decoded = np.empty_like(embeddings)
    for sub in range(m):
        decoded[:, sub * dsub : (sub + 1) * dsub] = codebooks[sub][codes[:, sub]]
    # Re-normalize (PQ reconstruction breaks unit length).
    norms = np.linalg.norm(decoded, axis=1, keepdims=True)
    norms[norms == 0] = 1.0
    return decoded / norms


# ─── Variants ─────────────────────────────────────────────────────────


def variant_flat(q_vec: np.ndarray, chunk_mat: np.ndarray, k: int) -> np.ndarray:
    """Return indices of top-k chunks by cosine similarity (descending)."""
    scores = chunk_mat @ q_vec
    if k >= len(scores):
        return np.argsort(-scores)
    idx = np.argpartition(-scores, k)[:k]
    return idx[np.argsort(-scores[idx])]


def variant_bm25_only(q_tokens: list[str], bm25: BM25, k: int) -> np.ndarray:
    scores = bm25.score(q_tokens)
    idx = np.argpartition(-scores, min(k, len(scores) - 1))[:k]
    return idx[np.argsort(-scores[idx])]


def variant_bm25_rerank(q_vec: np.ndarray, q_tokens: list[str],
                        bm25: BM25, chunk_mat: np.ndarray, k: int,
                        prefilter_k: int = 100) -> np.ndarray:
    bm_scores = bm25.score(q_tokens)
    pre = np.argpartition(-bm_scores, min(prefilter_k, len(bm_scores) - 1))[:prefilter_k]
    # Dense rerank within prefiltered set.
    sub = chunk_mat[pre]
    dense = sub @ q_vec
    order = np.argsort(-dense)
    return pre[order][:k]


def variant_atlas_tier(q_vec: np.ndarray, chunk_mat: np.ndarray,
                       chunks: list[Chunk], atom_mat: np.ndarray,
                       atoms: list[dict], adj: dict[str, set[str]],
                       atoms_by_id: dict[str, dict],
                       top_atoms: int, expand_hops: int, k: int,
                       hide_atom_idx: int | None = None) -> np.ndarray:
    """Query → atom cosine match → union of atom section provenance
    (optionally + 1-hop neighbors via edges) → dense rank within that
    narrowed chunk set.

    hide_atom_idx: if set, that atom is excluded from the atom-matching
    pool. Used by the leave-one-atom-out evaluation: each query was
    derived from a specific atom, and hiding that atom forces the tier
    to reach the relevant sections via *other* atoms — testing whether
    the atlas graph carries the routing signal beyond the source atom.
    """
    atom_scores = atom_mat @ q_vec
    if hide_atom_idx is not None:
        atom_scores = atom_scores.copy()
        atom_scores[hide_atom_idx] = -np.inf
    atom_idx = np.argpartition(-atom_scores, min(top_atoms, len(atom_scores) - 1))[:top_atoms]
    atom_idx = atom_idx[np.argsort(-atom_scores[atom_idx])]
    # Collect section ids from matched atoms (+ neighbors if expand).
    candidate_sections: set[str] = set()
    for ai in atom_idx:
        a = atoms[ai]
        candidate_sections.update(atom_sections(a))
        if expand_hops > 0:
            for nb_id in adj.get(a.get("id", ""), set()):
                nb = atoms_by_id.get(nb_id)
                if nb:
                    candidate_sections.update(atom_sections(nb))
    if not candidate_sections:
        # Fallback: tier produced no provenance — degrade to flat top-k.
        return variant_flat(q_vec, chunk_mat, k)
    chunk_mask = np.array([c.section_id in candidate_sections for c in chunks])
    narrowed_idx = np.where(chunk_mask)[0]
    if len(narrowed_idx) == 0:
        return variant_flat(q_vec, chunk_mat, k)
    sub = chunk_mat[narrowed_idx]
    scores = sub @ q_vec
    # Top-k within narrowed set.
    kk = min(k, len(narrowed_idx))
    order = np.argsort(-scores)[:kk]
    return narrowed_idx[order]


# ─── Evaluation ───────────────────────────────────────────────────────


def _is_relevant(chunk: Chunk, query: Query, golden: bool) -> bool:
    if golden:
        return chunk.chunk_id in query.golden_chunks
    return chunk.section_id in query.relevant_sections


def recall_at_k(results_idx: np.ndarray, chunks: list[Chunk],
                query: Query, ks: list[int], golden: bool) -> dict[int, int]:
    out: dict[int, int] = {}
    for k in ks:
        topk = results_idx[:k]
        hit = any(_is_relevant(chunks[i], query, golden) for i in topk)
        out[k] = int(hit)
    return out


def mrr(results_idx: np.ndarray, chunks: list[Chunk], query: Query,
        golden: bool) -> float:
    for rank, i in enumerate(results_idx, start=1):
        if _is_relevant(chunks[i], query, golden):
            return 1.0 / rank
    return 0.0


# ─── Main ─────────────────────────────────────────────────────────────


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", required=True)
    ap.add_argument("--chunks", type=Path, required=True)
    ap.add_argument("--queries", type=Path, required=True)
    ap.add_argument("--variants", default="flat-fp32,flat-fp16,flat-pq,bm25-only,bm25-rerank,atlas-tier,atlas-tier-prune,atlas-tier-loo,atlas-tier-loo-hop")
    ap.add_argument("--k", type=int, default=50, help="Max k evaluated")
    ap.add_argument("--ks", default="1,5,10,50")
    ap.add_argument("--top-atoms", type=int, default=5,
                    help="Atom-tier top-N used for atlas variants")
    ap.add_argument("--out-json", type=Path, default=Path("bench-results.json"))
    ap.add_argument("--out-md", type=Path, default=Path("bench-report.md"))
    ap.add_argument("--daemon", default=DAEMON_URL)
    ap.add_argument("--embed-model", default=DEFAULT_EMBED_MODEL,
                    help="Embedding model id used for queries + atom display "
                         "text. Must match the model used for chunks.")
    ap.add_argument("--query-prefix", default="",
                    help="String prepended to every query before embedding "
                         "(asymmetric retrieval models want 'query: ' here)")
    ap.add_argument("--passage-prefix", default="",
                    help="String prepended to every atom display text before "
                         "embedding. Should match the prefix used in "
                         "prep_chunks.py for chunks.")
    ap.add_argument("--golden-gt", type=Path, default=None,
                    help="label_gt.py output file. When set, restrict to the "
                         "queries it covers and score chunk-id containment "
                         "instead of section-id containment (breaks atom "
                         "circularity).")
    args = ap.parse_args()

    variants = [v.strip() for v in args.variants.split(",") if v.strip()]
    ks = [int(x) for x in args.ks.split(",")]
    max_k = max(max(ks), args.k)

    # ── Load data ──
    t0 = time.time()
    print("loading chunks...")
    chunks, chunk_mat = load_chunks(args.chunks)
    dim = chunk_mat.shape[1]
    print(f"  {len(chunks):,} chunks, dim={dim}")

    print("loading queries...")
    queries = load_queries(args.queries)
    print(f"  {len(queries):,} queries")

    # ── Optional: attach golden-GT, restrict to queries it covers ──
    golden = False
    if args.golden_gt:
        print(f"loading golden GT from {args.golden_gt}")
        gt_map = load_golden_gt(args.golden_gt)
        # Restrict queries to those judged, attach chunk-level labels.
        before = len(queries)
        queries = [q for q in queries if q.query in gt_map]
        for q in queries:
            q.golden_chunks = gt_map[q.query]
        golden = True
        print(f"  restricted: {before} -> {len(queries)} queries, "
              f"mode=golden-chunk-id")

    print("loading atlas...")
    atoms, atoms_by_id, edges = load_atlas(args.corpus)
    adj = edge_neighbors(atoms_by_id, edges)
    print(f"  {len(atoms):,} atoms, {len(edges):,} edges")

    # ── Embed queries (once, shared across dense variants) ──
    client = httpx.Client(base_url=args.daemon)
    print(f"embedding {len(queries)} queries via daemon (model={args.embed_model}, "
          f"query_prefix={args.query_prefix!r})...")
    q_mat = embed_many(client, [args.query_prefix + q.query for q in queries],
                       args.embed_model)
    print(f"  query matrix: {q_mat.shape}")

    # ── Embed atom texts if needed ──
    needs_atlas = any(v.startswith("atlas-") for v in variants)
    atom_mat = None
    atom_id_to_idx: dict[str, int] = {}
    if needs_atlas:
        print(f"embedding {len(atoms)} atoms via daemon (model={args.embed_model}, "
              f"passage_prefix={args.passage_prefix!r})...")
        atom_mat = embed_many(
            client,
            [args.passage_prefix + atom_display_text(a) for a in atoms],
            args.embed_model,
        )
        atom_id_to_idx = {a.get("id", ""): i for i, a in enumerate(atoms)}
        print(f"  atom matrix: {atom_mat.shape}")

    # ── Build BM25 if needed ──
    bm25 = None
    if any(v.startswith("bm25") for v in variants):
        print("building BM25 index...")
        tokenized = [tokenize(c.content) for c in chunks]
        bm25 = BM25(tokenized)
        print(f"  {len(bm25.postings):,} unique terms")

    # ── fp16 variant: cast chunk matrix ──
    chunk_mat_fp16 = chunk_mat.astype(np.float16).astype(np.float32)  # simulate fp16 storage
    # Re-normalize after fp16 round-trip (tiny drift).
    n = np.linalg.norm(chunk_mat_fp16, axis=1, keepdims=True)
    n[n == 0] = 1.0
    chunk_mat_fp16 /= n

    # ── pq variant: decode once ──
    chunk_mat_pq = None
    if "flat-pq" in variants:
        print("encoding chunks via PQ (m=16, ks=256, iters=20)...")
        t_pq = time.time()
        chunk_mat_pq = pq_encode_decode(chunk_mat, m=16, ks=256, iters=20)
        print(f"  PQ encode/decode: {time.time() - t_pq:.1f}s")

    # ── Score loop ──
    # Accumulators: variant -> class -> {recall@k counts, mrr sum, n}
    agg: dict[str, dict[str, dict[str, float]]] = {
        v: defaultdict(lambda: {"n": 0.0, "mrr": 0.0,
                                 **{f"r@{k}": 0.0 for k in ks}})
        for v in variants
    }

    latency_ms: dict[str, list[float]] = defaultdict(list)

    for qi, q in enumerate(tqdm(queries, desc="scoring")):
        q_vec = q_mat[qi]
        q_tokens = tokenize(q.query) if bm25 is not None else []
        for v in variants:
            t_start = time.perf_counter()
            if v == "flat-fp32":
                res = variant_flat(q_vec, chunk_mat, max_k)
            elif v == "flat-fp16":
                res = variant_flat(q_vec, chunk_mat_fp16, max_k)
            elif v == "flat-pq":
                res = variant_flat(q_vec, chunk_mat_pq, max_k)
            elif v == "bm25-only":
                res = variant_bm25_only(q_tokens, bm25, max_k)
            elif v == "bm25-rerank":
                res = variant_bm25_rerank(q_vec, q_tokens, bm25, chunk_mat, max_k)
            elif v == "atlas-tier":
                res = variant_atlas_tier(q_vec, chunk_mat, chunks, atom_mat,
                                          atoms, adj, atoms_by_id,
                                          top_atoms=args.top_atoms,
                                          expand_hops=1, k=max_k)
            elif v == "atlas-tier-prune":
                res = variant_atlas_tier(q_vec, chunk_mat, chunks, atom_mat,
                                          atoms, adj, atoms_by_id,
                                          top_atoms=args.top_atoms,
                                          expand_hops=0, k=max_k)
            elif v == "atlas-tier-loo":
                # Leave-one-out: hide the atom this query was derived from,
                # so the tier must reach the relevant sections via OTHER
                # atoms whose provenance overlaps. Breaks the most direct
                # circular pathway (query→its-own-source-atom→its-own-sections).
                hide = atom_id_to_idx.get(q.source_id)
                res = variant_atlas_tier(q_vec, chunk_mat, chunks, atom_mat,
                                          atoms, adj, atoms_by_id,
                                          top_atoms=args.top_atoms,
                                          expand_hops=0, k=max_k,
                                          hide_atom_idx=hide)
            elif v == "atlas-tier-loo-hop":
                hide = atom_id_to_idx.get(q.source_id)
                res = variant_atlas_tier(q_vec, chunk_mat, chunks, atom_mat,
                                          atoms, adj, atoms_by_id,
                                          top_atoms=args.top_atoms,
                                          expand_hops=1, k=max_k,
                                          hide_atom_idx=hide)
            else:
                print(f"unknown variant: {v}", file=sys.stderr)
                return 2
            latency_ms[v].append((time.perf_counter() - t_start) * 1000.0)

            # Score.
            rec = recall_at_k(res, chunks, q, ks, golden)
            m = mrr(res, chunks, q, golden)
            for bucket in (agg[v][q.cls], agg[v]["__all__"]):
                bucket["n"] += 1
                bucket["mrr"] += m
                for k in ks:
                    bucket[f"r@{k}"] += rec[k]

    # ── Report ──
    results: dict = {
        "corpus": args.corpus,
        "n_chunks": len(chunks),
        "n_queries": len(queries),
        "n_atoms": len(atoms),
        "n_edges": len(edges),
        "gt_mode": "golden-chunk" if golden else "atom-section",
        "gt_path": str(args.golden_gt) if args.golden_gt else None,
        "variants": variants,
        "ks": ks,
        "metrics": {},
        "latency_ms": {v: {
            "p50": float(np.percentile(latency_ms[v], 50)),
            "p95": float(np.percentile(latency_ms[v], 95)),
            "mean": float(np.mean(latency_ms[v])),
        } for v in variants},
    }
    for v in variants:
        results["metrics"][v] = {}
        for cls, bucket in agg[v].items():
            n = bucket["n"]
            if n == 0:
                continue
            results["metrics"][v][cls] = {
                "n": int(n),
                "mrr": bucket["mrr"] / n,
                **{f"r@{k}": bucket[f"r@{k}"] / n for k in ks},
            }

    args.out_json.write_text(json.dumps(results, indent=2))
    print(f"\nwrote {args.out_json}")

    # Markdown summary — headline table over __all__.
    gt_mode_str = "golden-chunk (judge)" if golden else "atom-section (circular)"
    lines = [
        f"# Atlas retrieval bench — `{args.corpus}`",
        "",
        f"- chunks: **{len(chunks):,}** (dim {dim})",
        f"- queries: **{len(queries):,}**",
        f"- atoms: **{len(atoms):,}**, edges: **{len(edges):,}**",
        f"- GT mode: **{gt_mode_str}**",
        f"- bench wall-clock: **{time.time() - t0:.1f}s**",
        "",
        "## Headline (all queries)",
        "",
        "| variant | " + " | ".join(f"r@{k}" for k in ks) + " | MRR | p50 ms | p95 ms |",
        "|" + "---|" * (len(ks) + 4),
    ]
    for v in variants:
        a = results["metrics"][v].get("__all__", {})
        lat = results["latency_ms"][v]
        lines.append(
            f"| {v} | "
            + " | ".join(f"{a.get(f'r@{k}', 0):.3f}" for k in ks)
            + f" | {a.get('mrr', 0):.3f}"
            + f" | {lat['p50']:.2f} | {lat['p95']:.2f} |"
        )

    # Per-class headline (recall@10 only, readable).
    lines += ["", "## Per-class recall@10", ""]
    all_classes = sorted({
        cls for v in variants for cls in agg[v].keys() if cls != "__all__"
    })
    header = ["variant"] + [f"{c} (n)" for c in all_classes]
    lines.append("| " + " | ".join(header) + " |")
    lines.append("|" + "---|" * len(header))
    for v in variants:
        row = [v]
        for cls in all_classes:
            m = results["metrics"][v].get(cls)
            if m is None:
                row.append("—")
            else:
                row.append(f"{m.get('r@10', 0):.3f} ({m['n']})")
        lines.append("| " + " | ".join(row) + " |")

    args.out_md.write_text("\n".join(lines) + "\n")
    print(f"wrote {args.out_md}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
