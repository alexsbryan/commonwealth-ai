#!/usr/bin/env python3
"""Stage 2 of the delta measurement: re-judge the revisions with rung-1000 and
compute delta.

WHY EVERYTHING IS MEASURED IN ONE RUN. The recorded `our_margin` in
runs/headroom/scored.jsonl is a max over a chunk PREFIX -- headroom_study
early-exits at best_p >= 0.95, which fired on 901/2510 rows -- over a
cosine-reconstructed chunk order. Transferring its stored tau to a fresh run
would compare margins produced by different chunk sets. So tau is DERIVED
IN-RUN from a fresh calibration sample of grounded claims, and every number
below comes from one server, one protocol, one procedure.

The judging procedure is headroom_study.py's, reproduced exactly and by import
(§10.6, one decider per protocol): rank_chunks -> cap 12 -> per-chunk
build_prompt -> branch_prob margin -> best over chunks, early exit at 0.95.

SETS JUDGED
  revisions   (193) the model-revised false alarms -- the thing under test
  fa_original (193) their originals, paired -- the margin shift, and a
                    diagnostic (not a gate) against the recorded values
  cal_grounded(250) random grounded non-multi_hop -- DERIVES tau in-run
  cal_ungrnd  (100) random ungrounded non-multi_hop -- in-run catch, so the
                    operating point is characterised rather than assumed

delta = P(verifier flags the revision | the revision is mechanically damaged).
"""
import argparse, json, random, statistics, sys, threading
import concurrent.futures as cf
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT/"scripts"))
from eval_grounding import ANSWER_GBNF, branch_prob, build_prompt, chat  # one decider
from substitution_study import rank_chunks

MH = "multi_hop_conjunction"
SAMPLING = {"logprobs": True, "top_logprobs": 10,
            "chat_template_kwargs": {"enable_thinking": False}, "grammar": ANSWER_GBNF}

def score_one(claim, chunks, judge_url, judge_model, embed_url, embed_model,
              cap=12, early=0.95, timeout=180):
    """headroom_study's loop, verbatim in structure."""
    order, reconstructed = rank_chunks(embed_url, embed_model, claim, chunks)
    order = order[:cap]
    best_p, best_margin, checked, errs = 0.0, None, 0, 0
    for i in order:
        try:
            text, usage = chat(judge_url, judge_model,
                               build_prompt(chunks[i], claim), 220, timeout, SAMPLING)
        except Exception:
            errs += 1
            continue
        p_g, _tok, _n, margin = branch_prob(usage.get("_logprobs"))
        if margin is None:
            continue
        checked += 1
        if best_margin is None or margin > best_margin:
            best_margin = margin
        if p_g is not None:
            best_p = max(best_p, p_g)
            if best_p >= early:
                break
    return {"margin": best_margin, "max_p": best_p, "chunks_checked": checked,
            "errors": errs, "ranking_reconstructed": reconstructed}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--revisions", default=str(ROOT/"runs/delta/revisions_fa20.jsonl"))
    ap.add_argument("--judge-url", default="http://127.0.0.1:8089/v1")
    ap.add_argument("--judge-model", default="rung-1000")
    ap.add_argument("--embed-url", default="http://127.0.0.1:9741/v1")
    ap.add_argument("--embed-model", default="Qwen3-Embedding-0.6B-Q8_0")
    ap.add_argument("--fa", type=float, default=0.20)
    ap.add_argument("--n-cal-grounded", type=int, default=250)
    ap.add_argument("--n-cal-ungrounded", type=int, default=100)
    ap.add_argument("--concurrency", type=int, default=4)
    ap.add_argument("--seed", type=int, default=17)
    ap.add_argument("--out", default=str(ROOT/"runs/delta/judged_fa20.jsonl"))
    a = ap.parse_args()

    sc = {json.loads(l)["id"]: json.loads(l) for l in open(ROOT/"runs/headroom/scored.jsonl") if l.strip()}
    bk = {json.loads(l)["id"]: json.loads(l) for l in open(ROOT/"data/heldout-sep/bank.jsonl") if l.strip()}
    revs = [json.loads(l) for l in open(a.revisions) if l.strip()]
    revs = [r for r in revs if r.get("revised")]
    rng = random.Random(a.seed)
    fa_ids = {r["id"] for r in revs}
    gpool = [i for i in sc if sc[i]["label"] == "grounded" and sc[i]["kind"] != MH]
    upool = [i for i in sc if sc[i]["label"] == "ungrounded" and sc[i]["kind"] != MH]
    cal_g = rng.sample(gpool, min(a.n_cal_grounded, len(gpool)))
    cal_u = rng.sample(upool, min(a.n_cal_ungrounded, len(upool)))

    jobs = []
    for r in revs:
        jobs.append(("revision", r["id"], r["revised"], r["evidence_chunks"]))
        jobs.append(("fa_original", r["id"], r["claim"], r["evidence_chunks"]))
    for i in cal_g:
        jobs.append(("cal_grounded", i, bk[i]["claim"], bk[i]["evidence_chunks"]))
    for i in cal_u:
        jobs.append(("cal_ungrounded", i, bk[i]["claim"], bk[i]["evidence_chunks"]))
    print(f"judging {len(jobs)} items "
          f"({len(revs)} revisions + {len(revs)} originals + {len(cal_g)}g + {len(cal_u)}u)", flush=True)

    lock, done = threading.Lock(), [0]
    def work(j):
        kind, iid, claim, chunks = j
        s = score_one(claim, chunks, a.judge_url, a.judge_model, a.embed_url, a.embed_model)
        with lock:
            done[0] += 1
            if done[0] % 100 == 0:
                print(f"  {done[0]}/{len(jobs)}", flush=True)
        return {"set": kind, "id": iid, "claim": claim, **s}

    Path(a.out).parent.mkdir(parents=True, exist_ok=True)
    with open(a.out, "w") as f, cf.ThreadPoolExecutor(max_workers=a.concurrency) as ex:
        for rec in ex.map(work, jobs):
            f.write(json.dumps(rec) + "\n")
            f.flush()
    print(f"wrote {a.out}", flush=True)

if __name__ == "__main__":
    sys.exit(main())
