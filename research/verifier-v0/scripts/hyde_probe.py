#!/usr/bin/env python3
"""HyDE: does querying with a HYPOTHETICAL ANSWER recover what the question cannot?

The measured failure mode (§13, §14): the facts no retrieval configuration
reaches are terms the QUESTION never contains -- MacCallum, descriptivism,
Worrall, BonJour. §13's oracle (the fact as the query) recovers 8/8, proving
reachability. §14's question-decomposition recovers none of it, because
sub-queries stay inside the question's vocabulary. HyDE / query2doc is the
untested member of that family and the only one that supplies the ANSWER's
vocabulary from the question alone -- which is exactly the gap. SEP already has
an FTS index (chunks.lance/_indices, two inverted + one IVF) and search.rs:336
is hybrid, so a generated passage carrying rare proper nouns feeds BOTH the
dense and the lexical side.

ARMS -- one instrument (`corpus search` on sep), one matcher
(score.rs::partition_facts), one truncated-snippet haystack, identically biased:
  baseline    the question as the query                      (PAIRED CONTROL)
  hyde        a generated hypothetical passage as the query
  hyde_concat question + generated passage                   (query2doc shape)
Every arm draws the same k=80 pool, so every window is a prefix and the sweep is
free. §13's oracle (8/8) is the standing ceiling.

The generation prompt is the canonical HyDE shape -- "write a passage that
answers the question" -- deliberately WITHOUT any nudge toward names or
technical terms. Asking for proper nouns would be teaching to the test when the
answer key is proper nouns.

PRE-REGISTERED BARS (before any generation). Every one carries a paired control,
because two bars in this arc were mis-specified for want of one (§14, §15).
  B1 PRIMARY -- hard core at k=28, HyDE minus BASELINE (not minus zero):
      >= +2 of 8 -> live; the answer's vocabulary reaches what the question's
                    cannot, and the §13 oracle has a practical approximation.
      <= 0       -> dead. Report and stop; HyDE is not the bridge either, and
                    the reformulation family is exhausted.
  B2 SECONDARY -- the context trade, which is what §15 says actually matters.
      Baseline needs k=55 for 139/158 (§16). If HyDE reaches that at k=28, one
      small generation buys a ~2x context saving. Reported as the k at which
      each arm crosses 139/158.
  B3 -- reported in BOTH directions (§18.6), including the real possibility that
      HyDE is WORSE: a hypothetical passage can drift off-topic and retrieve a
      confidently wrong neighbourhood. A negative result here is a result.

  PREDICTION: HyDE beats baseline on the hard core, because a generated passage
  about Berlin's two concepts of liberty is likely to name MacCallum while the
  question cannot. I expect it to be WEAKER on total coverage than the k lever,
  since k=80 already reaches 90.5%. Recorded so both halves can be wrong.
"""
import json, re, subprocess, sys, time, urllib.request
from pathlib import Path

D = Path("/home/alexbryan/dev/commonwealth-ai/sovereign/bench/sep_atlas/map-conversion-rung6")
GEN = "http://127.0.0.1:9741/v1/chat/completions"
MODEL = "Qwen3.5-4B-UD-MTP-Q6_K_XL"
POOL_K = 80
NS = [10, 20, 28, 40, 55, 80]

def toks(f): return [t for t in f.lower().replace("-", " ").split() if len(t) >= 3]
def hit(f, hay): return all(t in hay.lower() for t in toks(f))

def retrieve(q, k=POOL_K):
    q = " ".join(q.split())[:1200]      # the CLI takes one argv; keep it one line
    p = subprocess.run(["sovereign", "corpus", "search", "sep", q, "--limit", str(k)],
                       capture_output=True, text=True, timeout=300)
    return [x.strip() for x in re.split(r"^\s*\d+\.\s+\[", p.stdout or "", flags=re.M)[1:]]

def hypothesise(question):
    body = {"model": MODEL, "temperature": 0.3, "max_tokens": 220,
            "chat_template_kwargs": {"enable_thinking": False},
            "messages": [{"role": "user",
                          "content": f"Write a passage that answers this question.\n\n{question}"}]}
    req = urllib.request.Request(GEN, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=180) as r:
        return json.load(r)["choices"][0]["message"]["content"].strip()

R = {r["question_id"]: r for r in json.load(open(D / "armA.json"))["results"]}
QS = sorted(R)
hard = {}
ARMS4 = ("armA", "armA2", "armB", "armB2")
RA = {a: {r["question_id"]: r for r in json.load(open(D / f"{a}.json"))["results"]} for a in ARMS4}
for q in QS:
    miss = None
    for a in ARMS4:
        m = set(x.strip() for x in RA[a][q]["fact_score"]["missing"])
        miss = m if miss is None else (miss & m)
    if miss: hard[q] = sorted(miss)

CKPT = D / "hyde_pools.json"
data = json.loads(CKPT.read_text()) if CKPT.exists() else {}
gen_wall = 0.0
for i, q in enumerate(QS, 1):
    if q in data: continue
    rec = R[q]
    try:
        t0 = time.time(); hyp = hypothesise(rec["question"]); gen_wall += time.time() - t0
    except Exception as e:
        print(f"[{i}/{len(QS)}] {q}  GENERATION FAILED: {str(e)[:80]}", flush=True)
        continue                                   # never a silent substitution (§18.3)
    data[q] = {
        "hyp": hyp,
        "baseline": retrieve(rec["question"]),
        "hyde": retrieve(hyp),
        "hyde_concat": retrieve(rec["question"] + " " + hyp),
    }
    CKPT.write_text(json.dumps(data))
    print(f"[{i}/{len(QS)}] {q}  hyp {len(hyp)} chars", flush=True)

done = [q for q in QS if q in data]
if len(done) < len(QS):
    print(f"\n{len(QS)-len(done)} question(s) unscorable — excluded, never defaulted")

facts = {q: ([x.strip() for x in R[q]["fact_score"]["matched"]] +
             [x.strip() for x in R[q]["fact_score"]["missing"]]) for q in done}
tot = sum(len(facts[q]) for q in done)
hard_n = sum(len(hard.get(q, [])) for q in done)

def cov(arm, k):
    return sum(sum(1 for f in facts[q] if hit(f, "\n".join(data[q][arm][:k]))) for q in done)
def hardcov(arm, k):
    return sum(sum(1 for f in hard.get(q, []) if hit(f, "\n".join(data[q][arm][:k]))) for q in done)
def chars(arm, k):
    return sum(len("\n".join(data[q][arm][:k])) for q in done)

print(f"\n{len(done)} questions, {tot} expected facts, {hard_n} hard-core, "
      f"{gen_wall/max(1,len(done)):.2f}s/question generation\n")
print(f"{'k':>4}  " + "  ".join(f"{a:>22}" for a in ("baseline", "hyde", "hyde_concat")))
for k in NS:
    cells = []
    for a in ("baseline", "hyde", "hyde_concat"):
        cells.append(f"{cov(a,k):>4}/{tot} {cov(a,k)/tot:>6.1%} h{hardcov(a,k)}/{hard_n}")
    print(f"{k:>4}  " + "  ".join(f"{c:>22}" for c in cells))

print("\n=== B1 — hard core at k=28, against the paired baseline ===")
b, h, c = hardcov("baseline", 28), hardcov("hyde", 28), hardcov("hyde_concat", 28)
print(f"  baseline {b}/{hard_n}   hyde {h}/{hard_n} ({h-b:+d})   hyde_concat {c}/{hard_n} ({c-b:+d})")
best = max(h, c); delta = best - b
print(f"  best HyDE variant vs baseline: {delta:+d}   (bar: >= +2 live, <= 0 dead)")
print(f"  VERDICT: {'LIVE' if delta>=2 else 'DEAD' if delta<=0 else 'WEAK'}")

print("\n=== B2 — the context trade (baseline needs k=55 for 139/158) ===")
TARGET = 139
for a in ("baseline", "hyde", "hyde_concat"):
    kx = next((k for k in NS if cov(a, k) >= TARGET), None)
    print(f"  {a:<12} reaches {TARGET}/{tot} at k={kx}"
          f"{'  (' + format(chars(a,kx), ',') + ' chars)' if kx else '  — never within k=80'}")
