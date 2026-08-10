#!/usr/bin/env python3
"""Slice 1 of the verifier substitution study: can our checkpoint stand in for
the shipped 35B critic INSIDE the production grounding gate?

WHY THIS EXISTS, AND WHY IT IS NOT ANOTHER BENCH RUN. Every number the verifier
program has produced so far is against LLM-AggreFact, and on 2026-08-06 that
bench was measured to be ~82% solvable from the claim alone (leak AUC 0.8157,
note 5698d555) -- our own trained 4B reading the document scores 0.805. A rank
on that card therefore cannot distinguish grounding skill from claim
plausibility, and it says nothing about the one deployment the spec justifies
itself by (`VERIFIER_V0.md:16-19`: replace the gate's per-claim judge calls).

So this scores the model on the REAL task instead: claims extracted by the
production `extract_claim_list` from real corpus summaries, judged against real
retrieved chunks, with the incumbent's own verdict as the comparison. The input
is a `svrn bench faithfulness run` artifact, whose rows are produced by the
production registers -- `extract_claim_list` and `claim_chunk_support`, the
latter being the gate's own `forced_choice_ab` pass.

EVIDENCE IS HELD FIXED, DELIBERATELY. The prior attempt at an external
grounding verifier failed both gates at every threshold
(`SITUATED_HARNESS_STUDY.md:96-126`), diagnosed as competence and honesty being
coupled through RETRIEVAL quality: answers scored 0.96-0.98 unsupported while
being parametric-correct, because the chunk was never retrieved. Scoring both
judges on the identical chunk list removes that confound entirely. If the two
still disagree, the disagreement is about judgment, not about search.

WE MIRROR THE INCUMBENT'S PROCEDURE, AND MIRRORING IT IS NOT OPTIONAL. The
faithfulness lane does NOT judge every attached chunk: it ranks the claim's
member window by cosine against the claim, takes `CHUNK_CAP` (12), judges in
that order tracking the max, and BREAKS at `EARLY_EXIT_SUPPORT` (0.95); the
verdict is `max_support >= SUPPORTED_TAU` (0.5). Measured on a 180-claim
artifact: mean 5.9 chunks judged against mean 68.9 attached. Scoring our
candidate over all 68.9 and taking the max would hand it ~12x the evidence the
incumbent had and inflate every "supported" call -- a rigged comparison that
would have looked like a win.

The row records `chunks_checked` as a COUNT, never the SET, so the artifact
cannot reproduce its own verdict (contra `faithfulness.rs:15-22`, which calls
each tuple self-contained). We therefore RECONSTRUCT the procedure: re-embed the
member texts with the same embedding model, rank, cap, and replay the early
exit. That is reproducible but it is a reconstruction, and rows record
`ranking_reconstructed: true` so no reader mistakes it for the incumbent's own
chunk set. The durable fix is for the lane to emit `judged_chunk_ids`.

The ranking signal is `margin`, never `p_grounded` -- p_grounded saturates to
exactly 1.0 whenever the losing branch leaves the top-k window, which ties items
together and caps AUC mechanically (see `branch_prob`'s docstring). That defect
manufactured a declining learning curve once already.

Usage:
  substitution_study.py --faith runs/faith.jsonl --out runs/substitution.jsonl \
      --base-url http://127.0.0.1:8090/v1 --model rung-1000
"""
import argparse
import json
import math
import os
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eval_grounding import (  # noqa: E402  -- one decider per protocol (ARCH §10.6)
    ANSWER_GBNF,
    branch_prob,
    build_prompt,
    chat,
    parse_verdict_tolerant,
)


def embed(base_url, model, texts, timeout=600):
    """Batch-embed via the daemon's OpenAI-compatible endpoint."""
    import urllib.request
    body = json.dumps({"model": model, "input": texts}).encode()
    req = urllib.request.Request(f"{base_url}/embeddings", data=body,
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        out = json.load(r)
    rows = sorted(out["data"], key=lambda d: d.get("index", 0))
    return [d["embedding"] for d in rows]


def cosine(a, b):
    num = sum(x * y for x, y in zip(a, b))
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(x * x for x in b))
    return num / (na * nb) if na and nb else float("-inf")


def rank_chunks(base_url, model, claim, chunks):
    """Reproduce the incumbent's pre-cap ordering: cosine(claim, chunk), desc.

    Returns (ordered_indices, reconstructed). On any embedding failure we fall
    back to DOCUMENT ORDER -- which is also the incumbent's own fallback
    (`n_rank_fallback`) -- and say so per row rather than silently scoring a
    different chunk set (ARCH §18.3).
    """
    try:
        vecs = embed(base_url, model, [claim] + list(chunks))
    except Exception:
        return list(range(len(chunks))), False
    if len(vecs) != len(chunks) + 1:
        return list(range(len(chunks))), False
    cv, cvs = vecs[0], vecs[1:]
    scored = sorted(range(len(chunks)), key=lambda i: -cosine(cv, cvs[i]))
    return scored, True


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--faith", required=True,
                    help="`svrn bench faithfulness run` JSONL artifact")
    ap.add_argument("--out", required=True)
    ap.add_argument("--base-url", default="http://127.0.0.1:8090/v1")
    ap.add_argument("--model", default="rung-1000")
    ap.add_argument("--max-tokens", type=int, default=512)
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("--logprobs", type=int, default=20)
    ap.add_argument("--concurrency", type=int, default=4)
    ap.add_argument("--limit", type=int, default=0,
                    help="cap claims scored (smoke runs); 0 = all")
    # --- the incumbent's procedure, mirrored. Defaults are ITS constants
    # (faithfulness.rs:55-57); changing one makes the comparison incomparable.
    ap.add_argument("--chunk-cap", type=int, default=12,
                    help="faithfulness.rs CHUNK_CAP")
    ap.add_argument("--early-exit", type=float, default=0.95,
                    help="faithfulness.rs EARLY_EXIT_SUPPORT")
    ap.add_argument("--supported-tau", type=float, default=0.5,
                    help="faithfulness.rs SUPPORTED_TAU")
    ap.add_argument("--embed-url", default="http://127.0.0.1:9741/v1",
                    help="daemon endpoint used to reconstruct the cosine "
                         "ranking the incumbent applied before the cap")
    ap.add_argument("--embed-model", default="commonwealth/embed")
    ap.add_argument("--max-evidence-chars", type=int, default=24000,
                    help="truncate a single chunk before prompting. The gate "
                         "feeds whole chunks; a chunk over this is a corpus "
                         "outlier, and truncating is recorded per row rather "
                         "than silently dropping the claim (ARCH §18.3).")
    args = ap.parse_args()

    # no-think + grammar: the SAME protocol rung-1000 was measured under in the
    # head-to-head (`score_checkpoint.sh` PROTOCOL=nothink-grammar). Changing it
    # here would make this run incomparable with every committed number.
    sampling = {
        "logprobs": True,
        "top_logprobs": args.logprobs,
        "chat_template_kwargs": {"enable_thinking": False},
        "grammar": ANSWER_GBNF,
    }

    rows = [json.loads(l) for l in open(args.faith)]
    if args.limit:
        rows = rows[: args.limit]
    done = set()
    if os.path.exists(args.out):
        with open(args.out) as f:
            for line in f:
                try:
                    done.add(json.loads(line)["id"])
                except json.JSONDecodeError:
                    pass
    todo = [r for r in rows if r["id"] not in done]
    print(f"claims: {len(rows)} total, {len(todo)} to score "
          f"({len(done)} already done)")

    lock = threading.Lock()
    out_f = open(args.out, "a")
    stats = {"done": 0, "errors": 0, "unparseable": 0, "t0": time.time(),
             "chunk_calls": 0, "chunk_seconds": 0.0}

    def work(row):
        all_chunks = row.get("evidence_chunks") or []
        order, reconstructed = rank_chunks(args.embed_url, args.embed_model,
                                           row["claim"], all_chunks)
        # rank -> cap -> judge in order -> break on strong support. Exactly the
        # incumbent's loop (faithfulness.rs:440-457).
        order = order[: args.chunk_cap]
        per_chunk = []
        best_p = 0.0
        for i in order:
            chunk = all_chunks[i]
            truncated = len(chunk) > args.max_evidence_chars
            doc = chunk[: args.max_evidence_chars]
            t0 = time.time()
            try:
                text, usage = chat(args.base_url, args.model,
                                   build_prompt(doc, row["claim"]),
                                   args.max_tokens, args.timeout, sampling)
            except Exception as e:  # record, never drop the claim
                with lock:
                    stats["errors"] += 1
                per_chunk.append({"chunk_index": i, "error": str(e)[:200]})
                continue
            dt = time.time() - t0
            pred, _cls, _how = parse_verdict_tolerant(text)
            p_g, dec_tok, n_cand, margin = branch_prob(usage.get("_logprobs"))
            with lock:
                stats["chunk_calls"] += 1
                stats["chunk_seconds"] += dt
            per_chunk.append({
                "chunk_index": i,
                "chunk_id": (row.get("evidence_chunk_ids") or [None] * len(chunks))[i],
                "pred": pred,
                "margin": margin,
                "p_grounded": p_g,
                "branch_candidates": n_cand,
                "decision_token": dec_tok,
                "completion_tokens": usage.get("completion_tokens"),
                "seconds": round(dt, 3),
                "evidence_truncated": truncated,
            })
            if p_g is not None:
                best_p = max(best_p, p_g)
                if best_p >= args.early_exit:
                    break  # EARLY_EXIT_SUPPORT — the incumbent stops here too

        scored = [c for c in per_chunk if c.get("margin") is not None]
        # Mirror `claim_chunk_support`: the claim's support is the BEST chunk's.
        # Ranked on MARGIN (p_grounded saturates and ties); the argmax VERDICT
        # below uses p_grounded against the incumbent's own tau, because that is
        # the decision rule being substituted for.
        best = max(scored, key=lambda c: c["margin"]) if scored else None
        rec = {
            "id": row["id"],
            "corpus_id": row.get("corpus_id"),
            "claim": row["claim"],
            "node_id": row.get("node_id"),
            "level": row.get("level"),
            "chunks_available": len(all_chunks),
            "chunks_scored": len(scored),
            "ranking_reconstructed": reconstructed,
            # --- incumbent (the shipped 35B critic, via production registers)
            "incumbent_verdict": row.get("verdict"),
            "incumbent_max_support": row.get("max_support"),
            "incumbent_chunks_checked": row.get("chunks_checked"),
            "incumbent_model": row.get("judge_model"),
            # --- candidate (our checkpoint), aggregated the incumbent's way
            "verifier_margin": best["margin"] if best else None,
            "verifier_p_grounded": best["p_grounded"] if best else None,
            "verifier_pred": best["pred"] if best else None,
            # The substitution verdict: the INCUMBENT's decision rule
            # (max_support >= SUPPORTED_TAU) applied to our support estimate.
            # This, not the raw argmax, is what a judge-slot swap would ship.
            "verifier_max_support": round(best_p, 4) if scored else None,
            "verifier_verdict": ("supported" if best_p >= args.supported_tau
                                 else "unsupported") if scored else None,
            "verifier_best_chunk": best["chunk_index"] if best else None,
            "verifier_seconds_total": round(
                sum(c.get("seconds", 0.0) for c in per_chunk), 3),
            "per_chunk": per_chunk,
        }
        with lock:
            if best is None:
                stats["unparseable"] += 1
            stats["done"] += 1
            out_f.write(json.dumps(rec, ensure_ascii=False) + "\n")
            out_f.flush()
            n = stats["done"]
            if n % 25 == 0 or n == len(todo):
                el = time.time() - stats["t0"]
                print(f"  {n}/{len(todo)} claims  {el:.0f}s  "
                      f"({stats['chunk_calls']} chunk-judgements, "
                      f"{stats['chunk_seconds'] / max(1, stats['chunk_calls']):.2f}s each)  "
                      f"unparseable {stats['unparseable']}  errors {stats['errors']}",
                      flush=True)

    with ThreadPoolExecutor(max_workers=args.concurrency) as ex:
        list(ex.map(work, todo))
    out_f.close()

    el = time.time() - stats["t0"]
    print(f"\ndone: {stats['done']} claims in {el:.0f}s")
    print(f"  chunk-judgements {stats['chunk_calls']}, "
          f"mean {stats['chunk_seconds'] / max(1, stats['chunk_calls']):.3f}s each "
          f"(wall/claim {el / max(1, stats['done']):.3f}s at concurrency {args.concurrency})")
    if stats["unparseable"]:
        print(f"  WARNING: {stats['unparseable']} claims produced no scorable "
              f"chunk — they are RECORDED as null, not dropped. Any agreement "
              f"rate must say how it treated them.")
    if stats["errors"]:
        print(f"  WARNING: {stats['errors']} transport errors")
    print(f"  -> {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
