#!/usr/bin/env python3
"""Contamination pass: training streams vs external test sets.

Two checks, per VERIFIER_V0.md section 3:
  1. Canary strings: LLM-AggreFact rows embed a `contamination_identifier`
     canary. If any appears in a training stream, that stream contains
     benchmark rows verbatim.
  2. 13-gram word-shingle overlap (the GPT-3/Llama dedup convention):
     shingles of every *test* document, hashed into a set; every *training*
     document is scanned for a colliding shingle. A collision means the
     training doc shares a >=13-word verbatim span with a test doc.

Streams (`--stream`):
  halluguard  HalluGuard-Preferences-76k (Stream A), read from the HF cache.
  stream_b    Our synthetic harness output — the JSONL `svrn bench verifier
              export` writes. Checks the sealed evidence window AND the claim.

Stream B's substrate is our own bench corpora, so the risk profile differs by
corpus and the report keeps them separate: chaos-saltgrass is an original
document authored for the bank (collision would mean something is badly
wrong), while chaos-secret-agent is Conrad's public-domain novel, which could
plausibly sit inside a benchmark's source documents. That is exactly the case
worth measuring rather than assuming.

Test sets: LLM-AggreFact test parquet + FaithBench data_for_release sources.
Output: findings/<report>.json (collision counts + examples).

Usage:
  contamination_pass.py --stream halluguard
  contamination_pass.py --stream stream_b --input data/stream_b/*/stream_b.jsonl
"""

import argparse
import glob
import hashlib
import json
import os
import re
import sys

N = 13
ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
WORD_RE = re.compile(r"[a-z0-9]+")


def shingles(text: str):
    """Stable 8-byte digests, NOT builtin hash().

    `hash()` on str is salted per process (PYTHONHASHSEED), so an index built
    in one run and a scan in another would silently never collide. Both halves
    happen to run in one process today; this makes the report reproducible and
    the two halves separable.
    """
    words = WORD_RE.findall(text.lower())
    for i in range(len(words) - N + 1):
        gram = " ".join(words[i : i + N]).encode()
        yield hashlib.blake2b(gram, digest_size=8).digest()


def build_index():
    """Test-set shingle index → (index, canaries, provenance)."""
    import pyarrow.parquet as pq

    test_docs = {}  # doc text -> (benchmark, subset/sample id)
    aggre = os.path.join(ROOT, "data/llm-aggrefact/test.parquet")
    if not os.path.isfile(aggre):
        sys.exit(
            f"error: missing {aggre}\n"
            "  LLM-AggreFact is gated; fetch with a bearer token (hf download "
            "chokes on this repo's hash format — see README 'Gotchas')."
        )
    t = pq.read_table(aggre)
    for ds, doc in zip(t.column("dataset").to_pylist(), t.column("doc").to_pylist()):
        test_docs.setdefault(doc, ("LLM-AggreFact", ds))
    canaries = set(t.column("contamination_identifier").to_pylist())

    fb_files = sorted(glob.glob(os.path.join(ROOT, "data/FaithBench/data_for_release/batch_*.json")))
    if not fb_files:
        sys.exit(
            f"error: no FaithBench batches under {ROOT}/data/FaithBench/data_for_release/\n"
            "  clone it: git clone --depth 1 https://github.com/vectara/FaithBench.git"
        )
    for fp in fb_files:
        for s in json.load(open(fp))["samples"]:
            test_docs.setdefault(s["source"], ("FaithBench", os.path.basename(fp)))

    print(f"unique test docs: {len(test_docs)}")
    index = {}  # shingle -> (benchmark, subset) of first-seen doc
    for doc, origin in test_docs.items():
        for h in shingles(doc):
            index.setdefault(h, origin)
    print(f"test shingle index: {len(index):,} {N}-grams")
    return index, canaries, {
        "LLM-AggreFact": f"test.parquet ({t.num_rows:,} rows)",
        "FaithBench": f"{len(fb_files)} release batches",
    }, len(test_docs)


def first_collision(text, index):
    for h in shingles(text):
        if h in index:
            return index[h]
    return None


# ───────────────────────────── stream readers ────────────────────────────
#
# Each yields (row_id, group, doc_text, claim_text). `group` partitions the
# report — corpus for Stream B, a single bucket for Stream A.


def read_halluguard(_paths):
    hits = glob.glob(
        os.path.expanduser(
            "~/.cache/huggingface/hub/datasets--lrsbrgrn--HalluGuard-Preferences-76k/"
            "snapshots/*/halluguard-main.jsonl"
        )
    )
    if not hits:
        sys.exit(
            "error: HalluGuard-Preferences-76k is not in this box's HF cache.\n"
            "  It lives on the eval box; run --stream halluguard there, or "
            "`hf download lrsbrgrn/HalluGuard-Preferences-76k --repo-type dataset`."
        )
    with open(hits[0]) as f:
        for i, line in enumerate(f):
            d = json.loads(line)
            pj = json.loads(d["prompt"][0]["content"])
            yield i + 1, "HalluGuard-76k", pj["document"], pj["claim"]


def read_stream_b(paths):
    if not paths:
        sys.exit("error: --stream stream_b needs --input <stream_b.jsonl> (repeatable/globbable)")
    for path in paths:
        with open(path) as f:
            for line in f:
                r = json.loads(line)
                yield (
                    r["id"],
                    r.get("corpus_id") or os.path.basename(os.path.dirname(path)),
                    "\n\n".join(r.get("evidence_chunks") or []),
                    r.get("claim", ""),
                )


READERS = {"halluguard": read_halluguard, "stream_b": read_stream_b}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--stream", choices=sorted(READERS), default="halluguard")
    ap.add_argument("--input", nargs="*", default=[], help="stream_b JSONL path(s)")
    ap.add_argument("--out", help="report path (default: findings/contamination_report[_<stream>].json)")
    args = ap.parse_args()

    index, canaries, test_provenance, n_test_docs = build_index()

    rows = 0
    canary_hits = 0
    collisions = []
    per_benchmark = {"LLM-AggreFact": 0, "FaithBench": 0}
    per_group = {}
    raw_blobs = []

    for row_id, group, doc, claim in READERS[args.stream](args.input):
        rows += 1
        g = per_group.setdefault(group, {"rows": 0, "doc_collisions": 0, "claim_collisions": 0})
        g["rows"] += 1
        # The evidence window is the contamination surface that matters: it is
        # verbatim source text. The claim is checked too, but claims are often
        # under 13 words and then yield no shingles at all — a clean claim
        # result is weaker evidence than a clean doc result, so they are
        # reported separately rather than summed.
        hit = first_collision(doc, index)
        if hit:
            per_benchmark[hit[0]] += 1
            g["doc_collisions"] += 1
            collisions.append({"row": row_id, "group": group, "where": "evidence",
                               "benchmark": hit[0], "subset": hit[1], "claim": claim[:120]})
        chit = first_collision(claim, index)
        if chit:
            # Count claim hits into per_benchmark too. Until 2026-07-31 only the
            # evidence path did, so a claim-only collision was invisible in the
            # top-line `colliding_training_rows` while showing in per_group —
            # the two halves of the report disagreed. Guarded so a row that
            # collides on BOTH surfaces is still one row against the benchmark.
            if not (hit and hit[0] == chit[0]):
                per_benchmark[chit[0]] += 1
            g["claim_collisions"] += 1
            collisions.append({"row": row_id, "group": group, "where": "claim",
                               "benchmark": chit[0], "subset": chit[1], "claim": claim[:120]})
        raw_blobs.append(doc)
        raw_blobs.append(claim)

    blob = "\n".join(raw_blobs)
    for c in canaries:
        if c and c in blob:
            canary_hits += 1

    report = {
        "training_stream": args.stream,
        "inputs": args.input or ["<HF cache>"],
        "training_rows": rows,
        "ngram": N,
        "test_sets": test_provenance,
        "unique_test_docs": n_test_docs,
        "canary_hits": canary_hits,
        "colliding_training_rows": per_benchmark,
        "per_group": per_group,
        "collision_examples": collisions[:50],
        "verdict": (
            "CLEAN" if canary_hits == 0 and not collisions else "COLLISIONS FOUND"
        ),
    }
    default = ("contamination_report.json" if args.stream == "halluguard"
               else f"contamination_report_{args.stream}.json")
    out = args.out or os.path.join(ROOT, "findings", default)
    os.makedirs(os.path.dirname(out), exist_ok=True)
    with open(out, "w") as f:
        json.dump(report, f, indent=2)
    print(json.dumps({k: v for k, v in report.items() if k != "collision_examples"}, indent=2))
    print(f"report -> {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
