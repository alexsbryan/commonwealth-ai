# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "httpx>=0.27",
#   "numpy>=1.26",
#   "tqdm>=4.66",
# ]
# ///
"""Prepare paragraph-level chunks + embeddings for the atlas retrieval bench.

Reads the source book referenced by the enrichment config, walks the sections
recorded in chapters.json, splits each section into paragraph-sized sub-chunks,
and embeds them via the running daemon's /v1/embeddings endpoint.

Emits: chunks.jsonl — one chunk per line with fields
    { chunk_id, section_id, paragraph_index, content, embedding }

chunk_id format: "sec_0001::para_0003" — keeps the section id the atlas already
references as a prefix, so ground-truth lookups stay one lookup away.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path

import httpx
import numpy as np
from tqdm import tqdm


SOVEREIGN_ROOT = Path.home() / ".sovereign"
DAEMON_URL = "http://127.0.0.1:9741"
DEFAULT_EMBED_MODEL = "qwen-embedding-0.6b"

# Paragraph chunker defaults — deliberately matching corpus-engine/src/chunkers/paragraph.rs
# so retrieval numbers translate to production. If those defaults drift we'll
# need to re-sync, but that's a one-constant change.
MAX_CHARS = 1024
OVERLAP_CHARS = 128


def load_enrichment_config(corpus: str) -> dict:
    path = SOVEREIGN_ROOT / "enrichment" / corpus / "config.json"
    with open(path) as f:
        return json.load(f)


def load_chapters(corpus: str) -> list[dict]:
    path = SOVEREIGN_ROOT / "indexes" / corpus / "chapters.json"
    with open(path) as f:
        return json.load(f)["chapters"]


def section_body(text: str, start_byte: int, end_byte: int | None) -> str:
    """Extract a section's body bytes from the raw source text.

    chapters.json records heading_start_byte as the offset of the heading;
    the body runs from just-past-heading to the next heading's start. We treat
    the first newline after heading_start_byte as the heading boundary.
    """
    raw = text.encode("utf-8")
    seg = raw[start_byte:end_byte] if end_byte else raw[start_byte:]
    # Drop the heading line itself (first \n).
    nl = seg.find(b"\n")
    if nl >= 0:
        seg = seg[nl + 1 :]
    return seg.decode("utf-8", errors="replace").strip()


def paragraph_split(body: str, max_chars: int, overlap: int) -> list[str]:
    """Split a section body into overlapping paragraph-sized chunks.

    Matches corpus-engine's ParagraphChunker contract: each emitted chunk is
    at most max_chars, with overlap_chars carried from the tail of the
    previous chunk. Breaks prefer paragraph/sentence boundaries when they
    land near the max.
    """
    body = body.replace("\r\n", "\n").strip()
    if not body:
        return []

    out: list[str] = []
    i = 0
    n = len(body)
    while i < n:
        end = min(i + max_chars, n)
        # Prefer to end on a paragraph or sentence boundary within the last
        # quarter of the window, to avoid mid-sentence cuts.
        if end < n:
            window_start = i + max_chars - max_chars // 4
            # Paragraph break first (double newline).
            pb = body.rfind("\n\n", window_start, end)
            if pb > i:
                end = pb
            else:
                # Sentence break.
                sb = max(body.rfind(". ", window_start, end),
                         body.rfind("? ", window_start, end),
                         body.rfind("! ", window_start, end))
                if sb > i:
                    end = sb + 1
        chunk = body[i:end].strip()
        if chunk:
            out.append(chunk)
        if end >= n:
            break
        i = max(end - overlap, i + 1)
    return out


def embed_batch(client: httpx.Client, texts: list[str], model: str) -> np.ndarray:
    """Call /v1/embeddings with a batch, return (N, dim) float32."""
    r = client.post(
        "/v1/embeddings",
        json={"input": texts, "model": model},
        timeout=180.0,
    )
    r.raise_for_status()
    payload = r.json()
    # OpenAI-compat response shape.
    vecs = [np.asarray(d["embedding"], dtype=np.float32) for d in payload["data"]]
    return np.stack(vecs)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", required=True,
                    help="Corpus id under ~/.sovereign (e.g. brothers_karamazov)")
    ap.add_argument("--out", type=Path, default=None,
                    help="Output chunks.jsonl path (default: ./chunks-<corpus>.jsonl)")
    ap.add_argument("--batch", type=int, default=32,
                    help="Embed batch size — matches production 256-chunk flush? No, embed_batch on daemon is 256 but our sub-batch is limited by n_seq_max=16 inside the slot, so 32 here is a safe multiple.")
    ap.add_argument("--daemon", default=DAEMON_URL)
    ap.add_argument("--embed-model", default=DEFAULT_EMBED_MODEL,
                    help="Embedding model id registered with the daemon")
    ap.add_argument("--passage-prefix", default="",
                    help="String prepended to every chunk before embedding "
                         "(asymmetric retrieval models like Jina v5 want "
                         "'passage: ' here)")
    ap.add_argument("--max-chars", type=int, default=MAX_CHARS)
    ap.add_argument("--overlap-chars", type=int, default=OVERLAP_CHARS)
    ap.add_argument("--limit-sections", type=int, default=None,
                    help="Cap section count for smoke tests")
    args = ap.parse_args()

    out_path = args.out or Path(f"chunks-{args.corpus}.jsonl")

    cfg = load_enrichment_config(args.corpus)
    source_path = Path(cfg["source_path"])
    if not source_path.exists():
        print(f"error: source_path does not exist: {source_path}", file=sys.stderr)
        return 1

    print(f"reading {source_path} ({source_path.stat().st_size:,} bytes)")
    with open(source_path, "rb") as f:
        text_bytes = f.read()
    text = text_bytes.decode("utf-8", errors="replace")

    chapters = load_chapters(args.corpus)
    if args.limit_sections:
        chapters = chapters[: args.limit_sections]
    print(f"chapters: {len(chapters)}")

    # Build (section_id, paragraph_index, content) tuples first; embed in batches.
    items: list[tuple[str, int, str]] = []
    for idx, ch in enumerate(chapters):
        hb = int(ch["metadata"]["heading_start_byte"])
        next_hb = (int(chapters[idx + 1]["metadata"]["heading_start_byte"])
                   if idx + 1 < len(chapters) else None)
        body = section_body(text, hb, next_hb)
        paras = paragraph_split(body, args.max_chars, args.overlap_chars)
        for p_idx, para in enumerate(paras):
            items.append((ch["id"], p_idx, para))
    print(f"paragraph chunks: {len(items):,}")

    if not items:
        print("no chunks produced", file=sys.stderr)
        return 2

    client = httpx.Client(base_url=args.daemon)
    t0 = time.time()
    with open(out_path, "w") as out:
        for i in tqdm(range(0, len(items), args.batch), desc="embed"):
            batch = items[i : i + args.batch]
            texts = [args.passage_prefix + t[2] for t in batch]
            vecs = embed_batch(client, texts, args.embed_model)
            for (sec_id, p_idx, content), vec in zip(batch, vecs):
                out.write(json.dumps({
                    "chunk_id": f"{sec_id}::para_{p_idx:04d}",
                    "section_id": sec_id,
                    "paragraph_index": p_idx,
                    "content": content,
                    "embedding": vec.tolist(),
                }) + "\n")
    dt = time.time() - t0
    dim = vecs.shape[1] if len(items) else 0
    print(f"done: wrote {out_path} — {len(items):,} chunks, dim={dim}, "
          f"{dt:.1f}s ({len(items) / max(dt, 0.01):.1f} chunks/s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
