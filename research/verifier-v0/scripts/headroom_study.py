#!/usr/bin/env python3
"""Step 1 of the quality arc: measure the INCUMBENT's blind spots directly.

The gate's shipped fabrication number (0.00) is measured BY the incumbent
judge, so it structurally cannot show the incumbent's own misses (§18.1: a
guard asserting on a field the subject supplies). The Stream B teacher-label
discards gave the first subject-independent reading — the 35B under the
HalluGuard register misses ~11% of constructed entity swaps and ~9% of
negation flips — but that was a different prompt register from the one the
production gate actually runs. This script closes that gap: it scores the
incumbent under its EXACT production procedure, and our checkpoint under its
own native protocol, on the SAME held-out constructed cases whose labels are
mechanical (labels-by-construction — the only referee neither model controls).

INCUMBENT SIDE — the production register, not a paraphrase of it:
  - prompt + passage cap 2,400 chars: `grounding/judge.rs:366-385`
    (`claim_chunk_support`), transcribed verbatim below and cited; a Python
    copy is unavoidable across the language boundary, same as the procedure
    constants in substitution_study.py.
  - forced-choice A/B: daemon `/v1/chat/completions` with the
    `x_forced_choice` structured-output sentinel — verified live to return
    the {"A": p, "B": p} map (`judge.rs:40-87` builds the same request
    in-process). model="primary" resolves to the shipped critic.
  - per-claim loop: cosine-rank chunks, cap 12, early-exit at 0.95, verdict
    = max_support >= 0.5 (`faithfulness.rs:55-57`; ranking reconstructed via
    the daemon's own embed model, as in substitution_study.py).

OUR SIDE — the checkpoint's native protocol, unchanged from every committed
number: build_prompt + no-think + grammar + margin from logprobs, max-tokens
16 (validated 40/40 verdict-identical vs uncapped).

Cases: `svrn bench verifier export` rows (id, kind, label, claim,
evidence_chunks, spans, witness). Evidence held fixed for both sides.
Resumable by case id. One row out per case with both sides' detail.

Usage:
  headroom_study.py --cases data/heldout-sep/bank.jsonl --out runs/headroom/scored.jsonl \
      --our-url http://127.0.0.1:8089/v1 --our-model rung-1000
"""
import argparse
import json
import os
import sys
import threading
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from eval_grounding import (  # noqa: E402  — one decider per protocol (§10.6)
    ANSWER_GBNF,
    branch_prob,
    build_prompt,
    chat,
    parse_verdict_tolerant,
)
from substitution_study import rank_chunks  # noqa: E402  — same reconstruction

# `grounding/judge.rs:373-381`, verbatim. Do not edit without editing the
# source of truth first — this copy exists only because the register lives
# across a language boundary.
INCUMBENT_PROMPT = (
    'PASSAGE:\n"""\n{passage}\n"""\n\n'
    "CLAIM: {claim}\n\n"
    "Does the passage state or clearly imply this claim? Paraphrase counts; "
    "the passage merely mentioning the people or things involved, without "
    "establishing the claimed connection between them, does NOT count.\n\n"
    "Answer with exactly one letter — A = the passage supports the claim, "
    "B = it does not."
)
INCUMBENT_SYSTEM = "You are a careful classifier. Answer with a single letter."
PASSAGE_CAP = 2_400  # judge.rs:372


def incumbent_support(daemon_url, model, passage, claim, timeout):
    """One production support probe. Returns (support, p_a, p_b) or None."""
    prompt = INCUMBENT_PROMPT.format(passage=passage[:PASSAGE_CAP], claim=claim)
    body = json.dumps({
        "model": model,
        # Pin the judgement to THIS box's primary. `model: "primary"` alone is
        # the load-balancing path SLOT_POLICY §7 flagged — a peer's "primary"
        # (possibly a different quant) could silently serve the call, which
        # would make the incumbent side of this study a different instrument
        # per row (§18.4). LocalOnly is also what the production gate sends.
        "oicp": {"oicp_version": "0.4.0",
                 "privacy": {"sharding": "local_only"}},
        "messages": [
            {"role": "system", "content": INCUMBENT_SYSTEM},
            {"role": "user", "content": prompt},
        ],
        "max_tokens": 1,
        "temperature": 0.0,
        "think_budget": 0,
        "chat_template_kwargs": {"enable_thinking": False},
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "fc",
                "schema": {"type": "string", "enum": ["A", "B"],
                           "x_forced_choice": True},
                "strict": True,
            },
        },
    }).encode()
    req = urllib.request.Request(
        f"{daemon_url}/chat/completions", data=body,
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        out = json.load(r)
    content = out["choices"][0]["message"]["content"]
    m = json.loads(content)
    a, b = float(m.get("A", 0.0)), float(m.get("B", 0.0))
    denom = a + b
    return (a / denom if denom > 0 else 0.0), a, b


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", required=True,
                    help="`svrn bench verifier export` JSONL")
    ap.add_argument("--out", required=True)
    ap.add_argument("--daemon-url", default="http://127.0.0.1:9741/v1")
    ap.add_argument("--incumbent-model", default="primary")
    ap.add_argument("--our-url", default="http://127.0.0.1:8089/v1")
    ap.add_argument("--our-model", default="rung-1000")
    ap.add_argument("--our-max-tokens", type=int, default=16)
    ap.add_argument("--logprobs", type=int, default=20)
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("--concurrency", type=int, default=3,
                    help="modest: the incumbent side rides the daemon's "
                         "primary slot")
    ap.add_argument("--limit", type=int, default=0, help="0 = all")
    # the incumbent's procedure constants (faithfulness.rs:55-57)
    ap.add_argument("--chunk-cap", type=int, default=12)
    ap.add_argument("--early-exit", type=float, default=0.95)
    ap.add_argument("--supported-tau", type=float, default=0.5)
    ap.add_argument("--embed-url", default="http://127.0.0.1:9741/v1")
    ap.add_argument("--embed-model", default="commonwealth/embed")
    args = ap.parse_args()

    sampling = {
        "logprobs": True,
        "top_logprobs": args.logprobs,
        "chat_template_kwargs": {"enable_thinking": False},
        "grammar": ANSWER_GBNF,
    }

    rows = [json.loads(l) for l in open(args.cases)]
    if args.limit:
        rows = rows[: args.limit]
    done = set()
    if os.path.exists(args.out):
        with open(args.out) as f:
            for line in f:
                try:
                    r = json.loads(line)
                    # a row whose incumbent or our side never produced a
                    # verdict was starved by transient contention, not scored
                    # — leave it in `todo` so a resume retries it (§18.3: an
                    # Err must not become a success-shaped tombstone)
                    if r["incumbent_verdict"] is not None \
                            and r["our_verdict"] is not None:
                        done.add(r["id"])
                except json.JSONDecodeError:
                    pass
    todo = [r for r in rows if r["id"] not in done]
    print(f"cases: {len(rows)} total, {len(todo)} to score ({len(done)} done)")

    lock = threading.Lock()
    out_f = open(args.out, "a")
    stats = {"done": 0, "inc_errors": 0, "our_errors": 0, "t0": time.time()}

    def work(row):
        chunks = row.get("evidence_chunks") or []
        order, reconstructed = rank_chunks(
            args.embed_url, args.embed_model, row["claim"], chunks)
        order = order[: args.chunk_cap]

        # ── incumbent, production procedure ──────────────────────────
        inc_chunks, inc_max = [], 0.0
        for i in order:
            t0 = time.time()
            try:
                support, pa, pb = incumbent_support(
                    args.daemon_url, args.incumbent_model,
                    chunks[i], row["claim"], args.timeout)
            except Exception as e:
                with lock:
                    stats["inc_errors"] += 1
                inc_chunks.append({"chunk_index": i, "error": str(e)[:200]})
                continue
            inc_chunks.append({"chunk_index": i, "support": round(support, 6),
                               "seconds": round(time.time() - t0, 3)})
            inc_max = max(inc_max, support)
            if inc_max >= args.early_exit:
                break
        inc_scored = [c for c in inc_chunks if "support" in c]
        inc_verdict = (("supported" if inc_max >= args.supported_tau
                        else "unsupported") if inc_scored else None)

        # ── ours, native protocol (substitution_study's loop) ────────
        our_chunks, best_p, best_margin = [], 0.0, None
        for i in order:
            t0 = time.time()
            try:
                text, usage = chat(args.our_url, args.our_model,
                                   build_prompt(chunks[i], row["claim"]),
                                   args.our_max_tokens, args.timeout, sampling)
            except Exception as e:
                with lock:
                    stats["our_errors"] += 1
                our_chunks.append({"chunk_index": i, "error": str(e)[:200]})
                continue
            pred, _cls, _how = parse_verdict_tolerant(text)
            p_g, _tok, n_cand, margin = branch_prob(usage.get("_logprobs"))
            our_chunks.append({
                "chunk_index": i, "pred": pred, "margin": margin,
                "p_grounded": p_g, "branch_candidates": n_cand,
                "seconds": round(time.time() - t0, 3),
            })
            if margin is not None and (best_margin is None or margin > best_margin):
                best_margin = margin
            if p_g is not None:
                best_p = max(best_p, p_g)
                if best_p >= args.early_exit:
                    break
        our_scored = [c for c in our_chunks if c.get("margin") is not None]
        our_verdict = (("supported" if best_p >= args.supported_tau
                        else "unsupported") if our_scored else None)

        rec = {
            "id": row["id"],
            "kind": row["kind"],
            "label": row["label"],           # ground truth, by construction
            "corpus_id": row.get("corpus_id"),
            "claim": row["claim"],
            "spans": row.get("spans"),
            "chunks_available": len(chunks),
            "ranking_reconstructed": reconstructed,
            "incumbent_verdict": inc_verdict,
            "incumbent_max_support": round(inc_max, 6) if inc_scored else None,
            "incumbent_chunks_checked": len(inc_scored),
            "incumbent_per_chunk": inc_chunks,
            "our_verdict": our_verdict,
            "our_max_p": round(best_p, 6) if our_scored else None,
            "our_margin": best_margin,
            "our_chunks_checked": len(our_scored),
            "our_per_chunk": our_chunks,
        }
        with lock:
            stats["done"] += 1
            out_f.write(json.dumps(rec, ensure_ascii=False) + "\n")
            out_f.flush()
            if stats["done"] % 25 == 0:
                dt = time.time() - stats["t0"]
                print(f"  {stats['done']}/{len(todo)} "
                      f"({dt/stats['done']:.1f}s/case, "
                      f"inc_err={stats['inc_errors']} "
                      f"our_err={stats['our_errors']})", flush=True)

    with ThreadPoolExecutor(max_workers=args.concurrency) as ex:
        list(ex.map(work, todo))

    print(json.dumps({k: v for k, v in stats.items() if k != "t0"}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
