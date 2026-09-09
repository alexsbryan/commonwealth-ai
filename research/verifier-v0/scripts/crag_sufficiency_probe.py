#!/usr/bin/env python3
"""Can a CHEAP evaluator tell that a retrieved window is insufficient -- and is
routing on it worth more than just raising k?

§14 established retrieval is ~2-3% of a turn and §15 that context is the scarce
resource, which together argue for the CRAG shape: grade the retrieved set with
something cheap, escalate only when it is judged insufficient, and synthesise
once. That whole architecture rests on ONE unmeasured assumption -- that
sufficiency is PREDICTABLE without the answer key. If it is not, there is no
signal to route on and the design collapses. This measures exactly that and
nothing else.

CHEAPEST POSSIBLE, by construction
  - ONE retrieval pass per question at k=80. Every window (k=5..80) is a PREFIX
    of it, so the k-sweep costs zero extra retrievals.
  - Ground truth is FREE: the bench's own matcher over the bank's expected
    facts (score.rs::partition_facts, reproduced).
  - FREE PREDICTORS ARE TESTED FIRST. If retrieval-score statistics already
    predict sufficiency, no evaluator model is needed at all -- and this arc has
    landed on "the boring lever wins" three times (§8.1 threshold>jury,
    §11 bound>ladder, §14 k>decomposition), so it is the hypothesis to beat.
  - The model evaluator is ONE constrained token on the resident 4B. The daemon
    returns no logprobs (recorded dead end), so the verdict is binary and
    UNCALIBRATED -- a limitation, stated, not worked around.

PRE-REGISTERED BARS (written before any call), and each has a PAIRED CONTROL
because two bars in this arc were mis-specified for want of one (§14, §15).

  B1 -- IS THERE A SIGNAL AT ALL? Rank questions by predicted sufficiency and
    measure AUC against the ground-truth binary (window covers ALL expected
    facts). Control: 0.50, chance.
      best predictor AUC >= 0.70 -> routable signal exists.
      <= 0.55 -> no signal; CRAG-style routing is DEAD on this bank and the
                 pipeline should not be built. Report and stop.
  B2 -- IS THE MODEL WORTH ITS CALL? The 4B evaluator's AUC minus the best FREE
    predictor's AUC.
      >= +0.10 -> the model earns its keep.
      <= +0.03 -> free score statistics are as good; use them, skip the model.
    Reported in both directions (§18.6).
  B3 -- THE PRODUCT BAR, and the only one that decides a build. Simulate the
    policy: start at k=28, escalate to k=80 only when the evaluator says
    insufficient. Against two controls -- fixed k=28 and fixed k=80:
      adaptive must capture >= 90% of the coverage gain (k28 -> k80) at
      <= 50% of the extra context cost.
      Otherwise fixed-k is simpler and better, and simpler wins by default.

  PREDICTION: I expect a signal (B1 passes) and I expect the free predictors to
  be weak, because max-similarity says how well the top chunk matches the
  QUESTION, and §13/§14 showed the misses are terms the question never contains
  -- exactly the case a similarity score cannot see. Recorded so it can be wrong.
"""
import json, re, subprocess, time, urllib.request
from pathlib import Path

D = Path("/home/alexbryan/dev/commonwealth-ai/sovereign/bench/sep_atlas/map-conversion-rung6")
POOL_K, BASE_K, ESC_K = 80, 28, 80
GEN = "http://127.0.0.1:9741/v1/chat/completions"
MODEL = "Qwen3.5-4B-UD-MTP-Q6_K_XL"

def toks(f): return [t for t in f.lower().replace("-", " ").split() if len(t) >= 3]
def hit(f, hay): return all(t in hay.lower() for t in toks(f))

def retrieve(q, k=POOL_K):
    p = subprocess.run(["sovereign", "corpus", "search", "sep", q, "--limit", str(k)],
                       capture_output=True, text=True, timeout=300)
    out = p.stdout or ""
    parts = re.split(r"^\s*\d+\.\s+\[", out, flags=re.M)[1:]
    hits = []
    for x in parts:
        m = re.match(r"([0-9.]+)\]\s*(.*)", x.strip(), re.S)
        hits.append((float(m.group(1)) if m else 0.0, x.strip()))
    return hits

def evaluate(question, window):
    """The CRAG evaluator, cheapest form: one constrained token from the 4B."""
    ctx = "\n---\n".join(t for _, t in window)[:12000]
    body = {"model": MODEL, "temperature": 0.0, "max_tokens": 4,
            "chat_template_kwargs": {"enable_thinking": False},
            "messages": [{"role": "user", "content":
                f"Passages:\n{ctx}\n\nQuestion: {question}\n\n"
                "Do these passages contain everything needed to answer fully? "
                "Reply with one word: SUFFICIENT or INSUFFICIENT."}]}
    req = urllib.request.Request(GEN, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=180) as r:
        t = json.load(r)["choices"][0]["message"]["content"].strip().upper()
    return 0.0 if "INSUF" in t else 1.0        # low score = predicted insufficient

def auc(scores, labels):
    """Rank AUC; ties get 0.5 credit. labels: 1 = sufficient."""
    pos = [s for s, l in zip(scores, labels) if l == 1]
    neg = [s for s, l in zip(scores, labels) if l == 0]
    if not pos or not neg: return None
    tot = sum((1.0 if p > n else 0.5 if p == n else 0.0) for p in pos for n in neg)
    return tot / (len(pos) * len(neg))

R = {r["question_id"]: r for r in json.load(open(D / "armA.json"))["results"]}
QS = sorted(R)
RESUME = D / "crag_phase1.json"

rows, pools, wall = [], {}, 0.0
if RESUME.exists():
    _c = json.loads(RESUME.read_text())
    rows = _c["rows"]; pools = {k: [(s_, t) for s_, t in v] for k, v in _c["pools"].items()}
    print(f"resumed phase 1 from {RESUME.name} ({len(rows)} questions, 0 retrievals)")
for i, q in enumerate(QS if not rows else [], 1):
    rec = R[q]
    facts = ([x.strip() for x in rec["fact_score"]["matched"]] +
             [x.strip() for x in rec["fact_score"]["missing"]])
    t0 = time.time(); pool = retrieve(rec["question"]); wall += time.time() - t0
    base = pool[:BASE_K]
    hay_b = "\n".join(t for _, t in base)
    hay_f = "\n".join(t for _, t in pool)
    cov_b = sum(1 for f in facts if hit(f, hay_b))
    cov_f = sum(1 for f in facts if hit(f, hay_f))
    sc = [s for s, _ in base]
    pools[q] = base                      # reused by the evaluator; no second retrieval
    rows.append({
        "q": q, "question": rec["question"], "n_facts": len(facts),
        "cov_base": cov_b, "cov_full": cov_f,
        "sufficient": int(cov_b == len(facts)),
        # FREE predictors, zero inference
        "f_max":   max(sc) if sc else 0.0,
        "f_mean5": sum(sorted(sc, reverse=True)[:5]) / min(5, len(sc)) if sc else 0.0,
        "f_gap":   (max(sc) - sorted(sc, reverse=True)[min(9, len(sc)-1)]) if len(sc) > 1 else 0.0,
        "f_titles": len({t.split()[0] for _, t in base}),
        "chars_base": len(hay_b), "chars_full": len(hay_f),
    })
    print(f"[{i}/{len(QS)}] {q}  cov {cov_b}/{len(facts)} -> {cov_f}/{len(facts)}", flush=True)

CKPT = D / "crag_phase1.json"
CKPT.write_text(json.dumps({"rows": rows,
                            "pools": {k: [[s_, t] for s_, t in v] for k, v in pools.items()}}))
print(f"\nphase 1 checkpointed -> {CKPT.name}", flush=True)

print("evaluator (one 4B token per question)...", flush=True)
for r in rows:
    try:
        r["f_model"] = evaluate(r["question"], pools[r["q"]])
    except Exception as e:                  # never silently a verdict (§18.3)
        print(f"  EVALUATOR FAILED on {r['q']}: {str(e)[:80]}", flush=True)
        r["f_model"] = None
n_bad = sum(1 for r in rows if r["f_model"] is None)
if n_bad:
    print(f"  {n_bad}/{len(rows)} unscorable — dropped from B1/B2/B3, never counted as a verdict")
rows = [r for r in rows if r["f_model"] is not None]
labels_all = [r["sufficient"] for r in rows]

Path(D / "crag_sufficiency.json").write_text(json.dumps(rows, indent=1))
labels = [r["sufficient"] for r in rows]
print(f"\nground truth: {sum(labels)}/{len(labels)} windows fully sufficient at k={BASE_K}")

print("\n=== B1/B2 — is sufficiency predictable? ===")
print(f"  {'predictor':<12} {'AUC':>6}")
best_free, best_free_name = 0.0, None
for name in ("f_max", "f_mean5", "f_gap", "f_titles"):
    a = auc([r[name] for r in rows], labels)
    print(f"  {name:<12} {a if a is None else round(a,3):>6}")
    if a and a > best_free: best_free, best_free_name = a, name
a_model = auc([r["f_model"] for r in rows], labels)
print(f"  {'f_model (4B)':<12} {a_model if a_model is None else round(a_model,3):>6}")
print(f"\n  best FREE: {best_free_name} = {best_free:.3f}   (bar: >=0.70 signal, <=0.55 dead)")
if a_model is not None:
    print(f"  model lift over free: {a_model-best_free:+.3f}   "
          f"(bar: >=+0.10 earns its call, <=+0.03 skip the model)")

print("\n=== B3 — the product bar: adaptive vs fixed-k ===")
tot_f = sum(r["n_facts"] for r in rows)
c28 = sum(r["cov_base"] for r in rows); c80 = sum(r["cov_full"] for r in rows)
ch28 = sum(r["chars_base"] for r in rows); ch80 = sum(r["chars_full"] for r in rows)
esc = [r for r in rows if r["f_model"] == 0.0]
c_ad = sum((r["cov_full"] if r["f_model"] == 0.0 else r["cov_base"]) for r in rows)
ch_ad = sum((r["chars_full"] if r["f_model"] == 0.0 else r["chars_base"]) for r in rows)
print(f"  fixed k={BASE_K}   coverage {c28}/{tot_f} = {c28/tot_f:.1%}   context {ch28:,}")
print(f"  fixed k={ESC_K}   coverage {c80}/{tot_f} = {c80/tot_f:.1%}   context {ch80:,}")
print(f"  adaptive     coverage {c_ad}/{tot_f} = {c_ad/tot_f:.1%}   context {ch_ad:,}"
      f"   (escalated {len(esc)}/{len(rows)})")
if c80 > c28:
    gain = (c_ad - c28) / (c80 - c28); cost = (ch_ad - ch28) / (ch80 - ch28) if ch80 > ch28 else 0
    print(f"\n  coverage gain captured {gain:.1%}  (bar >= 90%)")
    print(f"  extra context spent    {cost:.1%}  (bar <= 50%)")
    print(f"  VERDICT: {'BUILD — adaptive beats fixed-k' if gain>=0.9 and cost<=0.5 else 'NO — fixed-k is simpler and not worse'}")
else:
    print("  k=80 gains nothing over k=28 on this bank; B3 is unscorable.")
